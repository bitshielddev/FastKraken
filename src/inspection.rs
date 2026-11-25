use crate::{
    Tab, kit,
    repository::{
        ChangedFile, Repository, changed_status, checked, paired_status, parse_changed_files,
        raw_path, validate_oid, validate_path,
    },
    theme::*,
    view::{self, Desk, Menu, Selection},
};
use gpui::{Context, Div, FontWeight, SharedString, Window, div, prelude::*, px, rgb, uniform_list};
use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

const HISTORY_PAGE: usize = 64;

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Command, Group, Side, Target, busy, commit, operating};
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
        Command::new(
            "file.history",
            Group::View,
            |ui, target| {
                if listing(ui, target) { "Hide file history" } else { "File history" }.into()
            },
            |ui, target, _| {
                let Target::File { path, side: Side::Commit(oid) } = target else {
                    return Availability::Hidden;
                };
                let shown = matches!(&ui.selection, Some(Selection::Commit(id)) if id == oid)
                    && ui.inspection.oid.as_ref() == Some(oid)
                    && ui.inspection.files.iter().any(|file| file.path == *path);
                Availability::unless(busy(ui).or_else(|| operating(ui)).or_else(|| {
                    (!shown).then(|| "Select the commit first".to_owned())
                }))
            },
            |ui, target| {
                let Target::File { path, .. } = target else {
                    return None;
                };
                Some(view::Action::Inspect(if listing(ui, target) {
                    Action::CloseHistory
                } else {
                    Action::FileHistory(path.clone())
                }))
            },
        )
        .icon("history"),
    ]
};

