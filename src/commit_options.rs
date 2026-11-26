use crate::{
    Tab, kit,
    repository::{Choices, Hooks, Repository, Run},
    settings::{self, Memory, Settings},
    theme::*,
    view::{self, CommitDraft, CommitInput, Desk, Tone, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, SharedString, Stateful, Window, deferred, div, prelude::*, rgb};
use crate::theme::px;
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
};

const TEMPLATE_LIMIT: u64 = 64 * 1024;
pub(crate) const SHOWN_AUTHORS: usize = 8;
const COAUTHOR_KEY: &str = "Co-authored-by";
const SKIP_HOOKS: &str = "Skip hooks";
const PUSH_AFTER: &str = "Push after committing";
const SIGN: &str = "Sign this commit";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Author {
    pub(crate) name: String,
    pub(crate) email: String,
}

impl Author {
    pub(crate) fn trailer(&self) -> String {
        format!("{COAUTHOR_KEY}: {} <{}>", self.name, self.email)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Push {
    Upstream(String),
    Publish(String),
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    ToggleOpen,
    ToggleSkipHooks,
    ToggleSign,
    TogglePush,
    TogglePushDefault,
    ToggleTemplate,
    OpenCoauthors,
    CloseCoauthors,
    AddCoauthor(Author),
}

pub(crate) struct State {
    pub(crate) open: bool,
    pub(crate) skip_hooks: bool,
    pub(crate) sign_chosen: Option<bool>,
    pub(crate) push_chosen: Option<bool>,
    pub(crate) template: Option<CommitDraft>,
    picking: bool,
    pub(crate) filter: Entity<CommitInput>,
    pick: Option<(String, usize)>,
    ticket: u64,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let filter = cx.new(|cx| CommitInput::with_placeholder(cx, "Filter authors"));
        cx.observe(&filter, |_, _, cx| cx.notify()).detach();
        Self {
            open: false,
            skip_hooks: false,
            sign_chosen: None,
            push_chosen: None,
            template: None,
            picking: false,
            filter,
            pick: None,
            ticket: 0,
        }
    }

    pub(crate) fn picking(&self) -> bool {
        self.picking
    }

    pub(crate) fn hooks(&self) -> Hooks {
        if self.skip_hooks { Hooks::Skip } else { Hooks::Run }
    }

    pub(crate) fn choices(&self) -> Choices {
        Choices { hooks: self.hooks(), sign: self.sign_chosen }
    }
}

pub(crate) fn signs(ui: &Ui) -> bool {
    ui.commit_options.sign_chosen.unwrap_or_else(|| crate::signing::config(ui).commits)
}

pub(crate) fn wants_push(ui: &Ui, cx: &App) -> bool {
    ui.commit_options.push_chosen.unwrap_or_else(|| settings::settings(cx).push_after_commit)
}

pub(crate) fn push_refusal(ui: &Ui) -> Option<&'static str> {
    if ui.refs.remotes.is_empty() {
        Some("This repository has no remote to push to")
    } else if ui.detached {
        Some("Check out a branch to push after committing")
    } else if ui.amend.is_some() {
        Some("Amend replaces the last commit, so push it yourself afterwards")
    } else {
        None
    }
}

pub(crate) fn push_after(ui: &Ui, cx: &App) -> Option<Push> {
    if !wants_push(ui, cx) || push_refusal(ui).is_some() || ui.branch.is_empty() {
        return None;
    }
    Some(match &ui.upstream {
        Some(upstream) => Push::Upstream(upstream.clone()),
        None => Push::Publish(ui.branch.clone()),
    })
}

fn counted(staged: usize) -> String {
    if staged == 1 { "1 file".to_owned() } else { format!("{staged} files") }
}

pub(crate) fn commit_label(ui: &Ui, cx: &App) -> String {
    let staged = ui.file_ranges().1.len();
    let skipping = ui.commit_options.skip_hooks;
    let label = if ui.amend.is_some() {
        match staged {
            0 => "Amend last commit".to_owned(),
            1 => "Amend with 1 staged file".to_owned(),
            n => format!("Amend with {n} staged files"),
        }
    } else if staged == 0 {
        return "Commit".to_owned();
    } else {
        match push_after(ui, cx) {
            Some(Push::Upstream(upstream)) => format!("Commit and push to {upstream}"),
            Some(Push::Publish(branch)) => format!("Commit and publish {branch}"),
            None if skipping => format!("Commit {}", counted(staged)),
            None => match ui.checked_out_branch() {
                Some(branch) => format!("Commit {} to {branch}", counted(staged)),
                None => format!("Commit {}", counted(staged)),
            },
        }
    };
    if skipping { format!("{label}, skipping hooks") } else { label }
}

