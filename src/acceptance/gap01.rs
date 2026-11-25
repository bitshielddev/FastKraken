use super::*;
use crate::{
    commands::{self, Target},
    merge, notice,
    prompt::{self, Stop},
    refs::{Section, SideRow},
    stash, sync,
    view::{Column, DiffMode, Tone},
};
use crate::navigate::Zone;
use gpui::{App, ClipboardItem, Focusable, Point};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

fn notices(tab: &Tab) -> Vec<(Tone, String, usize)> {
    tab.ui
        .notices
        .items()
        .iter()
        .map(|n| (n.tone, n.title.clone(), n.count))
        .collect()
}

fn saved(state: &Path) -> Result<(serde_json::Value, serde_json::Value)> {
    let read = |name: &str| -> Result<serde_json::Value> {
        Ok(serde_json::from_slice(&fs::read(state.join(name))?)?)
    };
    Ok((read("settings.json")?, read("state.json")?))
}

fn header_open(tab: &Tab, section: Section) -> Option<bool> {
    tab.ui.refs.rows.iter().find_map(|row| match row {
        SideRow::Header { section: s, open, .. } if *s == section => Some(*open),
        _ => None,
    })
}

fn folder_open(tab: &Tab, key: &str) -> Option<bool> {
    tab.ui.refs.rows.iter().find_map(|row| match row {
        SideRow::Folder { key: k, open, .. } if k == key => Some(*open),
        _ => None,
    })
}

