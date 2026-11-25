use crate::{
    Tab,
    commands::{Availability, Command, Group, on_repo},
    diff_doc::{DiffDoc, Kind},
    notice::Notice,
    repository::Repository,
    settings::{self, Settings},
    theme,
    view::{self, Selection, Tone},
};
use gpui::{Context, Window};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use syntect::{
    easy::ScopeRangeIterator,
    parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

pub(crate) const FILE_LIMIT: u64 = 512 * 1024;
pub(crate) const LINE_LIMIT: usize = 2000;
const JOB_LIMIT: u64 = 4 * 1024 * 1024;
const KEPT: usize = 32;
const CHECK_EVERY: usize = 256;

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "view.colour-code",
    Group::View,
    |ui, _| if ui.syntax.on { "Show code without colours" } else { "Colour code by language" }.into(),
    |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::Syntax(Action::Toggle)),
)
.words(&["syntax highlighting", "toggle syntax highlighting", "colours", "colors", "language"])];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Token {
    Keyword,
    String,
    Comment,
    Number,
    Type,
    Function,
    Constant,
    Punctuation,
    Tag,
    Attribute,
}

impl Token {
    pub(crate) const ALL: [Token; 10] = [
        Token::Keyword,
        Token::String,
        Token::Comment,
        Token::Number,
        Token::Type,
        Token::Function,
        Token::Constant,
        Token::Punctuation,
        Token::Tag,
        Token::Attribute,
    ];

    pub(crate) fn color(self) -> u32 {
        match self {
            Token::Keyword => theme::SYNTAX_KEYWORD,
            Token::String => theme::SYNTAX_STRING,
            Token::Comment => theme::SYNTAX_COMMENT,
            Token::Number => theme::SYNTAX_NUMBER,
            Token::Type => theme::SYNTAX_TYPE,
            Token::Function => theme::SYNTAX_FUNCTION,
            Token::Constant => theme::SYNTAX_CONSTANT,
            Token::Punctuation => theme::SYNTAX_PUNCTUATION,
            Token::Tag => theme::SYNTAX_TAG,
            Token::Attribute => theme::SYNTAX_ATTRIBUTE,
        }
    }
}

pub(crate) type Spans = Vec<(Range<usize>, Token)>;

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Toggle,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) on: bool,
    pub(crate) pending: bool,
    pub(crate) highlighted: usize,
    pub(crate) too_large: usize,
    cancel: Option<Arc<AtomicBool>>,
}

impl State {
    pub(crate) fn new(on: bool) -> Self {
        Self { on, ..Self::default() }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.pending = false;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Stored(String),
    Working(PathBuf),
}

#[derive(Clone, Debug)]
pub(crate) struct Part {
    side: Side,
    key: Option<String>,
    rows: Vec<Wanted>,
}

#[derive(Clone, Copy, Debug)]
struct Wanted {
    row: usize,
    line: u32,
    hash: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    path: PathBuf,
    hint: String,
    parts: Vec<Part>,
}

#[derive(Default)]
pub(crate) struct Outcome {
    pub(crate) rows: Vec<(usize, Spans)>,
    pub(crate) highlighted: usize,
    pub(crate) too_large: usize,
}

struct ColouredLine {
    hash: u64,
    spans: Spans,
}

struct Coloured {
    lines: Vec<ColouredLine>,
    complete: bool,
}

impl Coloured {
    fn covers(&self, line: u32) -> bool {
        self.complete || self.lines.len() >= line as usize
    }
}

#[derive(Default)]
struct Cache {
    entries: Vec<((String, String), Arc<Coloured>)>,
}

impl Cache {
    fn get(&mut self, key: &(String, String), upto: u32) -> Option<Arc<Coloured>> {
        let at = self.entries.iter().position(|(held, coloured)| held == key && coloured.covers(upto))?;
        let entry = self.entries.remove(at);
        let coloured = entry.1.clone();
        self.entries.push(entry);
        Some(coloured)
    }

    fn put(&mut self, key: (String, String), coloured: Arc<Coloured>) {
        self.entries.retain(|(held, _)| *held != key);
        self.entries.push((key, coloured));
        if self.entries.len() > KEPT {
            let extra = self.entries.len() - KEPT;
            self.entries.drain(..extra);
        }
    }
}

fn cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn grammars() -> &'static SyntaxSet {
    static GRAMMARS: OnceLock<SyntaxSet> = OnceLock::new();
    GRAMMARS.get_or_init(SyntaxSet::load_defaults_newlines)
}