fn chosen(ui: &Ui, cx: &App) -> usize {
    usize::from(ui.commit_options.skip_hooks) + usize::from(signs(ui)) + usize::from(wants_push(ui, cx) && push_refusal(ui).is_none())
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() && !matches!(action, Action::TogglePushDefault | Action::ToggleTemplate) {
        return;
    }
    match action {
        Action::ToggleOpen => tab.ui.commit_options.open = !tab.ui.commit_options.open,
        Action::ToggleSkipHooks => {
            if !tab.ui.can_edit_index() {
                return;
            }
            tab.ui.commit_options.skip_hooks = !tab.ui.commit_options.skip_hooks;
        }
        Action::ToggleSign => {
            if !tab.ui.can_edit_index() {
                return;
            }
            let on = !signs(&tab.ui);
            let configured = crate::signing::config(&tab.ui).commits;
            tab.ui.commit_options.sign_chosen = (on != configured).then_some(on);
        }
        Action::TogglePush => {
            if push_refusal(&tab.ui).is_some() {
                return;
            }
            let on = !wants_push(&tab.ui, cx);
            tab.ui.commit_options.push_chosen = Some(on);
            if let Some(repo) = &tab.repo {
                let common = repo.common_dir().to_owned();
                Memory::update(cx, |memory| memory.change_repo(&common, |view| view.push_after = Some(on)));
            }
        }
        Action::TogglePushDefault => Settings::update(cx, |s| s.push_after_commit = !s.push_after_commit),
        Action::ToggleTemplate => {
            Settings::update(cx, |s| s.fill_from_template = !s.fill_from_template);
            fill(tab, cx);
        }
        Action::OpenCoauthors => {
            let state = &mut tab.ui.commit_options;
            if state.picking {
                state.picking = false;
                view::focus(&tab.ui, window);
            } else {
                state.picking = true;
                state.pick = None;
                tab.ui.menu = None;
                let filter = state.filter.clone();
                filter.update(cx, |field, cx| field.clear(cx));
                window.focus(&filter.focus_handle(cx));
            }
        }
        Action::CloseCoauthors => {
            tab.ui.commit_options.picking = false;
            view::focus(&tab.ui, window);
        }
        Action::AddCoauthor(author) => {
            tab.ui.commit_options.picking = false;
            let description = tab.ui.description.clone();
            description.update(cx, |field, cx| {
                let written = with_coauthor(field.text(), &author);
                field.rewrite(&written, cx);
            });
            window.focus(&description.focus_handle(cx));
        }
    }
    cx.notify();
}

pub(crate) fn key(tab: &mut Tab, action: crate::navigate::Action, window: &mut Window, cx: &mut Context<Tab>) {
    use crate::navigate::{Action as Key, how, moved};
    let shown: Vec<Author> = matching(&tab.ui, cx).into_iter().take(SHOWN_AUTHORS).collect();
    let current = highlighted(&tab.ui, cx);
    if action == Key::Choose {
        if let Some(author) = current.and_then(|at| shown.get(at)).cloned() {
            dispatch(tab, Action::AddCoauthor(author), window, cx);
        }
        return;
    }
    let Some(step) = how(action, SHOWN_AUTHORS) else { return };
    let Some(row) = moved(current, shown.len(), step) else { return };
    let text = tab.ui.commit_options.filter.read(cx).text().to_owned();
    tab.ui.commit_options.pick = Some((text, row));
    cx.notify();
}

pub(crate) fn restore(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = &tab.repo else { return };
    let common = repo.common_dir().to_owned();
    tab.ui.commit_options.push_chosen =
        settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.push_after);
}

pub(crate) fn load_template(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.commit_options;
    state.ticket += 1;
    let ticket = state.ticket;
    let epoch = tab.epoch;
    Tab::work(cx, move || read_template(&repo), move |tab, read, cx| {
        if tab.epoch != epoch || tab.ui.commit_options.ticket != ticket {
            return;
        }
        match read {
            Ok(template) => {
                tab.ui.commit_options.template = template;
                fill(tab, cx);
            }
            Err(error) => {
                tab.ui.commit_options.template = None;
                tab.ui.notices.show(crate::notice::Notice::new(Tone::Info, "The commit template could not be read", error));
            }
        }
        cx.notify();
    });
}