struct CommitDetails {
    author: String,
    subject: String,
    written: String,
    parents: Vec<String>,
    body: String,
    files: Vec<ChangedFile>,
    stats: HashMap<PathBuf, (u32, u32)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileCommit {
    pub oid: String,
    pub subject: String,
    pub path: PathBuf,
}

impl Repository {
    fn commit_details(&self, oid: &str) -> Result<CommitDetails, String> {
        validate_oid(oid)?;
        let output = checked(self.read_command().args([
            "show",
            "--format=%an%x00%s%x00%ad%x00%P%x00%b",
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
        let mut header = output.splitn(6, |&b| b == 0);
        let mut text = |missing: &'static str| {
            header
                .next()
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .ok_or(missing)
        };
        let author = text("Missing commit author")?;
        let subject = text("Missing commit subject")?;
        let written = text("Missing commit date")?;
        let parents = text("Missing commit parents")?;
        let body = text("Missing commit message")?;
        let paths = header.next().ok_or("Missing changed-file list")?;
        let paths = paths.strip_prefix(b"\n").unwrap_or(paths);
        let files = parse_changed_files(paths)?;
        Ok(CommitDetails {
            author,
            subject,
            written,
            parents: parents.split_whitespace().map(str::to_owned).collect(),
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

    fn file_history(
        &self,
        oid: &str,
        path: &Path,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<FileCommit>, String> {
        validate_oid(oid)?;
        validate_path(path)?;
        let output = checked(
            self.read_command()
                .args([
                    "log",
                    "--topo-order",
                    "--follow",
                    "--name-status",
                    "--diff-merges=first-parent",
                    "--no-show-signature",
                    "--no-notes",
                    "--no-color",
                    "--format=%H%x00%s",
                    "-z",
                ])
                .arg(format!("--max-count={}", offset + limit))
                .args([oid, "--"])
                .arg(path),
        )?;
        let mut commits = parse_file_history(&output, path)?;
        commits.drain(..offset.min(commits.len()));
        Ok(commits)
    }
}

fn parse_file_history(output: &[u8], path: &Path) -> Result<Vec<FileCommit>, String> {
    let mut commits = Vec::new();
    let Some(records) = output.strip_suffix(b"\0") else {
        return if output.is_empty() { Ok(commits) } else { Err("Incomplete file history record".into()) };
    };
    let mut fields = records.split(|&b| b == 0).peekable();
    let mut named = path.to_owned();
    while let Some(oid) = fields.next() {
        let oid = String::from_utf8_lossy(oid.strip_prefix(b"\n").unwrap_or(oid)).into_owned();
        validate_oid(&oid).map_err(|_| "Invalid file history record".to_owned())?;
        let subject = fields.next().ok_or("Missing file history subject")?;
        let code = fields.peek().map(|code| code.strip_prefix(b"\n").unwrap_or(code));
        if let Some(code) = code.filter(|code| changed_status(code).is_some()) {
            let paired = paired_status(code);
            fields.next();
            let mut name = || fields.next().map(raw_path).ok_or("Missing file history path");
            if paired {
                name()?;
            }
            named = name()?;
        }
        commits.push(FileCommit {
            oid,
            subject: String::from_utf8_lossy(subject).into_owned(),
            path: named.clone(),
        });
    }
    Ok(commits)
}

#[derive(Default)]
pub struct State {
    oid: Option<String>,
    loading_details: bool,
    pub author: String,
    pub subject: Option<String>,
    pub written: String,
    pub parents: Vec<String>,
    pub body: String,
    pub(crate) coauthors: Vec<crate::commit_options::Author>,
    pub files: Vec<ChangedFile>,
    stats: HashMap<PathBuf, (u32, u32)>,
    pub path: Option<PathBuf>,
    pub history: Option<Vec<FileCommit>>,
    opening: Option<PathBuf>,
    history_ticket: u64,
    loading_history: bool,
    more_history: bool,
    pub(crate) scroll: gpui::UniformListScrollHandle,
}

impl State {
    pub(crate) fn from(&self, oid: &str, path: &Path) -> Option<PathBuf> {
        if self.oid.as_deref() != Some(oid) {
            return None;
        }
        self.files.iter().find(|file| file.path == path).and_then(|file| file.from.clone())
    }

    fn close_history(&mut self) {
        self.history_ticket += 1;
        self.history = None;
        self.loading_history = false;
        self.more_history = false;
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    OpenFile(PathBuf),
    AllChanges,
    History,
    FileHistory(PathBuf),
    OpenRevision { oid: String, path: PathBuf },
    MoreHistory,
    CloseHistory,
}

pub fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.inspection;
    state.close_history();
    state.path = None;
    state.opening = None;
    if oid.is_some()
        && state.oid.as_deref() == oid
        && (state.loading_details || state.subject.is_some())
    {
        return;
    }
    state.author.clear();
    state.subject = None;
    state.written.clear();
    state.parents.clear();
    state.body.clear();
    state.coauthors.clear();
    state.files.clear();
    state.stats.clear();
    state.oid = oid.map(str::to_owned);
    state.loading_details = false;
    let (Some(oid), Some(repo)) = (oid.map(str::to_owned), tab.repo.clone()) else {
        return;
    };
    state.loading_details = true;
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
                    state.written = details.written;
                    state.parents = details.parents;
                    state.coauthors = crate::commit_options::coauthors(&details.body);
                    state.body = details.body;
                    state.stats = details.stats;
                    state.files = details.files;
                    tab.ui.diff_title = details.subject.clone();
                    state.subject = Some(details.subject);
                    open_waiting(tab, cx);
                }
                Err(error) => tab.error(error, cx),
            }
            cx.notify();
        },
    );
}

pub fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if let Action::FileHistory(path) = action {
        if tab.ui.inspection.path.as_ref() != Some(&path) || !tab.ui.diff_open {
            dispatch(tab, Action::OpenFile(path.clone()), window, cx);
        }
        if tab.ui.inspection.path.as_ref() == Some(&path) {
            dispatch(tab, Action::History, window, cx);
        }
        return;
    }
    if let Action::OpenRevision { oid, path } = action {
        tab.dispatch(view::Action::Select(Selection::Commit(oid.clone())), window, cx);
        if tab.ui.selection == Some(Selection::Commit(oid)) {
            tab.ui.inspection.opening = Some(path);
            open_waiting(tab, cx);
        }
        return;
    }
    let Some(Selection::Commit(oid)) = tab.ui.selection.clone() else {
        return;
    };
    let state = &mut tab.ui.inspection;
    if state.oid.as_ref() != Some(&oid) {
        return;
    }
    match action {
        Action::OpenFile(path) => open_file(tab, oid, path, cx),
        Action::AllChanges => {
            state.close_history();
            state.path = None;
            tab.ui.diff_open = true;
            tab.load_selected_patch(Selection::Commit(oid), None, cx);
        }
        Action::History | Action::MoreHistory => {
            let (Some(repo), Some(path)) = (tab.repo.clone(), state.path.clone()) else {
                return;
            };
            if state.loading_history {
                return;
            }
            let offset = match action {
                Action::History => {
                    state.close_history();
                    0
                }
                Action::MoreHistory if state.more_history => {
                    state.history.as_ref().map_or(0, Vec::len)
                }
                _ => return,
            };
            let epoch = tab.epoch;
            state.loading_history = true;
            state.history_ticket += 1;
            let ticket = state.history_ticket;
            Tab::work(
                cx,
                move || repo.file_history(&oid, &path, offset, HISTORY_PAGE + 1),
                move |tab, result, cx| {
                    let state = &mut tab.ui.inspection;
                    if tab.epoch != epoch || state.history_ticket != ticket {
                        return;
                    }
                    state.loading_history = false;
                    match result {
                        Ok(mut rows) => {
                            state.more_history = rows.len() > HISTORY_PAGE;
                            rows.truncate(HISTORY_PAGE);
                            state.history.get_or_insert_with(Vec::new).extend(rows);
                        }
                        Err(error) => tab.error(error, cx),
                    }
                    cx.notify();
                },
            );
        }
        Action::CloseHistory => state.close_history(),
        Action::FileHistory(_) | Action::OpenRevision { .. } => unreachable!(),
    }
    cx.notify();
}

fn open_file(tab: &mut Tab, oid: String, path: PathBuf, cx: &mut Context<Tab>) {
    let Some(index) = tab.ui.inspection.files.iter().position(|file| file.path == path) else {
        return;
    };
    tab.ui.navigate.opened(&oid, index);
    let state = &mut tab.ui.inspection;
    state.close_history();
    state.path = Some(path.clone());
    tab.ui.diff_open = true;
    tab.load_selected_patch(Selection::Commit(oid), Some(path), cx);
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

fn listing(ui: &view::Ui, target: &crate::commands::Target) -> bool {
    matches!(target, crate::commands::Target::File { path, .. }
        if showing_history(ui) && ui.inspection.path.as_ref() == Some(path))
}

pub(crate) fn showing_history(ui: &view::Ui) -> bool {
    matches!(ui.selection, Some(Selection::Commit(_)))
        && (ui.inspection.history.is_some() || ui.inspection.loading_history)
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
    let subject = state.subject.clone().unwrap_or_else(|| {
        ui.history
            .iter()
            .find(|row| &row.oid == oid)
            .map_or_else(String::new, |row| row.subject.clone())
    });
    let more_open = matches!(ui.menu, Some(Menu::CommitActions));
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
        .border_color(rgb(LINE))
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(subject),
                )
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
                    .id("commit-body")
                    .max_h(px(136.))
                    .overflow_y_scroll()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(TEXT_2))
                    .child(state.body.clone()),
            )
        })
        .when(!state.author.is_empty(), |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::avatar(&state.author))
                    .child(kit::fit(state.author.clone()).size(TEXT_SMALL))
                    .child(
                        kit::fit(state.written.clone())
                            .size(TEXT_SMALL)
                            .color(TEXT_3)
                            .grow(),
                    ),
            )
        })
        .children(state.coauthors.iter().enumerate().map(|(index, coauthor)| {
            div()
                .id(("commit-coauthor", index))
                .flex()
                .items_center()
                .gap(px(6.))
                .child(kit::avatar(&coauthor.name))
                .child(kit::fit(coauthor.name.clone()).size(TEXT_SMALL))
                .child(kit::fit("co-author").size(TEXT_SMALL).color(TEXT_3).grow())
                .tooltip(kit::tip(coauthor.email.clone()))
        }))
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
                        .bg(rgb(RAISED))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(HOVER)))
                        .font_family(MONO)
                        .text_size(px(11.5))
                        .child(short)
                        .child(kit::icon("copy").size(px(11.)).text_color(rgb(TEXT_3)))
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
                    for (i, parent) in state.parents.iter().enumerate().take(2) {
                        let target = parent.clone();
                        row = row.child(
                            div()
                                .id(("parent", i))
                                .cursor_pointer()
                                .font_family(MONO)
                                .text_size(px(11.5))
                                .text_color(rgb(ACCENT))
                                .child(parent.chars().take(7).collect::<String>())
                                .on_click(cx.listener(move |app, _, w, cx| {
                                    app.dispatch(
                                        view::Action::Select(Selection::Commit(target.clone())),
                                        w,
                                        cx,
                                    )
                                })),
                        );
                    }
                    row
                }),
        )
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
                    .text_color(rgb(ADDED)),
            )
        })
        .when(deleted > 0, |d| {
            d.child(
                kit::label(format!("−{deleted}"))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(DELETED)),
            )
        })
        .when(count > 1, |d| {
            d.child(
                kit::btn("show-changed-files", "View all")
                    .small()
                    .ghost()
                    .on(view::Action::Inspect(Action::AllChanges), cx),
            )
        });
    let files = uniform_list(
        SharedString::from(format!("committed-file-{oid}")),
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let ui = app.ui();
            let state = &ui.inspection;
            let cursor = ui.navigate.file_cursor(ui);
            range
                .filter_map(|i| {
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
                            .h(px(FILE_ROW))
                            .pl(px(10.))
                            .pr_3()
                            .gap_2()
                            .child(kit::file_label(
                                ("committed-file-label", i),
                                file.status,
                                &file.path,
                                file.from.as_deref(),
                            ))
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
                                                    .text_color(rgb(ADDED))
                                                    .child(format!("+{added}")),
                                            )
                                        })
                                        .when(deleted > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(rgb(DELETED))
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
                            ),
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
        .child(files)
}