pub(crate) fn line_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn absent(id: &str) -> bool {
    id.bytes().all(|b| b == b'0')
}

fn hint(path: &Path) -> String {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => format!(".{}", extension.to_ascii_lowercase()),
        None => path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

fn alias(extension: &str) -> &str {
    match extension {
        "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" => "js",
        "h" => "c",
        other => other,
    }
}

pub(crate) fn grammar<'a>(set: &'a SyntaxSet, path: &Path, first_line: Option<&str>) -> Option<&'a SyntaxReference> {
    let name = path.file_name()?.to_str()?;
    let by_name = set.find_syntax_by_extension(name).or_else(|| {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        set.find_syntax_by_extension(alias(&extension))
    });
    let found = match by_name {
        Some(found) => Some(found),
        None if path.extension().is_none() => first_line.and_then(|line| set.find_syntax_by_first_line(line)),
        None => None,
    };
    found.filter(|syntax| syntax.name != "Plain Text")
}

pub(crate) fn requests(doc: &DiffDoc, working: Option<&Path>) -> Vec<Request> {
    let mut requests = Vec::new();
    for file in &doc.files {
        let Some(path) = &file.path else { continue };
        let (mut old_rows, mut new_rows) = (Vec::new(), Vec::new());
        for row in file.rows.clone() {
            let Some(line) = doc.lines.get(row) else { break };
            let hash = || line_hash(doc.code(line));
            match (line.kind, line.old, line.new) {
                (Kind::Deleted, Some(number), _) => old_rows.push(Wanted { row, line: number, hash: hash() }),
                (Kind::Added | Kind::Context, _, Some(number)) => new_rows.push(Wanted { row, line: number, hash: hash() }),
                _ => {}
            }
        }
        let mut parts = Vec::new();
        if let Some(id) = file.old_blob.as_deref().filter(|id| !absent(id) && !old_rows.is_empty()) {
            parts.push(Part { side: Side::Stored(id.to_owned()), key: Some(id.to_owned()), rows: old_rows });
        }
        if let Some(id) = file.new_blob.as_deref().filter(|id| !absent(id) && !new_rows.is_empty()) {
            let side = match working {
                Some(root) => Side::Working(root.join(path)),
                None => Side::Stored(id.to_owned()),
            };
            parts.push(Part { side, key: Some(id.to_owned()), rows: new_rows });
        }
        if !parts.is_empty() {
            requests.push(Request { hint: hint(path), path: path.clone(), parts });
        }
    }
    requests
}

fn upto(part: &Part) -> u32 {
    part.rows.iter().map(|wanted| wanted.line).max().unwrap_or(0)
}

fn place(part: &Part, coloured: &Coloured, rows: &mut Vec<(usize, Spans)>) {
    for wanted in &part.rows {
        let Some(line) = (wanted.line as usize).checked_sub(1).and_then(|index| coloured.lines.get(index)) else {
            continue;
        };
        if line.hash == wanted.hash && !line.spans.is_empty() {
            rows.push((wanted.row, line.spans.clone()));
        }
    }
}

pub(crate) fn from_cache(doc: &mut DiffDoc, requests: Vec<Request>) -> Vec<Request> {
    let mut rows = Vec::new();
    let mut rest = Vec::new();
    {
        let mut cache = cache();
        for mut request in requests {
            request.parts.retain(|part| {
                let Some(id) = &part.key else { return true };
                match cache.get(&(id.clone(), request.hint.clone()), upto(part)) {
                    Some(coloured) => {
                        place(part, &coloured, &mut rows);
                        false
                    }
                    None => true,
                }
            });
            if !request.parts.is_empty() {
                rest.push(request);
            }
        }
    }
    apply(doc, rows);
    rest
}

pub(crate) fn apply(doc: &mut DiffDoc, rows: Vec<(usize, Spans)>) {
    for (row, spans) in rows {
        if let Some(line) = doc.lines.get_mut(row) {
            line.syntax = spans;
        }
    }
}

pub(crate) fn clear(doc: &mut DiffDoc) {
    for line in &mut doc.lines {
        line.syntax = Vec::new();
    }
}

