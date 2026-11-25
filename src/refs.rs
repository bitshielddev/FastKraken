use crate::repository::Run;
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    ops::Range,
    os::unix::ffi::{OsStrExt, OsStringExt},
};

use gpui::{
    App, Context, Div, Entity, Focusable, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::{
    Tab, kit,
    commands::Target,
    repository::{Repository, checked, validate_oid},
    theme::*,
    view::{self, CommitInput, Desk, Menu, Selection},
};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, busy, commit, operating, on_repo, unselected};
    fn here(ui: &view::Ui, target: &Target) -> Availability {
        match commit(target) {
            Some(oid) => Availability::unless(
                busy(ui).or_else(|| operating(ui)).or_else(|| unselected(ui, oid)),
            ),
            None => Availability::Hidden,
        }
    }
    fn from_ref(ui: &view::Ui, target: &Target) -> Availability {
        if ui.refs.oid_of(target).is_none() {
            return Availability::Hidden;
        }
        Availability::unless(busy(ui).or_else(|| operating(ui)))
    }
    fn name(target: &Target) -> Option<(&OsString, &'static str)> {
        match target {
            Target::Branch(name) | Target::RemoteBranch(name) => Some((name, "branch name")),
            Target::Tag(name) => Some((name, "tag name")),
            _ => None,
        }
    }
    &[
        Command::new(
            "commit.branch",
            Group::Create,
            |_, _| "Create branch here…".into(),
            |ui, target, _| here(ui, target),
            |_, _| Some(view::Action::Refs(Action::New(Kind::Branch))),
        )
        .icon("branch"),
        Command::new(
            "commit.tag",
            Group::Create,
            |_, _| "Create tag here…".into(),
            |ui, target, _| here(ui, target),
            |_, _| Some(view::Action::Refs(Action::New(Kind::Tag))),
        )
        .icon("tag"),
        Command::new(
            "ref.branch",
            Group::Create,
            |_, _| "Create branch here…".into(),
            |ui, target, _| from_ref(ui, target),
            |ui, target| {
                let oid = ui.refs.oid_of(target)?.to_owned();
                Some(view::Action::Refs(Action::NewAt(Kind::Branch, oid)))
            },
        )
        .icon("branch"),
        Command::new(
            "branch.switch",
            Group::Switch,
            |_, target| match target {
                Target::Branch(name) => format!("Switch to {}", name.to_string_lossy()),
                _ => String::new(),
            },
            |ui, target, _| {
                let Target::Branch(name) = target else {
                    return Availability::Hidden;
                };
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    (*name == *ui.branch && !ui.detached)
                        .then(|| format!("{} is already checked out", ui.branch))
                }))
            },
            |_, target| match target {
                Target::Branch(name) => Some(view::Action::Refs(Action::Switch(name.clone()))),
                _ => None,
            },
        )
        .icon("branch"),
        Command::new(
            "ref.copy-name",
            Group::Copy,
            |_, target| name(target).map_or_else(String::new, |(_, what)| format!("Copy {what}")),
            |_, target, _| {
                if name(target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |_, target| {
                name(target).map(|(name, what)| view::Action::CopyText {
                    text: name.to_string_lossy().into_owned(),
                    what,
                })
            },
        )
        .icon("copy"),
        Command::new(
            "repo.labels",
            Group::View,
            |ui, _| {
                if show_labels(&ui.refs) { "Hide branch and tag labels" } else { "Show branch and tag labels" }
                    .into()
            },
            |ui, target, _| {
                if on_repo(ui, target) { Availability::unless(operating(ui)) } else { Availability::Hidden }
            },
            |_, _| Some(view::Action::Refs(Action::ToggleLabels)),
        )
        .words(&["tags", "decorations"])
        .icon("tag")
        .bar(Bar::View),
        Command::new(
            "repo.new-branch",
            Group::Create,
            |_, _| "New branch…".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    ui.head.is_none().then(|| "Make the first commit before creating a branch".to_owned())
                }))
            },
            |ui, _| Some(view::Action::Refs(Action::NewAt(Kind::Branch, ui.head.clone()?))),
        )
        .words(&["create branch"])
        .icon("branch")
        .keys("cmd-b")
        .bar(Bar::Branch),
        Command::new(
            "repo.new-tag",
            Group::Create,
            |_, _| "New tag…".into(),
            |ui, target, _| {
                if !on_repo(ui, target) {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    ui.head.is_none().then(|| "Make the first commit before creating a tag".to_owned())
                }))
            },
            |ui, _| Some(view::Action::Refs(Action::NewAt(Kind::Tag, ui.head.clone()?))),
        )
        .words(&["create tag", "release", "version"])
        .icon("tag"),
        Command::new(
            "repo.switch-branch",
            Group::Switch,
            |_, _| "Switch branch…".into(),
            |ui, target, _| {
                if !on_repo(ui, target) || ui.branch.is_empty() {
                    return Availability::Hidden;
                }
                Availability::unless(busy(ui).or_else(|| operating(ui)))
            },
            |_, _| Some(view::Action::Refs(Action::ToggleList)),
        )
        .words(&["checkout", "change branch"])
        .icon("branch")
        .bar(Bar::Branch),
    ]
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Branch,
    Remote,
    Tag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    Branches,
    Remotes,
    Tags,
    Stashes,
}

#[derive(Clone, Debug)]
pub enum Action {
    ToggleList,
    New(Kind),
    NewAt(Kind, String),
    ToggleCheckout,
    TogglePush,
    ToggleLabels,
    Submit,
    Escape,
    Inspect(Kind, OsString),
    Switch(OsString),
    ToggleSection(Section),
    ToggleFolder(String),
    FoldAll { section: Section, fold: bool },
    CreateBranch(String),
}

#[derive(Clone, Debug)]
pub struct NamedRef {
    pub kind: Kind,
    pub name: OsString,
    pub oid: Option<String>,
    pub object: String,
    pub annotation: Option<Annotation>,
    pub ahead: usize,
    pub behind: usize,
    pub upstream: Option<String>,
    pub gone: bool,
    pub time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub subject: String,
    pub tagger: String,
    pub date: String,
}

pub struct Draft {
    kind: Kind,
    oid: String,
    subject: String,
    checkout: bool,
    push: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum SideRow {
    Header {
        section: Section,
        count: usize,
        open: bool,
    },
    Folder {
        depth: usize,
        name: String,
        key: String,
        open: bool,
        remote: bool,
    },
    Ref {
        depth: usize,
        index: usize,
        label: String,
    },
    Stash(usize),
    Note,
}

pub struct State {
    pub input: Entity<CommitInput>,
    pub message: Entity<CommitInput>,
    pub filter: Entity<CommitInput>,
    pub find: Entity<CommitInput>,
    pub items: Vec<NamedRef>,
    pub remotes: Vec<String>,
    pub github: Vec<String>,
    pub addresses: std::collections::BTreeMap<String, crate::remotes::Addresses>,
    pub draft: Option<Draft>,
    pub list_open: bool,
    pub show_labels: bool,
    closed: Vec<Section>,
    folded: HashSet<String>,
    pub(crate) hidden_sections: Vec<Section>,
    pub(crate) picked: Option<crate::picked::Picked<OsString>>,
    pub(crate) primary: Option<OsString>,
    pub(crate) rows: Vec<SideRow>,
    ticket: u64,
    pub(crate) scroll: gpui::UniformListScrollHandle,
    switcher_scroll: gpui::UniformListScrollHandle,
    remote_scroll: gpui::UniformListScrollHandle,
    pick: Option<(String, usize)>,
}

impl State {
    pub fn new<T: 'static>(show_labels: bool, cx: &mut Context<T>) -> Self {
        let mut field = |prompt: &'static str, lines: bool| {
            let input = cx.new(|cx| {
                if lines { CommitInput::multiline(cx, prompt) } else { CommitInput::with_placeholder(cx, prompt) }
            });
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            input
        };
        Self {
            input: field("Name", false),
            message: field("Message (optional), for an annotated tag", true),
            filter: field("Filter", false),
            find: field("Find a branch", false),
            items: Vec::new(),
            remotes: Vec::new(),
            github: Vec::new(),
            addresses: Default::default(),
            draft: None,
            list_open: false,
            show_labels,
            closed: Vec::new(),
            folded: HashSet::new(),
            hidden_sections: Vec::new(),
            picked: None,
            primary: None,
            rows: Vec::new(),
            ticket: 0,
            scroll: Default::default(),
            switcher_scroll: Default::default(),
            remote_scroll: Default::default(),
            pick: None,
        }
    }

    pub(crate) fn restore(&mut self, view: Option<&crate::settings::RepoView>) {
        self.closed = view.map(|view| view.closed.clone()).unwrap_or_default();
        self.folded = view
            .map(|view| view.folded.iter().cloned().collect())
            .unwrap_or_default();
    }

    pub(crate) fn folded(&self, key: &str) -> bool {
        self.folded.contains(key)
    }

    fn remembered(&self) -> (Vec<Section>, Vec<String>) {
        let mut folded: Vec<_> = self.folded.iter().cloned().collect();
        folded.sort();
        (self.closed.clone(), folded)
    }

    pub(crate) fn draft_target(&self) -> Option<(Kind, &str)> {
        self.draft.as_ref().map(|draft| (draft.kind, draft.oid.as_str()))
    }

    pub(crate) fn draft_push(&self) -> Option<bool> {
        self.draft.as_ref().filter(|draft| draft.kind == Kind::Tag).map(|draft| draft.push)
    }

    pub(crate) fn switcher_rows(ui: &view::Ui, cx: &App) -> (Vec<String>, Option<String>, Option<usize>) {
        let found = matches_find(ui, cx).into_iter().map(|item| item.name.to_string_lossy().into_owned()).collect();
        (found, new_name(ui, cx), highlighted(ui, cx))
    }

    pub(crate) fn switcher_remotes(ui: &view::Ui, cx: &App) -> Vec<String> {
        remote_matches(ui, cx).into_iter().map(|item| item.name.to_string_lossy().into_owned()).collect()
    }

    pub fn validation(&self, cx: &App) -> Option<String> {
        let draft = self.draft.as_ref()?;
        let name = self.input.read(cx).text();
        if name.is_empty() {
            return None;
        }
        self.name_problem(draft.kind, name)
    }

    pub(crate) fn name_problem(&self, kind: Kind, name: &str) -> Option<String> {
        name_problem(kind, name, self.items[self.range(kind)].iter().map(|item| item.name.as_os_str()))
    }

    pub(crate) fn rename_problem(&self, old: &OsStr, name: &str) -> Option<String> {
        let others = self.items[self.range(Kind::Branch)]
            .iter()
            .map(|item| item.name.as_os_str())
            .filter(|taken| *taken != old);
        name_problem(Kind::Branch, name, others)
    }

    pub(crate) fn local(&self, name: &OsStr) -> Option<&NamedRef> {
        self.items[self.range(Kind::Branch)].iter().find(|item| item.name == name)
    }

    pub(crate) fn tag(&self, name: &OsStr) -> Option<&NamedRef> {
        self.items[self.range(Kind::Tag)].iter().find(|item| item.name == name)
    }

    fn range(&self, kind: Kind) -> Range<usize> {
        let start = self.items.partition_point(|item| (item.kind as u8) < kind as u8);
        let end = self.items.partition_point(|item| item.kind as u8 <= kind as u8);
        start..end
    }

    pub fn current(&self, branch: &str) -> Option<&NamedRef> {
        self.items[self.range(Kind::Branch)]
            .iter()
            .find(|item| item.name == branch)
    }

    pub fn lists(&self, kind: Kind, name: &str) -> bool {
        self.rows.iter().any(|row| {
            matches!(row, SideRow::Ref { index, .. }
                if self.items.get(*index).is_some_and(|item| item.kind == kind && item.name == name))
        })
    }

    pub fn branch(&self, name: &str) -> Option<&NamedRef> {
        self.items[self.range(Kind::Branch)].iter().find(|item| item.name == name)
    }

    pub fn oid_of(&self, target: &Target) -> Option<&str> {
        let (kind, name) = match target {
            Target::Branch(name) => (Kind::Branch, name),
            Target::RemoteBranch(name) => (Kind::Remote, name),
            Target::Tag(name) => (Kind::Tag, name),
            _ => return None,
        };
        self.items[self.range(kind)]
            .iter()
            .find(|item| item.name == *name)
            .and_then(|item| item.oid.as_deref())
    }
}

