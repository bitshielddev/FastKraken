use crate::{
    Tab, kit,
    repository::{
        ChangedFile, Repository, checked, parse_changed_files, raw_path, validate_oid,
    },
    theme::*,
    view::{self, Desk, Menu, Selection},
};
use crate::view::commit_input::{CommitInput, Look};
use crate::file_list::{self, Row, Which};
use gpui::{Context, Div, Focusable, FontWeight, SharedString, Window, div, prelude::*, rgb, uniform_list};
use crate::theme::px;
use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

pub(crate) struct Message {
    pub(crate) subject: gpui::Entity<CommitInput>,
    pub(crate) body: gpui::Entity<CommitInput>,
}

impl Message {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let subject = Look { size: 14., line: 20., weight: FontWeight::SEMIBOLD, secondary: false };
        let body = Look { size: TEXT_SMALL, line: 18., weight: FontWeight::NORMAL, secondary: true };
        Self {
            subject: cx.new(|cx| CommitInput::reader(cx, "Commit subject", subject)),
            body: cx.new(|cx| CommitInput::reader(cx, "Commit message", body)),
        }
    }

    pub(crate) fn focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.subject.focus_handle(cx).is_focused(window) || self.body.focus_handle(cx).is_focused(window)
    }
}

fn show_message(tab: &mut Tab, subject: &str, body: &str, cx: &mut Context<Tab>) {
    let message = &tab.ui.message;
    message.subject.update(cx, |field, cx| field.show(subject, cx));
    message.body.update(cx, |field, cx| field.show(body, cx));
}

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, commit};
    &[
        Command::new(
            "commit.copy-sha",
            Group::Copy,
            |_, _| "Copy SHA".into(),
            |_, target, _| {
                if commit(target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |_, target| commit(target).map(copy_sha),
        )
        .words(&["hash", "id", "clipboard"])
        .icon("copy"),
    ]
};

struct CommitDetails {
    author: String,
    email: String,
    committer: String,
    committer_email: String,
    subject: String,
    written: String,
    committed: String,
    parents: Vec<String>,
    coauthors: Vec<crate::commit_options::Author>,
    body: String,
    files: Vec<ChangedFile>,
    stats: HashMap<PathBuf, (u32, u32)>,
}

impl Repository {
    fn commit_details(&self, oid: &str) -> Result<CommitDetails, String> {
        validate_oid(oid)?;
        let output = checked(self.read_command().args([
            "show",
            "--format=%an%x00%ae%x00%cn%x00%ce%x00%s%x00%ad%x00%cd%x00%P%x00%(trailers:key=Co-authored-by,valueonly)%x00%b",
            "--date=format-local:%-d %b %Y, %H:%M",
            "--no-show-signature",
            "--no-notes",
            "--root",
            "--diff-merges=first-parent",
            "--find-renames",
            "--no-color",
            "--name-status",
            "-z",
            oid,
            "--",
        ]))?;
        let mut header = output.splitn(11, |&b| b == 0);
        let mut text = |missing: &'static str| {
            header
                .next()
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .ok_or(missing)
        };
        let author = text("Missing commit author")?;
        let email = text("Missing author email")?;
        let committer = text("Missing committer")?;
        let committer_email = text("Missing committer email")?;
        let subject = text("Missing commit subject")?;
        let written = text("Missing commit date")?;
        let committed = text("Missing commit date")?;
        let parents = text("Missing commit parents")?;
        let coauthors = text("Missing commit trailers")?;
        let body = text("Missing commit message")?;
        let paths = header.next().ok_or("Missing changed-file list")?;
        let paths = paths.strip_prefix(b"\n").unwrap_or(paths);
        let files = parse_changed_files(paths)?;
        Ok(CommitDetails {
            author,
            email,
            committer,
            committer_email,
            subject,
            written,
            committed,
            parents: parents.split_whitespace().map(str::to_owned).collect(),
            coauthors: coauthors.lines().filter_map(crate::commit_options::coauthor).collect(),
            body: body.trim().to_owned(),
            files,
            stats: self.commit_stats(oid).unwrap_or_default(),
        })
    }

    fn commit_stats(&self, oid: &str) -> Result<HashMap<PathBuf, (u32, u32)>, String> {
        let output = checked(self.read_command().args([
            "show",
            "--format=",
            "--no-show-signature",
            "--no-notes",
            "--root",
            "--diff-merges=first-parent",
            "--find-renames",
            "--no-color",
            "--numstat",
            "-z",
            oid,
            "--",
        ]))?;
        let mut stats = HashMap::new();
        let mut records = output.split(|&b| b == 0);
        while let Some(record) = records.next() {
            let record = record.strip_prefix(b"\n").unwrap_or(record);
            if record.is_empty() {
                continue;
            }
            let mut fields = record.splitn(3, |&b| b == b'\t');
            let mut count = || {
                std::str::from_utf8(fields.next()?)
                    .ok()?
                    .parse::<u32>()
                    .ok()
            };
            let counts = count().zip(count());
            let path = match fields.next() {
                Some([]) => {
                    records.next();
                    records.next()
                }
                path => path,
            };
            if let (Some(counts), Some(path)) = (counts, path) {
                stats.insert(raw_path(path), counts);
            }
        }
        Ok(stats)
    }
}