impl Replay<'_> {
    pub(super) async fn relaunch(&mut self) -> Result<()> {
        let old = self.tab;
        self.cx.update(|cx| {
            settings::flush(cx);
            settings::load(storage::override_directory(), cx);
        })?;
        self.tab = crate::tabs::Front(self.cx.update(|cx| {
            cx.open_window(hidden(cx), |window, cx| {
                cx.new(|cx| {
                    let mut desk = crate::tabs::Workspace::new(window, cx, None);
                    desk.launch(None, window, cx);
                    desk
                })
            })
        })??);
        old.update(self.cx, |_, window, _| window.remove_window())?;
        Ok(())
    }

    pub(super) async fn gap01(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0101");
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let root = init(dir, "Session")?;
        record(&root, "trail.txt", TRAIL, "Start session trail")?;
        git(&root, &["branch", "feature/map"])?;
        git(&root, &["tag", "v1"])?;
        let head = oid(&root, "HEAD")?;
        let top = git(&root, &["rev-parse", "--show-toplevel"])?;
        let top = PathBuf::from(top.trim_end());
        self.open(&root)?;
        self.until("Session open with its folder of branches", |tab| {
            tab.ui.repo == "Session"
                && tab.ui.history.len() == 1
                && folder_open(tab, "Branches:feature") == Some(true)
                && header_open(tab, Section::Tags) == Some(true)
        })
        .await?;
        let (sidebar, panel) = self.tab.update(self.cx, |tab, window, cx| {
            let width = f32::from(window.viewport_size().width);
            view::resize_column(tab, Column::Sidebar, px(262.5), window, cx);
            view::resize_column(tab, Column::Panel, px(width - 342.5), window, cx);
            tab.ui.column_widths()
        })?;
        if (sidebar, panel) != (260., 340.) {
            return Err(format!("Dragging set the columns to {sidebar} and {panel}").into());
        }
        self.action(Action::DiffMode(DiffMode::Split))?;
        self.ref_action(refs::Action::ToggleLabels)?;
        self.ref_action(refs::Action::ToggleSection(Section::Tags))?;
        self.ref_action(refs::Action::ToggleFolder("Branches:feature".into()))?;
        self.until("tags closed and the feature folder folded", |tab| {
            header_open(tab, Section::Tags) == Some(false)
                && folder_open(tab, "Branches:feature") == Some(false)
        })
        .await?;

        self.relaunch().await?;
        self.until("the same repository, columns, diff mode and sidebar", |tab| {
            tab.ui.repo == "Session"
                && tab.ui.branch == "main"
                && tab.ui.history.len() == 1
                && tab.ui.column_widths() == (sidebar, panel)
                && tab.ui.diff_mode == DiffMode::Split
                && !tab.ui.refs.show_labels
                && header_open(tab, Section::Tags) == Some(false)
                && folder_open(tab, "Branches:feature") == Some(false)
                && header_open(tab, Section::Branches) == Some(true)
        })
        .await?;
        let (chosen, remembered) = saved(&state)?;
        if chosen["diff_mode"] != "split" || chosen["show_ref_labels"] != false {
            return Err(format!("settings.json does not hold the choices: {chosen}").into());
        }
        let last: Vec<u8> = serde_json::from_value(remembered["last_repository"].clone())?;
        if Path::new(OsStr::from_bytes(&last)) != top {
            return Err(format!("state.json remembers {last:?}, not {}", top.display()).into());
        }
        if remembered["columns"] != json!({"sidebar": 260.0, "panel": 340.0}) {
            return Err(format!("state.json holds the columns {}", remembered["columns"]).into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;

        let moved = dir.join("Session-moved");
        fs::rename(&root, &moved)?;
        self.relaunch().await?;
        let expected = top.display().to_string();
        self.until("the welcome screen naming the missing repository", |tab| {
            tab.repo.is_none()
                && tab.ui.repo.is_empty()
                && tab.ui.notices.newest().is_some_and(|notice| {
                    notice.tone == view::Tone::Info && notice.text().contains(&expected)
                })
        })
        .await?;
        fs::rename(&moved, &root)?;
        self.pass();

        self.notices(dir).await
    }

    async fn notices(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0102");
        let root = init(dir, "Notices")?;
        record(&root, "trail.txt", TRAIL, "Start the notice trail")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        let opening = self.tab.read_with(self.cx, |tab, _| tab.ui.opening.clone())?;
        if opening != Some(root.display().to_string()) {
            return Err(format!("Opening showed {opening:?} in the centre").into());
        }
        self.until("Notices open on main with no notice", |tab| {
            tab.ui.repo == "Notices"
                && tab.ui.opening.is_none()
                && tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(head.as_str())
                && tab.ui.upstream.is_none()
                && tab.ui.notices.items().is_empty()
        })
        .await?;
        self.action(Action::Sync(sync::Action::Pull))?;
        self.until("the pull's spinner replaced by its error", |tab| {
            notices(tab) == [(Tone::Error, "Pull Failed".to_owned(), 1)]
                && tab.ui.notice_text()
                    == "Pull Failed — Set an upstream branch for main before synchronizing."
        })
        .await?;
        let error = self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.notices.items()[0].id)?;

        fs::write(root.join("trail.txt"), WAYPOINT)?;
        self.action(Action::Refresh)?;
        self.until("trail.txt changed", |tab| files(tab, &[], &["trail.txt"]))
            .await?;
        self.action(Action::Stash(stash::Action::Save))?;
        self.until("the success beneath the unread error", |tab| {
            notices(tab)
                == [
                    (Tone::Error, "Pull Failed".to_owned(), 1),
                    (Tone::Success, "Changes stashed".to_owned(), 1),
                ]
                && tab.ui.notice_text() == "Changes stashed"
                && tab.ui.files.is_empty()
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        if git(&root, &["stash", "list", "--format=%gs"])?.lines().count() != 1 {
            return Err("The stash list does not hold exactly the one change".into());
        }
        git_is(&root, &["show", "stash@{0}:trail.txt"], WAYPOINT)?;

        let shown = Instant::now();
        self.until("the success faded and the error left in place", |tab| {
            notices(tab) == [(Tone::Error, "Pull Failed".to_owned(), 1)]
                && tab.ui.notices.get(error).is_some()
        })
        .await?;
        if shown.elapsed() < Duration::from_secs(3) {
            return Err("The success notice went before it had been shown for long".into());
        }
        self.action(Action::Notice(notice::Action::Dismiss(error)))?;
        self.until("no notice left", |tab| tab.ui.notices.items().is_empty())
            .await?;
        self.pass();

        self.start("GAP0103");
        self.select_commit(&head).await?;
        let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
        let copy = self.action(crate::inspection::copy_sha(&head));
        let copied = self
            .cx
            .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
        self.cx.update(|cx| {
            cx.write_to_clipboard(saved.unwrap_or_else(|| ClipboardItem::new_string(String::new())))
        })?;
        copy?;
        if copied.as_deref() != Some(head.as_str()) || head.len() != 40 {
            return Err(format!("The clipboard held {copied:?}, not {head}").into());
        }
        self.until("an information notice confirming the copy", |tab| {
            tab.ui.notices.newest().is_some_and(|n| {
                n.tone == Tone::Info && n.text() == "Copied SHA" && n.count == 1
            })
        })
        .await?;
        self.until("the confirmation faded", |tab| tab.ui.notices.items().is_empty())
            .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.pass();
        self.prompts(dir).await
    }

    pub(super) fn prompt_focus(&mut self) -> Result<Option<Stop>> {
        Ok(self
            .tab
            .update(self.cx, |tab, window, cx| tab.ui.prompt.focused(window, cx))?)
    }

    async fn ask_abort(&mut self) -> Result<()> {
        self.action(Action::Operation(crate::operation::Action::Abort))?;
        self.until("Abort merge asks first", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Operation(crate::operation::Ask::Abort(_))))
                && prompt::shown(&tab.ui).is_some_and(|spec| {
                    spec.danger
                        && spec.title == "Abort the merge of feature/detour into main?"
                        && spec.confirm.as_ref() == "Abort merge"
                        && spec.lines
                            == [
                                "Files you have already resolved go back to how they were before the merge.",
                                "The resolution of 1 file will be lost.",
                            ]
                })
        })
        .await?;
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The Abort merge prompt did not open with Cancel focused".into());
        }
        Ok(())
    }

    async fn prompts(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0104");
        let root = init(dir, "Crossing")?;
        let commit = |notes: &str, trail: &str, subject: &str| -> Result<()> {
            fs::write(root.join("notes.txt"), notes)?;
            git(&root, &["add", "--", "notes.txt"])?;
            record(&root, "trail.txt", trail, subject)
        };
        commit(
            "Notes: none yet\n",
            "FastKraken: git for the impatient\nWaypoint: Old ford\n",
            "Start the crossing",
        )?;
        git(&root, &["switch", "-qc", "feature/detour"])?;
        let ridge = "FastKraken: git for the impatient\nWaypoint: Ridge pass\n";
        commit("Notes: take the ridge\n", ridge, "Record ridge detour")?;
        let theirs = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        commit("Notes: cross at the bridge\n", WAYPOINT, "Add east signpost")?;
        let ours = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Crossing open on main", |tab| {
            tab.ui.repo == "Crossing" && tab.ui.branch == "main"
        })
        .await?;
        self.begin_merge("feature/detour").await?;
        self.until("both files in conflict", |tab| {
            let conflicted = tab.ui.operation.conflicted();
            conflicted.len() == 2
                && conflicted.contains(&Path::new("notes.txt"))
                && conflicted.contains(&Path::new("trail.txt"))
        })
        .await?;
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("trail.txt open in the resolver", |tab| {
            tab.ui.merge.editing() == Some(Path::new("trail.txt"))
        })
        .await?;
        self.action(Action::Merge(merge::Action::Choose(0, merge::Side::Incoming)))?;
        self.action(Action::Merge(merge::Action::Save))?;
        self.until("trail.txt resolved and notes.txt still in conflict", |tab| {
            tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.operation.conflicted() == [Path::new("notes.txt")]
        })
        .await?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let status = git(&root, &["status", "--porcelain=v1"])?;
        if status != "UU notes.txt\nM  trail.txt\n" || fs::read_to_string(root.join("trail.txt"))? != ridge {
            return Err(format!("The resolution was not saved as expected: {status:?}").into());
        }
        let unchanged = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "MERGE_HEAD"], &format!("{theirs}\n"))?;
            git_is(&root, &["rev-parse", "main"], &format!("{ours}\n"))?;
            git_is(&root, &["ls-files", "--stage"], &index)?;
            git_is(&root, &["status", "--porcelain=v1"], &status)?;
            if fs::read_to_string(root.join("trail.txt"))? != ridge {
                return Err(format!("{what} changed the resolved trail.txt").into());
            }
            Ok(())
        };
        let still_merging = |tab: &Tab| {
            !tab.ui.prompt.open()
                && tab.ui.operation.active()
                && tab.ui.operation.resolved() == [Path::new("trail.txt")]
                && tab.ui.operation.conflicted() == [Path::new("notes.txt")]
        };

        self.ask_abort().await?;
        for (key, expected) in [
            ("tab", Stop::Confirm),
            ("tab", Stop::Cancel),
            ("tab", Stop::Confirm),
            ("shift-tab", Stop::Cancel),
            ("shift-tab", Stop::Confirm),
            ("shift-tab", Stop::Cancel),
        ] {
            self.key(key)?;
            let focused = self.prompt_focus()?;
            if focused != Some(expected) {
                return Err(format!("{key} moved focus to {focused:?}, not {expected:?}").into());
            }
        }
        for key in ["cmd-1", "cmd-2", "cmd-l", "up", "down"] {
            self.key(key)?;
            let focused = self.prompt_focus()?;
            if focused != Some(Stop::Cancel) {
                return Err(format!("{key} moved focus out of the prompt to {focused:?}").into());
            }
        }
        self.key("enter")?;
        self.until("Return cancelled and the merge goes on", still_merging)
            .await?;
        unchanged("Return")?;

        self.ask_abort().await?;
        self.key("tab")?;
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("Tab did not move focus to Abort merge".into());
        }
        self.key("enter")?;
        self.until("Return on Abort merge cancelled and the merge goes on", still_merging)
            .await?;
        unchanged("Return on Abort merge")?;

        self.ask_abort().await?;
        let offered = self.tab.update(self.cx, |tab, _, cx| {
            let notice = notice::Notice::new(Tone::Error, "Push Failed", "The remote has new commits")
                .offer("Pull", Action::Sync(sync::Action::Pull));
            let id = tab.ui.notices.show(notice);
            cx.notify();
            id
        })?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        let kept = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.get(offered).is_some() && tab.ui.prompt.open() && !tab.ui.busy()
        })?;
        if !kept {
            return Err("An offered step taken under the prompt was lost or ran".into());
        }
        self.action(Action::Notice(notice::Action::Dismiss(offered)))?;
        unchanged("An offered step under the prompt")?;

        self.key("escape")?;
        self.until("Esc cancelled and the merge goes on", still_merging)
            .await?;
        unchanged("Esc")?;

        self.ask_abort().await?;
        self.key(if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" })?;
        self.until("the merge aborted and the tree clean", |tab| {
            !tab.ui.prompt.open()
                && !tab.ui.operation.active()
                && files(tab, &[], &[])
                && tab.ui.notice_text() == "Merge aborted"
        })
        .await?;
        git_is(&root, &["rev-parse", "main"], &format!("{ours}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if root.join(".git/MERGE_HEAD").exists()
            || fs::read_to_string(root.join("trail.txt"))? != WAYPOINT
            || fs::read_to_string(root.join("notes.txt"))? != "Notes: cross at the bridge\n"
        {
            return Err("Confirming did not restore the files from before the merge".into());
        }
        self.pass();
        self.commands(dir).await
    }

    pub(super) fn run_command(&mut self, id: &'static str) -> Result<()> {
        self.cx.update_window(self.tab.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.dispatch_action(Box::new(commands::Run { id }), cx);
        })?;
        Ok(())
    }

    pub(super) fn menu_rows(&mut self, target: &Target) -> Result<Vec<(&'static str, String, Option<String>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .map(|entry| (entry.id, entry.title, entry.disabled))
                .collect()
        })?)
    }

    async fn commands(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0110");
        let root = init(dir, "Commands")?;
        record(&root, "trail.txt", TRAIL, "Start the command trail")?;
        git(&root, &["branch", "feature/map"])?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Commands open on main with no upstream and no notice", |tab| {
            tab.ui.repo == "Commands"
                && tab.ui.branch == "main"
                && tab.ui.upstream.is_none()
                && tab.ui.refs.remotes.is_empty()
                && tab.ui.refs.lists(refs::Kind::Branch, "feature/map")
                && tab.ui.notices.items().is_empty()
        })
        .await?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let unchanged = || -> Result<()> {
            git_is(&root, &["rev-parse", "main"], &format!("{head}\n"))?;
            git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
            git_is(&root, &["ls-files", "--stage"], &index)?;
            git_is(&root, &["status", "--porcelain=v1"], "")?;
            if root.join(".git/FETCH_HEAD").exists() {
                return Err("Something was fetched".into());
            }
            Ok(())
        };
        let reason = "This branch has no upstream to pull from";
        self.run_command("repo.pull")?;
        self.until("the reason in an information notice", |tab| {
            notices(tab) == [(Tone::Info, reason.to_owned(), 1)]
        })
        .await?;
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_millis(600) {
            self.until("no operation running", |_| true).await?;
            let shown = self.tab.read_with(self.cx, |tab, _| (tab.ui.busy(), notices(tab)))?;
            if shown.0 || shown.1 != [(Tone::Info, reason.to_owned(), 1)] {
                return Err(format!("Running a disabled Pull did something: {shown:?}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(50)).await;
        }
        unchanged()?;

        self.run_command("repo.fetch-all")?;
        self.until("the fetch's reason in an information notice", |tab| {
            tab.ui.notices.newest().is_some_and(|n| {
                n.tone == Tone::Info && n.text() == "This repository has no remote to fetch from"
            })
        })
        .await?;
        unchanged()?;

        let main = Target::Branch("main".into());
        self.action(Action::Menu(Some(view::Menu::Object { target: main.clone(), at: Point::default() })))?;
        self.until("the menu of main open", |tab| {
            matches!(&tab.ui.menu, Some(view::Menu::Object { target, .. }) if *target == main)
        })
        .await?;
        let rows = self.menu_rows(&main)?;
        let expected = [
            ("ref.branch", "Create branch here…".to_owned(), None),
            ("branch.switch", "Switch to main".to_owned(), Some("main is already checked out".to_owned())),
            ("branch.merge", "Merge into main".to_owned(), Some("main is the checked-out branch".to_owned())),
            ("branch.rename", "Rename main…".to_owned(), None),
            ("branch.delete", "Delete main…".to_owned(), Some("Switch to another branch first".to_owned())),
            ("graph.hide", "Hide in the graph".to_owned(), None),
            ("graph.solo", "Solo".to_owned(), None),
            ("ref.copy-name", "Copy branch name".to_owned(), None),
        ];
        if rows != expected {
            return Err(format!("The menu of main lists {rows:?}").into());
        }
        let feature = Target::Branch("feature/map".into());
        self.action(Action::Menu(Some(view::Menu::Object { target: feature.clone(), at: Point::default() })))?;
        let rows = self.menu_rows(&feature)?;
        let expected = [
            ("ref.branch", "Create branch here…".to_owned(), None),
            ("branch.switch", "Switch to feature/map".to_owned(), None),
            ("branch.merge", "Merge feature/map into main".to_owned(), None),
            ("branch.rebase-onto", "Rebase main onto feature/map…".to_owned(), None),
            ("branch.rebase", "Rebase feature/map onto main…".to_owned(), None),
            ("branch.rename", "Rename feature/map…".to_owned(), None),
            ("branch.delete", "Delete feature/map…".to_owned(), None),
            ("graph.hide", "Hide in the graph".to_owned(), None),
            ("graph.solo", "Solo".to_owned(), None),
            ("ref.copy-name", "Copy branch name".to_owned(), None),
        ];
        if rows != expected {
            return Err(format!("The menu of feature/map lists {rows:?}").into());
        }
        self.action(Action::Dismiss)?;
        unchanged()?;

        git(&root, &["switch", "-qc", "feature/ridge"])?;
        let ridge = "FastKraken: git for the impatient\nWaypoint: Ridge pass\n";
        record(&root, "trail.txt", ridge, "Take the ridge")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let tip = oid(&root, "main")?;
        self.action(Action::Refresh)?;
        self.begin_merge("feature/ridge").await?;
        self.until("the merge of feature/ridge in progress", |tab| {
            tab.ui.operation.active() && !tab.ui.busy() && !tab.ui.prompt.open()
        })
        .await?;
        let reason = Some("Finish or abort the merge first".to_owned());
        let rows = self.menu_rows(&feature)?;
        let expected = [
            ("ref.branch", "Create branch here…".to_owned(), reason.clone()),
            ("branch.switch", "Switch to feature/map".to_owned(), reason.clone()),
            ("branch.merge", "Merge feature/map into main".to_owned(), reason.clone()),
            ("branch.rebase-onto", "Rebase main onto feature/map…".to_owned(), reason.clone()),
            ("branch.rebase", "Rebase feature/map onto main…".to_owned(), reason.clone()),
            ("branch.rename", "Rename feature/map…".to_owned(), reason.clone()),
            ("branch.delete", "Delete feature/map…".to_owned(), reason.clone()),
            ("graph.hide", "Hide in the graph".to_owned(), None),
            ("graph.solo", "Solo".to_owned(), None),
            ("ref.copy-name", "Copy branch name".to_owned(), None),
        ];
        if rows != expected {
            return Err(format!("During the merge the menu of feature/map lists {rows:?}").into());
        }
        let rows = self.menu_rows(&Target::Commit(tip))?;
        let disabled: Vec<_> = rows.iter().filter(|row| row.2.is_some()).map(|row| row.0).collect();
        if disabled != ["commit.branch", "commit.tag", "branch.switch", "commit.checkout", "commit.merge", "commit.revert", "commit.reset"]
            || rows.iter().rev().take(2).map(|row| row.0).collect::<Vec<_>>()
                != ["commit.copy-message", "commit.copy-sha"]
            || rows.iter().any(|row| row.2.is_some() && row.2 != reason)
        {
            return Err(format!("During the merge the commit menu lists {rows:?}").into());
        }
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, crate::publish::REMOTES_MENU, cx)
                .into_iter()
                .chain(commands::listed(&tab.ui, &Target::Repo, &["repo.labels"], cx))
                .map(|entry| (entry.id, entry.disabled))
                .collect::<Vec<_>>()
        })?;
        if rows.len() != 4 || rows.iter().any(|row| row.1 != reason) {
            return Err(format!("During the merge the remote and label commands are {rows:?}").into());
        }
        git(&root, &["merge", "--abort"])?;
        self.action(Action::Refresh)?;
        self.until("the merge aborted", |tab| !tab.ui.operation.active() && !tab.ui.busy()).await?;
        self.pass();
        self.labels(dir).await
    }

    fn open_rows(&mut self) -> Result<Vec<(&'static str, String, Vec<&'static str>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            tab.ui.menu.as_ref().map_or_else(Vec::new, |open| {
                commands::entries_of(&tab.ui, open, cx)
                    .into_iter()
                    .map(|entry| {
                        let children = entry.children.iter().map(|child| child.id).collect();
                        (entry.id, entry.title, children)
                    })
                    .collect()
            })
        })?)
    }

    fn menu_target(&mut self) -> Result<Option<Target>> {
        Ok(self.tab.read_with(self.cx, |tab, _| match &tab.ui.menu {
            Some(view::Menu::Object { target, .. }) => Some(target.clone()),
            _ => None,
        })?)
    }

    pub(super) async fn copied(&mut self, copy: Action, notice: &str) -> Result<Option<String>> {
        let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
        let ran = self.action(copy);
        let confirmed = match ran {
            Ok(()) => {
                self.until("the copy confirmed", |tab| {
                    tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.text() == notice)
                })
                .await
            }
            Err(error) => Err(error),
        };
        let copied = self.cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
        self.cx.update(|cx| {
            cx.write_to_clipboard(saved.unwrap_or_else(|| ClipboardItem::new_string(String::new())))
        })?;
        confirmed?;
        Ok(copied)
    }

    async fn labels(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0105");
        let root = init(dir, "Labels")?;
        record(&root, "trail.txt", TRAIL, "Start the label trail")?;
        let start = oid(&root, "HEAD")?;
        git(&root, &["branch", "feature/map"])?;
        git(&root, &["branch", "feature/ford"])?;
        git(&root, &["tag", "v1"])?;
        git(&root, &["tag", "v2"])?;
        let origin = dir.join("Labels-origin.git");
        git(&root, &["remote", "add", "origin", &origin.display().to_string()])?;
        git(&root, &["update-ref", "refs/remotes/origin/feature/map", &start])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let tip = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Labels open with its refs loaded", |tab| {
            tab.ui.repo == "Labels"
                && tab.ui.history.len() == 2
                && tab.ui.refs.remotes == ["origin"]
                && tab.ui.refs.lists(refs::Kind::Remote, "origin/feature/map")
                && tab.ui.refs.lists(refs::Kind::Tag, "v2")
        })
        .await?;
        let labels = self.tab.read_with(self.cx, |tab, _| {
            view::labels(&tab.ui.refs.remotes, &tab.ui.history[1].refs)
                .into_iter()
                .map(|label| label.target)
                .collect::<Vec<_>>()
        })?;
        let kinds: Vec<_> = labels
            .iter()
            .map(|target| match target {
                Some(Target::Branch(_)) => "branch",
                Some(Target::RemoteBranch(name)) if name == "origin/feature/map" => "remote",
                Some(Target::Tag(_)) => "tag",
                _ => "other",
            })
            .collect();
        if kinds != ["branch", "branch", "remote", "tag", "tag"] {
            return Err(format!("The first commit's labels stand for {labels:?}").into());
        }
        let Some(Some(Target::Branch(first))) = labels.first().cloned() else {
            return Err("The first label is not a branch".into());
        };
        let first = first.to_string_lossy().into_owned();

        self.select_commit(&start).await?;
        let commit = Target::Commit(start.clone());
        self.action(Action::Menu(Some(view::Menu::Object { target: commit, at: Point::default() })))?;
        let rows = self.open_rows()?;
        let ids: Vec<_> = rows.iter().map(|row| row.0).collect();
        if ids
            != [
                "commit.branch",
                "commit.tag",
                "commit.switch",
                "commit.checkout",
                "commit.merge",
                "commit.revert",
                "commit.reset",
                "commit.copy-sha",
                "commit.copy-message",
            ]
        {
            return Err(format!("The commit's menu lists {ids:?}").into());
        }
        let switch = rows.iter().find(|row| row.0 == "commit.switch");
        if switch.map(|row| (row.1.as_str(), row.2.as_slice()))
            != Some(("Switch to", ["branch.switch", "branch.switch"].as_slice()))
        {
            return Err(format!("The commit's menu has no Switch to submenu of both branches: {rows:?}").into());
        }
        self.action(Action::Dismiss)?;
        let first_target = Target::Branch(first.clone().into());
        let label = Point::default();
        self.action(view::label_menu(first_target.clone(), label))?;
        let target = self.menu_target()?;
        if target != Some(Target::Branch(first.clone().into())) {
            return Err(format!("A right-click on the {first} label opened the menu of {target:?}").into());
        }
        let rows: Vec<_> = self.open_rows()?.into_iter().map(|row| (row.0, row.1)).collect();
        let expected = [
            ("branch.push", format!("Push {first}")),
            ("branch.pull", format!("Pull {first}")),
            ("ref.branch", "Create branch here…".to_owned()),
            ("branch.switch", format!("Switch to {first}")),
            ("branch.merge", format!("Merge {first} into main")),
            ("branch.rebase-onto", format!("Rebase main onto {first}…")),
            ("branch.rebase", format!("Rebase {first} onto main…")),
            ("branch.set-upstream", format!("Set upstream of {first}…")),
            ("branch.rename", format!("Rename {first}…")),
            ("branch.delete", format!("Delete {first}…")),
            ("graph.hide", "Hide in the graph".to_owned()),
            ("graph.solo", "Solo".to_owned()),
            ("ref.copy-name", "Copy branch name".to_owned()),
        ];
        if rows != expected {
            return Err(format!("The {first} label's menu lists {rows:?}").into());
        }
        self.action(Action::Dismiss)?;
        let check_out = ("branch.checkout-remote", "Check out origin/feature/map".to_owned());
        let merge_remote = ("branch.merge-remote", "Merge origin/feature/map into main".to_owned());
        let tag_check_out = ("tag.checkout", "Check out the commit at v1".to_owned());
        let merge_tag = ("tag.merge", "Merge v1 into main".to_owned());
        let rebase_remote = ("branch.rebase-onto-remote", "Rebase main onto origin/feature/map…".to_owned());
        let rebase_tag = ("tag.rebase-onto", "Rebase main onto v1…".to_owned());
        let tag_rows = vec![
            ("tag.push", "Push v1 to origin".to_owned()),
            ("tag.annotate", "Annotate v1…".to_owned()),
            ("tag.delete", "Delete v1…".to_owned()),
            ("tag.delete-remote", "Delete v1 from origin…".to_owned()),
        ];
        for (target, own, merge, rebase, more, copy) in [
            (
                Target::RemoteBranch("origin/feature/map".into()),
                check_out,
                merge_remote,
                rebase_remote,
                vec![("branch.delete-remote", "Delete origin/feature/map…".to_owned())],
                "Copy branch name",
            ),
            (Target::Tag("v1".into()), tag_check_out, merge_tag, rebase_tag, tag_rows, "Copy tag name"),
        ] {
            let rows: Vec<_> = self.menu_rows(&target)?.into_iter().map(|row| (row.0, row.1)).collect();
            let expected: Vec<_> = std::iter::once(("ref.branch", "Create branch here…".to_owned()))
                .chain([own, merge, rebase])
                .chain(more)
                .chain([("graph.hide", "Hide in the graph".to_owned()), ("graph.solo", "Solo".to_owned())])
                .chain([("ref.copy-name", copy.to_owned())])
                .collect();
            if rows != expected {
                return Err(format!("The menu of {target:?} lists {rows:?}").into());
            }
        }

        self.action(Action::Menu(Some(view::Menu::Repo)))?;
        let rows: Vec<_> =
            self.open_rows()?.into_iter().filter(|row| row.0 != "repo.recent").map(|row| (row.0, row.1)).collect();
        let expected = [
            ("repo.open", "Open repository…".to_owned()),
            ("repo.close", "Close repository".to_owned()),
            ("repo.refresh", "Refresh".to_owned()),
            ("repo.reveal", "Reveal in Finder".to_owned()),
            ("repo.terminal", "Open in Terminal".to_owned()),
            ("repo.editor", "Open repository in external editor…".to_owned()),
            ("repo.labels", "Show branch and tag labels".to_owned()),
        ];
        if rows != expected {
            return Err(format!("The repository menu lists {rows:?}").into());
        }
        self.action(Action::Dismiss)?;

        self.action(Action::Menu(Some(view::Menu::Labels { oid: start.clone(), at: label })))?;
        let rows = self.open_rows()?;
        let hidden: Vec<_> = rows.iter().map(|row| (row.0, row.2.clone())).collect();
        if rows.len() != 2
            || hidden.iter().any(|row| {
                *row != (
                    "label",
                    vec![
                        "ref.branch",
                        "tag.checkout",
                        "tag.merge",
                        "tag.rebase-onto",
                        "tag.push",
                        "tag.annotate",
                        "tag.delete",
                        "tag.delete-remote",
                        "graph.hide",
                        "graph.solo",
                        "ref.copy-name",
                    ],
                )
            })
            || !rows.iter().all(|row| row.1 == "v1" || row.1 == "v2")
        {
            return Err(format!("The hidden labels are listed as {rows:?}").into());
        }
        let last = rows[1].1.clone();
        for key in ["up", "right", "up"] {
            self.key(key)?;
        }
        let cursor = self.tab.read_with(self.cx, |tab, _| tab.ui.menu_cursor)?;
        if cursor != (commands::Cursor { row: Some(1), open: true, sub: Some(10) }) {
            return Err(format!("Up, Right and Up left the highlight at {cursor:?}").into());
        }
        let copied = {
            let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
            let pressed = self.key("enter");
            let copied = self.cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
            self.cx.update(|cx| {
                cx.write_to_clipboard(saved.unwrap_or_else(|| ClipboardItem::new_string(String::new())))
            })?;
            pressed?;
            copied
        };
        if copied.as_deref() != Some(last.as_str()) {
            return Err(format!("Return on Copy tag name copied {copied:?}, not {last}").into());
        }
        self.until("the menu closed and the copy confirmed", |tab| {
            tab.ui.menu.is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.text() == "Copied tag name")
        })
        .await?;

        self.action(Action::Select(Selection::Changes))?;
        self.action(view::label_click(&start, &first_target, 1))?;
        self.until("the label's commit selected", |tab| {
            tab.ui.selection == Some(Selection::Commit(start.clone())) && tab.ui.branch == "main"
        })
        .await?;
        self.action(view::label_click(&start, &first_target, 2))?;
        self.until("switched to the label's branch", |tab| {
            tab.ui.branch == first && tab.ui.menu.is_none() && !tab.ui.busy()
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], &format!("refs/heads/{first}
"))?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{start}
"))?;
        git_is(&root, &["rev-parse", "main"], &format!("{tip}
"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.action(view::label_click(&start, &first_target, 1))?;
        self.action(view::label_click(&start, &first_target, 2))?;
        let already = format!("{first} is already checked out");
        self.until("the reason in an information notice", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.text() == already)
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], &format!("refs/heads/{first}
"))?;
        self.pass();
        self.file_menus(dir).await
    }

    async fn file_menus(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0106");
        let root = init(dir, "Staging")?;
        record(&root, "trail.txt", TRAIL, "Start the staging trail")?;
        let head = oid(&root, "HEAD")?;
        let top = PathBuf::from(git(&root, &["rev-parse", "--show-toplevel"])?.trim_end());
        fs::write(root.join("trail.txt"), WAYPOINT)?;
        self.open(&root)?;
        self.until("Staging open with trail.txt unstaged", |tab| {
            tab.ui.repo == "Staging" && files(tab, &[], &["trail.txt"])
        })
        .await?;
        self.action(Action::Select(Selection::Changes))?;
        self.until("the uncommitted changes shown", |tab| tab.ui.selection == Some(Selection::Changes))
            .await?;
        let unstaged = Target::File { path: "trail.txt".into(), side: commands::Side::Unstaged };
        self.action(Action::Menu(Some(view::Menu::Object { target: unstaged.clone(), at: Point::default() })))?;
        let target = self.menu_target()?;
        if target.as_ref() != Some(&unstaged) {
            return Err(format!("A right-click on trail.txt opened the menu of {target:?}").into());
        }
        let ids = |rows: Vec<(&'static str, String, Option<String>)>| -> Vec<&'static str> {
            rows.into_iter().map(|row| row.0).collect()
        };
        let rows = self.menu_rows(&unstaged)?;
        if ids(rows.clone())
            != [
                "file.stage",
                "file.discard",
                "file.ignore-menu",
                "file.stash",
                "file.edit",
                "file.open",
                "file.reveal",
                "file.copy-path",
                "file.copy-full-path",
            ]
            || rows.iter().any(|row| row.2.is_some())
        {
            return Err(format!("The menu of the unstaged trail.txt lists {rows:?}").into());
        }
        let handoffs = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &unstaged, &["file.open", "file.reveal"], cx)
                .into_iter()
                .map(|entry| format!("{:?}", entry.action))
                .collect::<Vec<_>>()
        })?;
        if handoffs
            != [
                r#"Some(Handoff(Open("trail.txt")))"#,
                r#"Some(Handoff(Reveal(Some("trail.txt"))))"#,
            ]
        {
            return Err(format!("Open and Reveal would hand on {handoffs:?}").into());
        }
        self.key("down")?;
        self.key("enter")?;
        self.until("trail.txt moved to Staged", |tab| {
            files(tab, &["trail.txt"], &[]) && tab.ui.menu.is_none()
        })
        .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "trail.txt
")?;
        git_is(&root, &["diff", "--name-only"], "")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}
"))?;
        if fs::read_to_string(root.join("trail.txt"))? != WAYPOINT {
            return Err("Staging changed trail.txt in the working tree".into());
        }
        let staged = Target::File { path: "trail.txt".into(), side: commands::Side::Staged };
        let rows = self.menu_rows(&staged)?;
        if ids(rows.clone())
            != ["file.unstage", "file.stash", "file.edit", "file.open", "file.reveal", "file.copy-path", "file.copy-full-path"]
        {
            return Err(format!("The menu of the staged trail.txt lists {rows:?}").into());
        }
        let full = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &staged, &["file.copy-full-path"], cx)
                .pop()
                .and_then(|entry| entry.action)
        })?;
        let copied = self.copied(full.ok_or("Copy full path cannot run")?, "Copied full path").await?;
        let expected = top.join("trail.txt").display().to_string();
        if copied.as_deref() != Some(expected.as_str()) {
            return Err(format!("Copy full path copied {copied:?}, not {expected}").into());
        }

        git(&root, &["commit", "-qm", WAYPOINT_COMMIT])?;
        let second = oid(&root, "HEAD")?;
        fs::write(root.join("notes.txt"), NOTE)?;
        git(&root, &["add", "--", "notes.txt"])?;
        git(&root, &["stash", "push", "-q", "-m", "Bridge note"])?;
        let saved = oid(&root, "refs/stash")?;
        self.action(Action::Refresh)?;
        self.until("the commit and the stash loaded", |tab| {
            tab.ui.history.first().is_some_and(|row| row.oid == second)
                && tab.ui.stash.entries.len() == 1
                && files(tab, &[], &[])
        })
        .await?;
        self.select_commit(&second).await?;
        self.until("the commit's files listed", |tab| committed_file(tab, "trail.txt", "Modified"))
            .await?;
        let committed = Target::File { path: "trail.txt".into(), side: commands::Side::Commit(second.clone()) };
        let rows = self.menu_rows(&committed)?;
        let listed = ["file.restore-version", "file.restore-before", "file.history", "file.reveal", "file.copy-path", "file.copy-full-path"];
        if ids(rows.clone()) != listed
            || rows.iter().any(|row| row.2.is_some())
        {
            return Err(format!("The menu of the committed trail.txt lists {rows:?}").into());
        }
        let history = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &committed, &["file.history"], cx)
                .pop()
                .and_then(|entry| entry.action)
        })?;
        self.action(history.ok_or("File history cannot run")?)?;
        self.until("the file's history listed", |tab| {
            tab.ui.inspection.path.as_deref() == Some(Path::new("trail.txt"))
                && tab.ui.inspection.history.as_ref().is_some_and(|rows| {
                    rows.iter().map(|row| row.oid.as_str()).collect::<Vec<_>>() == [second.as_str(), head.as_str()]
                })
        })
        .await?;
        let stashed = Target::File { path: "notes.txt".into(), side: commands::Side::Stash(saved.clone()) };
        let rows = self.menu_rows(&stashed)?;
        let reason = Some("notes.txt is not in the working tree".to_owned());
        if ids(rows.clone()) != ["file.reveal", "file.copy-path", "file.copy-full-path"] || rows[0].2 != reason {
            return Err(format!("The menu of the stashed notes.txt lists {rows:?}").into());
        }
        let rows = self.menu_rows(&Target::Stash(saved.clone()))?;
        if ids(rows.clone()) != ["stash.apply", "stash.pop", "stash.rename", "stash.drop"] || rows.iter().any(|row| row.2.is_some()) {
            return Err(format!("The stash's menu lists {rows:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{second}
"))?;
        git_is(&root, &["rev-parse", "refs/stash"], &format!("{saved}
"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        self.staging_keys(dir).await
    }

    fn summary(&mut self) -> Result<String> {
        Ok(self.tab.read_with(self.cx, |tab, cx| tab.ui.input.read(cx).text().to_owned())?)
    }

    async fn staging_keys(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0107");
        let root = init(dir, "Keys")?;
        record(&root, "trail.txt", TRAIL, "Start the key trail")?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("trail.txt"), WAYPOINT)?;
        fs::write(root.join("notes.txt"), NOTE)?;
        self.open(&root)?;
        self.until("Keys open with two unstaged files", |tab| {
            tab.ui.repo == "Keys" && files(tab, &[], &["notes.txt", "trail.txt"])
        })
        .await?;
        let worktree = || -> Result<()> {
            if fs::read_to_string(root.join("trail.txt"))? != WAYPOINT
                || fs::read_to_string(root.join("notes.txt"))? != NOTE
            {
                return Err("A staging key changed a file in the working tree".into());
            }
            git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))
        };
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("trail.txt open", |tab| tab.ui.selection == Some(file("trail.txt", false)))
            .await?;

        self.key("s")?;
        self.until("trail.txt staged by S", |tab| {
            files(tab, &["trail.txt"], &["notes.txt"]) && tab.ui.selection == Some(file("trail.txt", true))
        })
        .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "trail.txt\n")?;
        git_is(&root, &["diff", "--name-only"], "")?;
        worktree()?;
        self.key("s")?;
        self.until("S on the staged file explained", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.text() == "trail.txt is already staged")
        })
        .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "trail.txt\n")?;

        self.key("u")?;
        self.until("trail.txt unstaged by U", |tab| {
            files(tab, &[], &["notes.txt", "trail.txt"]) && tab.ui.selection == Some(file("trail.txt", false))
        })
        .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        git_is(&root, &["diff", "--name-only"], "trail.txt\n")?;
        worktree()?;

        self.key("cmd-shift-m")?;
        let focused = self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.focus_handle(cx).is_focused(window)
        })?;
        if !focused {
            return Err("Cmd-Shift-M did not put the cursor in the commit summary".into());
        }
        self.key("s")?;
        self.key("u")?;
        if self.summary()? != "su" {
            return Err(format!("Typing S and U in the summary gave {:?}", self.summary()?).into());
        }
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;

        self.key("cmd-shift-s")?;
        self.until("everything staged by Cmd-Shift-S", |tab| files(tab, &["notes.txt", "trail.txt"], &[]))
            .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "notes.txt\ntrail.txt\n")?;
        self.key("cmd-shift-u")?;
        self.until("everything unstaged by Cmd-Shift-U", |tab| files(tab, &[], &["notes.txt", "trail.txt"]))
            .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        worktree()?;

        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui.input.update(cx, |field, cx| field.set_text("Record the keyboard trail", cx))
        })?;
        self.key("cmd-shift-enter")?;
        self.until("everything committed by Cmd-Shift-Return", |tab| {
            files(tab, &[], &[]) && tab.ui.head.as_deref() != Some(head.as_str())
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD~1"], &format!("{head}\n"))?;
        git_is(&root, &["log", "-1", "--format=%s"], "Record the keyboard trail\n")?;
        git_is(&root, &["diff", "--name-status", "HEAD~1", "HEAD"], "A\tnotes.txt\nM\ttrail.txt\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if !self.summary()?.is_empty() {
            return Err(format!("The summary still reads {:?} after the commit", self.summary()?).into());
        }
        let committed = oid(&root, "HEAD")?;
        self.tab.update(self.cx, |tab, _, cx| tab.ui.input.update(cx, |field, cx| field.set_text("Nothing", cx)))?;
        self.key("cmd-shift-enter")?;
        self.until("an empty stage-all-and-commit explained", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.text() == "There are no changes to commit")
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{committed}\n"))?;
        self.pass();
        self.columns(dir).await
    }

    fn bar_menu(&mut self, name: &str) -> Result<Vec<String>> {
        let menus = self.cx.update(|cx| cx.get_menus())?.ok_or("The menu bar was never set")?;
        let menu = menus.into_iter().find(|menu| menu.name == name).ok_or_else(|| format!("No {name} menu"))?;
        Ok(menu
            .items
            .into_iter()
            .filter_map(|item| match item {
                gpui::OwnedMenuItem::Action { name, .. } => Some(name),
                _ => None,
            })
            .collect())
    }

    pub(super) fn window_title(&mut self) -> Result<String> {
        Ok(self.cx.update_window(self.tab.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.window_title()
        })?)
    }

    async fn columns(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0111");
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let root = init(dir, "Columns")?;
        record(&root, "trail.txt", TRAIL, "Start the column trail")?;
        self.open(&root)?;
        self.until("Columns open on main", |tab| tab.ui.repo == "Columns" && tab.ui.branch == "main")
            .await?;
        let title = self.window_title()?;
        if title != "Columns — main" {
            return Err(format!("The window is titled {title:?}").into());
        }
        let view_menu = self.bar_menu("View")?;
        if !view_menu.contains(&"Hide Sidebar".to_owned()) || !view_menu.contains(&"Hide Details Panel".to_owned()) {
            return Err(format!("The View menu lists {view_menu:?}").into());
        }
        let viewport = self.tab.update(self.cx, |_, window, _| f32::from(window.viewport_size().width))?;
        let (sidebar, centre, panel) = self.tab.read_with(self.cx, |tab, _| tab.ui.columns_drawn(viewport))?;
        if sidebar <= 0. || panel <= 0. {
            return Err(format!("Both columns should show: {sidebar} and {panel}").into());
        }

        self.key("cmd-j")?;
        self.until("the sidebar hidden", |tab| tab.ui.sidebar_hidden && !tab.ui.panel_hidden).await?;
        let drawn = self.tab.read_with(self.cx, |tab, _| tab.ui.columns_drawn(viewport))?;
        if drawn.0 != 0. || drawn.2 != panel || drawn.1 <= centre + sidebar {
            return Err(format!("With the sidebar hidden the columns are {drawn:?}").into());
        }
        let view_menu = self.bar_menu("View")?;
        if !view_menu.contains(&"Show Sidebar".to_owned()) {
            return Err(format!("With the sidebar hidden the View menu lists {view_menu:?}").into());
        }
        self.key("cmd-k")?;
        self.until("the panel hidden too", |tab| tab.ui.sidebar_hidden && tab.ui.panel_hidden).await?;
        let drawn = self.tab.read_with(self.cx, |tab, _| tab.ui.columns_drawn(viewport))?;
        if drawn != (0., viewport, 0.) {
            return Err(format!("With both columns hidden the columns are {drawn:?}").into());
        }
        self.key("cmd-k")?;
        self.until("the panel back", |tab| tab.ui.sidebar_hidden && !tab.ui.panel_hidden).await?;

        self.cx.update(|cx| settings::flush(cx))?;
        let (chosen, _) = saved(&state)?;
        if chosen["sidebar_hidden"] != true || chosen["panel_hidden"] != false {
            return Err(format!("settings.json holds {chosen}").into());
        }
        self.relaunch().await?;
        self.until("Columns reopened with the sidebar still hidden", |tab| {
            tab.ui.repo == "Columns" && tab.ui.branch == "main" && tab.ui.sidebar_hidden && !tab.ui.panel_hidden
        })
        .await?;
        let title = self.window_title()?;
        let view_menu = self.bar_menu("View")?;
        if title != "Columns — main" || !view_menu.contains(&"Show Sidebar".to_owned()) {
            return Err(format!("After the relaunch the title is {title:?} and View lists {view_menu:?}").into());
        }
        git(&root, &["switch", "-qc", "feature/columns"])?;
        self.action(Action::Refresh)?;
        self.until("feature/columns checked out", |tab| tab.ui.branch == "feature/columns").await?;
        let title = self.window_title()?;
        if title != "Columns — feature/columns" {
            return Err(format!("After switching the window is titled {title:?}").into());
        }

        self.key("cmd-j")?;
        self.until("the sidebar shown", |tab| !tab.ui.sidebar_hidden).await?;
        let drawn = self.tab.read_with(self.cx, |tab, _| tab.ui.columns_drawn(viewport))?;
        if drawn != (sidebar, centre, panel) {
            return Err(format!("With the sidebar back the columns are {drawn:?}").into());
        }
        self.cx.update(|cx| settings::flush(cx))?;
        let (chosen, _) = saved(&state)?;
        if chosen["sidebar_hidden"] != false {
            return Err(format!("settings.json holds {chosen}").into());
        }
        self.pass();
        self.resolver_escape(dir).await
    }
}