pub(crate) fn fill(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(template) = &tab.ui.commit_options.template else { return };
    if !settings::settings(cx).fill_from_template || tab.ui.amend.is_some() {
        return;
    }
    let draft = tab.ui.draft(cx);
    if draft.summary.is_empty() && draft.description.is_empty() {
        tab.ui.set_draft(template, cx);
    }
}

pub(crate) fn replaceable(shown: &CommitDraft, template: Option<&CommitDraft>) -> bool {
    shown.message().is_empty() || template == Some(shown)
}

pub(crate) fn committed(tab: &mut Tab, head: Option<String>, push: Option<Push>, cx: &mut Context<Tab>) {
    tab.ui.commit_options.skip_hooks = false;
    tab.ui.commit_options.sign_chosen = None;
    fill(tab, cx);
    match push {
        Some(Push::Upstream(_)) => match head {
            Some(head) if !tab.ui.branch.is_empty() => {
                let branch = tab.ui.branch.clone();
                crate::sync::push_tip(tab, branch, head, cx);
            }
            _ => tab.error("Push Failed — The new commit could not be found, so nothing was pushed.".to_owned(), cx),
        },
        Some(Push::Publish(_)) => crate::publish::offer_branch(tab, cx),
        None => {}
    }
}

fn config(repo: &Repository, args: &[&str]) -> Result<Option<String>, String> {
    let output = repo
        .read_command()
        .arg("config")
        .args(args)
        .run()
        .map_err(|error| format!("Run bundled Git: {error}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_owned())),
        Some(1) => Ok(None),
        _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    }
}

pub(crate) fn comment_string(repo: &Repository) -> Result<String, String> {
    let configured = match config(repo, &["--get", "core.commentString"])? {
        Some(value) => Some(value),
        None => config(repo, &["--get", "core.commentChar"])?,
    };
    Ok(match configured {
        Some(value) if !value.is_empty() && value != "auto" => value,
        _ => "#".to_owned(),
    })
}

pub(crate) fn template_path(repo: &Repository) -> Result<Option<PathBuf>, String> {
    let Some(named) = config(repo, &["--type=path", "--get", "commit.template"])?.filter(|named| !named.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(&named);
    Ok(Some(if path.is_absolute() { path.to_owned() } else { repo.root.join(path) }))
}

pub(crate) fn read_template(repo: &Repository) -> Result<Option<CommitDraft>, String> {
    let Some(path) = template_path(repo)? else { return Ok(None) };
    let file = std::fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(TEMPLATE_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let comment = comment_string(repo)?;
    Ok(draft_of(&uncommented(&String::from_utf8_lossy(&bytes), &comment)))
}

pub(crate) fn uncommented(text: &str, comment: &str) -> String {
    text.split('\n')
        .filter(|line| comment.is_empty() || !line.starts_with(comment))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn draft_of(text: &str) -> Option<CommitDraft> {
    let text = text.replace("\r\n", "\n");
    let text = text.trim_end();
    let (summary, rest) = text.split_once('\n').unwrap_or((text, ""));
    let summary = summary.trim_end();
    let description = rest
        .split('\n')
        .skip_while(|line| line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let draft = CommitDraft { summary: summary.to_owned(), description: description.trim_end().to_owned() };
    (!draft.summary.trim().is_empty() || !draft.description.trim().is_empty()).then_some(draft)
}

fn trailer_line(line: &str) -> bool {
    line.split_once(':').is_some_and(|(key, _)| {
        !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn last_paragraph(text: &str) -> &str {
    match text.rfind("\n\n") {
        Some(at) => text[at..].trim_start_matches('\n'),
        None => text,
    }
}

pub(crate) fn with_coauthor(description: &str, author: &Author) -> String {
    let trailer = author.trailer();
    let kept = description.trim_end();
    if kept.lines().any(|line| line.trim().eq_ignore_ascii_case(&trailer)) {
        return description.to_owned();
    }
    if kept.is_empty() {
        return trailer;
    }
    let trailers = last_paragraph(kept);
    let joined = trailers.lines().all(trailer_line)
        && trailers.lines().any(|line| line.split_once(':').is_some_and(|(key, _)| key.contains('-')));
    if joined { format!("{kept}\n{trailer}") } else { format!("{kept}\n\n{trailer}") }
}

pub(crate) fn coauthor(value: &str) -> Option<Author> {
    let (name, email) = value.trim().strip_suffix('>')?.rsplit_once('<')?;
    let name = name.trim();
    (!name.is_empty() && !email.is_empty()).then(|| Author { name: name.to_owned(), email: email.to_owned() })
}

pub(crate) fn authors(ui: &Ui) -> Vec<Author> {
    let own = ui.identity.effective().and_then(|known| known.email.clone()).map(|email| email.to_lowercase());
    let mut counted: HashMap<String, (usize, usize, Author)> = HashMap::new();
    for (index, row) in ui.history.iter().enumerate() {
        let key = row.email.to_lowercase();
        if key.is_empty() || row.author.trim().is_empty() || own.as_deref() == Some(key.as_str()) {
            continue;
        }
        let entry = counted
            .entry(key)
            .or_insert_with(|| (0, index, Author { name: row.author.clone(), email: row.email.clone() }));
        entry.0 += 1;
    }
    let mut listed: Vec<_> = counted.into_values().collect();
    listed.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    listed.into_iter().map(|(_, _, author)| author).collect()
}

pub(crate) fn matching(ui: &Ui, cx: &App) -> Vec<Author> {
    let query = ui.commit_options.filter.read(cx).text().trim().to_lowercase();
    authors(ui)
        .into_iter()
        .filter(|author| {
            query.split_whitespace().all(|word| {
                author.name.to_lowercase().contains(word) || author.email.to_lowercase().contains(word)
            })
        })
        .collect()
}

fn highlighted(ui: &Ui, cx: &App) -> Option<usize> {
    let shown = matching(ui, cx).len().min(SHOWN_AUTHORS);
    if shown == 0 {
        return None;
    }
    let text = ui.commit_options.filter.read(cx).text();
    Some(match &ui.commit_options.pick {
        Some((typed, row)) if typed == text => (*row).min(shown - 1),
        _ => 0,
    })
}

fn tick<T: Desk>(
    id: &'static str,
    on: bool,
    label: &'static str,
    tip: SharedString,
    action: Option<view::Action>,
    cx: &Context<T>,
) -> Stateful<Div> {
    let row = div()
        .id(id)
        .h(px(22.))
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(palette().text_2))
        .rounded(px(4.))
        .border_1()
        .border_color(kit::clear())
        .child(kit::checkbox(on))
        .child(label)
        .tooltip(kit::tip(tip));
    match action {
        Some(action) => {
            let row = row.cursor_pointer().focusable().focus(|s| s.border_color(rgb(palette().accent)));
            kit::pressable(row, action, cx)
        }
        None => row.opacity(0.42),
    }
}

pub(crate) fn disclosure<T: Desk>(app: &T, cx: &mut Context<T>) -> Stateful<Div> {
    let ui = app.ui();
    let open = ui.commit_options.open;
    let label = match chosen(ui, cx) {
        0 => "Options".to_owned(),
        n => format!("Options ({n})"),
    };
    let toggle = div()
        .id("commit-options")
        .h(px(22.))
        .px_1()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .rounded(px(4.))
        .border_1()
        .border_color(kit::clear())
        .cursor_pointer()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(palette().text_2))
        .hover(|s| s.text_color(rgb(palette().text)))
        .focusable()
        .focus(|s| s.border_color(rgb(palette().accent)))
        .child(label)
        .child(kit::icon(if open { "chevron-down" } else { "chevron-right" }).size(px(11.)).text_color(rgb(palette().text_3)))
        .tooltip(kit::tip(if open { "Hide the commit options" } else { "Skip hooks, sign, or push after committing" }));
    kit::pressable(toggle, view::Action::CommitOptions(Action::ToggleOpen), cx)
}

pub(crate) fn options<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<Div> {
    let ui = app.ui();
    if !ui.commit_options.open {
        return None;
    }
    let editable = ui.can_edit_index();
    let skip_tip: SharedString =
        "Commit with --no-verify, so pre-commit and commit-msg do not run. Cleared after each commit.".into();
    let skip = tick(
        "commit-skip-hooks",
        ui.commit_options.skip_hooks,
        SKIP_HOOKS,
        skip_tip,
        editable.then_some(view::Action::CommitOptions(Action::ToggleSkipHooks)),
        cx,
    );
    let config = crate::signing::config(ui);
    let sign_tip: SharedString = match (signs(ui), config.commits) {
        (true, true) => format!("{} Untick to commit this one unsigned (--no-gpg-sign).", config.described()).into(),
        (true, false) => format!("{} Only this commit is signed (-S).", config.described()).into(),
        (false, true) => "commit.gpgSign is on, but this commit will not be signed. Cleared after each commit.".into(),
        (false, false) => "Sign only this commit, with -S. Cleared after each commit.".into(),
    };
    let sign = tick(
        "commit-sign",
        signs(ui),
        SIGN,
        sign_tip,
        editable.then_some(view::Action::CommitOptions(Action::ToggleSign)),
        cx,
    );
    let refusal = push_refusal(ui);
    let push_tip: SharedString = match (refusal, &ui.upstream) {
        (Some(reason), _) => reason.into(),
        (None, Some(upstream)) => format!("Push to {upstream} once the commit is made. Remembered for this repository.").into(),
        (None, None) => "This branch has no upstream yet, so Publish branch opens once the commit is made. Remembered for this repository."
            .into(),
    };
    let push = tick(
        "commit-push-after",
        refusal.is_none() && wants_push(ui, cx),
        PUSH_AFTER,
        push_tip,
        refusal.is_none().then_some(view::Action::CommitOptions(Action::TogglePush)),
        cx,
    );
    Some(div().flex().flex_wrap().items_center().gap_x_4().child(skip).child(sign).child(push))
}

pub(crate) fn coauthor_row<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let picking = ui.commit_options.picking;
    let link = kit::link("add-coauthor", "Add co-author…", view::Action::CommitOptions(Action::OpenCoauthors), cx)
        .tooltip(kit::tip("Credit someone from the history with a Co-authored-by line"));
    div()
        .flex()
        .justify_end()
        .child(
            div()
                .relative()
                .child(link)
                .when(picking, |d| {
                    d.child(
                        deferred(div().absolute().bottom(px(22.)).right_0().child(picker(app, cx))).with_priority(1),
                    )
                }),
        )
}

fn picker<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let found = matching(ui, cx);
    let at = highlighted(ui, cx);
    let none = authors(ui).is_empty();
    let more = found.len().saturating_sub(SHOWN_AUTHORS);
    let mut list = div().py_1().flex().flex_col();
    for (index, author) in found.iter().take(SHOWN_AUTHORS).enumerate() {
        list = list.child(
            kit::item(("coauthor", index), author.name.clone())
                .meta(author.email.clone())
                .highlighted(at == Some(index))
                .on(view::Action::CommitOptions(Action::AddCoauthor(author.clone())), cx),
        );
    }
    kit::float()
        .w(px(320.))
        .flex()
        .flex_col()
        .child(
            div()
                .p_2()
                .border_b_1()
                .border_color(rgb(palette().line))
                .key_context("CoauthorPicker")
                .child(ui.commit_options.filter.clone()),
        )
        .when(none, |d| d.child(div().px_3().py_2().child(kit::muted("No one else has a commit in the loaded history."))))
        .when(!none && found.is_empty(), |d| d.child(div().px_3().py_2().child(kit::muted("No author matches."))))
        .when(!found.is_empty(), |d| d.child(list))
        .when(more > 0, |d| {
            let text = if more == 1 { "1 more. Type to narrow the list.".to_owned() } else { format!("{more} more. Type to narrow the list.") };
            d.child(div().px_3().pb_2().child(kit::muted(text)))
        })
}

pub(crate) fn preferences<T: Desk>(cx: &mut Context<T>) -> Div {
    let chosen = settings::settings(cx);
    div()
        .flex()
        .flex_col()
        .child(crate::preferences::tick(
            "preferences-push-after",
            chosen.push_after_commit,
            "Push after committing",
            "Ticks Push after committing under the commit box. A repository where you changed that tick keeps its own choice.",
            view::Action::CommitOptions(Action::TogglePushDefault),
            cx,
        ))
        .child(crate::preferences::tick(
            "preferences-commit-template",
            chosen.fill_from_template,
            "Fill the commit box from the repository’s template",
            "Uses the file Git’s commit.template names, without its comment lines, whenever the commit box is empty.",
            view::Action::CommitOptions(Action::ToggleTemplate),
            cx,
        ))
        .child(div().pt_3().child(kit::muted("Skip hooks is chosen for each commit and is never remembered.")))
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    !ui.commit_options.picking && ui.commit_options.filter.focus_handle(cx).is_focused(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn author(name: &str, email: &str) -> Author {
        Author { name: name.to_owned(), email: email.to_owned() }
    }

    fn head(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn hook(f: &Fixture, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let hooks = f.dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let path = hooks.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_template_loses_its_comment_lines_and_splits_into_summary_and_description() {
        let text = "Fix: \n# Say what broke\n\nWhy it broke:\n# One paragraph\n  # indented stays\n\nRefs: \n";
        let draft = draft_of(&uncommented(text, "#")).unwrap();
        assert_eq!(draft.summary, "Fix:");
        assert_eq!(draft.description, "Why it broke:\n  # indented stays\n\nRefs:");
        let blank = draft_of(&uncommented("\n# Summary goes here\n\nBody line\n", "#")).unwrap();
        assert_eq!(blank, CommitDraft { summary: String::new(), description: "Body line".to_owned() });
        assert_eq!(draft_of(&uncommented("# only comments\n#\n", "#")), None);
        let semicolons = draft_of(&uncommented("Title\n; note\n# kept\n", ";")).unwrap();
        assert_eq!(semicolons, CommitDraft { summary: "Title".to_owned(), description: "# kept".to_owned() });
        assert_eq!(draft_of("Title\r\n\r\nBody\r\n").unwrap().description, "Body");
    }

    #[test]
    fn an_empty_box_or_the_untouched_template_gives_way_to_an_operations_message() {
        let template = CommitDraft { summary: "Area:".to_owned(), description: "Why:".to_owned() };
        let empty = CommitDraft { summary: String::new(), description: String::new() };
        let edited = CommitDraft { summary: "Area: fixed".to_owned(), description: "Why:".to_owned() };
        assert!(replaceable(&empty, None));
        assert!(replaceable(&empty, Some(&template)));
        assert!(replaceable(&template, Some(&template)));
        assert!(!replaceable(&edited, Some(&template)));
        assert!(!replaceable(&template, None));
    }

    #[test]
    fn the_template_is_read_from_the_repositorys_configuration_with_its_comment_character() {
        let f = Fixture::new();
        assert_eq!(read_template(&f.repo).unwrap(), None);
        fs::write(f.dir.join("message.txt"), "Area: \n\n; Explain the change\nWhat changed\n# heading\n").unwrap();
        f.git(&["config", "commit.template", "message.txt"]);
        assert_eq!(template_path(&f.repo).unwrap(), Some(f.repo.root.join("message.txt")));
        let draft = read_template(&f.repo).unwrap().unwrap();
        assert_eq!(draft.summary, "Area:");
        assert_eq!(draft.description, "; Explain the change\nWhat changed");
        f.git(&["config", "core.commentChar", ";"]);
        assert_eq!(read_template(&f.repo).unwrap().unwrap().description, "What changed\n# heading");
        f.git(&["config", "core.commentChar", "auto"]);
        assert_eq!(comment_string(&f.repo).unwrap(), "#");
        f.git(&["config", "commit.template", f.dir.join("missing.txt").to_str().unwrap()]);
        let error = read_template(&f.repo).unwrap_err();
        assert!(error.contains("missing.txt"), "{error}");
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), "?? message.txt\n");
    }

    #[test]
    fn a_coauthor_trailer_gets_its_own_final_paragraph_once() {
        let ada = author("Ada Lovelace", "ada@example.invalid");
        let alan = author("Alan Turing", "alan@example.invalid");
        assert_eq!(with_coauthor("", &ada), "Co-authored-by: Ada Lovelace <ada@example.invalid>");
        assert_eq!(
            with_coauthor("Explains the change.\n", &ada),
            "Explains the change.\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>"
        );
        let once = with_coauthor("Body\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>", &alan);
        assert_eq!(
            once,
            "Body\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>\nCo-authored-by: Alan Turing <alan@example.invalid>"
        );
        assert_eq!(with_coauthor(&once, &alan), once);
        assert_eq!(
            with_coauthor("Body\n\nSee: the notes", &ada),
            "Body\n\nSee: the notes\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>"
        );
        assert_eq!(
            with_coauthor("Body\n\nSigned-off-by: Dan <dan@example.invalid>", &ada),
            "Body\n\nSigned-off-by: Dan <dan@example.invalid>\nCo-authored-by: Ada Lovelace <ada@example.invalid>"
        );
        assert_eq!(coauthor(" Ada Lovelace <ada@example.invalid> "), Some(ada));
        assert_eq!(coauthor("Alan Turing<alan@example.invalid>"), Some(alan));
        assert_eq!(coauthor("nobody"), None);
        assert_eq!(coauthor("<only@example.invalid>"), None);
        assert_eq!(coauthor("Nameless <>"), None);
    }

    #[test]
    fn skipping_hooks_commits_past_a_failing_pre_commit_but_still_runs_post_commit() {
        let f = Fixture::new();
        fs::write(f.dir.join("a.txt"), "one\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.repo.commit("Base").unwrap();
        let before = head(&f);
        hook(&f, "pre-commit", "echo refused >&2\nexit 1\n");
        hook(&f, "commit-msg", "exit 1\n");
        let marker = f.dir.join(".git/post-commit-ran");
        hook(&f, "post-commit", &format!(": > '{}'\n", marker.display()));
        fs::write(f.dir.join("a.txt"), "two\n").unwrap();
        f.git(&["add", "a.txt"]);
        let refused = f.repo.commit_with("Blocked", Hooks::Run).unwrap_err();
        assert_eq!(refused.plain.as_ref().map(|plain| plain.0.as_str()), Some("pre-commit stopped the commit"));
        assert_eq!(head(&f), before);
        assert!(!marker.exists());
        let committed = f.repo.commit_with("Past the hooks", Hooks::Skip).unwrap();
        assert_eq!(committed.head.as_deref(), Some(head(&f).as_str()));
        assert_ne!(head(&f), before);
        assert_eq!(committed.hooks, ["post-commit"]);
        assert!(marker.exists());
        assert_eq!(String::from_utf8(f.git(&["show", "HEAD:a.txt"])).unwrap(), "two\n");
        assert_eq!(String::from_utf8(f.git(&["diff", "--cached", "--name-only"])).unwrap(), "");
        assert_eq!(f.repo.runnable_hooks(&Hooks::Skip.events(crate::hooks::COMMIT)), ["post-commit"]);
    }

    #[test]
    fn skipping_hooks_on_amend_and_commit_all_keeps_their_guards() {
        let f = Fixture::new();
        fs::write(f.dir.join("a.txt"), "one\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.repo.commit("Base").unwrap();
        hook(&f, "pre-commit", "exit 1\n");
        let base = head(&f);
        let reference = std::ffi::OsString::from("refs/heads/main");
        fs::write(f.dir.join("b.txt"), "new\n").unwrap();
        let amended = f.repo.amend_with(&base, Some(&reference), Some("Base, reworded"), Hooks::Skip).unwrap();
        assert_ne!(head(&f), base);
        assert_eq!(amended.head.as_deref(), Some(head(&f).as_str()));
        assert_eq!(String::from_utf8(f.git(&["log", "-1", "--format=%s"])).unwrap(), "Base, reworded\n");
        let stale = f.repo.amend_with(&base, Some(&reference), Some("Again"), Hooks::Skip).unwrap_err();
        assert!(stale.text.contains("changed"), "{}", stale.text);
        let tip = head(&f);
        assert!(f.repo.commit_all_with(Some(&base), Some(&reference), "Stale", Hooks::Skip).is_err());
        assert_eq!(head(&f), tip);
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), "?? b.txt\n");
        f.repo.commit_all_with(Some(&tip), Some(&reference), "Everything", Hooks::Skip).unwrap();
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), "");
        assert_eq!(String::from_utf8(f.git(&["show", "HEAD:b.txt"])).unwrap(), "new\n");
    }

    #[test]
    fn the_hooks_skipped_by_no_verify_are_left_out_of_the_expected_list() {
        assert_eq!(Hooks::Run.events(crate::hooks::COMMIT), crate::hooks::COMMIT);
        assert_eq!(Hooks::Skip.events(crate::hooks::COMMIT), ["prepare-commit-msg", "post-commit"]);
        assert_eq!(Hooks::Skip.events(crate::hooks::AMEND), ["prepare-commit-msg", "post-commit", "post-rewrite"]);
    }
}
