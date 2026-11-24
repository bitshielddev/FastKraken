use crate::{
    Tab, kit,
    repository::{
        ChangedFile, Repository, checked, parse_changed_files, raw_path, validate_oid,
        validate_path,
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
            "--no-renames",
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
            "--no-renames",
            "--no-color",
            "--numstat",
            "-z",
            oid,
            "--",
        ]))?;
        let mut stats = HashMap::new();
        for record in output.split(|&b| b == 0).filter(|r| !r.is_empty()) {
            let record = record.strip_prefix(b"\n").unwrap_or(record);
            let mut fields = record.splitn(3, |&b| b == b'\t');
            let mut count = || {
                std::str::from_utf8(fields.next()?)
                    .ok()?
                    .parse::<u32>()
                    .ok()
            };
            if let (Some(added), Some(deleted)) = (count(), count())
                && let Some(path) = fields.next()
            {
                stats.insert(raw_path(path), (added, deleted));
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
                    "--no-patch",
                    "--no-show-signature",
                    "--no-notes",
                    "--no-color",
                    "--format=%H%x00%s",
                    "-z",
                ])
                .arg(format!("--skip={offset}"))
                .arg(format!("--max-count={limit}"))
                .args([oid, "--"])
                .arg(path),
        )?;
        let mut commits = Vec::new();
        if let Some(records) = output.strip_suffix(b"\0") {
            let mut fields = records.split(|&b| b == 0);
            while let Some(oid) = fields.next() {
                let subject = fields.next().ok_or("Missing file history subject")?;
                commits.push(FileCommit {
                    oid: String::from_utf8_lossy(oid).into_owned(),
                    subject: String::from_utf8_lossy(subject).into_owned(),
                });
            }
        } else if !output.is_empty() {
            return Err("Incomplete file history record".into());
        }
        Ok(commits)
    }
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
    pub files: Vec<ChangedFile>,
    stats: HashMap<PathBuf, (u32, u32)>,
    pub path: Option<PathBuf>,
    pub history: Option<Vec<FileCommit>>,
    history_ticket: u64,
    loading_history: bool,
    more_history: bool,
    pub(crate) scroll: gpui::UniformListScrollHandle,
}

impl State {
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
    MoreHistory,
    CloseHistory,
}

pub fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.inspection;
    state.close_history();
    state.path = None;
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
                    state.body = details.body;
                    state.stats = details.stats;
                    state.files = details.files;
                    tab.ui.diff_title = details.subject.clone();
                    state.subject = Some(details.subject);
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
    let Some(Selection::Commit(oid)) = tab.ui.selection.clone() else {
        return;
    };
    let state = &mut tab.ui.inspection;
    if state.oid.as_ref() != Some(&oid) {
        return;
    }
    match action {
        Action::OpenFile(path) => {
            let Some(index) = state.files.iter().position(|file| file.path == path) else {
                return;
            };
            tab.ui.navigate.opened(&oid, index);
            let state = &mut tab.ui.inspection;
            state.close_history();
            state.path = Some(path.clone());
            tab.ui.diff_open = true;
            tab.load_selected_patch(Selection::Commit(oid), Some(path), cx);
        }
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
        Action::FileHistory(_) => unreachable!(),
    }
    cx.notify();
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
                    let name = file
                        .path
                        .file_name()
                        .unwrap_or(file.path.as_os_str())
                        .to_string_lossy()
                        .into_owned();
                    let folder = file
                        .path
                        .parent()
                        .map(|parent| parent.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let stat = state.stats.get(&file.path).copied();
                    Some(
                        kit::cursor(kit::row(("committed-file", i), open), !open && cursor == Some(i))
                            .h(px(FILE_ROW))
                            .pl(px(10.))
                            .pr_3()
                            .gap_2()
                            .child(kit::badge(file.status))
                            .child(kit::fit(name).size(12.5).cut(kit::Cut::Middle))
                            .child(
                                kit::fit(folder)
                                    .size(11.5)
                                    .color(TEXT_3)
                                    .cut(kit::Cut::Start)
                                    .grow(),
                            )
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
                    let oid = row.oid.clone();
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
                            .on_click(cx.listener(move |app, _, w, cx| {
                                app.dispatch(
                                    view::Action::Select(Selection::Commit(oid.clone())),
                                    w,
                                    cx,
                                )
                            })),
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
                    path: notes.to_owned()
                },
                ChangedFile {
                    status: "Modified",
                    path: other.to_owned()
                }
            ]
        );
        let diff = f.repo.commit_path_diff(&tip, notes).unwrap();
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
                path: notes.to_owned()
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
                path: incoming.to_owned()
            }]
        );
        assert!(
            f.repo
                .commit_path_diff(&merge, incoming)
                .unwrap()
                .text
                .contains("+Creek")
        );
    }
}