pub(crate) fn remote_ref(remotes: &[String], label: &str) -> bool {
    remotes
        .iter()
        .any(|remote| label.strip_prefix(remote.as_str()).is_some_and(|rest| rest.starts_with('/')))
}

pub fn show_labels(state: &State) -> bool {
    state.show_labels
}

pub(crate) fn name_problem<'a>(
    kind: Kind,
    name: &str,
    existing: impl IntoIterator<Item = &'a OsStr>,
) -> Option<String> {
    let (names, one) = match kind {
        Kind::Tag => ("Tag names", "tag"),
        _ => ("Branch names", "branch"),
    };
    let rule = |text: &str| Some(format!("{names} {text}"));
    if name.is_empty() {
        return Some(format!("Type a name for the {one}"));
    }
    if name.chars().any(char::is_whitespace) {
        return rule("cannot contain spaces");
    }
    if name.chars().any(char::is_control) {
        return rule("cannot contain control characters");
    }
    if let Some(c) = name.chars().find(|c| "~^:?*[\\".contains(*c)) {
        return rule(&format!("cannot contain {c}"));
    }
    if name == "HEAD" || name == "@" {
        return Some(format!("A {one} cannot be named {name}"));
    }
    if name.starts_with('-') {
        return rule("cannot start with a hyphen");
    }
    if name.contains("..") {
        return rule("cannot contain two dots in a row");
    }
    if name.contains("@{") {
        return rule("cannot contain @{");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return rule("cannot start or end with a slash, or have two slashes in a row");
    }
    if name.ends_with('.') {
        return rule("cannot end with a dot");
    }
    if name.split('/').any(|part| part.starts_with('.')) {
        return rule("cannot have a part that starts with a dot");
    }
    if name.split('/').any(|part| part.ends_with(".lock")) {
        return rule("cannot have a part that ends in .lock");
    }
    for taken in existing {
        let taken = taken.to_string_lossy();
        if taken == name {
            return Some(format!("A {one} named {name} already exists"));
        }
        if name.strip_prefix(taken.as_ref()).is_some_and(|rest| rest.starts_with('/')) {
            return Some(format!("{name} cannot be created because the {one} {taken} exists"));
        }
        if taken.strip_prefix(name).is_some_and(|rest| rest.starts_with('/')) {
            return Some(format!("{name} cannot be created because the {one} {taken} uses it as a folder"));
        }
    }
    None
}

fn create(
    repo: &Repository,
    kind: Kind,
    name: &str,
    oid: &str,
    checkout: bool,
    message: &str,
) -> Result<(), String> {
    if let Some(error) = name_problem(kind, name, []) {
        return Err(error);
    }
    validate_oid(oid)?;
    let mut cmd = repo.command();
    match kind {
        Kind::Branch if checkout => {
            cmd.args(["switch", "--no-guess", "--no-track", "-c", name, oid])
        }
        Kind::Branch => cmd.args(["branch", "--no-track", "--", name, oid]),
        Kind::Tag if message.trim().is_empty() => cmd.args(["tag", "--no-sign", "--", name, oid]),
        Kind::Tag => cmd
            .args(["tag", "--no-sign", "--annotate", "--cleanup=whitespace", "-m"])
            .arg(message)
            .args(["--", name, oid]),
        Kind::Remote => return Err("Remote branches are created by pushing.".into()),
    };
    checked(&mut cmd).map(|_| ())
}

fn switch(repo: &Repository, name: &OsStr) -> Result<(), String> {
    checked(
        repo.command()
            .args(["switch", "--no-guess", "--"])
            .arg(name),
    )
    .map(|_| ())
}

pub(crate) fn list(repo: &Repository) -> Result<Vec<NamedRef>, String> {
    let records = checked(repo.read_command().args([
        "for-each-ref",
        "--sort=refname",
        "--format=%(refname)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)%00%(upstream)%00%(upstream:track,nobracket)%00%(upstream:remotename)%00%(contents:subject)%00%(taggername)%00%(taggerdate:short)%00%(committerdate:unix)",
        "refs/heads",
        "refs/remotes",
        "refs/tags",
    ]))?;
    let mut items = Vec::new();
    for line in records
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut fields = line.split(|b| *b == 0);
        let (Some(full), Some(ty), Some(target), Some(peeled_ty), Some(peeled)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err("Invalid ref record from Git.".into());
        };
        let upstream = fields.next().unwrap_or_default();
        let track = String::from_utf8_lossy(fields.next().unwrap_or_default());
        let remote = fields.next().unwrap_or_default();
        let upstream = upstream
            .strip_prefix(b"refs/remotes/")
            .filter(|short| !short.is_empty() && !remote.is_empty() && remote != b".")
            .map(|short| String::from_utf8_lossy(short).into_owned());
        let mut text = || String::from_utf8_lossy(fields.next().unwrap_or_default()).into_owned();
        let (subject, tagger, date) = (text(), text(), text());
        let time = text().parse().unwrap_or(0);
        let annotation = (ty == b"tag").then_some(Annotation { subject, tagger, date });
        let (kind, short) = if let Some(short) = full.strip_prefix(b"refs/heads/") {
            (Kind::Branch, short)
        } else if let Some(short) = full.strip_prefix(b"refs/remotes/") {
            if short.ends_with(b"/HEAD") {
                continue;
            }
            (Kind::Remote, short)
        } else if let Some(short) = full.strip_prefix(b"refs/tags/") {
            (Kind::Tag, short)
        } else {
            continue;
        };
        let oid = if ty == b"commit" {
            Some(String::from_utf8_lossy(target).into_owned())
        } else if peeled_ty == b"commit" {
            Some(String::from_utf8_lossy(peeled).into_owned())
        } else if peeled_ty == b"tag" {
            let mut revision = OsString::from_vec(full.to_vec());
            revision.push("^{commit}");
            let out = repo
                .read_command()
                .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
                .arg(revision)
                .run()
                .map_err(|e| e.to_string())?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
        } else {
            None
        };
        let distance = |side: &str| {
            track
                .split(", ")
                .find_map(|part| part.strip_prefix(side)?.trim().parse().ok())
                .unwrap_or(0)
        };
        items.push(NamedRef {
            kind,
            name: OsString::from_vec(short.to_vec()),
            oid,
            object: String::from_utf8_lossy(target).into_owned(),
            annotation,
            ahead: distance("ahead"),
            behind: distance("behind"),
            gone: upstream.is_some() && track == "gone",
            upstream,
            time,
        });
    }
    Ok(items)
}

fn remote_names(repo: &Repository) -> Result<Vec<String>, String> {
    let names = checked(repo.read_command().arg("remote"))?;
    Ok(String::from_utf8_lossy(&names)
        .lines()
        .map(str::to_owned)
        .collect())
}