#[derive(Default)]
pub struct State {
    oid: Option<String>,
    loading_details: bool,
    pub author: String,
    pub email: String,
    pub committer: String,
    pub committer_email: String,
    pub subject: Option<String>,
    pub written: String,
    pub committed: String,
    pub parents: Vec<String>,
    pub body: String,
    pub(crate) coauthors: Vec<crate::commit_options::Author>,
    pub files: crate::file_list::Files<ChangedFile>,
    stats: HashMap<PathBuf, (u32, u32)>,
    pub path: Option<PathBuf>,
    opening: Option<PathBuf>,
    pub(crate) scroll: gpui::UniformListScrollHandle,
    body_scroll: gpui::ScrollHandle,
}

impl State {
    pub(crate) fn from(&self, oid: &str, path: &Path) -> Option<PathBuf> {
        if self.oid.as_deref() != Some(oid) {
            return None;
        }
        self.files.iter().find(|file| file.path == path).and_then(|file| file.from.clone())
    }

    pub(crate) fn file(&self, oid: &str, path: &Path) -> Option<&ChangedFile> {
        if self.oid.as_deref() != Some(oid) {
            return None;
        }
        self.files.iter().find(|file| file.path == path)
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    OpenFile(PathBuf),
    AllChanges,
}

pub fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    crate::containing::selected(tab, oid, cx);
    crate::signing::selected(tab, oid, cx);
    let state = &mut tab.ui.inspection;
    state.path = None;
    state.opening = None;
    if oid.is_some()
        && state.oid.as_deref() == oid
        && (state.loading_details || state.subject.is_some())
    {
        return;
    }
    state.author.clear();
    state.email.clear();
    state.committer.clear();
    state.committer_email.clear();
    state.subject = None;
    state.written.clear();
    state.committed.clear();
    state.parents.clear();
    state.body.clear();
    state.coauthors.clear();
    state.files.clear();
    state.stats.clear();
    state.oid = oid.map(str::to_owned);
    state.loading_details = false;
    let listed = oid.and_then(|oid| tab.ui.history.iter().find(|row| row.oid == oid)).map(|row| row.subject.clone());
    show_message(tab, listed.as_deref().unwrap_or_default(), "", cx);
    let (Some(oid), Some(repo)) = (oid.map(str::to_owned), tab.repo.clone()) else {
        return;
    };
    tab.ui.inspection.loading_details = true;
    let epoch = tab.epoch;
    let selected_oid = oid.clone();
    Tab::work(
        cx,
        move || repo.commit_details(&oid),
        move |tab, result, cx| {
            let state = &mut tab.ui.inspection;
            if tab.epoch != epoch || state.oid.as_ref() != Some(&selected_oid) {
                return;
            }
            state.loading_details = false;
            match result {
                Ok(details) => {
                    state.author = details.author;
                    state.email = details.email;
                    state.committer = details.committer;
                    state.committer_email = details.committer_email;
                    state.written = details.written;
                    state.committed = details.committed;
                    state.parents = details.parents;
                    state.coauthors = details.coauthors;
                    state.body = details.body;
                    state.stats = details.stats;
                    let paths = details.files.iter().map(|file| file.path.clone()).collect();
                    state.files.set(details.files);
                    tab.ui.diff_title = details.subject.clone();
                    state.subject = Some(details.subject.clone());
                    crate::lfs::listed(tab, crate::lfs::Source::Tree(selected_oid.clone()), paths, cx);
                    let body = tab.ui.inspection.body.clone();
                    show_message(tab, &details.subject, &body, cx);
                    open_waiting(tab, cx);
                }
                Err(error) => tab.error(error, cx),
            }
            cx.notify();
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    let Some(Selection::Commit(oid)) = tab.ui.selection.clone() else {
        return;
    };
    let state = &mut tab.ui.inspection;
    if state.oid.as_ref() != Some(&oid) {
        return;
    }
    match action {
        Action::OpenFile(path) => {
            if tab.ui.file_history.chosen().is_none_or(|entry| entry.oid != oid || entry.path != path) {
                crate::file_history::leave(tab);
            }
            open_file(tab, oid, path, cx)
        }
        Action::AllChanges => {
            crate::file_history::leave(tab);
            let state = &mut tab.ui.inspection;
            state.path = None;
            tab.ui.diff_open = true;
            tab.load_selected_patch(Selection::Commit(oid), None, cx);
        }
    }
    cx.notify();
}

fn open_file(tab: &mut Tab, oid: String, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(index) = tab.ui.inspection.files.iter().position(|file| file.path == path) else {
        return;
    };
    tab.ui.navigate.opened(&oid, index);
    let state = &mut tab.ui.inspection;
    state.path = Some(path.clone());
    tab.ui.diff_open = true;
    tab.load_selected_patch(Selection::Commit(oid), Some(path), cx);
}

pub(crate) fn open_when_loaded(tab: &mut Tab, path: PathBuf, cx: &mut Context<Tab>) {
    tab.ui.inspection.opening = Some(path);
    open_waiting(tab, cx);
}

fn open_waiting(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &tab.ui.inspection;
    if state.loading_details || state.subject.is_none() || state.opening.is_none() {
        return;
    }
    let (Some(Selection::Commit(oid)), Some(path)) = (tab.ui.selection.clone(), tab.ui.inspection.opening.take()) else {
        return;
    };
    if tab.ui.inspection.oid.as_ref() == Some(&oid) {
        open_file(tab, oid, path, cx);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Person {
    pub name: String,
    pub email: String,
    pub when: String,
}

fn same_person(state: &State) -> bool {
    state.committer.is_empty()
        || (state.committer == state.author && state.committer_email.eq_ignore_ascii_case(&state.email))
}

pub(crate) fn people(state: &State) -> Vec<Person> {
    if state.author.is_empty() {
        return Vec::new();
    }
    let same = same_person(state);
    let mut when = format!("authored {}", state.written);
    if same && !state.committed.is_empty() && state.committed != state.written {
        when.push_str(&format!(" · committed {}", state.committed));
    }
    let mut people = vec![Person { name: state.author.clone(), email: state.email.clone(), when }];
    if !same {
        people.push(Person {
            name: state.committer.clone(),
            email: state.committer_email.clone(),
            when: format!("committed {}", state.committed),
        });
    }
    people
}

fn person_row(index: usize, person: Person) -> gpui::Stateful<Div> {
    let tip = view::author_tip(&person.name, &person.email);
    div()
        .id(("commit-person", index))
        .flex()
        .items_start()
        .gap(px(6.))
        .child(div().pt(px(1.)).child(kit::avatar(&person.name).email(&person.email)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(kit::fit(person.name).size(TEXT_SMALL))
                        .child(kit::fit(person.email).size(TEXT_SMALL).color(palette().text_3).grow()),
                )
                .child(kit::fit(person.when).size(TEXT_SMALL).color(palette().text_3)),
        )
        .tooltip(kit::tip(tip))
}

pub(crate) fn copy_sha(oid: &str) -> view::Action {
    view::Action::CopyText {
        text: oid.to_owned(),
        what: "SHA",
    }
}

pub fn panel<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.inspection;
    let Some(Selection::Commit(oid)) = &ui.selection else {
        return div();
    };
    let more_open = matches!(ui.menu, Some(Menu::CommitActions));
    let reword = crate::rewrite::offered(ui, "commit.reword", std::slice::from_ref(oid), cx);
    let (added, deleted) = state
        .stats
        .values()
        .fold((0, 0), |sum, stat| (sum.0 + stat.0, sum.1 + stat.1));
    let count = state.files.len();
    let short: String = oid.chars().take(7).collect();
    let summary = div()
        .p_3()
        .flex()
        .flex_col()
        .gap(px(7.))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(div().flex_1().min_w_0().child(ui.message.subject.clone()))
                .when_some(reword, |d, entry| {
                    let tip = entry.disabled.clone().unwrap_or_else(|| "Edit this commit's message".to_owned());
                    d.child(
                        kit::btn("commit-edit-message", "Edit")
                            .small()
                            .ghost()
                            .icon("pencil")
                            .enabled(entry.disabled.is_none())
                            .tip(tip)
                            .on(entry.action.unwrap_or(view::Action::Dismiss), cx),
                    )
                })
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(kit::icon_btn(
                            "commit-more",
                            "more",
                            "More actions",
                            view::Action::Menu((!more_open).then_some(Menu::CommitActions)),
                            cx,
                        ))
                        .when(more_open, |d| {
                            let entries = crate::commands::entries_of(ui, &Menu::CommitActions, cx);
                            let surface = crate::commands::surface(entries, 270., ui.menu_cursor, cx);
                            d.child(kit::popover(surface, true))
                        }),
                ),
        )
        .when(!state.body.is_empty(), |d| {
            d.child(
                div()
                    .child(
                        div()
                            .id("commit-body")
                            .max_h(px(136.))
                            .overflow_y_scroll()
                            .track_scroll(&state.body_scroll)
                            .child(ui.message.body.clone()),
                    )
                    .child(kit::scrollbar("commit-body-scrollbar", &state.body_scroll)),
            )
        })
        .children(people(state).into_iter().enumerate().map(|(index, person)| person_row(index, person)))
        .children(state.coauthors.iter().enumerate().map(|(index, coauthor)| {
            div()
                .id(("commit-coauthor", index))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::avatar(&coauthor.name).email(&coauthor.email))
                .child(kit::fit(coauthor.name.clone()).size(TEXT_SMALL))
                .child(kit::fit("co-author").size(TEXT_SMALL).color(palette().text_3).grow())
                .tooltip(kit::tip(coauthor.email.clone()))
        }))
        .children(crate::containing::lines(ui, oid, cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id("copy-sha")
                        .h(px(20.))
                        .px(px(6.))
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap(px(5.))
                        .rounded(px(RADIUS_CHIP))
                        .bg(rgb(palette().raised))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(palette().hover)))
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .child(short)
                        .child(kit::icon("copy").size(px(11.)).text_color(rgb(palette().text_3)))
                        .tooltip(kit::tip("Copy the full SHA"))
                        .on_click(cx.listener({
                            let oid = oid.clone();
                            move |app, _, w, cx| {
                                app.dispatch(copy_sha(&oid), w, cx)
                            }
                        })),
                )
                .when(!state.parents.is_empty(), |d| {
                    let mut row = d.child(kit::muted(if state.parents.len() == 1 {
                        "parent"
                    } else {
                        "parents"
                    }));
                    for (i, parent) in state.parents.iter().enumerate() {
                        let target = parent.clone();
                        row = row.child(
                            div()
                                .id(("parent", i))
                                .cursor_pointer()
                                .font_family(MONO)
                                .text_size(px(11.5))
                                .text_color(rgb(palette().accent))
                                .child(parent.chars().take(7).collect::<String>())
                                .tooltip(kit::tip("Show this parent in the graph"))
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    app.dispatch(
                                        view::Action::Reveal(crate::reveal::Action::Commit(target.clone())),
                                        w,
                                        cx,
                                    )
                                })),
                        );
                    }
                    row
                }),
        )
        .children(crate::signing::lines(ui, oid, cx))
        .child(
            div()
                .pt(px(2.))
                .flex()
                .gap(px(6.))
                .child(
                    kit::btn("new-branch", "Branch…")
                        .small()
                        .icon("branch")
                        .enabled(!ui.busy())
                        .on(
                            view::Action::Refs(crate::refs::Action::New(crate::refs::Kind::Branch)),
                            cx,
                        ),
                )
                .child(
                    kit::btn("new-tag", "Tag…")
                        .small()
                        .icon("tag")
                        .enabled(!ui.busy())
                        .on(
                            view::Action::Refs(crate::refs::Action::New(crate::refs::Kind::Tag)),
                            cx,
                        ),
                ),
        );
    let heading = div()
        .h(px(28.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(kit::label(if state.loading_details {
            "Loading changed files…".to_owned()
        } else if count == 1 {
            "1 file changed".to_owned()
        } else {
            format!("{count} files changed")
        }))
        .child(kit::grow())
        .when(added > 0, |d| {
            d.child(
                kit::label(format!("+{added}"))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(palette().added)),
            )
        })
        .when(deleted > 0, |d| {
            d.child(
                kit::label(format!("−{deleted}"))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(palette().deleted)),
            )
        })
        .when(count > 1, |d| {
            d.child(
                kit::btn("show-changed-files", "View all")
                    .small()
                    .ghost()
                    .on(view::Action::Inspect(Action::AllChanges), cx),
            )
        })
        .when(count > 0, |d| d.child(file_list::tools(ui, file_list::Panel::Commit, cx)));
    let rows = file_list::rows(ui, Which::Commit);
    let files = uniform_list(
        SharedString::from(format!("committed-file-{oid}")),
        rows.len(),
        cx.processor(move |app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let state = &ui.inspection;
            let cursor = ui.navigate.file_cursor(ui);
            let mode = ui.file_list.mode;
            let lfs_tree = state.oid.clone().map(crate::lfs::Source::Tree);
            range
                .filter_map(|at| {
                    let (i, depth) = match rows.get(at)? {
                        Row::File { index, depth } => (*index, *depth),
                        row => {
                            let side = Which::Commit.side(ui);
                            return Some(file_list::folder_row(("committed-folder", at), Which::Commit, row, side, None, cx));
                        }
                    };
                    let file = state.files.get(i)?;
                    let path = file.path.clone();
                    let target = crate::commands::Target::File {
                        path: file.path.clone(),
                        side: crate::commands::Side::Commit(state.oid.clone().unwrap_or_default()),
                    };
                    let open = ui.diff_open && state.path.as_ref() == Some(&file.path);
                    let stat = state.stats.get(&file.path).copied();
                    Some(
                        kit::cursor(kit::row(("committed-file", i), open), !open && cursor == Some(i))
                            .h(whole_px(FILE_ROW))
                            .pl(px(file_list::indent(mode, depth)))
                            .pr_3()
                            .gap_2()
                            .child(file_list::label(
                                ("committed-file-label", i),
                                file.status,
                                &file.path,
                                file.from.as_deref(),
                                mode,
                            ))
                            .when(lfs_tree.as_ref().is_some_and(|tree| ui.lfs.marked(tree, &file.path)), |d| d.child(crate::lfs::badge()))
                            .when_some(stat, |d, (added, deleted)| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_shrink_0()
                                        .gap(px(5.))
                                        .text_size(px(TEXT_TINY))
                                        .font_weight(FontWeight::MEDIUM)
                                        .when(added > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(rgb(palette().added))
                                                    .child(format!("+{added}")),
                                            )
                                        })
                                        .when(deleted > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(rgb(palette().deleted))
                                                    .child(format!("−{deleted}")),
                                            )
                                        }),
                                )
                            })
                            .on_click(cx.listener(move |app, _, w, cx| {
                                app.dispatch(
                                    view::Action::Inspect(Action::OpenFile(path.clone())),
                                    w,
                                    cx,
                                )
                            }))
                            .on_mouse_down(
                                gpui::MouseButton::Right,
                                cx.listener(move |app, event: &gpui::MouseDownEvent, w, cx| {
                                    let target = target.clone();
                                    app.dispatch(
                                        view::Action::Menu(Some(Menu::Object { target, at: event.position })),
                                        w,
                                        cx,
                                    )
                                }),
                            )
                            .into_any_element(),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(state.scroll.clone())
    .flex_1()
    .min_h_0();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(summary)
        .child(heading)
        .children(file_list::filter_field(ui, file_list::Panel::Commit, cx))
        .children(file_list::nothing_matches(ui, file_list::Panel::Commit))
        .child(files)
        .child(kit::scrollbar("committed-files-scrollbar", &state.scroll))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn details_state(details: CommitDetails) -> State {
        State {
            author: details.author,
            email: details.email,
            committer: details.committer,
            committer_email: details.committer_email,
            written: details.written,
            committed: details.committed,
            ..State::default()
        }
    }

    #[test]
    fn commit_details_name_the_committer_both_emails_both_dates_and_the_coauthor_trailers() {
        let f = Fixture::new();
        fs::write(f.dir.join("camp.txt"), "camp\n").unwrap();
        f.git(&["add", "camp.txt"]);
        let message = "Pitch the camp\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>\nSigned-off-by: Cedar <cedar@example.invalid>\nco-authored-by: Alan Turing <alan@example.invalid>";
        checked(
            f.repo
                .command()
                .env("GIT_AUTHOR_NAME", "Juniper Fox")
                .env("GIT_AUTHOR_EMAIL", "juniper@example.invalid")
                .env("GIT_AUTHOR_DATE", "2026-03-04T05:06:00Z")
                .env("GIT_COMMITTER_NAME", "Rowan Ash")
                .env("GIT_COMMITTER_EMAIL", "rowan@example.invalid")
                .env("GIT_COMMITTER_DATE", "2026-05-06T07:08:00Z")
                .args(["commit", "-qm", message]),
        )
        .unwrap();
        let tip = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let refs = f.git(&["for-each-ref"]);
        let index = f.git(&["ls-files", "-s"]);
        let details = f.repo.commit_details(&tip).unwrap();
        assert_eq!((details.author.as_str(), details.email.as_str()), ("Juniper Fox", "juniper@example.invalid"));
        assert_eq!((details.committer.as_str(), details.committer_email.as_str()), ("Rowan Ash", "rowan@example.invalid"));
        assert_eq!(details.subject, "Pitch the camp");
        assert!(details.written.contains("2026") && details.committed.contains("2026"));
        assert_ne!(details.written, details.committed);
        let names: Vec<_> = details.coauthors.iter().map(|author| (author.name.as_str(), author.email.as_str())).collect();
        assert_eq!(names, [("Ada Lovelace", "ada@example.invalid"), ("Alan Turing", "alan@example.invalid")]);
        assert!(details.body.contains("Co-authored-by: Ada Lovelace"));
        assert_eq!(details.files.len(), 1);
        let (written, committed) = (details.written.clone(), details.committed.clone());
        let people = people(&details_state(details));
        assert_eq!(
            people,
            [
                Person { name: "Juniper Fox".into(), email: "juniper@example.invalid".into(), when: format!("authored {written}") },
                Person { name: "Rowan Ash".into(), email: "rowan@example.invalid".into(), when: format!("committed {committed}") },
            ]
        );
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(f.git(&["ls-files", "-s"]), index);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
    }

    #[test]
    fn the_committer_line_shows_only_when_someone_else_committed() {
        let f = Fixture::new();
        fs::write(f.dir.join("camp.txt"), "camp\n").unwrap();
        f.git(&["add", "camp.txt"]);
        checked(
            f.repo
                .command()
                .env("GIT_AUTHOR_DATE", "2026-03-04T05:06:00Z")
                .env("GIT_COMMITTER_DATE", "2026-03-04T05:06:00Z")
                .args(["commit", "-qm", "Same hands\n\nCo-authored-by: not a trailer\nplain text"]),
        )
        .unwrap();
        let tip = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let details = f.repo.commit_details(&tip).unwrap();
        assert!(details.coauthors.is_empty());
        let written = details.written.clone();
        let mut state = details_state(details);
        assert_eq!(
            people(&state),
            [Person { name: "Cedar".into(), email: "cedar@example.invalid".into(), when: format!("authored {written}") }]
        );
        state.committed = "6 May 2026, 07:08".into();
        state.committer_email = "CEDAR@example.invalid".into();
        assert_eq!(people(&state).len(), 1);
        assert_eq!(people(&state)[0].when, format!("authored {written} · committed 6 May 2026, 07:08"));
        assert!(f.repo.commit_details("--all").is_err());
    }

    fn write(f: &Fixture, path: &Path, content: &str) {
        fs::write(f.dir.join(path), content).unwrap();
        f.repo.stage_file(path).unwrap();
    }

    #[test]
    fn obs02_ext08_selected_file_patch_and_history_are_literal_and_anchored() {
        let f = Fixture::new();
        let notes = Path::new(":(glob)*\nnotes.txt");
        let other = Path::new("checklist.txt");
        f.git(&["config", "user.name", "Cedar Contributor"]);
        write(&f, notes, "First draft\n");
        f.repo.commit("Start notebook").unwrap();
        write(&f, other, "Unrelated\n");
        f.repo.commit("Add checklist").unwrap();
        write(&f, notes, "First draft\nFastKraken: git for the impatient\n");
        write(&f, other, "Change in the same commit\n");
        f.repo.commit("Revise notebook").unwrap();
        let tip = f.repo.status().unwrap().head.unwrap();
        write(&f, notes, "Later edit\n");
        f.repo.commit("Later revision").unwrap();

        let details = f.repo.commit_details(&tip).unwrap();
        assert_eq!(details.author, "Cedar Contributor");
        assert_eq!(details.subject, "Revise notebook");
        assert_eq!(
            details.files,
            [
                ChangedFile {
                    status: "Modified",
                    path: notes.to_owned(),
                    from: None,
                    submodule: None
                },
                ChangedFile {
                    status: "Modified",
                    path: other.to_owned(),
                    from: None,
                    submodule: None
                }
            ]
        );
        let diff = f.repo.commit_path_diff(&tip, notes, None).unwrap();
        assert!(diff.text.contains("+FastKraken: git for the impatient"));
        assert!(!diff.text.contains("Change in the same commit"));
        let start = crate::file_history::Start::Commit(tip.clone());
        let history = f.repo.file_revisions(&start, notes, 0, 64).unwrap().1;
        let subjects = history
            .iter()
            .map(|r| r.subject.as_str())
            .collect::<Vec<_>>();
        assert_eq!(subjects, ["Revise notebook", "Start notebook"]);
        assert_eq!(
            f.repo.file_revisions(&start, notes, 1, 1).unwrap().1,
            history[1..]
        );
        let first = f.repo.commit_details(&history[1].oid).unwrap();
        assert_eq!(
            first.files,
            [ChangedFile {
                status: "Added",
                path: notes.to_owned(),
                from: None,
                submodule: None
            }]
        );
    }

    #[test]
    fn first_parent_files_include_incoming_addition_and_empty_commit_has_no_files() {
        let f = Fixture::new();
        let main_path = Path::new("trail.txt");
        let incoming = Path::new("map.txt");
        f.git(&["commit", "-qm", "Start notebook", "--allow-empty"]);
        let empty = f.repo.status().unwrap().head.unwrap();
        assert!(f.repo.commit_details(&empty).unwrap().files.is_empty());
        f.git(&["switch", "-qc", "feature/map"]);
        write(&f, incoming, "Creek\n");
        f.repo.commit("Add creek map").unwrap();
        f.git(&["switch", "-q", "main"]);
        write(&f, main_path, "Cedar\n");
        f.repo.commit("Main route").unwrap();
        f.git(&["merge", "-q", "--no-edit", "feature/map"]);
        let merge = f.repo.status().unwrap().head.unwrap();
        assert_eq!(
            f.repo.commit_details(&merge).unwrap().files,
            [ChangedFile {
                status: "Added",
                path: incoming.to_owned(),
                from: None,
                submodule: None
            }]
        );
        assert!(
            f.repo
                .commit_path_diff(&merge, incoming, None)
                .unwrap()
                .text
                .contains("+Creek")
        );
    }

    const TRAIL: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    fn moved_notes(f: &Fixture) -> Vec<String> {
        let old = Path::new("notes.txt");
        write(f, old, TRAIL);
        f.repo.commit("Start notes").unwrap();
        write(f, old, &TRAIL.replace("two", "TWO"));
        write(f, Path::new("other.txt"), "other\n");
        f.repo.commit("Edit notes").unwrap();
        fs::create_dir(f.dir.join("docs")).unwrap();
        f.git(&["mv", "notes.txt", "docs/notes é.md"]);
        write(f, Path::new("docs/notes é.md"), &TRAIL.replace("two", "TWO").replace("nine", "NINE"));
        write(f, Path::new("other.txt"), "other moved\n");
        f.repo.commit("Move notes").unwrap();
        write(f, Path::new("docs/notes é.md"), &TRAIL.replace("two", "TWO").replace("nine", "NINE").replace("ten", "TEN"));
        f.repo.commit("Edit moved notes").unwrap();
        String::from_utf8(f.git(&["log", "--format=%H"])).unwrap().lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_commit_that_renamed_a_file_lists_one_row_and_its_patch_shows_only_the_changed_lines() {
        let f = Fixture::new();
        let commits = moved_notes(&f);
        let moved = &commits[1];
        let new = Path::new("docs/notes é.md");
        let details = f.repo.commit_details(moved).unwrap();
        assert_eq!(
            details.files,
            [
                ChangedFile { status: "Renamed", path: new.to_owned(), from: Some("notes.txt".into()), submodule: None },
                ChangedFile { status: "Modified", path: "other.txt".into(), from: None, submodule: None },
            ]
        );
        assert_eq!(details.stats.get(new), Some(&(1, 1)));
        assert_eq!(details.stats.get(Path::new("other.txt")), Some(&(1, 1)));
        assert_eq!(details.stats.len(), 2);
        let diff = f.repo.commit_path_diff(moved, new, Some(Path::new("notes.txt"))).unwrap();
        let changed: Vec<&str> = diff
            .text
            .lines()
            .filter(|line| (line.starts_with('+') || line.starts_with('-')) && !line.starts_with("+++") && !line.starts_with("---"))
            .collect();
        assert_eq!(changed, ["-nine", "+NINE"]);
        assert!(!diff.text.contains("other moved"));
        let whole = f.repo.commit_diff(moved).unwrap();
        assert!(whole.text.contains("rename from notes.txt"));
        assert!(!whole.text.contains("-eight"));
    }
}
