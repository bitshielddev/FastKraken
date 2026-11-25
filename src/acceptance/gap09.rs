use super::*;
use crate::{
    commands::{self, Side, Target},
    compare::{self, Rev},
    file_history,
    picked::How,
    view::DiffMode,
};

type Listed = Vec<(String, String, Option<String>)>;

fn status_word(code: &str) -> &'static str {
    match code.chars().next() {
        Some('A') => "Added",
        Some('D') => "Deleted",
        Some('R') => "Renamed",
        Some('C') => "Copied",
        Some('T') => "Type changed",
        _ => "Modified",
    }
}

fn git_listed(root: &Path, args: &[&str]) -> Result<Listed> {
    let mut command = vec!["diff", "--name-status", "--find-renames", "--no-color"];
    command.extend_from_slice(args);
    Ok(git(root, &command)?
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let word = status_word(fields[0]).to_owned();
            match fields.as_slice() {
                [_, from, to] => (word, (*to).to_owned(), Some((*from).to_owned())),
                [_, path, ..] => (word, (*path).to_owned(), None),
                _ => (word, String::new(), None),
            }
        })
        .collect())
}

fn compared(tab: &Tab) -> Listed {
    tab.ui
        .compare
        .files
        .iter()
        .map(|file| {
            (
                file.status.to_owned(),
                file.path.to_string_lossy().into_owned(),
                file.from.as_ref().map(|from| from.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

fn short(oid: &str) -> String {
    oid.chars().take(7).collect()
}

fn showing(tab: &Tab, selection: &Selection, files: &Listed) -> bool {
    tab.ui.selection.as_ref() == Some(selection) && !tab.ui.compare.loading() && compared(tab) == *files
}

fn heading(tab: &Tab) -> Option<compare::Heading> {
    compare::heading(&tab.ui)
}

fn untouched(root: &Path) -> Result<String> {
    Ok([
        git(root, &["for-each-ref"])?,
        git(root, &["ls-files", "-s"])?,
        git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?,
    ]
    .join("\n"))
}

const CONFIRM: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };

fn revisions(tab: &Tab) -> Vec<String> {
    tab.ui.file_history.entries().iter().map(|entry| entry.oid.clone()).collect()
}

fn listed_oids(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let mut command = vec!["log", "--follow", "--format=%H"];
    command.extend_from_slice(args);
    Ok(git(root, &command)?.lines().map(str::to_owned).collect())
}

fn in_history(tab: &Tab, index: usize, oid: &str, path: &str) -> bool {
    view::centre(&tab.ui) == view::Centre::FileHistory
        && tab.ui.file_history.chosen_index() == Some(index)
        && tab.ui.selection == Some(Selection::Commit(oid.to_owned()))
        && tab.ui.diff_open
        && tab.ui.inspection.path.as_deref() == Some(Path::new(path))
        && !tab.ui.loading_diff()
}

fn whole_file(tab: &Tab, contents: &str) -> bool {
    !tab.ui.loading_diff() && tab.ui.diff.file.as_ref().and_then(|shown| shown.contents.as_deref()) == Some(contents)
}

impl Replay<'_> {
    pub(super) async fn gap09(&mut self, dir: &Path) -> Result<()> {
        self.compare_two_commits(dir).await?;
        self.compare_since_they_diverged(dir).await?;
        self.compare_with_my_files(dir).await?;
        self.combine_a_run(dir).await?;
        self.step_through_history(dir).await?;
        self.history_from_my_files(dir).await
    }

    fn pick_commit(&mut self, oid: &str, how: How) -> Result<()> {
        self.action(Action::Compare(compare::Action::Pick { rev: Rev::Commit(oid.to_owned()), how }))
    }

    async fn select_only(&mut self, oid: &str) -> Result<()> {
        self.action(Action::Select(Selection::Commit(oid.to_owned())))?;
        self.until("one commit selected and nothing picked", |tab| {
            tab.ui.selection == Some(Selection::Commit(oid.to_owned())) && tab.ui.compare.count() == 0
        })
        .await
    }

    fn entry_action(&mut self, target: &Target, id: &str) -> Result<(String, Action)> {
        let found = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| Some((entry.title, entry.action?)))
        })?;
        found.ok_or_else(|| format!("{target:?} offers no enabled {id}").into())
    }

    async fn open_compared(&mut self, path: &str, line: &str) -> Result<()> {
        self.action(Action::Compare(compare::Action::OpenFile(path.into())))?;
        self.until("the compared file's diff over the graph", |tab| {
            tab.ui.compare.path.as_deref() == Some(Path::new(path))
                && view::centre(&tab.ui) == view::Centre::Diff
                && patch(tab, line)
        })
        .await
    }

    async fn compare_two_commits(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0901");
        let root = init(dir, "Compare-trail")?;
        record(&root, "camp.txt", "Base camp by the pines\nFire ring\nWater from the creek\n", "Pitch the camp")?;
        record(&root, "trail.txt", "Trail\n", "Mark the trail")?;
        let trail = oid(&root, "HEAD")?;
        git(&root, &["mv", "camp.txt", "base.txt"])?;
        git(&root, &["commit", "-qm", "Rename the camp"])?;
        record(&root, "lake.txt", "Map the lake\n", "Map the lake")?;
        let lake = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Compare-trail open with four commits", |tab| {
            tab.ui.repo == "Compare-trail" && tab.ui.history.len() == 4
        })
        .await?;
        let expected = git_listed(&root, &[&trail, &lake])?;
        if !expected.iter().any(|row| row.1 == "base.txt" && row.2.as_deref() == Some("camp.txt")) {
            return Err(format!("Git lists {expected:?} between the two commits").into());
        }
        let between = Selection::Compare { from: Rev::Commit(trail.clone()), to: Rev::Commit(lake.clone()), since_fork: false };

        self.select_only(&lake).await?;
        self.pick_commit(&trail, How::Toggle)?;
        self.until("the two commits compared, older first", |tab| showing(tab, &between, &expected)).await?;
        let title = format!("Comparing {} with {}", short(&trail), short(&lake));
        let read = self.tab.read_with(self.cx, |tab, _| {
            (heading(tab).map(|heading| heading.title), tab.ui.compare.picks(&trail) && tab.ui.compare.picks(&lake))
        })?;
        if read != (Some(title.clone()), true) {
            return Err(format!("Picking down the graph reads {read:?}").into());
        }
        self.open_compared("lake.txt", "+Map the lake").await?;
        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        self.until("the compared file's history open", |tab| revisions(tab) == [lake.clone()] && in_history(tab, 0, &lake, "lake.txt"))
            .await?;
        self.key("escape")?;
        self.until("Esc returns to the compared file", |tab| {
            !tab.ui.file_history.open()
                && showing(tab, &between, &expected)
                && tab.ui.compare.path.as_deref() == Some(Path::new("lake.txt"))
                && view::centre(&tab.ui) == view::Centre::Diff
                && patch(tab, "+Map the lake")
        })
        .await?;

        self.select_only(&trail).await?;
        self.pick_commit(&lake, How::Toggle)?;
        self.until("the same comparison picked up the graph", |tab| showing(tab, &between, &expected)).await?;
        let title_up = self.tab.read_with(self.cx, |tab, _| heading(tab).map(|heading| heading.title))?;
        if title_up != Some(title) {
            return Err(format!("Picking up the graph reads {title_up:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn compare_since_they_diverged(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0902");
        let root = init(dir, "Fork-trail")?;
        record(&root, "camp.txt", "camp\n", "Pitch the camp")?;
        git(&root, &["switch", "-qc", "ridge"])?;
        record(&root, "ridge.txt", "Survey the ridge\n", "Survey the ridge")?;
        let ridge = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "lake.txt", "Map the lake\n", "Map the lake")?;
        let main = oid(&root, "HEAD")?;
        let before = untouched(&root)?;
        self.open(&root)?;
        self.until("Fork-trail open with ridge in the sidebar", |tab| {
            tab.ui.repo == "Fork-trail"
                && tab.ui.head.as_deref() == Some(main.as_str())
                && tab.ui.refs.local(std::ffi::OsStr::new("ridge")).is_some_and(|item| item.oid.as_deref() == Some(ridge.as_str()))
        })
        .await?;
        let target = Target::Branch("ridge".into());
        let (title, action) = self.entry_action(&target, "ref.compare-current")?;
        if title != "Compare with main" {
            return Err(format!("ridge's menu offers {title:?}").into());
        }
        let main_menu = self.menu_rows(&Target::Branch("main".into()))?;
        if main_menu.iter().any(|row| row.0 == "ref.compare-current") {
            return Err("The checked-out branch offers to compare with itself".into());
        }
        let since = git_listed(&root, &[&format!("{main}...{ridge}")])?;
        if since != [("Added".to_owned(), "ridge.txt".to_owned(), None)] {
            return Err(format!("Git lists {since:?} since the fork").into());
        }
        self.action(action)?;
        let forked = Selection::Compare { from: Rev::Commit(main.clone()), to: Rev::Commit(ridge.clone()), since_fork: true };
        self.until("ridge compared with main since they diverged", |tab| showing(tab, &forked, &since)).await?;
        let read = self.tab.read_with(self.cx, |tab, _| heading(tab).map(|heading| heading.title))?;
        if read.as_deref() != Some("Comparing ridge with main") {
            return Err(format!("The comparison reads {read:?}").into());
        }

        let tips = git_listed(&root, &[&main, &ridge])?;
        if tips.len() != 2 {
            return Err(format!("Git lists {tips:?} between the tips").into());
        }
        self.action(Action::Compare(compare::Action::SinceFork(false)))?;
        let tip_to_tip = Selection::Compare { from: Rev::Commit(main.clone()), to: Rev::Commit(ridge.clone()), since_fork: false };
        self.until("ridge compared with main tip to tip", |tab| showing(tab, &tip_to_tip, &tips)).await?;
        let still = self.tab.read_with(self.cx, |tab, _| heading(tab).map(|heading| heading.title))?;
        if still.as_deref() != Some("Comparing ridge with main") {
            return Err(format!("Tip to tip reads {still:?}").into());
        }
        if untouched(&root)? != before {
            return Err("Comparing changed a ref, the index or a file".into());
        }
        self.pass();
        Ok(())
    }

    async fn compare_with_my_files(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0903");
        let root = init(dir, "Desk-trail")?;
        record(&root, "camp.txt", "camp\n", "Pitch the camp")?;
        let base = oid(&root, "HEAD")?;
        record(&root, "trail.txt", "trail\n", "Mark the trail")?;
        fs::write(root.join("camp.txt"), "camp by the lake\n")?;
        fs::write(root.join("loose.txt"), "not tracked\n")?;
        self.open(&root)?;
        self.until("Desk-trail open with an edit and an untracked file", |tab| {
            tab.ui.repo == "Desk-trail" && files(tab, &[], &["camp.txt", "loose.txt"]) && tab.ui.history.len() == 2
        })
        .await?;
        let (title, action) = self.entry_action(&Target::Commit(base.clone()), "commit.compare-working")?;
        if title != "Compare with my files" {
            return Err(format!("The commit's menu offers {title:?}").into());
        }
        let expected = git_listed(&root, &[&base])?;
        if expected.iter().any(|row| row.1 == "loose.txt") || !expected.iter().any(|row| row.1 == "camp.txt") {
            return Err(format!("Git lists {expected:?} against the working tree").into());
        }
        self.action(action)?;
        let mine = Selection::Compare { from: Rev::Commit(base.clone()), to: Rev::WorkingTree, since_fork: false };
        self.until("the commit compared with the working tree", |tab| showing(tab, &mine, &expected)).await?;
        let read = self.tab.read_with(self.cx, |tab, _| heading(tab))?;
        let wanted_title = format!("Comparing {} with your files", short(&base));
        if !read.as_ref().is_some_and(|heading| {
            heading.title == wanted_title && heading.notes.iter().any(|note| note == compare::UNTRACKED_LEFT_OUT)
        }) {
            return Err(format!("The comparison reads {read:?}").into());
        }
        self.open_compared("camp.txt", "+camp by the lake").await?;

        fs::write(root.join("camp.txt"), "camp by the river\n")?;
        self.until("the open comparison follows the edit", |tab| {
            tab.ui.selection.as_ref() == Some(&mine) && patch(tab, "+camp by the river")
        })
        .await?;

        let head = oid(&root, "HEAD")?;
        let (title, action) = self.entry_action(&Target::Branch("main".into()), "ref.compare-working")?;
        if title != "Compare with my files" {
            return Err(format!("main's menu offers {title:?}").into());
        }
        let on_main = git_listed(&root, &[&head])?;
        if on_main != [("Modified".to_owned(), "camp.txt".to_owned(), None)] {
            return Err(format!("Git lists {on_main:?} between main and the working tree").into());
        }
        self.action(action)?;
        let desk = Selection::Compare { from: Rev::Commit(head), to: Rev::WorkingTree, since_fork: false };
        self.until("main compared with the working tree", |tab| showing(tab, &desk, &on_main)).await?;
        let read = self.tab.read_with(self.cx, |tab, _| heading(tab).map(|heading| heading.title))?;
        if read.as_deref() != Some("Comparing main with your files") {
            return Err(format!("The branch comparison reads {read:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn combine_a_run(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0904");
        let root = init(dir, "Run-trail")?;
        let mut commits = Vec::new();
        for (path, text, subject) in [
            ("camp.txt", "camp\n", "Pitch the camp"),
            ("trail.txt", "trail\n", "Mark the trail"),
            ("camp.txt", "camp by the lake\n", "Move the camp"),
            ("ridge.txt", "ridge\n", "Survey the ridge"),
            ("lake.txt", "lake\n", "Map the lake"),
        ] {
            record(&root, path, text, subject)?;
            commits.push(oid(&root, "HEAD")?);
        }
        commits.reverse();
        let (newest, second, third, fourth) = (commits[0].clone(), commits[1].clone(), commits[2].clone(), commits[3].clone());
        self.open(&root)?;
        self.until("Run-trail open with five commits", |tab| tab.ui.repo == "Run-trail" && tab.ui.history.len() == 5).await?;
        let parent = oid(&root, &format!("{fourth}^"))?;
        let expected = git_listed(&root, &[&parent, &second])?;
        if expected.len() != 3 {
            return Err(format!("Git lists {expected:?} over the three commits").into());
        }

        self.select_only(&second).await?;
        self.pick_commit(&fourth, How::Range)?;
        let combined = Selection::Compare { from: Rev::Commit(parent.clone()), to: Rev::Commit(second.clone()), since_fork: false };
        self.until("the run compared as one change", |tab| showing(tab, &combined, &expected)).await?;
        let read = self.tab.read_with(self.cx, |tab, _| (heading(tab).map(|heading| heading.title), tab.ui.compare.count()))?;
        if read != (Some("Changes in 3 commits".to_owned()), 3) {
            return Err(format!("The run reads {read:?}").into());
        }
        self.open_compared("camp.txt", "+camp by the lake").await?;
        let whole = git(&root, &["diff", "--no-color", &parent, &second, "--", "camp.txt"])?;
        let shown = self.tab.read_with(self.cx, |tab, _| tab.diff.as_ref().map(|diff| diff.text.clone()))?;
        let added = |text: &str| text.lines().filter(|line| line.starts_with('+') && !line.starts_with("+++")).map(str::to_owned).collect::<Vec<_>>();
        if shown.as_deref().map(added) != Some(added(&whole)) {
            return Err(format!("The combined diff reads {shown:?}, Git says {whole:?}").into());
        }

        self.pick_commit(&newest, How::Toggle)?;
        self.pick_commit(&third, How::Toggle)?;
        self.until("three commits that are not one run", |tab| {
            tab.ui.selection == Some(Selection::Commits) && tab.ui.compare.count() == 3
        })
        .await?;
        let read = self.tab.read_with(self.cx, |tab, _| heading(tab))?;
        if !read.as_ref().is_some_and(|heading| {
            heading.title == "3 commits selected" && heading.notes.iter().any(|note| note == compare::NO_COMBINED_CHANGE)
        }) {
            return Err(format!("The pick reads {read:?}").into());
        }
        let menu = self.tab.read_with(self.cx, |tab, _| compare::menu_for(&tab.ui, &second, Default::default()))?;
        let Some(menu) = menu else {
            return Err("A picked row has no menu for the pick".into());
        };
        self.action(menu)?;
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            tab.ui.menu.as_ref().map_or_else(Vec::new, |open| {
                commands::entries_of(&tab.ui, open, cx).into_iter().map(|entry| (entry.id, entry.title)).collect::<Vec<_>>()
            })
        })?;
        if !rows.iter().any(|(id, title)| *id == "commits.copy-shas" && title == "Copy 3 SHAs") {
            return Err(format!("The pick's menu lists {rows:?}").into());
        }
        self.action(Action::Dismiss)?;
        let kept = self.tab.read_with(self.cx, |tab, _| (tab.ui.selection.clone(), compare::picked_commits(&tab.ui)))?;
        if kept != (Some(Selection::Commits), vec![newest, second, fourth]) {
            return Err(format!("After the menu the pick is {kept:?}").into());
        }
        self.hide_every_picked_commit(dir).await?;
        self.pass();
        Ok(())
    }

    async fn hide_every_picked_commit(&mut self, dir: &Path) -> Result<()> {
        let root = init(dir, "Hidden-run")?;
        record(&root, "camp.txt", "camp\n", "Pitch the camp")?;
        git(&root, &["switch", "-qc", "ridge"])?;
        let mut ridge = Vec::new();
        for n in 0..4 {
            record(&root, &format!("ridge-{n}.txt"), "ridge\n", &format!("Survey ridge {n}"))?;
            ridge.push(oid(&root, "HEAD")?);
        }
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "lake.txt", "lake\n", "Map the lake")?;
        let lake = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Hidden-run open with six commits", |tab| tab.ui.repo == "Hidden-run" && tab.ui.history.len() == 6).await?;
        self.select_only(&ridge[3]).await?;
        self.pick_commit(&ridge[1], How::Toggle)?;
        self.pick_commit(&ridge[0], How::Toggle)?;
        self.until("three ridge commits that are not one run", |tab| {
            tab.ui.selection == Some(Selection::Commits) && tab.ui.compare.count() == 3
        })
        .await?;
        self.action(Action::GraphView(crate::graph_view::Action::Hide("refs/heads/ridge".into())))?;
        self.until("ridge hidden and the empty pick left for the newest commit", |tab| {
            tab.ui.history.len() == 2
                && tab.ui.selection == Some(Selection::Commit(lake.clone()))
                && tab.ui.compare.count() == 0
                && heading(tab).is_none()
        })
        .await
    }

    async fn step_through_history(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0905");
        let root = init(dir, "History-trail")?;
        let rest = "Water from the creek\nTents in a row\nFood in the tree\nMap on the board\nLamp by the door\n";
        let pitched = format!("Base camp by the pines\n{rest}");
        let moved_text = format!("Base camp by the lake\n{rest}");
        let ring_text = format!("Base camp by the lake\n{rest}Fire ring\n");
        record(&root, "camp.txt", &pitched, "Pitch the camp")?;
        let pitch = oid(&root, "HEAD")?;
        record(&root, "trail.txt", "Trail\n", "Mark the trail")?;
        git(&root, &["mv", "camp.txt", "site.txt"])?;
        record(&root, "site.txt", &moved_text, "Move the camp")?;
        let moved = oid(&root, "HEAD")?;
        record(&root, "site.txt", &ring_text, "Build the fire ring")?;
        let ring = oid(&root, "HEAD")?;
        let logged = listed_oids(&root, &["--", "site.txt"])?;
        if logged != [ring.clone(), moved.clone(), pitch.clone()] {
            return Err(format!("git log --follow lists {logged:?} for site.txt").into());
        }
        let dates = git(&root, &["log", "--follow", "--format=%as", "--", "site.txt"])?;
        let dates: Vec<&str> = dates.lines().collect();
        self.open(&root)?;
        self.until("History-trail open at its tip", |tab| {
            tab.ui.repo == "History-trail" && tab.ui.history.len() == 4 && tab.ui.selection == Some(Selection::Commit(ring.clone()))
        })
        .await?;
        self.open_file("site.txt").await?;
        let (mode, offered) = self.tab.read_with(self.cx, |tab, _| (tab.ui.diff_mode, file_history::offered_here(&tab.ui)))?;
        if !offered {
            return Err("The diff header of site.txt offers no History".into());
        }

        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        self.until("the file's history beside its newest diff", |tab| {
            revisions(tab) == [ring.clone(), moved.clone(), pitch.clone()]
                && in_history(tab, 0, &ring, "site.txt")
                && patch(tab, "+Fire ring")
        })
        .await?;
        let entries = self.tab.read_with(self.cx, |tab, _| {
            tab.ui
                .file_history
                .entries()
                .iter()
                .map(|entry| (entry.subject.clone(), entry.author.clone(), entry.date.clone(), entry.moved()))
                .collect::<Vec<_>>()
        })?;
        let expected = [
            ("Build the fire ring", None),
            ("Move the camp", Some("renamed from camp.txt".to_owned())),
            ("Pitch the camp", None),
        ];
        let read = entries.iter().zip(expected.iter()).zip(dates.iter()).all(|(((subject, author, date, moved), (want, renamed)), day)| {
            subject == want && author == "Cedar Contributor" && date == day && moved == renamed
        });
        if !read || entries.len() != 3 {
            return Err(format!("The history lists {entries:?}").into());
        }

        self.action(Action::DiffMode(DiffMode::Split))?;
        self.key("down")?;
        self.until("Down shows the next revision with the view still open", |tab| {
            in_history(tab, 1, &moved, "site.txt")
                && patch(tab, "+Base camp by the lake")
                && !tab.ui.diff.has("+Fire ring")
                && tab.ui.shown_mode() == DiffMode::Split
        })
        .await?;
        self.key("down")?;
        self.until("Down again shows the file being added under its old name", |tab| {
            in_history(tab, 2, &pitch, "camp.txt") && patch(tab, "+Base camp by the pines") && tab.ui.shown_mode() == DiffMode::Split
        })
        .await?;
        self.key("down")?;
        self.until("Down at the oldest entry stays there", |tab| in_history(tab, 2, &pitch, "camp.txt")).await?;

        self.action(Action::FileView(crate::file_view::Action::Show(crate::file_view::View::File)))?;
        self.until("the whole file at the oldest revision", |tab| in_history(tab, 2, &pitch, "camp.txt") && whole_file(tab, &pitched))
            .await?;
        self.key("up")?;
        self.until("Up shows the whole file as it was after the move", |tab| {
            in_history(tab, 1, &moved, "site.txt") && whole_file(tab, &moved_text)
        })
        .await?;
        self.action(Action::FileView(crate::file_view::Action::Show(crate::file_view::View::Diff)))?;
        self.action(Action::DiffMode(mode))?;
        self.until("the diff back in its own layout", |tab| {
            in_history(tab, 1, &moved, "site.txt") && tab.ui.diff.file.is_none() && tab.ui.diff_mode == mode
        })
        .await?;

        let entry = Target::Revision { oid: moved.clone(), path: "site.txt".into() };
        let rows = self.menu_rows(&entry)?;
        let ids: Vec<&str> = rows.iter().map(|row| row.0).collect();
        let titles: Vec<&str> = rows.iter().map(|row| row.1.as_str()).collect();
        if ids != ["revision.restore", "revision.show-in-graph", "revision.copy-sha"]
            || titles != ["Restore this version…", "Show in graph", "Copy SHA"]
            || rows.iter().any(|row| row.2.is_some())
        {
            return Err(format!("The entry menu lists {rows:?}").into());
        }
        let refs = git(&root, &["for-each-ref"])?;
        self.choose(&entry, "revision.restore")?;
        self.asked_about(&format!("Restore site.txt from {}?", short(&moved))).await?;
        self.key(CONFIRM)?;
        self.noticed(&format!("Restored site.txt from {}", short(&moved))).await?;
        if fs::read(root.join("site.txt"))? != git(&root, &["show", &format!("{moved}:site.txt")])?.into_bytes() {
            return Err("site.txt does not equal its version in Move the camp".into());
        }
        git_is(&root, &["status", "--porcelain=v1"], " M site.txt\n")?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        git_is(&root, &["for-each-ref"], &refs)?;
        self.until("the history still open on the restored entry", |tab| in_history(tab, 1, &moved, "site.txt")).await?;

        self.key("escape")?;
        self.until("Esc returns to the file the history was opened from", |tab| {
            !tab.ui.file_history.open()
                && view::centre(&tab.ui) == view::Centre::Diff
                && tab.ui.selection == Some(Selection::Commit(ring.clone()))
                && tab.ui.inspection.path.as_deref() == Some(Path::new("site.txt"))
                && patch(tab, "+Fire ring")
        })
        .await?;
        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        self.until("the history open again", |tab| revisions(tab).len() == 3 && in_history(tab, 0, &ring, "site.txt")).await?;
        self.choose(&Target::Revision { oid: pitch.clone(), path: "camp.txt".into() }, "revision.show-in-graph")?;
        self.until("the oldest entry shown in the graph", |tab| {
            !tab.ui.file_history.open()
                && view::centre(&tab.ui) == view::Centre::History
                && tab.ui.selection == Some(Selection::Commit(pitch.clone()))
        })
        .await?;
        git(&root, &["checkout", "--", "site.txt"])?;
        self.pass();
        Ok(())
    }

    async fn history_from_my_files(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0906");
        let root = init(dir, "Desk-history")?;
        record(&root, "camp.txt", "Base camp\n", "Pitch the camp")?;
        record(&root, "trail.txt", "Trail\n", "Mark the trail")?;
        record(&root, "camp.txt", "Base camp\nFire ring\n", "Build the fire ring")?;
        let head = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "ridge"])?;
        record(&root, "camp.txt", "Base camp\nFire ring\nRidge view\n", "Look over the ridge")?;
        let ridge = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        fs::write(root.join("camp.txt"), "Base camp\nFire ring\nKettle on\n")?;
        fs::write(root.join("loose.txt"), "Loose\n")?;
        let expected = listed_oids(&root, &["HEAD", "--", "camp.txt"])?;
        if expected.len() != 2 || expected[0] != head || expected.contains(&ridge) {
            return Err(format!("git log --follow HEAD lists {expected:?} for camp.txt").into());
        }
        self.open(&root)?;
        self.until("Desk-history open with its changes", |tab| {
            tab.ui.repo == "Desk-history" && files(tab, &[], &["camp.txt", "loose.txt"])
        })
        .await?;
        self.action(Action::Select(file("camp.txt", false)))?;
        self.until("the unstaged camp.txt open", |tab| patch(tab, "+Kettle on")).await?;
        let before = untouched(&root)?;

        let unstaged = Target::File { path: "camp.txt".into(), side: Side::Unstaged };
        let offered = self.menu_rows(&unstaged)?.into_iter().find(|row| row.0 == "file.history");
        if offered != Some(("file.history", "File history".to_owned(), None)) {
            return Err(format!("The unstaged camp.txt offers {offered:?}").into());
        }
        let loose = Target::File { path: "loose.txt".into(), side: Side::Unstaged };
        let refused = self.menu_rows(&loose)?.into_iter().find(|row| row.0 == "file.history").and_then(|row| row.2);
        if refused.as_deref() != Some("This file has never been committed") {
            return Err(format!("File history on the untracked loose.txt reads {refused:?}").into());
        }
        self.choose(&unstaged, "file.history")?;
        self.until("the history of camp.txt starting at HEAD", |tab| {
            tab.ui.file_history.start() == Some(head.as_str())
                && revisions(tab) == expected
                && in_history(tab, 0, &head, "camp.txt")
                && patch(tab, "+Fire ring")
                && !tab.ui.diff.has("+Kettle on")
        })
        .await?;
        self.key("escape")?;
        self.until("Esc returns to my unstaged camp.txt", |tab| {
            !tab.ui.file_history.open() && tab.ui.selection == Some(file("camp.txt", false)) && patch(tab, "+Kettle on")
        })
        .await?;
        if untouched(&root)? != before {
            return Err("Looking at the history of camp.txt changed a ref, the index or a file".into());
        }
        self.action(Action::Select(file("loose.txt", false)))?;
        self.until("the untracked loose.txt open", |tab| tab.ui.selection == Some(file("loose.txt", false)) && patch(tab, "+Loose"))
            .await?;
        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        let read = self.tab.read_with(self.cx, |tab, _| (tab.ui.file_history.open(), file_history::refused_here(&tab.ui)))?;
        if read != (false, Some("This file has never been committed".to_owned())) {
            return Err(format!("History on the open untracked loose.txt reads {read:?}").into());
        }

        git(&root, &["mv", "camp.txt", "base.txt"])?;
        self.action(Action::Refresh)?;
        self.until("the staged rename listed", |tab| {
            files(tab, &["base.txt"], &["base.txt", "loose.txt"])
                && tab.ui.files.iter().any(|row| row.staged && row.from.as_deref() == Some(Path::new("camp.txt")))
        })
        .await?;
        self.action(Action::Select(file("base.txt", true)))?;
        self.until("the staged base.txt open", |tab| tab.ui.selection == Some(file("base.txt", true)) && !tab.ui.loading_diff())
            .await?;
        let before = untouched(&root)?;
        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        self.until("the staged rename's history follows its old name from HEAD", |tab| {
            tab.ui.file_history.path() == Some(Path::new("base.txt"))
                && tab.ui.file_history.start() == Some(head.as_str())
                && revisions(tab) == expected
                && tab.ui.file_history.entries().iter().all(|entry| entry.path == Path::new("camp.txt"))
                && in_history(tab, 0, &head, "camp.txt")
        })
        .await?;
        self.key("escape")?;
        self.until("Esc returns to the staged base.txt", |tab| {
            !tab.ui.file_history.open() && tab.ui.selection == Some(file("base.txt", true))
        })
        .await?;
        if untouched(&root)? != before {
            return Err("Looking at the history of base.txt changed a ref, the index or a file".into());
        }

        git(&root, &["stash", "push", "-q"])?;
        let saved = oid(&root, "refs/stash")?;
        self.action(Action::Refresh)?;
        self.until("the stash listed", |tab| tab.ui.stash.entries.iter().any(|entry| entry.oid == saved)).await?;
        self.action(Action::Select(Selection::Stash(saved.clone())))?;
        self.until("the stash's renamed base.txt listed", |tab| {
            tab.ui.selection == Some(Selection::Stash(saved.clone()))
                && tab.ui.stash.from(&saved, Path::new("base.txt")).as_deref() == Some(Path::new("camp.txt"))
        })
        .await?;
        self.action(Action::Stash(crate::stash::Action::OpenFile("base.txt".into())))?;
        self.until("the stashed base.txt open", |tab| {
            tab.ui.diff_open && tab.ui.stash.open_path() == Some(Path::new("base.txt")) && patch(tab, "+Kettle on")
        })
        .await?;
        let before = untouched(&root)?;
        self.action(Action::FileHistory(file_history::Action::OpenHere))?;
        self.until("the stashed rename's history follows its old name from where it was stashed", |tab| {
            tab.ui.file_history.path() == Some(Path::new("base.txt"))
                && tab.ui.file_history.start() == Some(head.as_str())
                && revisions(tab) == expected
                && in_history(tab, 0, &head, "camp.txt")
        })
        .await?;
        self.key("escape")?;
        self.until("Esc returns to the stashed base.txt", |tab| {
            !tab.ui.file_history.open()
                && tab.ui.selection == Some(Selection::Stash(saved.clone()))
                && tab.ui.diff_open
                && tab.ui.stash.open_path() == Some(Path::new("base.txt"))
                && patch(tab, "+Kettle on")
        })
        .await?;
        if untouched(&root)? != before {
            return Err("Looking at the history of the stashed base.txt changed a ref, the index or a file".into());
        }
        self.pass();
        Ok(())
    }
}
