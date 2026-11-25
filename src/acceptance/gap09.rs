use super::*;
use crate::{
    commands::{self, Target},
    compare::{self, Rev},
    picked::How,
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

impl Replay<'_> {
    pub(super) async fn gap09(&mut self, dir: &Path) -> Result<()> {
        self.compare_two_commits(dir).await?;
        self.compare_since_they_diverged(dir).await?;
        self.compare_with_my_files(dir).await?;
        self.combine_a_run(dir).await
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
        self.pass();
        Ok(())
    }
}