pub fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let epoch = tab.epoch;
    tab.ui.refs.ticket += 1;
    let ticket = tab.ui.refs.ticket;
    Tab::work(
        cx,
        move || {
            let addresses = crate::remotes::addresses(&repo).unwrap_or_default();
            Ok::<_, String>((list(&repo)?, remote_names(&repo)?, crate::connect::unbound_among(&repo, addresses.clone()), addresses))
        },
        move |tab, result, cx| {
            if tab.epoch != epoch || tab.ui.refs.ticket != ticket {
                return;
            }
            match result {
                Ok((items, remotes, github, addresses)) => {
                    tab.ui.refs.items = items;
                    tab.ui.refs.remotes = remotes;
                    tab.ui.refs.github = github;
                    tab.ui.refs.addresses = addresses;
                    crate::pin::refs_loaded(&mut tab.ui);
                    crate::graph_view::follow_upstream(tab, cx);
                }
                Err(error) => tab.error(error, cx),
            }
            cx.notify();
        },
    );
}

pub fn escape(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.ui.refs.draft.is_some() {
        if !tab.ui.busy() {
            tab.ui.refs.draft = None;
        }
    } else {
        tab.ui.refs.list_open = false;
    }
    view::focus(&tab.ui, window);
    cx.notify();
}

fn remember_view(tab: &Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else {
        return;
    };
    let (closed, folded) = tab.ui.refs.remembered();
    crate::settings::Memory::update(cx, |memory| {
        memory.change_repo(repo.common_dir(), |view| {
            view.closed = closed;
            view.folded = folded;
        })
    });
}

fn start_draft(tab: &mut Tab, kind: Kind, oid: String, window: &mut Window, cx: &mut Context<Tab>) {
    let subject = tab
        .ui
        .history
        .iter()
        .find(|r| r.oid == oid)
        .map_or_else(|| oid.chars().take(8).collect(), |r| r.subject.clone());
    tab.ui.refs.list_open = false;
    tab.ui.refs.draft = Some(Draft {
        kind,
        oid,
        subject,
        checkout: true,
        push: kind == Kind::Tag && crate::settings::settings(cx).push_new_tags,
    });
    tab.ui.refs.input.update(cx, |input, cx| input.clear(cx));
    tab.ui.refs.message.update(cx, |input, cx| input.clear(cx));
    window.focus(&tab.ui.refs.input.focus_handle(cx));
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if matches!(action, Action::Escape) {
        escape(tab, window, cx);
        return;
    }
    match &action {
        Action::ToggleSection(section) => {
            let closed = &mut tab.ui.refs.closed;
            match closed.iter().position(|s| s == section) {
                Some(i) => {
                    closed.remove(i);
                }
                None => closed.push(*section),
            }
            remember_view(tab, cx);
            cx.notify();
            return;
        }
        Action::ToggleFolder(key) => {
            let folded = &mut tab.ui.refs.folded;
            if !folded.remove(key) {
                folded.insert(key.clone());
            }
            remember_view(tab, cx);
            cx.notify();
            return;
        }
        Action::FoldAll { section, fold } => {
            let keys = folder_keys(&tab.ui.refs, *section);
            let folded = &mut tab.ui.refs.folded;
            for key in keys {
                if *fold {
                    folded.insert(key);
                } else {
                    folded.remove(&key);
                }
            }
            remember_view(tab, cx);
            cx.notify();
            return;
        }
        _ => {}
    }
    if tab.ui.busy() || tab.repo.is_none() {
        return;
    }

    match action {
        Action::ToggleList => {
            tab.ui.refs.list_open = !tab.ui.refs.list_open;
            tab.ui.refs.pick = None;
            if tab.ui.refs.list_open {
                tab.ui.refs.find.update(cx, |input, cx| input.clear(cx));
                window.focus(&tab.ui.refs.find.focus_handle(cx));
            }
            refresh(tab, cx);
        }
        Action::ToggleLabels => {
            let show = !tab.ui.refs.show_labels;
            tab.ui.refs.show_labels = show;
            crate::settings::Settings::update(cx, |s| s.show_ref_labels = show);
        }
        Action::New(kind) => {
            let oid = match (&tab.ui.selection, &tab.ui.head) {
                (Some(Selection::Commit(oid)), _) => oid.clone(),
                (_, Some(head)) => head.clone(),
                _ => return,
            };
            start_draft(tab, kind, oid, window, cx);
        }
        Action::NewAt(kind, oid) => {
            if validate_oid(&oid).is_ok() {
                start_draft(tab, kind, oid, window, cx);
            }
        }
        Action::ToggleCheckout => {
            if let Some(draft) = &mut tab.ui.refs.draft {
                draft.checkout = !draft.checkout;
            }
        }
        Action::TogglePush => {
            if let Some(draft) = tab.ui.refs.draft.as_mut().filter(|draft| draft.kind == Kind::Tag) {
                draft.push = !draft.push;
                let push = draft.push;
                crate::settings::Settings::update(cx, |s| s.push_new_tags = push);
            }
        }
        Action::Submit => {
            let Some(draft) = &tab.ui.refs.draft else {
                return;
            };
            let name = tab.ui.refs.input.read(cx).text().to_owned();
            if tab.ui.refs.name_problem(draft.kind, &name).is_some() {
                return;
            }
            let (kind, target) = (draft.kind, draft.oid.clone());
            let checkout = kind == Kind::Branch && draft.checkout;
            let (message, push) = match kind {
                Kind::Tag => (
                    tab.ui.refs.message.read(cx).text().to_owned(),
                    draft.push.then(|| crate::tag::home_remote(&tab.ui)).flatten(),
                ),
                _ => (String::new(), None),
            };
            view::focus(&tab.ui, window);
            submit(tab, kind, name, target, checkout, Made { message, push }, cx);
        }
        Action::CreateBranch(name) => {
            let Some(head) = tab.ui.head.clone() else {
                return;
            };
            if let Some(problem) = tab.ui.refs.name_problem(Kind::Branch, &name) {
                tab.ui.notices.show(crate::notice::Notice::new(view::Tone::Info, &problem, ""));
                cx.notify();
                return;
            }
            view::focus(&tab.ui, window);
            submit(tab, Kind::Branch, name, head, true, Made::default(), cx);
        }
        Action::Inspect(kind, name) => {
            let oid = tab
                .ui
                .refs
                .items
                .iter()
                .find(|item| item.kind == kind && item.name == name)
                .and_then(|item| item.oid.clone());
            if let Some(oid) = oid {
                view::focus(&tab.ui, window);
                tab.follow = crate::Follow::Nothing;
                tab.ui.diff_open = false;
                crate::reveal::dispatch(tab, crate::reveal::Action::Commit(oid), window, cx);
            }
        }
        Action::Switch(name) => {
            let stash = tab.ui.autostash.always;
            switch_to(tab, name, stash, window, cx);
        }
        Action::Escape | Action::ToggleSection(_) | Action::ToggleFolder(_) | Action::FoldAll { .. } => unreachable!(),
    }
    cx.notify();
}

pub(crate) fn switch_to(tab: &mut Tab, name: OsString, stash: bool, window: &mut Window, cx: &mut Context<Tab>) {
    if !tab.ui.refs.items.iter().any(|item| item.kind == Kind::Branch && item.name == name) {
        return;
    }
    view::focus(&tab.ui, window);
    switch_then(tab, name, stash, |_, _| {}, cx);
}

pub(crate) fn switch_then(
    tab: &mut Tab,
    name: OsString,
    stash: bool,
    then: impl FnOnce(&mut Tab, &mut Context<Tab>) + 'static,
    cx: &mut Context<Tab>,
) {
    if !tab.ui.refs.items.iter().any(|item| item.kind == Kind::Branch && item.name == name) {
        return;
    }
    let kept = crate::autostash::Kept::default();
    let stopped = kept.clone();
    tab.run_operation_or(
        cx,
        "Switching branch…",
        "Switched branch",
        true,
        move |repo| switched(&repo, &name, stash, &kept),
        move |tab, cx| {
            tab.follow = crate::Follow::ChangesOrHead;
            tab.ui.refs.list_open = false;
            tab.ui.refs.draft = None;
            then(tab, cx);
        },
        move |tab, _| {
            if let Some(stash) = stopped.take() {
                tab.ui.operation.stash = Some(stash);
                tab.ui.refs.list_open = false;
                tab.ui.refs.draft = None;
            }
        },
    );
}

fn switched(
    repo: &Repository,
    name: &OsStr,
    stash: bool,
    kept: &crate::autostash::Kept,
) -> Result<crate::notice::Outcome, crate::notice::Failure> {
    let old = crate::branch::detached_head(repo);
    let shown = name.to_string_lossy();
    let how = crate::autostash::Around { doing: format!("switching to {shown}"), stash, kept };
    crate::autostash::around(
        repo,
        how,
        || crate::autostash::Retry::Switch(name.to_owned()),
        |repo| {
            switch(repo, name)?;
            let outcome = crate::notice::Outcome::detail(format!("Now on {shown}."));
            Ok(crate::branch::left_on_no_branch(repo, old.as_deref(), outcome))
        },
        |_| Some(format!("switched to {shown}")),
    )
}

#[derive(Default)]
struct Made {
    message: String,
    push: Option<String>,
}