fn read(repo: &Repository, requests: &[Request], outcome: &mut Outcome) -> HashMap<Side, Vec<u8>> {
    let stored: Vec<String> = requests
        .iter()
        .flat_map(|request| &request.parts)
        .filter_map(|part| match &part.side {
            Side::Stored(id) => Some(id.clone()),
            Side::Working(_) => None,
        })
        .collect();
    let sizes = if stored.is_empty() { HashMap::new() } else { repo.object_sizes(&stored).unwrap_or_default() };
    let mut budget = JOB_LIMIT;
    let mut wanted = Vec::new();
    let mut working = Vec::new();
    for part in requests.iter().flat_map(|request| &request.parts) {
        let size = match &part.side {
            Side::Stored(id) => sizes.get(id).copied(),
            Side::Working(path) => std::fs::symlink_metadata(path)
                .ok()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len()),
        };
        let Some(size) = size else { continue };
        if size > FILE_LIMIT {
            outcome.too_large += 1;
            continue;
        }
        if size > budget {
            continue;
        }
        budget -= size;
        match &part.side {
            Side::Stored(id) => wanted.push(id.clone()),
            Side::Working(path) => working.push(path.clone()),
        }
    }
    let mut contents: HashMap<Side, Vec<u8>> = if wanted.is_empty() {
        HashMap::new()
    } else {
        repo.blobs(&wanted)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, bytes)| (Side::Stored(id), bytes))
            .collect()
    };
    for path in working {
        if let Ok(bytes) = std::fs::read(&path)
            && bytes.len() as u64 <= FILE_LIMIT
        {
            contents.insert(Side::Working(path), bytes);
        }
    }
    contents
}

pub(crate) fn colour(repo: &Repository, requests: &[Request], cancel: &AtomicBool) -> Outcome {
    let mut outcome = Outcome::default();
    let set = grammars();
    let named: Vec<bool> = requests.iter().map(|request| grammar(set, &request.path, None).is_some()).collect();
    let readable: Vec<Request> = requests
        .iter()
        .zip(&named)
        .filter(|(request, named)| **named || request.path.extension().is_none())
        .map(|(request, _)| request.clone())
        .collect();
    let contents = read(repo, &readable, &mut outcome);
    for request in &readable {
        for part in &request.parts {
            if cancel.load(Ordering::Relaxed) {
                return outcome;
            }
            let Some(bytes) = contents.get(&part.side) else { continue };
            let text = String::from_utf8_lossy(bytes);
            let Some(syntax) = grammar(set, &request.path, text.lines().next()) else { continue };
            let Some(coloured) = colour_text(set, syntax, &text, upto(part), cancel) else { return outcome };
            outcome.highlighted += 1;
            place(part, &coloured, &mut outcome.rows);
            if let Some(id) = &part.key {
                cache().put((id.clone(), request.hint.clone()), Arc::new(coloured));
            }
        }
    }
    outcome
}

#[derive(Clone, Copy)]
enum Class {
    Strong(Token),
    Weak(Token),
}

fn rules() -> &'static [(Scope, Class)] {
    static RULES: OnceLock<Vec<(Scope, Class)>> = OnceLock::new();
    RULES.get_or_init(|| {
        use Class::{Strong, Weak};
        [
            ("comment", Strong(Token::Comment)),
            ("string", Strong(Token::String)),
            ("markup.raw", Strong(Token::String)),
            ("constant.numeric", Strong(Token::Number)),
            ("constant", Strong(Token::Constant)),
            ("support.constant", Strong(Token::Constant)),
            ("variable.other.constant", Strong(Token::Constant)),
            ("entity.name.constant", Strong(Token::Constant)),
            ("variable.language", Strong(Token::Keyword)),
            ("keyword", Strong(Token::Keyword)),
            ("keyword.operator", Strong(Token::Punctuation)),
            ("keyword.operator.word", Strong(Token::Keyword)),
            ("storage", Strong(Token::Keyword)),
            ("entity.name.section", Strong(Token::Keyword)),
            ("markup.heading", Strong(Token::Keyword)),
            ("entity.name", Strong(Token::Type)),
            ("entity.other.inherited-class", Strong(Token::Type)),
            ("support.type", Strong(Token::Type)),
            ("support.class", Strong(Token::Type)),
            ("entity.name.function", Strong(Token::Function)),
            ("support.function", Strong(Token::Function)),
            ("variable.function", Strong(Token::Function)),
            ("entity.name.tag", Strong(Token::Tag)),
            ("entity.other.attribute-name", Strong(Token::Attribute)),
            ("support.type.property-name", Strong(Token::Attribute)),
            ("meta.attribute", Weak(Token::Attribute)),
            ("meta.annotation", Weak(Token::Attribute)),
            ("punctuation", Strong(Token::Punctuation)),
            ("punctuation.definition", Weak(Token::Punctuation)),
        ]
        .into_iter()
        .filter_map(|(name, class)| Some((Scope::new(name).ok()?, class)))
        .collect()
    })
}