pub(crate) fn history_button<T: Desk>(app: &T, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let listing = showing_history(app.ui());
    kit::btn("file-history", "File history")
        .small()
        .ghost()
        .selected(listing)
        .on(
            view::Action::Inspect(if listing {
                Action::CloseHistory
            } else {
                Action::History
            }),
            cx,
        )
}

pub(crate) fn file_history<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let state = &app.ui().inspection;
    let count = state.history.as_ref().map_or(0, Vec::len);
    let body = div().flex_1().min_h_0().flex().flex_col();
    if count == 0 {
        return body.child(div().p_4().child(kit::muted(if state.loading_history {
            "Loading file history…"
        } else {
            "No commits changed this path."
        })));
    }
    let id = SharedString::from(format!(
        "file-revision-{}-{}",
        state.oid.as_deref().unwrap_or(""),
        state.path.as_deref().unwrap_or(Path::new("")).display()
    ));
    let list = uniform_list(
        id,
        count,
        cx.processor(|app, range: Range<usize>, _, cx| {
            let state = &app.ui().inspection;
            range
                .filter_map(|i| {
                    let row = state.history.as_ref()?.get(i)?;
                    let open = view::Action::Inspect(Action::OpenRevision { oid: row.oid.clone(), path: row.path.clone() });
                    let moved = (state.path.as_ref() != Some(&row.path)).then(|| row.path.to_string_lossy().into_owned());
                    Some(
                        kit::row(("file-revision", i), false)
                            .h(px(ROW))
                            .pl_3()
                            .pr_3()
                            .gap_3()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .font_family(MONO)
                                    .text_size(px(11.5))
                                    .text_color(rgb(TEXT_3))
                                    .child(row.oid.chars().take(7).collect::<String>()),
                            )
                            .child(kit::fit(row.subject.clone()).grow())
                            .when_some(moved, |d, path| {
                                d.child(
                                    kit::fit(path)
                                        .size(11.5)
                                        .color(TEXT_3)
                                        .cut(kit::Cut::Start),
                                )
                            })
                            .on_click(cx.listener(move |app, _, w, cx| app.dispatch(open.clone(), w, cx))),
                    )
                })
                .collect::<Vec<_>>()
        }),
    )
    .flex_1()
    .min_h_0();
    body.child(list).when(state.more_history, |d| {
        d.child(
            div().p_2().flex().justify_end().child(
                kit::btn("older-file-history", "Load older")
                    .small()
                    .enabled(!state.loading_history)
                    .on(view::Action::Inspect(Action::MoreHistory), cx),
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

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
                    from: None
                },
                ChangedFile {
                    status: "Modified",
                    path: other.to_owned(),
                    from: None
                }
            ]
        );
        let diff = f.repo.commit_path_diff(&tip, notes, None).unwrap();
        assert!(diff.text.contains("+FastKraken: git for the impatient"));
        assert!(!diff.text.contains("Change in the same commit"));
        let history = f.repo.file_history(&tip, notes, 0, 64).unwrap();
        let subjects = history
            .iter()
            .map(|r| r.subject.as_str())
            .collect::<Vec<_>>();
        assert_eq!(subjects, ["Revise notebook", "Start notebook"]);
        assert_eq!(
            f.repo.file_history(&tip, notes, 1, 1).unwrap(),
            history[1..]
        );
        let first = f.repo.commit_details(&history[1].oid).unwrap();
        assert_eq!(
            first.files,
            [ChangedFile {
                status: "Added",
                path: notes.to_owned(),
                from: None
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
                from: None
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
                ChangedFile { status: "Renamed", path: new.to_owned(), from: Some("notes.txt".into()) },
                ChangedFile { status: "Modified", path: "other.txt".into(), from: None },
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

    #[test]
    fn file_history_crosses_the_rename_and_names_the_path_at_each_commit() {
        let f = Fixture::new();
        let commits = moved_notes(&f);
        let new = Path::new("docs/notes é.md");
        let history = f.repo.file_history(&commits[0], new, 0, 64).unwrap();
        let rows: Vec<(&str, &Path)> = history.iter().map(|row| (row.subject.as_str(), row.path.as_path())).collect();
        assert_eq!(
            rows,
            [
                ("Edit moved notes", new),
                ("Move notes", new),
                ("Edit notes", Path::new("notes.txt")),
                ("Start notes", Path::new("notes.txt")),
            ]
        );
        assert_eq!(history.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>(), commits);
        assert_eq!(f.repo.file_history(&commits[0], new, 2, 1).unwrap(), history[2..3]);
        let older = &history[2];
        let details = f.repo.commit_details(&older.oid).unwrap();
        assert!(details.files.iter().any(|file| file.path == older.path && file.from.is_none()));
        let diff = f.repo.commit_path_diff(&older.oid, &older.path, None).unwrap();
        assert!(diff.text.contains("+TWO") && diff.text.contains("b/notes.txt"));
    }

    #[test]
    fn file_history_includes_a_merge_through_its_first_parent_and_reads_paths_with_newlines() {
        let output = [
            "a".repeat(40).as_bytes(), b"\0Merge side\0\nM\0new\nname\0",
            "b".repeat(40).as_bytes(), b"\0Move\0\nR100\0old\0new\nname\0",
            "c".repeat(40).as_bytes(), b"\0Start\0\nA\0old\0",
        ]
        .concat();
        let rows = parse_file_history(&output, Path::new("new\nname")).unwrap();
        let paths: Vec<&Path> = rows.iter().map(|row| row.path.as_path()).collect();
        assert_eq!(paths, [Path::new("new\nname"), Path::new("new\nname"), Path::new("old")]);
        assert_eq!(rows[1].subject, "Move");
        assert!(parse_file_history(b"zz\0x\0", Path::new("a")).is_err());

        let f = Fixture::new();
        write(&f, Path::new("f.txt"), "1\n2\n3\n4\n5\n6\n");
        f.repo.commit("Start").unwrap();
        f.git(&["switch", "-qc", "side"]);
        write(&f, Path::new("f.txt"), "1\n2\n3\n4\n5\n6\nside\n");
        f.repo.commit("Side").unwrap();
        f.git(&["switch", "-q", "main"]);
        write(&f, Path::new("f.txt"), "one\n2\n3\n4\n5\n6\n");
        f.repo.commit("Main").unwrap();
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.git(&["mv", "f.txt", "g.txt"]);
        f.repo.commit("Move").unwrap();
        let tip = f.repo.status().unwrap().head.unwrap();
        let subjects: Vec<String> =
            f.repo.file_history(&tip, Path::new("g.txt"), 0, 64).unwrap().into_iter().map(|row| row.subject).collect();
        assert_eq!(subjects, ["Move", "Merge branch 'side'", "Side", "Main", "Start"]);
    }
}