fn submit(
    tab: &mut Tab,
    kind: Kind,
    name: String,
    target: String,
    checkout: bool,
    made: Made,
    cx: &mut Context<Tab>,
) {
    let Made { message, push } = made;
    let pushed = push.map(|remote| (OsString::from(&name), remote));
    let (progress, notice) = match kind {
        Kind::Branch if checkout => ("Creating the branch…", "Branch created and checked out"),
        Kind::Tag => ("Creating the tag…", "Tag created"),
        _ => ("Creating the branch…", "Branch created"),
    };
    tab.run_operation_then(
        cx,
        progress,
        notice,
        checkout,
        move |repo| {
            let old = (kind == Kind::Branch && checkout).then(|| crate::branch::detached_head(&repo)).flatten();
            create(&repo, kind, &name, &target, checkout, &message)?;
            let short = crate::notice::short(&target);
            let outcome = crate::notice::Outcome::detail(if kind == Kind::Tag && !message.trim().is_empty() {
                format!("{name} at {short}, annotated")
            } else {
                format!("{name} at {short}")
            });
            Ok::<_, String>(if kind == Kind::Branch && !checkout {
                outcome.offer(
                    format!("Switch to {name}"),
                    view::Action::Refs(Action::Switch(name.into())),
                )
            } else {
                crate::branch::left_on_no_branch(&repo, old.as_deref(), outcome)
            })
        },
        move |tab, cx| {
            if checkout {
                tab.follow = crate::Follow::ChangesOrHead;
            }
            tab.ui.refs.draft = None;
            tab.ui.refs.list_open = false;
            if let Some((name, remote)) = pushed {
                crate::tag::push_new(tab, name, remote, cx);
            }
        },
    );
}

pub(crate) fn activate_row(tab: &mut Tab, index: usize, window: &mut Window, cx: &mut Context<Tab>) {
    let action = match tab.ui.refs.rows.get(index) {
        Some(SideRow::Header { section, .. }) => view::Action::Refs(Action::ToggleSection(*section)),
        Some(SideRow::Folder { key, .. }) => view::Action::Refs(Action::ToggleFolder(key.clone())),
        Some(SideRow::Ref { index, .. }) => {
            let item = &tab.ui.refs.items[*index];
            view::Action::Refs(Action::Inspect(item.kind, item.name.clone()))
        }
        Some(SideRow::Stash(index)) => {
            view::Action::Reveal(crate::reveal::Action::Stash(tab.ui.stash.entries[*index].oid.clone()))
        }
        Some(SideRow::Note) => view::Action::Publish(crate::publish::Action::Publish),
        None => return,
    };
    tab.dispatch(action, window, cx);
}


fn kind_of(section: Section) -> Option<Kind> {
    match section {
        Section::Branches => Some(Kind::Branch),
        Section::Remotes => Some(Kind::Remote),
        Section::Tags => Some(Kind::Tag),
        Section::Stashes => None,
    }
}

fn parts<'a>(remotes: &'a [String], kind: Kind, name: &'a str) -> Vec<&'a str> {
    let remote = (kind == Kind::Remote)
        .then(|| {
            remotes
                .iter()
                .filter_map(|remote| Some((remote.as_str(), name.strip_prefix(remote.as_str())?.strip_prefix('/')?)))
                .max_by_key(|(remote, _)| remote.len())
        })
        .flatten();
    match remote {
        Some((remote, branch)) => std::iter::once(remote).chain(branch.split('/')).collect(),
        None => name.split('/').collect(),
    }
}

pub(crate) fn folder_keys(state: &State, section: Section) -> Vec<String> {
    let Some(kind) = kind_of(section) else { return Vec::new() };
    let names: Vec<String> = state.items[state.range(kind)].iter().map(|item| item.name.to_string_lossy().into_owned()).collect();
    keys_of(section, kind, &names, &state.remotes)
}