fn class(scope: Scope, memo: &mut HashMap<Scope, Option<Class>>) -> Option<Class> {
    *memo.entry(scope).or_insert_with(|| {
        rules()
            .iter()
            .filter(|(prefix, _)| prefix.is_prefix_of(scope))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, class)| *class)
    })
}

fn token(stack: &[Scope], memo: &mut HashMap<Scope, Option<Class>>) -> Option<Token> {
    let mut weak = None;
    for scope in stack.iter().rev() {
        match class(*scope, memo) {
            Some(Class::Strong(token)) => return Some(token),
            Some(Class::Weak(token)) => weak = weak.or(Some(token)),
            None => {}
        }
    }
    weak
}

fn too_long(line: &str) -> bool {
    line.len() > LINE_LIMIT && line.chars().count() > LINE_LIMIT
}

fn colour_text(set: &SyntaxSet, syntax: &SyntaxReference, text: &str, upto: u32, cancel: &AtomicBool) -> Option<Coloured> {
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut memo = HashMap::new();
    let mut lines = Vec::new();
    let mut complete = true;
    for (index, line) in LinesWithEndings::from(text).enumerate() {
        if index >= upto as usize {
            complete = false;
            break;
        }
        if index % CHECK_EVERY == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        let hash = line_hash(content);
        if too_long(content) {
            lines.push(ColouredLine { hash, spans: Vec::new() });
            continue;
        }
        let Ok(ops) = state.parse_line(line, set) else { break };
        let mut spans: Spans = Vec::new();
        for (range, op) in ScopeRangeIterator::new(&ops, line) {
            if stack.apply(op).is_err() {
                break;
            }
            let range = range.start.min(content.len())..range.end.min(content.len());
            if range.is_empty() {
                continue;
            }
            let Some(token) = token(stack.as_slice(), &mut memo) else { continue };
            match spans.last_mut() {
                Some((last, held)) if *held == token && last.end == range.start => last.end = range.end,
                _ => spans.push((range, token)),
            }
        }
        lines.push(ColouredLine { hash, spans });
    }
    Some(Coloured { lines, complete })
}

pub(crate) fn colour_lines(path: &Path, text: &str, cancel: &AtomicBool) -> Option<Vec<Spans>> {
    if text.len() as u64 > FILE_LIMIT {
        return None;
    }
    let set = grammars();
    let syntax = grammar(set, path, text.lines().next())?;
    let coloured = colour_text(set, syntax, text, u32::MAX, cancel)?;
    Some(coloured.lines.into_iter().map(|line| line.spans).collect())
}

pub(crate) fn working_root(repo: &Repository, selection: &Selection) -> Option<PathBuf> {
    matches!(selection, Selection::File { staged: false, .. }).then(|| repo.root.clone())
}

pub(crate) fn run(tab: &mut Tab, requests: Vec<Request>, cx: &mut Context<Tab>) {
    tab.ui.syntax.stop();
    let Some(repo) = tab.repo.clone() else { return };
    if requests.is_empty() {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    tab.ui.syntax.cancel = Some(cancel.clone());
    tab.ui.syntax.pending = true;
    let (epoch, ticket) = (tab.epoch, tab.diff_ticket);
    Tab::work(
        cx,
        {
            let cancel = cancel.clone();
            move || colour(&repo, &requests, &cancel)
        },
        move |tab, outcome, cx| {
            if cancel.load(Ordering::Relaxed) || tab.epoch != epoch || tab.diff_ticket != ticket {
                return;
            }
            tab.ui.syntax.cancel = None;
            tab.ui.syntax.pending = false;
            tab.ui.syntax.highlighted += outcome.highlighted;
            tab.ui.syntax.too_large += outcome.too_large;
            apply(&mut tab.ui.diff, outcome.rows);
            cx.notify();
        },
    );
}

pub(crate) fn start(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(repo), Some(selection)) = (tab.repo.clone(), tab.ui.selection.clone()) else { return };
    let working = if tab.ui.diff.file.is_some() {
        crate::file_view::working_root(&tab.ui.diff, &repo.root)
    } else {
        working_root(&repo, &selection)
    };
    let wanted = requests(&tab.ui.diff, working.as_deref());
    let rest = from_cache(&mut tab.ui.diff, wanted);
    run(tab, rest, cx);
}