type PaletteRow = (&'static str, String, Option<&'static str>, Option<String>);

fn keys_at(tab: &Tab) -> (Zone, Option<usize>, Option<usize>) {
    let ui = &tab.ui;
    (ui.navigate.zone(ui), ui.navigate.file_cursor(ui), ui.navigate.side_cursor(ui))
}

fn commit_selected(tab: &Tab, oid: &str) -> bool {
    tab.ui.selection == Some(Selection::Commit(oid.to_owned()))
}

impl Replay<'_> {
    fn press(&mut self, click: Option<Action>) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            view::press_started(&mut tab.ui);
            if tab.ui.transient_open() {
                view::pressed_outside(tab, window, cx);
            }
            if let Some(action) = click {
                tab.dispatch(action, window, cx);
            }
        })?;
        Ok(())
    }

    fn in_dialog(&mut self) -> Result<bool> {
        Ok(self.tab.update(self.cx, |tab, window, cx| tab.ui.modal_focus.contains_focused(window, cx))?)
    }

    fn patch_offset(&mut self) -> Result<f32> {
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        Ok(self.tab.read_with(self.cx, |tab, _| f32::from(tab.ui.diff_scroll.0.borrow().base_handle.offset().y))?)
    }

    async fn resolver_escape(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0108");
        let root = init(dir, "Ford")?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Old ford\n", "Start the ford")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nWaypoint: Ridge pass\n", "Take the ridge")?;
        let theirs = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, "Cross at the bridge")?;
        let ours = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Ford open on main", |tab| tab.ui.repo == "Ford" && tab.ui.branch == "main").await?;
        self.begin_merge("feature/ridge").await?;
        self.until("trail.txt in conflict", |tab| tab.ui.operation.conflicted() == [Path::new("trail.txt")])
            .await?;
        let conflicted = fs::read_to_string(root.join("trail.txt"))?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let unchanged = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "MERGE_HEAD"], &format!("{theirs}\n"))?;
            git_is(&root, &["rev-parse", "main"], &format!("{ours}\n"))?;
            git_is(&root, &["ls-files", "--stage"], &index)?;
            git_is(&root, &["status", "--porcelain=v1"], "UU trail.txt\n")?;
            if fs::read_to_string(root.join("trail.txt"))? != conflicted {
                return Err(format!("{what} changed the conflicted trail.txt").into());
            }
            Ok(())
        };
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("trail.txt open in the resolver", |tab| tab.ui.merge.editing() == Some(Path::new("trail.txt")))
            .await?;
        self.action(Action::Merge(merge::Action::Choose(0, merge::Side::Incoming)))?;
        self.key("escape")?;
        self.until("the resolver closed and the merge still in progress", |tab| {
            tab.ui.merge.editing().is_none()
                && tab.ui.operation.active()
                && tab.ui.operation.conflicted() == [Path::new("trail.txt")]
                && !tab.ui.prompt.open()
        })
        .await?;
        unchanged("Esc")?;
        self.key("escape")?;
        self.until("the merge still in progress after a second Esc", |tab| {
            tab.ui.operation.active() && tab.ui.merge.editing().is_none()
        })
        .await?;
        unchanged("A second Esc")?;
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("trail.txt open in the resolver again", |tab| {
            tab.ui.merge.editing() == Some(Path::new("trail.txt"))
        })
        .await?;
        self.key("escape")?;
        self.until("the resolver closed again", |tab| tab.ui.merge.editing().is_none() && tab.ui.operation.active())
            .await?;
        unchanged("Esc after reopening")?;
        self.action(Action::Merge(merge::Action::Open("trail.txt".into())))?;
        self.until("trail.txt open in the resolver for Cmd-W", |tab| {
            tab.ui.merge.editing() == Some(Path::new("trail.txt"))
        })
        .await?;
        self.key("cmd-w")?;
        self.until("Cmd-W closed the resolver", |tab| tab.ui.merge.editing().is_none() && tab.ui.operation.active())
            .await?;
        unchanged("Cmd-W in the resolver")?;
        self.pass();
        self.keyboard_walk(dir).await
    }

    async fn keyboard_walk(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0112");
        let root = init(dir, "Walk")?;
        record(&root, "trail.txt", TRAIL, "Start the walk")?;
        let start = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "feature/side"])?;
        record(&root, "side.txt", "Side: one\n", "Take the side path")?;
        record(&root, "side.txt", "Side: two\n", "Walk the side path further")?;
        let side = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        let long: String = (1..=400).map(|n| format!("Marker {n}\n")).collect();
        fs::write(root.join("long.txt"), &long)?;
        fs::write(root.join("main.txt"), "Main: one\n")?;
        fs::write(root.join("notes.txt"), NOTE)?;
        git(&root, &["add", "--", "long.txt", "main.txt", "notes.txt"])?;
        record(&root, "trail.txt", WAYPOINT, "Mark four waypoints")?;
        let marks = oid(&root, "HEAD")?;
        git(&root, &["merge", "-q", "--no-ff", "-m", "Join the side path", "feature/side"])?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Walk open with five commits", |tab| tab.ui.repo == "Walk" && tab.ui.history.len() == 5)
            .await?;
        let rows: Vec<String> = self.tab.read_with(self.cx, |tab, _| tab.ui.history.iter().map(|r| r.oid.clone()).collect())?;
        if rows[0] != head || rows[1] == marks || rows[4] != start {
            return Err(format!("The graph is not interleaved as the scenario needs: {rows:?}").into());
        }
        let before = git(&root, &["show-ref"])?;
        self.select_commit(&head).await?;

        self.key("j")?;
        self.until("J moved down a row", |tab| commit_selected(tab, &rows[1])).await?;
        self.key("k")?;
        self.until("K moved back up", |tab| commit_selected(tab, &head)).await?;
        self.key("shift-down")?;
        self.until("Shift-Down on the merge's first parent", |tab| commit_selected(tab, &marks)).await?;
        self.key("shift-down")?;
        self.until("Shift-Down on the root", |tab| commit_selected(tab, &start)).await?;
        self.key("shift-down")?;
        self.until("Shift-Down at the root stays", |tab| commit_selected(tab, &start)).await?;
        self.key("shift-up")?;
        self.until("Shift-Up back on main's line", |tab| commit_selected(tab, &marks)).await?;
        self.key("shift-up")?;
        self.until("Shift-Up on the merge", |tab| commit_selected(tab, &head)).await?;
        self.key("shift-up")?;
        self.until("Shift-Up from HEAD to the working tree", |tab| tab.ui.selection == Some(Selection::Changes))
            .await?;
        self.key("cmd-down")?;
        self.until("Cmd-Down on the last commit", |tab| commit_selected(tab, &start)).await?;
        self.key("cmd-up")?;
        self.until("Cmd-Up on the top row", |tab| tab.ui.selection == Some(Selection::Changes)).await?;
        self.key("end")?;
        self.until("End on the last commit", |tab| commit_selected(tab, &start)).await?;
        self.key("home")?;
        self.until("Home on the top row", |tab| tab.ui.selection == Some(Selection::Changes)).await?;
        self.key("pagedown")?;
        self.until("Page Down a screenful, here to the last commit", |tab| commit_selected(tab, &start)).await?;
        self.key("pageup")?;
        self.until("Page Up back to the top row", |tab| tab.ui.selection == Some(Selection::Changes)).await?;

        self.select_commit(&marks).await?;
        self.until("the commit's four files listed", |tab| tab.ui.inspection.files.len() == 4).await?;
        let names: Vec<PathBuf> = self.tab.read_with(self.cx, |tab, _| tab.ui.inspection.files.iter().map(|f| f.path.clone()).collect())?;
        self.key("right")?;
        self.until("Right in the file list on its first file", |tab| {
            keys_at(tab) == (Zone::Files, Some(0), None) && !tab.ui.diff_open
        })
        .await?;
        self.key("j")?;
        self.until("J on the second file", |tab| keys_at(tab).1 == Some(1) && !tab.ui.diff_open).await?;
        self.key("enter")?;
        self.until("Return opened the second file", |tab| {
            tab.ui.diff_open && tab.ui.inspection.path.as_ref() == Some(&names[1]) && commit_selected(tab, &marks)
        })
        .await?;
        self.key("down")?;
        self.until("Down opened the third file", |tab| {
            tab.ui.diff_open && tab.ui.inspection.path.as_ref() == Some(&names[2]) && commit_selected(tab, &marks)
        })
        .await?;
        self.key("cmd-up")?;
        self.until("Cmd-Up opened the first file", |tab| {
            tab.ui.inspection.path.as_ref() == Some(&names[0]) && commit_selected(tab, &marks)
        })
        .await?;
        let long_at = names.iter().position(|name| name == Path::new("long.txt")).ok_or("long.txt is not listed")?;
        self.action(Action::Inspect(inspection::Action::OpenFile("long.txt".into())))?;
        self.until("long.txt open", |tab| patch(tab, "+Marker 400") && keys_at(tab).1 == Some(long_at)).await?;
        if self.patch_offset()? != 0. {
            return Err("The patch did not open at its top".into());
        }
        self.key("pagedown")?;
        let paged = self.patch_offset()?;
        if paged >= 0. {
            return Err(format!("Page Down left the patch at {paged}").into());
        }
        self.key("end")?;
        let bottom = self.patch_offset()?;
        if bottom >= paged {
            return Err(format!("End left the patch at {bottom}, above {paged}").into());
        }
        self.key("pageup")?;
        let up = self.patch_offset()?;
        if up <= bottom || up > 0. {
            return Err(format!("Page Up left the patch at {up}").into());
        }
        self.key("home")?;
        if self.patch_offset()? != 0. {
            return Err("Home did not return to the top of the patch".into());
        }
        let still = self.tab.read_with(self.cx, |tab, _| commit_selected(tab, &marks) && tab.ui.inspection.path.as_deref() == Some(Path::new("long.txt")))?;
        if !still {
            return Err("Scrolling the patch changed the selection".into());
        }
        self.key("left")?;
        self.until("Left back in the graph with the patch closed", |tab| {
            keys_at(tab).0 == Zone::Graph && !tab.ui.diff_open && commit_selected(tab, &marks)
        })
        .await?;
        self.key("down")?;
        self.until("Down on the next commit", |tab| commit_selected(tab, &start)).await?;

        self.key("h")?;
        self.until("H in the sidebar", |tab| keys_at(tab).0 == Zone::Sidebar && keys_at(tab).2.is_some()).await?;
        let (at, branch_row, header_row) = self.tab.read_with(self.cx, |tab, _| {
            let rows = &tab.ui.refs.rows;
            let branch = rows.iter().position(|row| matches!(row, SideRow::Ref { index, .. }
                if tab.ui.refs.items[*index].name == "feature/side"));
            let header = rows.iter().position(|row| matches!(row, SideRow::Header { section: Section::Branches, .. }));
            (keys_at(tab).2.unwrap_or(0), branch, header)
        })?;
        let (branch_row, header_row) = (branch_row.ok_or("feature/side is not in the sidebar")?, header_row.ok_or("No Branches header")?);
        let steps = branch_row as isize - at as isize;
        for _ in 0..steps.unsigned_abs() {
            self.key(if steps > 0 { "j" } else { "k" })?;
        }
        self.until("feature/side highlighted", |tab| keys_at(tab).2 == Some(branch_row)).await?;
        self.key("enter")?;
        self.until("Return showed feature/side's commit and the keys stayed in the sidebar", |tab| {
            commit_selected(tab, &side) && keys_at(tab).0 == Zone::Sidebar && keys_at(tab).2 == Some(branch_row)
        })
        .await?;
        self.key("cmd-up")?;
        self.until("Cmd-Up on the first sidebar row", |tab| keys_at(tab).2 == Some(0)).await?;
        if header_row != 0 {
            return Err(format!("The Branches heading is row {header_row}, not the first").into());
        }
        self.key("enter")?;
        self.until("Branches closed", |tab| header_open(tab, Section::Branches) == Some(false)).await?;
        self.key("enter")?;
        self.until("Branches open again", |tab| header_open(tab, Section::Branches) == Some(true)).await?;
        self.key("l")?;
        self.until("L back in the graph", |tab| keys_at(tab).0 == Zone::Graph).await?;

        fs::write(root.join("notes.txt"), "Remember the side path.\n")?;
        self.action(Action::Refresh)?;
        self.until("notes.txt changed", |tab| files(tab, &[], &["notes.txt"])).await?;
        self.action(Action::Select(Selection::Changes))?;
        self.key("right")?;
        self.until("Right opened notes.txt", |tab| {
            tab.ui.selection == Some(file("notes.txt", false)) && tab.ui.diff_open && keys_at(tab).0 == Zone::Files
        })
        .await?;
        self.key("left")?;
        self.until("Left back on the working tree with the patch closed", |tab| {
            tab.ui.selection == Some(Selection::Changes) && !tab.ui.diff_open && keys_at(tab).0 == Zone::Graph
        })
        .await?;
        git_is(&root, &["show-ref"], &before)?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], " M notes.txt\n")?;
        self.pass();
        self.switcher_keys(dir).await
    }

    async fn switcher_keys(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0113");
        let root = init(dir, "Switcher")?;
        record(&root, "trail.txt", TRAIL, "Start the switcher")?;
        let first = oid(&root, "HEAD")?;
        git(&root, &["branch", "feature/ford"])?;
        git(&root, &["branch", "feature/map"])?;
        record(&root, "trail.txt", WAYPOINT, "Move main on")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Switcher open on main", |tab| tab.ui.repo == "Switcher" && tab.ui.history.len() == 2)
            .await?;
        let draft = |tab: &Tab| tab.ui.refs.draft_target().map(|(kind, oid)| (kind, oid.to_owned()));

        self.select_commit(&first).await?;
        self.key("cmd-b")?;
        self.until("Cmd-B opened a branch dialog at HEAD", |tab| draft(tab) == Some((refs::Kind::Branch, head.clone())))
            .await?;
        for (name, said) in [
            ("my branch", "Branch names cannot contain spaces"),
            ("feature/map", "A branch named feature/map already exists"),
            ("feature", "feature cannot be created because the branch feature/ford uses it as a folder"),
            ("feature/map/x", "feature/map/x cannot be created because the branch feature/map exists"),
            ("wip..", "Branch names cannot contain two dots in a row"),
        ] {
            self.retype_ref(name)?;
            let shown = self.tab.read_with(self.cx, |tab, cx| tab.ui.refs.validation(cx))?;
            if shown.as_deref() != Some(said) {
                return Err(format!("Typing {name:?} said {shown:?}, not {said:?}").into());
            }
        }
        self.retype_ref("keys/plain")?;
        for _ in 0..4 {
            self.key("tab")?;
            if !self.in_dialog()? {
                return Err("Tab moved focus out of the branch dialog".into());
            }
        }
        let on_tick = |replay: &mut Self| -> Result<()> {
            for _ in 0..4 {
                let input = replay.tab.update(replay.cx, |tab, window, cx| tab.ui.refs.input.focus_handle(cx).is_focused(window))?;
                if !input {
                    break;
                }
                replay.key("tab")?;
            }
            Ok(())
        };
        on_tick(self)?;
        self.key("space")?;
        self.key("shift-tab")?;
        let input = self.tab.update(self.cx, |tab, window, cx| tab.ui.refs.input.focus_handle(cx).is_focused(window))?;
        if !input {
            return Err("Shift-Tab from the tick box did not return to the name".into());
        }
        self.key("enter")?;
        self.until("keys/plain created without checking it out", |tab| {
            tab.ui.refs.draft.is_none() && tab.ui.refs.branch("keys/plain").is_some()
        })
        .await?;
        git_is(&root, &["rev-parse", "keys/plain"], &format!("{head}\n"))?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "main\n")?;

        self.select_commit(&first).await?;
        self.key("k")?;
        self.until("K moved the selection after the dialog closed", |tab| commit_selected(tab, &head)).await?;
        self.select_commit(&first).await?;
        self.run_command("repo.new-branch")?;
        self.until("New branch at HEAD again", |tab| draft(tab) == Some((refs::Kind::Branch, head.clone()))).await?;
        self.key("escape")?;
        self.until("the dialog closed", |tab| tab.ui.refs.draft.is_none()).await?;

        self.ref_list().await?;
        let rows = |tab: &Tab, cx: &App| refs::State::switcher_rows(&tab.ui, cx);
        let (found, create, at) = self.tab.read_with(self.cx, |tab, cx| rows(tab, cx))?;
        if found != ["feature/ford", "feature/map", "keys/plain", "main"] || create.is_some() || at != Some(0) {
            return Err(format!("The switcher opened with {found:?}, {create:?} and the highlight on {at:?}").into());
        }
        self.key("down")?;
        self.key("down")?;
        self.key("up")?;
        let at = self.tab.read_with(self.cx, |tab, cx| rows(tab, cx).2)?;
        if at != Some(1) {
            return Err(format!("Down, Down and Up left the highlight on {at:?}").into());
        }
        self.key("enter")?;
        self.until("switched to feature/map", |tab| tab.ui.branch == "feature/map" && !tab.ui.refs.list_open)
            .await?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "feature/map\n")?;

        self.ref_list().await?;
        self.key("k")?;
        let typed = self.tab.read_with(self.cx, |tab, cx| tab.ui.refs.find.read(cx).text().to_owned())?;
        if typed != "k" {
            return Err(format!("K typed {typed:?} in the switcher's field").into());
        }
        self.find_branch("spike")?;
        let (found, create, at) = self.tab.read_with(self.cx, |tab, cx| rows(tab, cx))?;
        if !found.is_empty() || create.as_deref() != Some("spike") || at != Some(0) {
            return Err(format!("Typing spike showed {found:?}, {create:?} and the highlight on {at:?}").into());
        }
        self.key("enter")?;
        self.until("spike created and checked out", |tab| tab.ui.branch == "spike" && !tab.ui.refs.list_open)
            .await?;
        git_is(&root, &["rev-parse", "spike"], &format!("{first}\n"))?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "spike\n")?;

        let refs_now = git(&root, &["show-ref"])?;
        self.ref_list().await?;
        self.find_branch("feature")?;
        let (found, create, _) = self.tab.read_with(self.cx, |tab, cx| rows(tab, cx))?;
        if found != ["feature/ford", "feature/map"] || create.as_deref() != Some("feature") {
            return Err(format!("Typing feature showed {found:?} and {create:?}").into());
        }
        self.key("down")?;
        self.key("down")?;
        self.key("down")?;
        let at = self.tab.read_with(self.cx, |tab, cx| rows(tab, cx).2)?;
        if at != Some(2) {
            return Err(format!("Down did not reach the Create branch row: {at:?}").into());
        }
        self.key("enter")?;
        self.until("the clash explained", |tab| {
            tab.ui.notices.newest().is_some_and(|n| {
                n.tone == Tone::Info
                    && n.text() == "feature cannot be created because the branch feature/ford uses it as a folder"
            })
        })
        .await?;
        git_is(&root, &["show-ref"], &refs_now)?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "spike\n")?;
        self.key("escape")?;
        self.until("the switcher closed", |tab| !tab.ui.refs.list_open).await?;
        self.pass();
        self.menu_clicks(dir).await
    }

    fn retype_ref(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, _, cx| tab.ui.refs.input.update(cx, |field, cx| field.set_text("", cx)))?;
        self.type_ref(text)
    }

    fn find_branch(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.refs.find.update(cx, |field, cx| {
                field.set_text("", cx);
                field.replace_text_in_range(None, text, window, cx);
            })
        })?;
        Ok(())
    }

    async fn menu_clicks(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0114");
        let root = init(dir, "Pointer")?;
        record(&root, "trail.txt", TRAIL, "Start the pointer trail")?;
        let first = oid(&root, "HEAD")?;
        record(&root, "trail.txt", WAYPOINT, "Point at the bridge")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Pointer open", |tab| tab.ui.repo == "Pointer" && tab.ui.history.len() == 2).await?;
        self.select_commit(&head).await?;
        let repo_menu = |tab: &Tab| matches!(tab.ui.menu, Some(view::Menu::Repo));
        let open_repo = || Action::Menu(Some(view::Menu::Repo));

        self.press(Some(open_repo()))?;
        self.until("the repository menu open", repo_menu).await?;
        self.press(Some(open_repo()))?;
        self.until("the repository menu closed by its button", |tab| tab.ui.menu.is_none()).await?;
        self.press(Some(open_repo()))?;
        self.until("the repository menu open again", repo_menu).await?;
        self.press(Some(Action::Refs(refs::Action::ToggleList)))?;
        self.until("the switcher open and the menu closed", |tab| tab.ui.refs.list_open && tab.ui.menu.is_none())
            .await?;
        self.press(Some(Action::Refs(refs::Action::ToggleList)))?;
        self.until("the switcher closed by its button", |tab| !tab.ui.refs.list_open).await?;

        self.press(Some(open_repo()))?;
        self.until("the repository menu open for the row click", repo_menu).await?;
        self.press(Some(Action::Select(Selection::Commit(first.clone()))))?;
        self.until("the menu closed and the second commit selected", |tab| {
            tab.ui.menu.is_none() && commit_selected(tab, &first)
        })
        .await?;
        self.press(Some(open_repo()))?;
        self.until("the repository menu open for the right-click", repo_menu).await?;
        let menu_of = |oid: &str| Action::Menu(Some(view::Menu::Object { target: Target::Commit(oid.to_owned()), at: Point::default() }));
        self.press(Some(menu_of(&head)))?;
        self.until("the first commit's menu open", |tab| {
            matches!(&tab.ui.menu, Some(view::Menu::Object { target: Target::Commit(oid), .. }) if *oid == head)
        })
        .await?;
        self.press(Some(menu_of(&first)))?;
        self.until("the second commit's menu open", |tab| {
            matches!(&tab.ui.menu, Some(view::Menu::Object { target: Target::Commit(oid), .. }) if *oid == first)
        })
        .await?;
        self.press(None)?;
        self.until("the menu closed by a press beside it", |tab| tab.ui.menu.is_none()).await?;
        self.press(Some(open_repo()))?;
        self.until("the repository menu open once more", repo_menu).await?;
        self.key("escape")?;
        self.until("the menu closed by Esc", |tab| tab.ui.menu.is_none()).await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        self.dialog_focus(dir).await
    }

    async fn dialog_focus(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0115");
        let root = init(dir, "Focus")?;
        record(&root, "trail.txt", TRAIL, "Start the focus trail")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Focus open", |tab| tab.ui.repo == "Focus" && tab.ui.history.len() == 1).await?;
        self.select_commit(&head).await?;
        let refs_before = git(&root, &["show-ref"])?;
        for (what, open, shut) in [
            ("the remotes dialog", Action::Publish(crate::publish::Action::Menu), "escape"),
            ("the keyboard shortcuts sheet", Action::Run("app.shortcuts"), "escape"),
            ("the branch dialog", Action::Run("repo.new-branch"), "escape"),
        ] {
            self.action(open)?;
            self.until(what, |tab| tab.ui.modal_open()).await?;
            self.key("tab")?;
            for key in ["tab", "tab", "tab", "tab", "tab", "shift-tab", "shift-tab", "shift-tab", "shift-tab", "shift-tab", "shift-tab", "shift-tab"] {
                self.key(key)?;
                if !self.in_dialog()? {
                    return Err(format!("{key} moved focus out of {what}").into());
                }
            }
            self.key("j")?;
            self.key(shut)?;
            self.until("the dialog closed by Esc", |tab| !tab.ui.modal_open()).await?;
            let moved = self.tab.read_with(self.cx, |tab, _| !commit_selected(tab, &head))?;
            if moved {
                return Err(format!("A key behind {what} moved the selection").into());
            }
        }
        git_is(&root, &["show-ref"], &refs_before)?;

        fs::write(root.join("trail.txt"), WAYPOINT)?;
        self.action(Action::Refresh)?;
        self.until("trail.txt changed", |tab| files(tab, &[], &["trail.txt"])).await?;
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("trail.txt open", |tab| tab.ui.diff_open).await?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.refs.filter.update(cx, |field, cx| field.replace_text_in_range(None, "ma", window, cx));
            tab.ui.notices.show(notice::Notice::new(Tone::Error, "A test error", "It stays until dismissed."));
        })?;
        self.key("escape")?;
        self.until("Esc closed the file", |tab| !tab.ui.diff_open && tab.ui.selection == Some(Selection::Changes))
            .await?;
        self.key("escape")?;
        self.until("Esc cleared the sidebar filter", |tab| {
            !tab.ui.diff_open && tab.ui.notices.items().iter().any(|n| n.title == "A test error")
        })
        .await?;
        let filter = self.tab.read_with(self.cx, |tab, cx| tab.ui.refs.filter.read(cx).text().to_owned())?;
        if !filter.is_empty() {
            return Err(format!("The sidebar filter still reads {filter:?}").into());
        }
        self.key("escape")?;
        self.until("Esc dismissed the error notice", |tab| {
            !tab.ui.notices.items().iter().any(|n| n.title == "A test error")
        })
        .await?;
        let second = |tab: &Tab| tab.ui.notices.items().iter().any(|n| n.title == "A second error");
        self.tab.update(self.cx, |tab, window, cx| {
            window.focus(&tab.ui.input.focus_handle(cx));
            tab.ui.notices.show(notice::Notice::new(Tone::Error, "A second error", "It stays too."));
        })?;
        self.key("escape")?;
        let (typing, shown) = self.tab.update(self.cx, |tab, window, cx| {
            (tab.ui.input.focus_handle(cx).is_focused(window), second(tab))
        })?;
        if typing || !shown {
            return Err(format!("Esc in the summary left it focused: {typing}, notice shown: {shown}").into());
        }
        self.key("escape")?;
        self.until("the next Esc dismissed the second error", |tab| !second(tab)).await?;
        let gone = self.tab.update(self.cx, |_, window, cx| {
            let gone = cx.focus_handle();
            window.focus(&gone);
            gone
        })?;
        self.key("j")?;
        self.until("J moved after focus was left on nothing", |tab| commit_selected(tab, &head)).await?;
        drop(gone);
        git_is(&root, &["status", "--porcelain=v1"], " M trail.txt\n")?;
        self.pass();
        self.palette(dir).await
    }

    pub(super) fn type_keys(&mut self, text: &str) -> Result<()> {
        for letter in text.chars() {
            match letter {
                ' ' => self.key("space")?,
                letter => self.key(&letter.to_string())?,
            }
        }
        Ok(())
    }

    pub(super) fn palette_rows(&mut self) -> Result<(Vec<PaletteRow>, Option<usize>)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let rows = crate::palette::rows(&tab.ui, cx)
                .into_iter()
                .map(|entry| (entry.id, entry.title, entry.shortcut, entry.disabled))
                .collect();
            (rows, crate::palette::cursor(&tab.ui, cx))
        })?)
    }

    async fn palette(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0109");
        let sandbox = dir.join("palette");
        fs::create_dir(&sandbox)?;
        let root = init(&sandbox, "Palette")?;
        record(&root, "trail.txt", TRAIL, "Start the palette trail")?;
        let head = oid(&root, "HEAD")?;
        git(&root, &["branch", "feature/map"])?;
        git(&sandbox, &["init", "--bare", "-q", "-b", "main", "origin.git"])?;
        git(&root, &["remote", "add", "origin", "../origin.git"])?;
        git(&root, &["push", "-qu", "origin", "main"])?;
        git(
            &sandbox,
            &[
                "clone",
                "-q",
                "--no-local",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Palette Peer",
                "-c",
                "user.email=peer@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "origin.git",
                "peer",
            ],
        )?;
        let peer = sandbox.join("peer");
        record(&peer, "signpost.txt", "East: footbridge\n", "Add east signpost")?;
        git(&peer, &["push", "-q", "origin", "main"])?;
        let pushed = oid(&peer, "HEAD")?;
        self.open(&root)?;
        self.until("Palette open on main tracking origin/main", |tab| {
            tab.ui.repo == "Palette"
                && tab.ui.branch == "main"
                && tab.ui.upstream.as_deref() == Some("origin/main")
                && tab.ui.refs.lists(refs::Kind::Branch, "feature/map")
        })
        .await?;
        self.select_commit(&head).await?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let unchanged = |tracking: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "main", "origin/main"], &format!("{head}\n{tracking}\n"))?;
            git_is(&root, &["ls-files", "--stage"], &index)?;
            git_is(&root, &["status", "--porcelain=v1"], "")
        };
        let open = |tab: &Tab| tab.ui.palette.open();

        self.key("cmd-p")?;
        self.until("the palette open", open).await?;
        let typing = self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.palette.field.focus_handle(cx).is_focused(window)
        })?;
        if !typing {
            return Err("Cmd-P did not put the cursor in the palette's field".into());
        }
        let (rows, at) = self.palette_rows()?;
        let has = |id: &str, title: &str, keys: Option<&str>| {
            rows.iter().any(|row| row.0 == id && row.1 == title && row.2 == keys && row.3.is_none())
        };
        if !has("commit.copy-sha", "Copy SHA", None)
            || !has("repo.fetch-all", "Fetch all remotes", Some("cmd-l"))
            || !has("app.shortcuts", "Keyboard shortcuts", Some("cmd-/"))
            || rows.iter().any(|row| row.0 == "app.palette" || row.0 == "file.stage")
            || at != Some(0)
        {
            return Err(format!("With the commit selected the palette lists {rows:?}, at {at:?}").into());
        }
        for key in ["down", "down", "up"] {
            self.key(key)?;
        }
        let (_, at) = self.palette_rows()?;
        if at != Some(1) {
            return Err(format!("Down, Down, Up left the highlight on {at:?}").into());
        }
        self.type_keys("shell")?;
        let (rows, at) = self.palette_rows()?;
        if rows.first().map(|row| row.0) != Some("repo.terminal") || at != Some(0) {
            return Err(format!("\"shell\" lists {rows:?}, at {at:?}").into());
        }
        self.key("escape")?;
        self.until("the palette closed by Esc", |tab| !open(tab)).await?;
        let shown = self.tab.read_with(self.cx, |tab, _| notices(tab))?;
        if !shown.is_empty() {
            return Err(format!("Closing the palette showed {shown:?}").into());
        }
        unchanged(&head)?;

        self.key("cmd-p")?;
        self.until("the palette open again", open).await?;
        let field = self.tab.read_with(self.cx, |tab, cx| tab.ui.palette.field.read(cx).text().to_owned())?;
        if !field.is_empty() {
            return Err(format!("The palette reopened with {field:?} in its field").into());
        }
        self.type_keys("main")?;
        let (rows, _) = self.palette_rows()?;
        let reason = "main is already checked out";
        if rows.first() != Some(&("branch.switch", "Switch to main".to_owned(), None, Some(reason.to_owned()))) {
            return Err(format!("\"main\" lists {rows:?}").into());
        }
        self.key("enter")?;
        self.until("the reason in an information notice", |tab| {
            open(tab) && notices(tab) == [(Tone::Info, reason.to_owned(), 1)]
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        self.key("cmd-p")?;
        self.until("the palette closed by Cmd-P", |tab| !open(tab)).await?;
        unchanged(&head)?;

        self.key("cmd-p")?;
        self.until("the palette open for the fetch", open).await?;
        self.type_keys("fetch")?;
        let (rows, at) = self.palette_rows()?;
        if rows.first().map(|row| (row.0, row.3.is_none())) != Some(("repo.fetch-all", true)) || at != Some(0) {
            return Err(format!("\"fetch\" lists {rows:?}, at {at:?}").into());
        }
        self.key("enter")?;
        self.until("the fetch finished from the palette", |tab| {
            !open(tab) && tab.ui.notices.newest().is_some_and(|n| n.title == "Fetched Successfully")
        })
        .await?;
        unchanged(&pushed)?;
        if root.join("signpost.txt").exists() {
            return Err("Fetching from the palette changed the working tree".into());
        }

        self.key("cmd-p")?;
        self.until("the palette open for a branch", open).await?;
        self.type_keys("feat")?;
        let (rows, _) = self.palette_rows()?;
        if rows.first() != Some(&("branch.switch", "Switch to feature/map".to_owned(), None, None)) {
            return Err(format!("\"feat\" lists {rows:?}").into());
        }
        self.key("enter")?;
        self.until("feature/map checked out from the palette", |tab| {
            !open(tab) && tab.ui.branch == "feature/map"
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/map\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }
}