fn keys_of(section: Section, kind: Kind, names: &[String], remotes: &[String]) -> Vec<String> {
    let mut keys = Vec::new();
    for name in names {
        let parts = parts(remotes, kind, name);
        for depth in 1..parts.len() {
            let key = format!("{section:?}:{}", parts[..depth].join("/"));
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

pub(crate) fn rebuild_rows(ui: &mut view::Ui, cx: &App) {
    crate::sidebar::retain(ui);
    let filter = ui.refs.filter.read(cx).text().trim().to_lowercase();
    let state = &ui.refs;
    let mut rows = Vec::new();
    for (section, kind) in [
        (Section::Branches, Some(Kind::Branch)),
        (Section::Remotes, Some(Kind::Remote)),
        (Section::Tags, Some(Kind::Tag)),
        (Section::Stashes, None),
    ] {
        if state.hidden_sections.contains(&section) {
            continue;
        }
        let open = !state.closed.contains(&section);
        let Some(kind) = kind else {
            let count = ui.stash.entries.len();
            rows.push(SideRow::Header {
                section,
                count,
                open,
            });
            if open {
                rows.extend((0..count).filter_map(|i| {
                    (filter.is_empty()
                        || ui.stash.entries[i].title.to_lowercase().contains(&filter))
                    .then_some(SideRow::Stash(i))
                }));
            }
            continue;
        };
        let range = state.range(kind);
        let count = if kind == Kind::Remote {
            state.remotes.len()
        } else {
            range.len()
        };
        rows.push(SideRow::Header {
            section,
            count,
            open,
        });
        if !open {
            continue;
        }
        if count == 0 && kind == Kind::Remote {
            rows.push(SideRow::Note);
            continue;
        }
        let mut stack: Vec<String> = Vec::new();
        let mut hidden: Option<usize> = None;
        let mut listed: Vec<&str> = Vec::new();
        let folder = |rows: &mut Vec<SideRow>,
                          stack: &mut Vec<String>,
                          hidden: &mut Option<usize>,
                          name: &str| {
            let depth = stack.len();
            stack.push(name.to_owned());
            if hidden.is_none() {
                let key = format!("{section:?}:{}", stack.join("/"));
                let open = !filter.is_empty() || !state.folded.contains(&key);
                rows.push(SideRow::Folder {
                    depth,
                    name: name.to_owned(),
                    key,
                    open,
                    remote: kind == Kind::Remote
                        && depth == 0
                        && state.remotes.iter().any(|remote| remote == name),
                });
                if !open {
                    *hidden = Some(depth);
                }
            }
        };
        for index in range {
            let name = state.items[index].name.to_string_lossy();
            if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                continue;
            }
            let remote = if kind == Kind::Remote {
                state
                    .remotes
                    .iter()
                    .filter_map(|remote| {
                        let rest = name.strip_prefix(remote.as_str())?.strip_prefix('/')?;
                        Some((remote.as_str(), rest))
                    })
                    .max_by_key(|(name, _)| name.len())
            } else {
                None
            };
            let mut parts: Vec<&str> = match remote {
                Some((remote, branch)) => std::iter::once(remote).chain(branch.split('/')).collect(),
                None => name.split('/').collect(),
            };
            let leaf = parts.pop().unwrap_or_default();
            let common = stack
                .iter()
                .zip(&parts)
                .take_while(|(open, part)| open.as_str() == **part)
                .count();
            stack.truncate(common);
            if hidden.is_some_and(|depth| common <= depth) {
                hidden = None;
            }
            for part in &parts[common..] {
                folder(&mut rows, &mut stack, &mut hidden, part);
            }
            if let Some((remote, _)) = remote
                && !listed.contains(&remote)
            {
                listed.push(remote);
            }
            if hidden.is_none() {
                rows.push(SideRow::Ref {
                    depth: parts.len(),
                    index,
                    label: leaf.to_owned(),
                });
            }
        }
        if kind == Kind::Remote {
            for remote in state.remotes.iter().filter(|r| {
                !listed.contains(&r.as_str()) && (filter.is_empty() || r.to_lowercase().contains(&filter))
            }) {
                rows.push(SideRow::Folder {
                    depth: 0,
                    name: remote.clone(),
                    key: format!("{section:?}:{remote}"),
                    open: false,
                    remote: true,
                });
            }
        }
    }
    ui.refs.rows = rows;
}

fn indent(depth: usize) -> gpui::Pixels {
    px(12. + depth as f32 * 14.)
}

pub(crate) fn distance(item: &NamedRef) -> Option<String> {
    match (item.ahead, item.behind) {
        (0, 0) => None,
        (ahead, 0) => Some(format!("↑{ahead}")),
        (0, behind) => Some(format!("↓{behind}")),
        (ahead, behind) => Some(format!("↑{ahead} ↓{behind}")),
    }
}

fn count_buttons<T: Desk>(item: &NamedRef, current: bool, row: usize, cx: &mut Context<T>) -> Vec<gpui::AnyElement> {
    crate::tracking::counts(item, current)
        .into_iter()
        .enumerate()
        .map(|(n, (id, text, tip))| {
            let target = Target::Branch(item.name.clone());
            div()
                .id(("side-count", row * 2 + n))
                .px(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(RAISED)))
                .child(kit::label(text).font_weight(FontWeight::MEDIUM))
                .tooltip(kit::tip(tip))
                .on_click(cx.listener(move |app, _, w, cx| {
                    cx.stop_propagation();
                    crate::commands::run_for(app, id, target.clone(), w, cx);
                }))
                .into_any_element()
        })
        .collect()
}

pub(crate) fn sidebar_click(kind: Kind, current: bool, name: &OsStr, click_count: usize) -> view::Action {
    match kind {
        Kind::Branch if click_count > 1 && !current => view::Action::Refs(Action::Switch(name.to_owned())),
        Kind::Remote if click_count > 1 => view::Action::Activate(Target::RemoteBranch(name.to_owned())),
        Kind::Tag if click_count > 1 => view::Action::Activate(Target::Tag(name.to_owned())),
        _ => view::Action::Refs(Action::Inspect(kind, name.to_owned())),
    }
}

pub(crate) fn sidebar<T: Desk>(app: &T, cx: &mut Context<T>) -> impl IntoElement {
    let count = app.ui().refs.rows.len();
    uniform_list(
        "sidebar",
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let cursor = ui.navigate.side_cursor(ui);
            range
                .filter_map(|i| {
                    let row = ui.refs.rows.get(i)?;
                    let at = div().w_full().when(cursor == Some(i), |d| d.bg(rgb(HOVER)));
                    Some(at.child(match row {
                        SideRow::Header {
                            section,
                            count,
                            open,
                        } => {
                            let section = *section;
                            let title = match section {
                                Section::Branches => "Branches",
                                Section::Remotes => "Remotes",
                                Section::Tags => "Tags",
                                Section::Stashes => "Stashes",
                            };
                            let add = match section {
                                Section::Branches => Some("New branch"),
                                Section::Remotes => Some("Add a remote or publish"),
                                _ => None,
                            };
                            let left_out = match crate::graph_view::section_prefix(section) {
                                Some(prefix) => ui.graph_view.hidden.contains(prefix),
                                None => ui.graph_view.hide_stashes,
                            };
                            div()
                                .id(("side-header", i))
                                .group("side-header")
                                .h(px(TREE_ROW))
                                .w_full()
                                .pl(px(12.))
                                .pr(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .cursor_pointer()
                                .when(i > 0, |d| d.border_t_1().border_color(rgb(LINE)))
                                .child(
                                    kit::icon(if *open {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    })
                                    .size(px(11.))
                                    .text_color(rgb(TEXT_3)),
                                )
                                .child(kit::label(title))
                                .when(section == Section::Remotes, |d| d.children(crate::autofetch::warning(ui, cx)))
                                .child(kit::grow())
                                .when_some(add, |d, tip| {
                                    d.child(
                                        div()
                                            .id(("side-add", i))
                                            .invisible()
                                            .group_hover("side-header", |s| s.visible())
                                            .size(px(18.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(4.))
                                            .hover(|s| s.bg(rgb(HOVER)))
                                            .child(kit::icon("plus").size(px(11.)))
                                            .tooltip(kit::tip(tip))
                                            .on_click(cx.listener(
                                                move |app, event: &gpui::ClickEvent, w, cx| {
                                                    cx.stop_propagation();
                                                    let action = if section == Section::Remotes {
                                                        view::Action::Menu(Some(Menu::Remotes {
                                                            at: event.position(),
                                                        }))
                                                    } else {
                                                        view::Action::Run("repo.new-branch")
                                                    };
                                                    app.dispatch(action, w, cx)
                                                },
                                            )),
                                    )
                                })
                                .child(
                                    div()
                                        .id(("side-more", i))
                                        .invisible()
                                        .group_hover("side-header", |s| s.visible())
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(4.))
                                        .hover(|s| s.bg(rgb(HOVER)))
                                        .child(kit::icon("more").size(px(12.)))
                                        .tooltip(kit::tip(format!("{title} actions")))
                                        .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                                            cx.stop_propagation();
                                            app.dispatch(
                                                view::Action::Menu(Some(Menu::Object {
                                                    target: Target::Section(section),
                                                    at: event.position(),
                                                })),
                                                w,
                                                cx,
                                            )
                                        })),
                                )
                                .when(left_out, |d| {
                                    d.child(
                                        div()
                                            .id(("side-header-hidden", i))
                                            .child(kit::icon("eye-off").size(px(11.)).text_color(rgb(TEXT_3)))
                                            .tooltip(kit::tip(format!("{title} are hidden in the graph"))),
                                    )
                                })
                                .child(kit::label(count.to_string()).font_weight(FontWeight::MEDIUM))
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    app.dispatch(
                                        view::Action::Refs(Action::ToggleSection(section)),
                                        w,
                                        cx,
                                    )
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                        app.dispatch(
                                            view::Action::Menu(Some(Menu::Object {
                                                target: Target::Section(section),
                                                at: event.position,
                                            })),
                                            w,
                                            cx,
                                        )
                                    }),
                                )
                                .into_any_element()
                        }
                        SideRow::Folder {
                            depth,
                            name,
                            key,
                            open,
                            remote,
                        } => {
                            let key = key.clone();
                            let local_folder = crate::sidebar::folder_prefix(&key);
                            let menu_folder = local_folder.clone();
                            let folder_picked = crate::sidebar::picked(ui).is_some() && local_folder.as_ref().is_some_and(|prefix| {
                                let inside = crate::sidebar::in_folder(ui, prefix);
                                !inside.is_empty() && inside.iter().all(|name| crate::sidebar::is_picked(ui, name))
                            });
                            let remote_name = name.clone();
                            let pointer_name = name.clone();
                            let folder_ref = format!("{}{name}/", crate::graph_view::REMOTES);
                            let left_out = *remote && crate::graph_view::left_out(ui, &folder_ref);
                            let eye = remote
                                .then(|| crate::graph_view::eye(ui, folder_ref, ("remote-eye", i), "side-folder", cx))
                                .flatten();
                            let dropped_on = Target::Remote(name.clone());
                            div()
                                .id(("side-folder", i))
                                .when(*remote, |d| crate::drag::target(d, dropped_on, None, crate::drag::Look::Row, cx))
                                .group("side-folder")
                                .h(px(TREE_ROW))
                                .w_full()
                                .pl(indent(*depth))
                                .pr(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .cursor_pointer()
                                .when(folder_picked, |d| d.bg(rgb(SELECTED)))
                                .hover(|s| s.bg(rgb(HOVER)))
                                .child(
                                    kit::icon(if *open {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    })
                                    .size(px(11.))
                                    .text_color(rgb(TEXT_3)),
                                )
                                .child(kit::fit(name.clone()).size(12.5).color(if left_out { TEXT_3 } else { TEXT_2 }).grow())
                                .when_some(menu_folder, |d, prefix| {
                                    d.on_mouse_down(MouseButton::Right, cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                        let inside = crate::sidebar::in_folder(app.ui(), &prefix);
                                        if inside.is_empty() {
                                            return;
                                        }
                                        app.dispatch(view::Action::Menu(Some(Menu::Object {
                                            target: Target::Branches(inside),
                                            at: event.position,
                                        })), w, cx);
                                    }))
                                })
                                .children(eye)
                                .when(*remote && !ui.busy(), |d| {
                                    d.on_mouse_down(MouseButton::Right, cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                                        app.dispatch(view::Action::Menu(Some(Menu::Object {
                                            target: Target::Remote(pointer_name.clone()),
                                            at: event.position,
                                        })), w, cx);
                                    }))
                                    .child(
                                        div()
                                            .id(("remote-actions", i))
                                            .invisible()
                                            .group_hover("side-folder", |s| s.visible())
                                            .size(px(18.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(4.))
                                            .hover(|s| s.bg(rgb(RAISED)))
                                            .child(kit::icon("more").size(px(12.)))
                                            .tooltip(kit::tip("Manage remote"))
                                            .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                                                cx.stop_propagation();
                                                app.dispatch(view::Action::Menu(Some(Menu::Object {
                                                    target: Target::Remote(remote_name.clone()),
                                                    at: event.position(),
                                                })), w, cx);
                                            })),
                                    )
                                })
                                .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                                    let modifiers = event.modifiers();
                                    let action = match &local_folder {
                                        Some(prefix) if modifiers.platform || modifiers.shift => {
                                            view::Action::Sidebar(crate::sidebar::Action::PickFolder(prefix.clone()))
                                        }
                                        _ => view::Action::Refs(Action::ToggleFolder(key.clone())),
                                    };
                                    app.dispatch(action, w, cx)
                                }))
                                .into_any_element()
                        }
                        SideRow::Ref {
                            depth,
                            index,
                            label,
                        } => {
                            let item = &ui.refs.items[*index];
                            let kind = item.kind;
                            let current =
                                kind == Kind::Branch && item.name.to_string_lossy() == ui.branch;
                            let shown = matches!((&ui.selection, &item.oid),
                                (Some(Selection::Commit(id)), Some(oid)) if id == oid);
                            let (switch_name, menu_name) = (item.name.clone(), item.name.clone());
                            let picked = kind == Kind::Branch && crate::sidebar::is_picked(ui, &item.name);
                            let full = crate::graph_view::of_ref(kind, &item.name.to_string_lossy());
                            let left_out = crate::graph_view::left_out(ui, &full);
                            let eye = crate::graph_view::eye(ui, full, ("side-eye", i), "side-ref", cx);
                            let chip = match kind {
                                Kind::Branch if current => kit::Chip::Head,
                                Kind::Branch => kit::Chip::Local,
                                Kind::Remote => kit::Chip::Remote,
                                Kind::Tag => kit::Chip::Tag,
                            };
                            let shown_ref = Target::of_ref(kind, item.name.clone());
                            let carried = crate::drag::Carried::new(shown_ref.clone(), None, chip, item.name.to_string_lossy())
                                .at(item.oid.clone());
                            let row = crate::drag::source(div().id(("side-ref", i)), carried);
                            crate::drag::target(row, shown_ref, None, crate::drag::Look::Row, cx)
                                .group("side-ref")
                                .h(px(TREE_ROW))
                                .w_full()
                                .pl(indent(*depth))
                                .pr(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .cursor_pointer()
                                .when(picked, |d| d.bg(rgb(SELECTED)))
                                .hover(|s| s.bg(rgb(HOVER)))
                                .child(
                                    div()
                                        .w(px(11.))
                                        .flex_shrink_0()
                                        .flex()
                                        .justify_center()
                                        .when(current, |d| {
                                            d.child(
                                                div().size(px(6.)).rounded_full().bg(rgb(ACCENT)),
                                            )
                                        }),
                                )
                                .child(
                                    kit::fit(label.clone())
                                        .size(12.5)
                                        .color(if left_out { TEXT_3 } else if current || shown { TEXT } else { TEXT_2 })
                                        .weight(if current {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .grow(),
                                )
                                .when(kind == Kind::Branch, |d| {
                                    d.children(count_buttons(item, current, i, cx))
                                        .when(item.gone, |d| d.child(kit::label("gone")))
                                        .tooltip(kit::tip(crate::tracking::tooltip(item)))
                                })
                                .when(kind == Kind::Tag, |d| d.tooltip(kit::tip(crate::tag::tooltip(item))))
                                .children(eye)
                                .on_click(cx.listener(move |app, event: &gpui::ClickEvent, w, cx| {
                                    let modifiers = event.modifiers();
                                    let how = if modifiers.platform {
                                        Some(crate::picked::How::Toggle)
                                    } else if modifiers.shift {
                                        Some(crate::picked::How::Range)
                                    } else {
                                        None
                                    };
                                    if kind == Kind::Branch {
                                        let pick = crate::sidebar::Action::Pick { name: switch_name.clone(), how };
                                        app.dispatch(view::Action::Sidebar(pick), w, cx);
                                        if how.is_some() {
                                            return;
                                        }
                                    }
                                    let action = sidebar_click(kind, current, &switch_name, event.click_count());
                                    app.dispatch(action, w, cx)
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                        let target = match kind {
                                            Kind::Branch => crate::sidebar::menu_target(app.ui(), &menu_name),
                                            _ => Target::of_ref(kind, menu_name.clone()),
                                        };
                                        app.dispatch(
                                            view::Action::Menu(Some(Menu::Object {
                                                target,
                                                at: event.position,
                                            })),
                                            w,
                                            cx,
                                        )
                                    }),
                                )
                                .into_any_element()
                        }
                        SideRow::Stash(index) => {
                            let entry = &ui.stash.entries[*index];
                            let oid = entry.oid.clone();
                            let menu_oid = oid.clone();
                            let selected = matches!(&ui.selection, Some(Selection::Stash(s)) if s == &oid);
                            kit::row(("side-stash", i), selected)
                                .h(px(TREE_ROW))
                                .pl(px(27.))
                                .pr(px(10.))
                                .gap_2()
                                .child(
                                    kit::fit(entry.title.clone())
                                        .size(12.5)
                                        .color(if selected { TEXT } else { TEXT_2 })
                                        .grow(),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(TEXT_SMALL))
                                        .text_color(rgb(if selected { TEXT_2 } else { TEXT_3 }))
                                        .whitespace_nowrap()
                                        .child(view::when(entry.time, &entry.date, view::now())),
                                )
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    app.dispatch(view::Action::Reveal(crate::reveal::Action::Stash(oid.clone())), w, cx)
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |app, event: &MouseDownEvent, w, cx| {
                                        app.dispatch(
                                            view::Action::Menu(Some(Menu::Object {
                                                target: Target::Stash(menu_oid.clone()),
                                                at: event.position,
                                            })),
                                            w,
                                            cx,
                                        )
                                    }),
                                )
                                .into_any_element()
                        }
                        SideRow::Note => div()
                            .id(("side-note", i))
                            .h(px(TREE_ROW))
                            .w_full()
                            .pl(px(29.))
                            .pr(px(10.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(12.5))
                            .child(div().text_color(rgb(TEXT_3)).child("No remote yet"))
                            .child(kit::grow())
                            .child(
                                div()
                                    .id(("side-publish", i))
                                    .cursor_pointer()
                                    .text_color(rgb(ACCENT))
                                    .child("Publish…")
                                    .on_click(cx.listener(|app, _, w, cx| {
                                        app.dispatch(
                                            view::Action::Publish(crate::publish::Action::Publish),
                                            w,
                                            cx,
                                        )
                                    })),
                            )
                            .into_any_element(),
                    }))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(app.ui().refs.scroll.clone())
    .flex_1()
    .min_h_0()
}