pub(crate) fn follow(tab: &mut Tab, on: bool, cx: &mut Context<Tab>) {
    tab.ui.syntax.on = on;
    if on {
        start(tab, cx);
    } else {
        tab.ui.syntax.stop();
        clear(&mut tab.ui.diff);
    }
    crate::merge::colour(tab, cx);
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Toggle => {
            let on = !settings::settings(cx).colour_code;
            Settings::update(cx, |s| s.colour_code = on);
            follow(tab, on, cx);
            let title = if on { "Code is coloured by language" } else { "Code is shown without colours" };
            tab.ui.notices.show(Notice::new(Tone::Info, title, ""));
        }
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;
    use crate::repository::tests::Fixture;

    fn spans_of(text: &str, path: &str) -> Vec<Spans> {
        let set = grammars();
        let syntax = grammar(set, Path::new(path), text.lines().next()).expect("a grammar");
        colour_text(set, syntax, text, u32::MAX, &AtomicBool::new(false))
            .unwrap()
            .lines
            .into_iter()
            .map(|line| line.spans)
            .collect()
    }

    fn write(f: &Fixture, name: &str, text: &str) {
        std::fs::write(f.dir.join(name), text).unwrap();
    }

    fn tokens_in<'a>(line: &'a str, spans: &Spans) -> Vec<(&'a str, Token)> {
        spans.iter().map(|(range, token)| (&line[range.clone()], *token)).collect()
    }

    #[test]
    fn grammars_come_from_the_file_name_then_the_first_line_of_a_file_without_an_extension() {
        let set = grammars();
        let name = |path: &str, first: Option<&str>| grammar(set, Path::new(path), first).map(|syntax| syntax.name.clone());
        assert_eq!(name("src/main.rs", None).as_deref(), Some("Rust"));
        assert_eq!(name("lib/Tool.PY", None).as_deref(), Some("Python"));
        assert_eq!(name("Makefile", None).as_deref(), Some("Makefile"));
        assert_eq!(name("web/app.ts", None).as_deref(), Some("JavaScript"));
        assert_eq!(name("bin/deploy", Some("#!/usr/bin/env python3")).as_deref(), Some("Python"));
        assert_eq!(name("bin/run", Some("#!/bin/sh")).as_deref(), Some("Bourne Again Shell (bash)"));
        assert_eq!(name("notes.txt", None), None);
        assert_eq!(name("notes.unknown", Some("#!/bin/sh")), None);
        assert_eq!(name("README", Some("Just words")), None);
    }

    #[test]
    fn scopes_map_onto_the_theme_tokens() {
        let text = "// note\nfn main() {\n    let count: u32 = 42;\n    println!(\"hi {}\", count);\n}\n";
        let spans = spans_of(text, "main.rs");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(tokens_in(lines[0], &spans[0]), [("// note", Token::Comment)]);
        let second = tokens_in(lines[1], &spans[1]);
        assert!(second.contains(&("fn", Token::Keyword)), "{second:?}");
        assert!(second.contains(&("main", Token::Function)), "{second:?}");
        let third = tokens_in(lines[2], &spans[2]);
        assert!(third.contains(&("let", Token::Keyword)), "{third:?}");
        assert!(third.contains(&("42", Token::Number)), "{third:?}");
        let fourth = tokens_in(lines[3], &spans[3]);
        assert!(fourth.contains(&("\"hi ", Token::String)) || fourth.iter().any(|(text, token)| text.starts_with('"') && *token == Token::String), "{fourth:?}");
        for line in &spans {
            for pair in line.windows(2) {
                assert!(pair[0].0.end <= pair[1].0.start, "{line:?}");
            }
        }
        let colours: std::collections::HashSet<u32> = Token::ALL.iter().map(|token| token.color()).collect();
        assert_eq!(colours.len(), Token::ALL.len());
    }

    #[test]
    fn a_line_inside_a_block_comment_is_coloured_from_the_whole_file() {
        let text = "/*\n first\n second\n*/\nint x = 1;\n";
        let spans = spans_of(text, "a.c");
        assert_eq!(tokens_in(" second", &spans[2]), [(" second", Token::Comment)]);
        assert!(tokens_in("int x = 1;", &spans[4]).contains(&("int", Token::Keyword)));
    }

    #[test]
    fn long_lines_stay_plain_and_colouring_stops_at_the_last_line_wanted() {
        let long = format!("let s = \"{}\";", "x".repeat(LINE_LIMIT));
        let text = format!("let a = 1;\n{long}\nlet b = 2;\nlet c = 3;\n");
        let set = grammars();
        let syntax = grammar(set, Path::new("a.rs"), None).unwrap();
        let coloured = colour_text(set, syntax, &text, 3, &AtomicBool::new(false)).unwrap();
        assert_eq!(coloured.lines.len(), 3);
        assert!(!coloured.complete);
        assert!(coloured.lines[1].spans.is_empty());
        assert!(!coloured.lines[2].spans.is_empty());
        assert!(colour_text(set, syntax, &text, 3, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn the_cache_keeps_the_most_recent_entries_and_answers_only_when_it_covers_the_line() {
        let mut cache = Cache::default();
        let entry = |lines: usize, complete| {
            Arc::new(Coloured { lines: (0..lines).map(|_| ColouredLine { hash: 0, spans: Vec::new() }).collect(), complete })
        };
        for index in 0..KEPT + 2 {
            cache.put((format!("{index}"), ".rs".into()), entry(10, true));
        }
        assert_eq!(cache.entries.len(), KEPT);
        assert!(cache.get(&("0".into(), ".rs".into()), 1).is_none());
        assert!(cache.get(&("2".into(), ".rs".into()), 1).is_some());
        assert_eq!(cache.entries.last().map(|(key, _)| key.0.as_str()), Some("2"));
        assert!(cache.get(&("2".into(), ".py".into()), 1).is_none());
        cache.put(("part".into(), ".rs".into()), entry(5, false));
        assert!(cache.get(&("part".into(), ".rs".into()), 5).is_some());
        assert!(cache.get(&("part".into(), ".rs".into()), 6).is_none());
    }

    #[test]
    fn requests_ask_each_side_for_its_own_rows() {
        let old = "1111111111111111111111111111111111111111";
        let new = "2222222222222222222222222222222222222222";
        let text = format!(
            "diff --git a/src/a.rs b/src/a.rs\nindex {old}..{new} 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,2 +3,2 @@\n keep\n-gone\n+came\n"
        );
        let doc = doc(&text);
        assert_eq!(doc.files.len(), 1);
        assert_eq!(doc.files[0].path.as_deref(), Some(Path::new("src/a.rs")));
        let requests = requests(&doc, None);
        assert_eq!(requests.len(), 1);
        let parts = &requests[0].parts;
        assert_eq!(parts[0].side, Side::Stored(old.into()));
        assert_eq!(parts[0].rows.iter().map(|wanted| (wanted.row, wanted.line)).collect::<Vec<_>>(), [(2, 4)]);
        assert_eq!(parts[1].side, Side::Stored(new.into()));
        assert_eq!(parts[1].rows.iter().map(|wanted| (wanted.row, wanted.line)).collect::<Vec<_>>(), [(1, 3), (3, 4)]);
        assert_eq!(parts[1].rows[1].hash, line_hash("came"));
        let working = super::requests(&doc, Some(Path::new("/repo")));
        assert_eq!(working[0].parts[1].side, Side::Working(PathBuf::from("/repo/src/a.rs")));
        let added = doc_with_new_file(new);
        assert_eq!(super::requests(&added, None)[0].parts.len(), 1);
    }

    fn doc_with_new_file(new: &str) -> DiffDoc {
        doc(&format!(
            "diff --git a/b.py b/b.py\nnew file mode 100644\nindex 0000000000000000000000000000000000000000..{new}\n--- /dev/null\n+++ b/b.py\n@@ -0,0 +1 @@\n+x = 1\n"
        ))
    }

    #[test]
    fn colouring_reads_both_sides_from_git_and_the_working_file_and_leaves_large_files_plain() {
        let f = Fixture::new();
        let start = "/* opening\n middle\n*/\nfn keep() {}\n";
        let staged = "/* opening\n changed\n*/\nfn keep() {}\n";
        let working = "/* opening\n changed\n*/\nfn kept() {}\n";
        write(&f, "code.rs", start);
        f.git(&["add", "code.rs"]);
        f.git(&["commit", "-qm", "code"]);
        write(&f, "code.rs", staged);
        f.git(&["add", "code.rs"]);
        write(&f, "code.rs", working);
        let large = format!("fn big() {{}}\n{}", "// filler line for size\n".repeat(25_000));
        write(&f, "large.rs", &large);
        f.git(&["add", "large.rs"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap();
        let index = String::from_utf8(f.git(&["ls-files", "-s"])).unwrap();

        let cancel = AtomicBool::new(false);
        let staged_diff = f.repo.file_diff(Path::new("code.rs"), true).unwrap();
        let mut staged_doc = doc(&staged_diff.text);
        let wanted = requests(&staged_doc, None);
        let outcome = colour(&f.repo, &wanted, &cancel);
        assert_eq!((outcome.highlighted, outcome.too_large), (2, 0));
        apply(&mut staged_doc, outcome.rows);
        let middle = staged_doc.lines.iter().find(|line| staged_doc.text(line) == "- middle").unwrap();
        assert_eq!(middle.syntax, [(0..7, Token::Comment)]);
        let changed = staged_doc.lines.iter().find(|line| staged_doc.text(line) == "+ changed").unwrap();
        assert_eq!(changed.syntax, [(0..8, Token::Comment)]);

        let mut again = doc(&staged_diff.text);
        let wanted = requests(&again, None);
        assert!(from_cache(&mut again, wanted).is_empty());
        assert_eq!(again.lines.iter().map(|line| line.syntax.clone()).collect::<Vec<_>>(), staged_doc.lines.iter().map(|line| line.syntax.clone()).collect::<Vec<_>>());

        let unstaged_diff = f.repo.file_diff(Path::new("code.rs"), false).unwrap();
        let mut unstaged_doc = doc(&unstaged_diff.text);
        let wanted = requests(&unstaged_doc, Some(&f.dir));
        assert!(matches!(wanted[0].parts.last().map(|part| &part.side), Some(Side::Working(_))));
        let outcome = colour(&f.repo, &wanted, &cancel);
        apply(&mut unstaged_doc, outcome.rows);
        let kept = unstaged_doc.lines.iter().find(|line| unstaged_doc.text(line) == "+fn kept() {}").unwrap();
        assert!(kept.syntax.contains(&(0..2, Token::Keyword)), "{:?}", kept.syntax);

        let large_diff = f.repo.file_diff(Path::new("large.rs"), true).unwrap();
        let large_doc = doc(&large_diff.text);
        let outcome = colour(&f.repo, &requests(&large_doc, None), &cancel);
        assert_eq!((outcome.highlighted, outcome.too_large, outcome.rows.len()), (0, 1, 0));

        assert_eq!(String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap(), head);
        assert_eq!(String::from_utf8(f.git(&["ls-files", "-s"])).unwrap(), index);
        assert_eq!(std::fs::read_to_string(f.dir.join("code.rs")).unwrap(), working);
    }

    #[test]
    fn a_row_whose_text_differs_from_the_file_stays_plain() {
        let f = Fixture::new();
        write(&f, "a.rs", "fn one() {}\n");
        f.git(&["add", "a.rs"]);
        f.git(&["commit", "-qm", "one"]);
        write(&f, "a.rs", "fn two() {}\n");
        let diff = f.repo.file_diff(Path::new("a.rs"), false).unwrap();
        write(&f, "a.rs", "let changed_since = 1;\n");
        let mut doc = doc(&diff.text);
        let outcome = colour(&f.repo, &requests(&doc, Some(&f.dir)), &AtomicBool::new(false));
        apply(&mut doc, outcome.rows);
        let added = doc.lines.iter().find(|line| doc.text(line) == "+fn two() {}").unwrap();
        assert!(added.syntax.is_empty());
        let removed = doc.lines.iter().find(|line| doc.text(line) == "-fn one() {}").unwrap();
        assert!(!removed.syntax.is_empty());
    }
}