fn matches_find<'a>(ui: &'a view::Ui, cx: &App) -> Vec<&'a NamedRef> {
    let find = ui.refs.find.read(cx).text().trim().to_lowercase();
    ui.refs.items[ui.refs.range(Kind::Branch)]
        .iter()
        .filter(|item| find.is_empty() || item.name.to_string_lossy().to_lowercase().contains(&find))
        .collect()
}

fn remote_matches<'a>(ui: &'a view::Ui, cx: &App) -> Vec<&'a NamedRef> {
    let find = ui.refs.find.read(cx).text().trim().to_lowercase();
    ui.refs.items[ui.refs.range(Kind::Remote)]
        .iter()
        .filter(|item| item.oid.is_some())
        .filter(|item| find.is_empty() || item.name.to_string_lossy().to_lowercase().contains(&find))
        .collect()
}

fn new_name(ui: &view::Ui, cx: &App) -> Option<String> {
    let typed = ui.refs.find.read(cx).text().trim();
    (!typed.is_empty() && ui.refs.branch(typed).is_none()).then(|| typed.to_owned())
}

fn highlighted(ui: &view::Ui, cx: &App) -> Option<usize> {
    let found = matches_find(ui, cx);
    let create = new_name(ui, cx);
    let remotes = remote_matches(ui, cx);
    let base = found.len() + usize::from(create.is_some());
    let rows = base + remotes.len();
    if rows == 0 {
        return None;
    }
    let text = ui.refs.find.read(cx).text();
    if let Some((typed, row)) = &ui.refs.pick
        && typed == text
    {
        return Some((*row).min(rows - 1));
    }
    if let Some(row) = found.iter().position(|item| item.name.to_string_lossy() != ui.branch) {
        return Some(row);
    }
    let typed = text.trim().as_bytes();
    let same_name = remotes.iter().position(|item| {
        crate::branch::split_remote(&ui.refs.remotes, &item.name).is_some_and(|(_, local)| local.as_bytes() == typed)
    });
    Some(match (same_name, &create) {
        (Some(row), _) => base + row,
        (None, Some(_)) => found.len(),
        (None, None) if found.is_empty() => 0,
        (None, None) => found.len() - 1,
    })
}

pub(crate) fn switcher_key(
    tab: &mut Tab,
    action: crate::navigate::Action,
    window: &mut Window,
    cx: &mut Context<Tab>,
) {
    use crate::navigate::{Action as Key, Move, how, moved};
    let ui = &tab.ui;
    let found: Vec<OsString> = matches_find(ui, cx).into_iter().map(|item| item.name.clone()).collect();
    let create = new_name(ui, cx);
    let remotes: Vec<OsString> = remote_matches(ui, cx).into_iter().map(|item| item.name.clone()).collect();
    let base = found.len() + usize::from(create.is_some());
    let rows = base + remotes.len();
    let current = highlighted(ui, cx);
    if action == Key::Choose {
        match current {
            Some(row) if row < found.len() => {
                let name = found[row].clone();
                if name == *tab.ui.branch && !tab.ui.detached {
                    let text = format!("{} is already checked out", tab.ui.branch);
                    tab.ui.notices.show(crate::notice::Notice::new(view::Tone::Info, &text, ""));
                    cx.notify();
                } else {
                    tab.dispatch(view::Action::Refs(Action::Switch(name)), window, cx);
                }
            }
            Some(row) if row >= base => {
                let target = Target::RemoteBranch(remotes[row - base].clone());
                tab.dispatch(view::Action::Activate(target), window, cx);
            }
            Some(_) => {
                if let Some(name) = create {
                    tab.dispatch(view::Action::Refs(Action::CreateBranch(name)), window, cx);
                }
            }
            None => {}
        }
        return;
    }
    let Some(step) = how(action, 8) else {
        return;
    };
    let Some(row) = moved(current, rows, step) else {
        return;
    };
    if row < found.len() {
        tab.ui.refs.switcher_scroll.scroll_to_item(row, gpui::ScrollStrategy::Center);
    } else if row >= base {
        tab.ui.refs.remote_scroll.scroll_to_item(row - base, gpui::ScrollStrategy::Center);
    } else if matches!(step, Move::By(_) | Move::Last) && !found.is_empty() {
        tab.ui.refs.switcher_scroll.scroll_to_item(found.len() - 1, gpui::ScrollStrategy::Bottom);
    }
    let text = tab.ui.refs.find.read(cx).text().to_owned();
    tab.ui.refs.pick = Some((text, row));
    cx.notify();
}

pub(crate) fn switcher<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let count = matches_find(ui, cx).len();
    let create = new_name(ui, cx);
    let at = highlighted(ui, cx);
    let remote_count = remote_matches(ui, cx).len();
    let any_remote = !ui.refs.range(Kind::Remote).is_empty();
    let remote_base = count + usize::from(create.is_some());
    let list = uniform_list(
        "branch-switcher",
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let found = matches_find(ui, cx);
            let at = highlighted(ui, cx);
            range
                .filter_map(|i| {
                    let item = found.get(i)?;
                    let name = item.name.to_string_lossy().into_owned();
                    let current = name == ui.branch;
                    let mut row = kit::item(("switch-local", i), name.clone())
                        .check(current)
                        .highlighted(at == Some(i))
                        .enabled(!ui.busy() && !current);
                    if let Some(text) = distance(item) {
                        row = row.meta(text);
                    }
                    Some(row.on(
                        view::Action::Refs(Action::Switch(item.name.clone())),
                        cx,
                    ))
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.refs.switcher_scroll.clone())
    .h(px(ROW * count.clamp(1, 9) as f32));
    let create_row = create.map(|name| {
        let problem = ui.refs.name_problem(Kind::Branch, &name);
        let ready = !ui.busy() && ui.head.is_some() && problem.is_none();
        let reason = problem.or_else(|| {
            ui.head.is_none().then(|| "Make the first commit before creating a branch".to_owned())
        });
        div()
            .py_1()
            .child(
                kit::item("switch-create", format!("Create branch {name}"))
                    .icon("plus")
                    .highlighted(at == Some(count))
                    .enabled(ready)
                    .tip(reason.clone().map(Into::into))
                    .on(view::Action::Refs(Action::CreateBranch(name)), cx),
            )
            .when_some(reason, |d, reason| {
                d.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(DELETED))
                        .child(reason),
                )
            })
    });
    let remote_list = uniform_list(
        "branch-switcher-remote",
        remote_count,
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let found = remote_matches(ui, cx);
            let at = highlighted(ui, cx);
            let command = crate::commands::find("branch.checkout-remote");
            range
                .filter_map(|i| {
                    let item = found.get(i)?;
                    let target = Target::RemoteBranch(item.name.clone());
                    let entry = crate::commands::entry(command?, ui, &target, cx)?;
                    Some(
                        kit::item(("switch-remote", i), item.name.to_string_lossy().into_owned())
                            .icon("cloud")
                            .highlighted(at == Some(remote_base + i))
                            .enabled(entry.action.is_some())
                            .tip(entry.disabled.map(Into::into))
                            .on(view::Action::Activate(target), cx),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.refs.remote_scroll.clone())
    .h(px(ROW * remote_count.clamp(1, 6) as f32));
    kit::float()
        .w(px(320.))
        .flex()
        .flex_col()
        .child(
            div()
                .p_2()
                .border_b_1()
                .border_color(rgb(LINE))
                .key_context("BranchSwitcher")
                .child(ui.refs.find.clone()),
        )
        .child(
            div()
                .px_3()
                .pt_2()
                .pb_1()
                .child(kit::label("Local")),
        )
        .when(count == 0, |d| {
            d.child(div().px_3().pb_2().child(kit::muted("No branch matches.")))
        })
        .when(count > 0, |d| d.child(list).child(kit::scrollbar("branch-switcher-scrollbar", &ui.refs.switcher_scroll)))
        .children(create_row)
        .when(any_remote, |d| {
            d.child(div().px_3().pt_2().pb_1().child(kit::label("Remote")))
                .when(remote_count == 0, |d| {
                    d.child(div().px_3().pb_2().child(kit::muted("No remote branch matches.")))
                })
                .when(remote_count > 0, |d| {
                    d.child(remote_list)
                        .child(kit::scrollbar("branch-switcher-remote-scrollbar", &ui.refs.remote_scroll))
                })
        })
        .child(
            div()
                .mt_1()
                .py_1()
                .border_t_1()
                .border_color(rgb(LINE))
                .child(
                    kit::item("switch-new", format!("New branch from {}…", ui.branch))
                        .icon("plus")
                        .meta(crate::commands::keys("cmd-b"))
                        .enabled(!ui.busy() && ui.head.is_some())
                        .on(view::Action::Run("repo.new-branch"), cx),
                ),
        )
}


pub(crate) fn dialog<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement> {
    let ui = app.ui();
    let draft = ui.refs.draft.as_ref()?;
    let error = ui.refs.validation(cx);
    let allowed = !ui.busy() && !ui.refs.input.read(cx).text().is_empty() && error.is_none();
    let title = match draft.kind {
        Kind::Tag => "New tag",
        _ => "New branch",
    };
    let short: String = draft.oid.chars().take(7).collect();
    let body = kit::dialog(title, 420.)
        .child(
            div()
                .px(px(10.))
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(CANVAS))
                .child(kit::fit(short).mono().size(11.5).color(TEXT_2))
                .child(kit::fit(draft.subject.clone()).grow()),
        )
        .child(
            div()
                .child(ui.refs.input.clone())
                .on_key_down(cx.listener(|app, e: &KeyDownEvent, w, cx| {
                    if e.keystroke.key == "enter" && e.keystroke.modifiers == Default::default() {
                        app.dispatch(view::Action::Refs(Action::Submit), w, cx);
                        cx.stop_propagation();
                    }
                })),
        )
        .when_some(error, |d, error| {
            d.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(DELETED))
                    .child(error),
            )
        })
        .when(draft.kind == Kind::Tag, |d| {
            let push = crate::tag::home_remote(ui).map(|remote| (remote, draft.push));
            d.child(ui.refs.message.clone()).when_some(push, |d, (remote, ticked)| {
                d.child(kit::pressable(
                    div()
                        .id("push-new-tag")
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .rounded(px(RADIUS_CHIP))
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_2))
                        .focusable()
                        .focus(|s| s.text_color(rgb(TEXT)))
                        .child(kit::checkbox(ticked))
                        .child(format!("Push to {remote}")),
                    view::Action::Refs(Action::TogglePush),
                    cx,
                ))
            })
        })
        .when(draft.kind == Kind::Branch, |d| {
            d.child(kit::pressable(
                div()
                    .id("checkout-new")
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .rounded(px(RADIUS_CHIP))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_2))
                    .focusable()
                    .focus(|s| s.text_color(rgb(TEXT)))
                    .child(kit::checkbox(draft.checkout))
                    .child("Check out after creating"),
                view::Action::Refs(Action::ToggleCheckout),
                cx,
            ))
        })
        .child(
            div()
                .mt_1()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    kit::btn("cancel-ref", "Cancel")
                        .enabled(!ui.busy())
                        .on(view::Action::Refs(Action::Escape), cx),
                )
                .child(
                    kit::btn("create-ref", "Create")
                        .primary()
                        .enabled(allowed)
                        .on(view::Action::Refs(Action::Submit), cx),
                ),
        );
    Some(kit::modal(
        body,
        Some(view::Action::Refs(Action::Escape)),
        &ui.modal_focus,
        cx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn oid(f: &Fixture, revision: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", "--verify", revision]))
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn notebook() -> (Fixture, String, String) {
        let f = Fixture::new();
        fs::write(f.dir.join("notes.txt"), "Start\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        f.git(&["commit", "-qm", "Start notebook"]);
        let base = oid(&f, "HEAD");
        fs::write(f.dir.join("notes.txt"), "Revised\n").unwrap();
        f.git(&["commit", "-qam", "Revise notebook"]);
        let tip = oid(&f, "HEAD");
        (f, base, tip)
    }

    #[test]
    fn a_branch_lists_its_remote_tracking_upstream_but_not_a_local_one() {
        let (f, base, tip) = notebook();
        f.git(&["remote", "add", "origin", "/nonexistent/origin.git"]);
        f.git(&["update-ref", "refs/remotes/origin/main", &base]);
        f.git(&["config", "branch.main.remote", "origin"]);
        f.git(&["config", "branch.main.merge", "refs/heads/main"]);
        f.git(&["branch", "--track", "local-follower", "main"]);
        f.git(&["branch", "--no-track", "loose", &tip]);
        let items = list(&f.repo).unwrap();
        let upstream = |name: &str| {
            items.iter().find(|item| item.kind == Kind::Branch && item.name == name).unwrap().upstream.clone()
        };
        assert_eq!(upstream("main").as_deref(), Some("origin/main"));
        assert_eq!(upstream("local-follower"), None);
        assert_eq!(upstream("loose"), None);
        let main = items.iter().find(|item| item.name == "main").unwrap();
        assert_eq!((main.ahead, main.behind), (1, 0));
    }

    #[test]
    fn obs11_obs12_create_checkout_and_switch_back() {
        let (f, _, tip) = notebook();
        create(&f.repo, Kind::Branch, "review-walk", &tip, true, "").unwrap();
        assert_eq!(
            f.git(&["symbolic-ref", "--short", "HEAD"]),
            b"review-walk\n"
        );
        assert_eq!(oid(&f, "refs/heads/review-walk"), tip);
        switch(&f.repo, OsStr::new("main")).unwrap();
        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), b"main\n");
        assert_eq!(oid(&f, "HEAD"), tip);
    }

    #[test]
    fn ext01_obs21_create_earlier_branch_and_lightweight_tag_without_moving_head() {
        let (f, base, tip) = notebook();
        create(&f.repo, Kind::Branch, "review/notes", &base, false, "").unwrap();
        create(&f.repo, Kind::Tag, "signpost-v1", &base, false, "").unwrap();
        let items = list(&f.repo).unwrap();
        for (kind, name) in [(Kind::Branch, "review/notes"), (Kind::Tag, "signpost-v1")] {
            let item = items
                .iter()
                .find(|item| item.kind == kind && item.name == name)
                .unwrap();
            assert_eq!(item.oid.as_deref(), Some(base.as_str()));
        }
        assert_eq!(
            f.git(&["cat-file", "-t", "refs/tags/signpost-v1"]),
            b"commit\n"
        );
        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), b"main\n");
        assert_eq!(oid(&f, "HEAD"), tip);
    }

    #[test]
    fn a_tag_with_a_message_is_annotated_and_lists_its_object_and_message() {
        let (f, base, tip) = notebook();
        create(&f.repo, Kind::Tag, "v2", &base, false, "# Release two\n\nThe notebook is revised.\n").unwrap();
        create(&f.repo, Kind::Tag, "v2-light", &base, false, "  \n").unwrap();
        assert_eq!(f.git(&["cat-file", "-t", "refs/tags/v2"]), b"tag\n");
        assert_eq!(f.git(&["cat-file", "-t", "refs/tags/v2-light"]), b"commit\n");
        assert_eq!(
            f.git(&["tag", "-l", "--format=%(contents)", "v2"]),
            b"# Release two\n\nThe notebook is revised.\n\n"
        );
        let object = oid(&f, "refs/tags/v2");
        let items = list(&f.repo).unwrap();
        let tag = |name: &str| items.iter().find(|item| item.kind == Kind::Tag && item.name == name).unwrap();
        assert_eq!(tag("v2").oid.as_deref(), Some(base.as_str()));
        assert_eq!(tag("v2").object, object);
        let annotation = tag("v2").annotation.clone().unwrap();
        assert_eq!((annotation.subject.as_str(), annotation.tagger.as_str()), ("# Release two", "Cedar"));
        assert_eq!(annotation.date.len(), "2026-11-22".len());
        assert_eq!((tag("v2-light").object.as_str(), tag("v2-light").annotation.clone()), (base.as_str(), None));
        let main = items.iter().find(|item| item.kind == Kind::Branch && item.name == "main").unwrap();
        assert_eq!((main.object.as_str(), main.annotation.clone()), (tip.as_str(), None));
        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), b"main\n");
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
    }

    #[test]
    fn ext02_invalid_tag_and_existing_tag_leave_refs_unchanged() {
        let (f, base, tip) = notebook();
        create(&f.repo, Kind::Tag, "v1", &base, false, "").unwrap();
        let original = f.git(&["show-ref"]);
        assert_eq!(
            name_problem(Kind::Tag, "release candidate", []).as_deref(),
            Some("Tag names cannot contain spaces")
        );
        assert!(create(&f.repo, Kind::Tag, "release candidate", &tip, false, "").is_err());
        assert!(create(&f.repo, Kind::Tag, "v1", &tip, false, "").is_err());
        assert_eq!(f.git(&["show-ref"]), original);
    }

    #[test]
    fn a_name_error_says_which_rule_it_breaks() {
        let problem = |name: &str| name_problem(Kind::Branch, name, []);
        for (name, said) in [
            ("my branch", "Branch names cannot contain spaces"),
            ("fix\tit", "Branch names cannot contain spaces"),
            ("what?", "Branch names cannot contain ?"),
            ("a~1", "Branch names cannot contain ~"),
            ("back\\slash", "Branch names cannot contain \\"),
            ("-x", "Branch names cannot start with a hyphen"),
            ("a..b", "Branch names cannot contain two dots in a row"),
            ("a@{1}", "Branch names cannot contain @{"),
            ("feature/", "Branch names cannot start or end with a slash, or have two slashes in a row"),
            ("a//b", "Branch names cannot start or end with a slash, or have two slashes in a row"),
            ("done.", "Branch names cannot end with a dot"),
            ("a/.hidden", "Branch names cannot have a part that starts with a dot"),
            ("x.lock", "Branch names cannot have a part that ends in .lock"),
            ("HEAD", "A branch cannot be named HEAD"),
            ("@", "A branch cannot be named @"),
            ("", "Type a name for the branch"),
        ] {
            assert_eq!(problem(name).as_deref(), Some(said), "{name:?}");
        }
        for name in ["feature/map", "v1.2", "fix-42", "a.b/c_d", "caf\u{e9}"] {
            assert_eq!(problem(name), None, "{name:?}");
        }
        assert_eq!(
            name_problem(Kind::Tag, "v 1", []).as_deref(),
            Some("Tag names cannot contain spaces")
        );
    }

    #[test]
    fn collapse_all_folds_every_folder_of_a_section_and_a_remote_counts_as_one() {
        let names: Vec<String> = ["feature/ui/menu", "feature/ui/strip", "fix/one", "main"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            keys_of(Section::Branches, Kind::Branch, &names, &[]),
            ["Branches:feature", "Branches:feature/ui", "Branches:fix"]
        );
        let remotes = vec!["origin".to_owned(), "origin/fork".to_owned()];
        let names: Vec<String> = ["origin/main", "origin/fork/topic", "origin/team/x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            keys_of(Section::Remotes, Kind::Remote, &names, &remotes),
            ["Remotes:origin", "Remotes:origin/fork", "Remotes:origin/team"]
        );
        assert!(keys_of(Section::Tags, Kind::Tag, &["v1".to_owned()], &[]).is_empty());
    }

    #[test]
    fn a_name_is_checked_against_the_refs_already_loaded() {
        let taken = [OsStr::new("main"), OsStr::new("feature"), OsStr::new("topic/one")];
        let problem = |name: &str| name_problem(Kind::Branch, name, taken);
        assert_eq!(problem("main").as_deref(), Some("A branch named main already exists"));
        assert_eq!(
            problem("feature/map").as_deref(),
            Some("feature/map cannot be created because the branch feature exists")
        );
        assert_eq!(
            problem("topic").as_deref(),
            Some("topic cannot be created because the branch topic/one uses it as a folder")
        );
        assert_eq!(problem("mainline"), None);
        assert_eq!(problem("topic/two"), None);
        assert_eq!(
            name_problem(Kind::Tag, "v1", [OsStr::new("v1")]).as_deref(),
            Some("A tag named v1 already exists")
        );
    }

    #[test]
    fn a_name_taken_after_loading_is_still_refused_by_git_and_nothing_moves() {
        let (f, base, tip) = notebook();
        assert_eq!(name_problem(Kind::Branch, "review", [OsStr::new("main")]), None);
        f.git(&["branch", "review", &base]);
        let refs = f.git(&["show-ref"]);
        let error = create(&f.repo, Kind::Branch, "review", &tip, true, "").unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(f.git(&["show-ref"]), refs);
        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), b"main\n");
        assert_eq!(oid(&f, "HEAD"), tip);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        let error = create(&f.repo, Kind::Branch, "review/next", &tip, false, "").unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(f.git(&["show-ref"]), refs);
    }

    #[test]
    fn conflicting_local_edits_prevent_checkout_and_branch_creation() {
        let (f, base, tip) = notebook();
        fs::write(f.dir.join("notes.txt"), "Uncommitted\n").unwrap();
        assert!(create(&f.repo, Kind::Branch, "review", &base, true, "").is_err());
        assert_eq!(oid(&f, "HEAD"), tip);
        assert!(!list(&f.repo).unwrap().iter().any(|r| r.name == "review"));
        assert_eq!(fs::read(f.dir.join("notes.txt")).unwrap(), b"Uncommitted\n");
    }

    #[test]
    fn a_switch_with_changes_in_its_way_offers_to_stash_and_then_restores_them() {
        let f = Fixture::new();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit\n").unwrap();
        f.git(&["add", "--", "route.txt"]);
        f.git(&["commit", "-qm", "Plan the route"]);
        f.git(&["switch", "-qc", "ridge"]);
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nSummit\n").unwrap();
        f.git(&["commit", "-qam", "Start north"]);
        let ridge = oid(&f, "ridge");
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        let kept = crate::autostash::Kept::default();
        let refused = switched(&f.repo, OsStr::new("ridge"), false, &kept).unwrap_err();
        let offered: Vec<_> = refused.actions.iter().map(|(label, action)| (label.to_string(), format!("{action:?}"))).collect();
        assert_eq!(offered, [(crate::autostash::RETRY.to_owned(), "Autostash(Retry(Switch(\"ridge\")))".to_owned())]);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");

        let done = switched(&f.repo, OsStr::new("ridge"), true, &kept).unwrap();
        assert_eq!((done.title.as_str(), done.detail.as_str()), ("Stashed your changes, switched to ridge and restored them", "Now on ridge."));
        assert_eq!(oid(&f, "HEAD"), ridge);
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/ridge\n");
        assert_eq!(fs::read(f.dir.join("route.txt")).unwrap(), b"Start north\nFord\nSummit east\n");
        assert_eq!(f.git(&["stash", "list"]), b"");
    }

    #[test]
    fn a_stashed_switch_off_a_detached_head_still_says_what_it_left_behind() {
        let f = Fixture::new();
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit\n").unwrap();
        f.git(&["add", "--", "route.txt"]);
        f.git(&["commit", "-qm", "Plan the route"]);
        f.git(&["switch", "-qc", "ridge"]);
        fs::write(f.dir.join("route.txt"), "Start north\nFord\nSummit\n").unwrap();
        f.git(&["commit", "-qam", "Start north"]);
        f.git(&["switch", "-q", "--detach", "main"]);
        fs::write(f.dir.join("camp.txt"), "Camp\n").unwrap();
        f.git(&["add", "--", "camp.txt"]);
        f.git(&["commit", "-qm", "Pitch camp on no branch"]);
        let lost = oid(&f, "HEAD");
        fs::write(f.dir.join("route.txt"), "Start\nFord\nSummit east\n").unwrap();
        let kept = crate::autostash::Kept::default();
        let done = switched(&f.repo, OsStr::new("ridge"), true, &kept).unwrap();
        assert_eq!(done.title, "Stashed your changes, switched to ridge and restored them");
        assert_eq!(done.detail, format!("Now on ridge. You left 1 commit behind on no branch, ending at {}.", &lost[..7]));
        assert_eq!(done.actions.iter().map(|(label, _)| label.to_string()).collect::<Vec<_>>(), [format!("Create branch at {}…", &lost[..7])]);
        assert!(done.sticky);
        assert_eq!(f.git(&["stash", "list"]), b"");
    }
}
