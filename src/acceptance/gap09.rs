use super::*;
use crate::{
    blame,
    commands::{self, Side, Target},
    compare::{self, Rev},
    file_history,
    file_list::{self, Mode, Panel, Row, Which},
    picked::How,
    view::DiffMode,
};
use gpui::Focusable;

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

fn porcelain(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    let mut command = vec!["blame", "--porcelain"];
    command.extend_from_slice(args);
    Ok(git(root, &command)?
        .lines()
        .filter(|line| {
            let mut fields = line.split(' ');
            let first = fields.next().unwrap_or_default();
            first.len() == 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) && fields.count() >= 2
        })
        .map(|line| line[..40].to_owned())
        .collect())
}

fn annotated(tab: &Tab) -> Vec<String> {
    let Some(notes) = tab.ui.blame.notes() else { return Vec::new() };
    (0..tab.ui.diff.lines.len()).filter_map(|line| notes.commit_of(line).map(|commit| commit.oid.clone())).collect()
}

fn blaming(tab: &Tab, path: &str, resolved: Option<&str>, lines: &[String]) -> bool {
    view::centre(&tab.ui) == view::Centre::Blame
        && tab.ui.blame.settled()
        && tab.ui.blame.failed().is_none()
        && tab.ui.blame.blamed().is_some_and(|blamed| blamed.path == Path::new(path))
        && tab.ui.blame.resolved() == resolved
        && tab.ui.diff.lines.len() == lines.len()
        && annotated(tab) == lines
}

fn drawn(tab: &Tab, which: Which) -> Vec<String> {
    let files = file_list::source(&tab.ui, which);
    file_list::rows(&tab.ui, which)
        .iter()
        .map(|row| match row {
            Row::File { index, depth } => format!("{}{}", "  ".repeat(*depth), files[*index].path.display()),
            Row::Folder { name, depth, count, folded, .. } => {
                format!("{}{} {name}/ ({count})", "  ".repeat(*depth), if *folded { "+" } else { "-" })
            }
        })
        .collect()
}

fn owned(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|row| (*row).to_owned()).collect()
}

fn chosen(tab: &Tab) -> Option<(String, PathBuf, Option<(String, PathBuf)>)> {
    let (block, commit) = tab.ui.blame.chosen()?;
    Some((commit.oid.clone(), block.path.clone(), block.previous.clone()))
}

impl Replay<'_> {
    pub(super) async fn gap09(&mut self, dir: &Path) -> Result<()> {
        self.compare_two_commits(dir).await?;
        self.compare_since_they_diverged(dir).await?;
        self.compare_with_my_files(dir).await?;
        self.combine_a_run(dir).await?;
        self.step_through_history(dir).await?;
        self.history_from_my_files(dir).await?;
        self.blame_a_file(dir).await?;
        self.blame_before_a_change(dir).await?;
        self.tree_stages_a_folder(dir).await?;
        self.filter_a_commits_files(dir).await
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
        git(&root, &["rm", "-q", "trail.txt"])?;
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
        let gone = Target::File { path: "trail.txt".into(), side: Side::Compare(Rev::Commit(lake.clone())) };
        let blame = self.menu_rows(&gone)?.into_iter().find(|row| row.0 == "file.blame").and_then(|row| row.2);
        if blame.as_deref() != Some("This file is deleted on the newer side, so it has no lines to blame") {
            return Err(format!("Blame on the deleted trail.txt reads {blame:?}").into());
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

    async fn blame_a_file(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0907");
        let root = init(dir, "Blame-trail")?;
        let pitched = "Base camp by the pines\nWater from the creek\nTents in a row\nFood in the tree\nMap on the board\nLamp by the door\n";
        record(&root, "camp.txt", pitched, "Pitch the camp")?;
        let pitch = oid(&root, "HEAD")?;
        let bridged = pitched.replace("Tents in a row", "Tents by the bridge");
        record(&root, "camp.txt", &bridged, "Move the tents")?;
        let tents = oid(&root, "HEAD")?;
        let edited = bridged.replace("Map on the board", "Map on the wall");
        fs::write(root.join("camp.txt"), &edited)?;
        fs::write(root.join("loose.txt"), "Loose\n")?;
        let uncommitted = "0".repeat(40);
        let working = porcelain(&root, &["--", "camp.txt"])?;
        let wanted = [&pitch, &pitch, &tents, &pitch, &uncommitted, &pitch].map(|oid| oid.to_owned());
        if working != wanted {
            return Err(format!("git blame lists {working:?} for camp.txt").into());
        }
        self.open(&root)?;
        self.until("Blame-trail open with its changes", |tab| {
            tab.ui.repo == "Blame-trail" && files(tab, &[], &["camp.txt", "loose.txt"])
        })
        .await?;
        let before = untouched(&root)?;
        let unstaged = Target::File { path: "camp.txt".into(), side: Side::Unstaged };
        let offered = self.menu_rows(&unstaged)?.into_iter().find(|row| row.0 == "file.blame");
        if offered != Some(("file.blame", "Blame".to_owned(), None)) {
            return Err(format!("The unstaged camp.txt offers {offered:?}").into());
        }
        let loose = Target::File { path: "loose.txt".into(), side: Side::Unstaged };
        let refused = self.menu_rows(&loose)?.into_iter().find(|row| row.0 == "file.blame").and_then(|row| row.2);
        if refused.as_deref() != Some("This file has never been committed") {
            return Err(format!("Blame on the untracked loose.txt reads {refused:?}").into());
        }
        self.action(Action::Select(file("camp.txt", false)))?;
        self.until("the unstaged camp.txt open", |tab| patch(tab, "+Map on the wall")).await?;
        let offered = self.tab.read_with(self.cx, |tab, _| (blame::offered_here(&tab.ui), blame::refused_here(&tab.ui)))?;
        if offered != (true, None) {
            return Err(format!("The diff header of camp.txt offers Blame as {offered:?}").into());
        }

        self.action(Action::Blame(blame::Action::OpenHere))?;
        self.until("every line of my camp.txt blamed over the graph", |tab| {
            blaming(tab, "camp.txt", None, &working) && whole_file(tab, &edited)
        })
        .await?;
        let read = self.tab.read_with(self.cx, |tab, _| {
            let labels: Vec<Option<String>> = (0..6).map(|line| blame::label(&tab.ui, line)).collect();
            let notes = tab.ui.blame.notes();
            let tip = notes.and_then(|notes| notes.commit_of(2)).map(|commit| blame::tooltip(commit));
            let time = notes.and_then(|notes| notes.commit_of(2)).map(|commit| commit.time).unwrap_or_default();
            (labels, tip, view::exact_time(time), blame::title(&tab.ui), blame::status(&tab.ui))
        })?;
        let (labels, tip, time, title, status) = read;
        let names = |line: usize| labels[line].as_deref().is_some_and(|label| label.ends_with("  Cedar Contributor"));
        if !(names(0) && names(2) && names(3) && names(5))
            || labels[1].is_some()
            || labels[4].as_deref() != Some(blame::NOT_COMMITTED)
        {
            return Err(format!("The blame column reads {labels:?}").into());
        }
        let hover = format!("Move the tents\nCedar Contributor · {time}\n{}", short(&tents));
        if tip.as_deref() != Some(hover.as_str()) {
            return Err(format!("Hovering the third line shows {tip:?}").into());
        }
        if title != Some(("camp.txt".to_owned(), "Working copy".to_owned())) || status.as_deref() != Some(blame::HINT) {
            return Err(format!("The blame view is headed {title:?} with {status:?}").into());
        }
        self.action(Action::Blame(blame::Action::Choose(2)))?;
        self.until("the third line's commit in the strip", |tab| {
            chosen(tab) == Some((tents.clone(), "camp.txt".into(), Some((pitch.clone(), "camp.txt".into()))))
        })
        .await?;
        self.action(Action::Blame(blame::Action::Choose(4)))?;
        self.until("the edited line not committed, with HEAD before it", |tab| {
            chosen(tab) == Some((uncommitted.clone(), "camp.txt".into(), Some((tents.clone(), "camp.txt".into()))))
        })
        .await?;
        self.key("escape")?;
        self.until("Esc returns to my unstaged camp.txt", |tab| {
            !tab.ui.blame.open()
                && view::centre(&tab.ui) == view::Centre::Diff
                && tab.ui.selection == Some(file("camp.txt", false))
                && patch(tab, "+Map on the wall")
        })
        .await?;

        self.action(Action::Select(Selection::Commit(tents.clone())))?;
        self.until("Move the tents selected", |tab| tab.ui.selection == Some(Selection::Commit(tents.clone()))).await?;
        self.open_file("camp.txt").await?;
        let committed = porcelain(&root, &[&tents, "--", "camp.txt"])?;
        self.choose(&Target::File { path: "camp.txt".into(), side: Side::Commit(tents.clone()) }, "file.blame")?;
        self.until("camp.txt blamed at Move the tents", |tab| {
            blaming(tab, "camp.txt", Some(&tents), &committed) && whole_file(tab, &bridged)
        })
        .await?;
        let title = self.tab.read_with(self.cx, |tab, _| blame::title(&tab.ui))?;
        if title != Some(("camp.txt".to_owned(), format!("At {}", short(&tents)))) {
            return Err(format!("The blame of Move the tents is headed {title:?}").into());
        }
        if committed.iter().filter(|oid| **oid == tents).count() != 1 || committed.len() != 6 || committed.contains(&uncommitted) {
            return Err(format!("git blame lists {committed:?} at Move the tents").into());
        }
        self.action(Action::Blame(blame::Action::Close))?;
        self.until("the committed camp.txt open again", |tab| {
            !tab.ui.blame.open()
                && view::centre(&tab.ui) == view::Centre::Diff
                && tab.ui.inspection.path.as_deref() == Some(Path::new("camp.txt"))
                && patch(tab, "+Tents by the bridge")
        })
        .await?;
        if untouched(&root)? != before {
            return Err("Blaming camp.txt changed a ref, the index or a file".into());
        }
        self.pass();
        Ok(())
    }

    async fn blame_before_a_change(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0908");
        let root = init(dir, "Blame-before")?;
        let rest = "Water from the creek\nTents in a row\nFood in the tree\nMap on the board\n";
        let pitched = format!("Base camp by the pines\n{rest}Lamp by the door\n");
        record(&root, "camp.txt", &pitched, "Pitch the camp")?;
        let pitch = oid(&root, "HEAD")?;
        git(&root, &["mv", "camp.txt", "site.txt"])?;
        let moved_text = format!("Base camp by the lake\n{rest}Lamp by the door\n");
        record(&root, "site.txt", &moved_text, "Move the camp")?;
        let moved = oid(&root, "HEAD")?;
        record(&root, "site.txt", &format!("Base camp by the lake\n{rest}Lantern by the door\n"), "Hang the lantern")?;
        let lantern = oid(&root, "HEAD")?;
        let newest = porcelain(&root, &[&lantern, "--", "site.txt"])?;
        let oldest = porcelain(&root, &[&pitch, "--", "camp.txt"])?;
        if newest != [&moved, &pitch, &pitch, &pitch, &pitch, &lantern].map(|oid| oid.to_owned()) || oldest != vec![pitch.clone(); 6] {
            return Err(format!("git blame lists {newest:?} at the tip and {oldest:?} before the move").into());
        }
        self.open(&root)?;
        self.until("Blame-before open at its tip", |tab| {
            tab.ui.repo == "Blame-before" && tab.ui.selection == Some(Selection::Commit(lantern.clone()))
        })
        .await?;
        let before = untouched(&root)?;
        self.open_file("site.txt").await?;
        self.action(Action::Blame(blame::Action::OpenHere))?;
        self.until("site.txt blamed at its tip", |tab| blaming(tab, "site.txt", Some(&lantern), &newest)).await?;
        self.action(Action::Blame(blame::Action::Choose(0)))?;
        self.until("the moved line's commit chosen, with its old name before it", |tab| {
            chosen(tab) == Some((moved.clone(), "site.txt".into(), Some((pitch.clone(), "camp.txt".into()))))
        })
        .await?;

        self.action(Action::Blame(blame::Action::Before))?;
        self.until("the blame recomputed at the parent under the old name", |tab| {
            blaming(tab, "camp.txt", Some(&pitch), &oldest)
                && whole_file(tab, &pitched)
                && tab.ui.blame.depth() == 2
                && chosen(tab).is_none()
        })
        .await?;
        let title = self.tab.read_with(self.cx, |tab, _| blame::title(&tab.ui))?;
        if title != Some(("camp.txt".to_owned(), format!("At {}", short(&pitch)))) {
            return Err(format!("The earlier blame is headed {title:?}").into());
        }
        self.action(Action::Blame(blame::Action::Choose(0)))?;
        self.action(Action::Blame(blame::Action::Before))?;
        self.until("the first commit has nothing before it", |tab| {
            chosen(tab) == Some((pitch.clone(), "camp.txt".into(), None))
                && tab.ui.blame.depth() == 2
                && blaming(tab, "camp.txt", Some(&pitch), &oldest)
        })
        .await?;

        self.key("escape")?;
        self.until("Esc returns to the blame at the tip", |tab| {
            blaming(tab, "site.txt", Some(&lantern), &newest) && tab.ui.blame.depth() == 1
        })
        .await?;
        self.action(Action::Blame(blame::Action::Choose(5)))?;
        self.action(Action::Blame(blame::Action::ShowInGraph))?;
        self.until("the lantern's commit shown in the graph", |tab| {
            !tab.ui.blame.open()
                && view::centre(&tab.ui) == view::Centre::History
                && tab.ui.selection == Some(Selection::Commit(lantern.clone()))
        })
        .await?;
        self.open_file("site.txt").await?;
        self.action(Action::Blame(blame::Action::OpenHere))?;
        self.until("site.txt blamed again", |tab| blaming(tab, "site.txt", Some(&lantern), &newest)).await?;
        self.key("escape")?;
        self.until("Esc from the first blame returns to the file", |tab| {
            !tab.ui.blame.open()
                && view::centre(&tab.ui) == view::Centre::Diff
                && tab.ui.inspection.path.as_deref() == Some(Path::new("site.txt"))
                && patch(tab, "+Lantern by the door")
        })
        .await?;
        if untouched(&root)? != before {
            return Err("Blaming site.txt changed a ref, the index or a file".into());
        }
        self.pass();
        Ok(())
    }

    fn ticked(&mut self, target: &Target) -> Result<Vec<(&'static str, Option<bool>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx).into_iter().map(|entry| (entry.id, entry.check)).collect()
        })?)
    }

    async fn run_entry(&mut self, target: &Target, id: &str, title: &str) -> Result<()> {
        let (found, action) = self.entry_action(target, id)?;
        if found != title {
            return Err(format!("{id} on {target:?} reads {found:?}").into());
        }
        self.action(action)
    }

    async fn tree_stages_a_folder(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0909");
        let root = init(dir, "Tree-camp")?;
        let committed = ["src/app/models/user.rs", "src/app/view.rs", "src/lib.rs", "src/application.rs", "docs/intro.md"];
        for path in committed {
            fs::create_dir_all(root.join(path).parent().unwrap_or(&root))?;
            fs::write(root.join(path), "first\n")?;
        }
        git(&root, &["add", "."])?;
        git(&root, &["commit", "-qm", "Lay out the camp"])?;
        for path in committed {
            fs::write(root.join(path), "second\n")?;
        }
        fs::write(root.join("src/app/models/team.rs"), "new\n")?;
        let refs_before = git(&root, &["for-each-ref"])?;
        let mut all = committed.to_vec();
        all.push("src/app/models/team.rs");
        self.open(&root)?;
        self.until("Tree-camp open with six unstaged files", |tab| tab.ui.repo == "Tree-camp" && files(tab, &[], &all)).await?;
        self.action(Action::Select(Selection::Changes))?;
        self.until("the working tree selected", |tab| tab.ui.selection == Some(Selection::Changes)).await?;

        let working = Target::FileList(Panel::Working);
        let ticks = self.ticked(&working)?;
        let wanted = vec![
            ("files.show-list", Some(true)),
            ("files.show-tree", Some(false)),
            ("files.sort-path", Some(true)),
            ("files.sort-name", Some(false)),
            ("files.sort-status", Some(false)),
            ("files.filter", None),
        ];
        if ticks != wanted {
            return Err(format!("The file list options read {ticks:?}").into());
        }
        self.run_entry(&working, "files.show-tree", "Show files as a tree").await?;
        let tree = owned(&[
            "- docs/ (1)",
            "  docs/intro.md",
            "- src/ (5)",
            "  - app/ (3)",
            "    - models/ (2)",
            "      src/app/models/team.rs",
            "      src/app/models/user.rs",
            "    src/app/view.rs",
            "  src/application.rs",
            "  src/lib.rs",
        ]);
        self.until("the unstaged files as a tree", |tab| drawn(tab, Which::Unstaged) == tree).await?;
        let remembered = self.tab.read_with(self.cx, |tab, cx| (crate::settings::settings(cx).file_list_mode, tab.ui.file_list.mode))?;
        if remembered != (Mode::Tree, Mode::Tree) {
            return Err(format!("After choosing the tree the settings and the list read {remembered:?}").into());
        }

        let app = PathBuf::from("src/app");
        self.action(Action::FileList(file_list::Action::Fold { which: Which::Unstaged, path: app.clone() }))?;
        let folded = owned(&["- docs/ (1)", "  docs/intro.md", "- src/ (5)", "  + app/ (3)", "  src/application.rs", "  src/lib.rs"]);
        self.until("src/app folded", |tab| drawn(tab, Which::Unstaged) == folded).await?;
        let folder = Target::Folder { path: app.clone(), side: Side::Unstaged };
        let offered: Vec<&str> = self.ticked(&folder)?.into_iter().map(|(id, _)| id).collect();
        if offered != ["folder.stage", "folder.discard", "folder.ignore", "folder.stash", "folder.copy-path"] {
            return Err(format!("The menu on src/app offers {offered:?}").into());
        }
        self.run_entry(&folder, "folder.stage", "Stage folder").await?;
        let staged = ["src/app/models/team.rs", "src/app/models/user.rs", "src/app/view.rs"];
        let left = ["docs/intro.md", "src/application.rs", "src/lib.rs"];
        self.until("exactly the files under src/app staged", |tab| files(tab, &staged, &left)).await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "src/app/models/team.rs\nsrc/app/models/user.rs\nsrc/app/view.rs\n")?;
        let lists = self.tab.read_with(self.cx, |tab, _| (drawn(tab, Which::Staged), drawn(tab, Which::Unstaged)))?;
        let wanted = (
            owned(&["- src/app/ (3)", "  - models/ (2)", "    src/app/models/team.rs", "    src/app/models/user.rs", "  src/app/view.rs"]),
            owned(&["- docs/ (1)", "  docs/intro.md", "- src/ (2)", "  src/application.rs", "  src/lib.rs"]),
        );
        if lists != wanted {
            return Err(format!("After staging src/app the staged and unstaged trees read {lists:?}").into());
        }

        let models = Target::Folder { path: PathBuf::from("src/app/models"), side: Side::Staged };
        let offered: Vec<&str> = self.ticked(&models)?.into_iter().map(|(id, _)| id).collect();
        if offered != ["folder.unstage", "folder.stash", "folder.copy-path"] {
            return Err(format!("The menu on the staged models folder offers {offered:?}").into());
        }
        self.run_entry(&models, "folder.unstage", "Unstage folder").await?;
        let unstaged = ["docs/intro.md", "src/application.rs", "src/lib.rs", "src/app/models/team.rs", "src/app/models/user.rs"];
        self.until("the models folder unstaged", |tab| files(tab, &["src/app/view.rs"], &unstaged)).await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "src/app/view.rs\n")?;
        let docs = Target::Folder { path: PathBuf::from("docs"), side: Side::Unstaged };
        self.run_entry(&docs, "folder.discard", "Discard folder…").await?;
        let (_, danger) = self.asked_about("Discard changes to intro.md?").await?;
        if !danger {
            return Err("Discarding the one-file docs folder asks without the danger style".into());
        }
        self.key("escape")?;
        self.until("the discard prompt cancelled", |tab| !tab.ui.prompt.open()).await?;
        if fs::read_to_string(root.join("docs/intro.md"))? != "second\n" {
            return Err("Cancelling the folder discard changed docs/intro.md".into());
        }

        self.run_entry(&working, "files.collapse-all", "Collapse all folders").await?;
        self.until("every folder folded", |tab| {
            drawn(tab, Which::Unstaged) == owned(&["+ docs/ (1)", "+ src/ (4)"]) && drawn(tab, Which::Staged) == owned(&["+ src/app/ (1)"])
        })
        .await?;
        self.run_entry(&working, "files.expand-all", "Expand all folders").await?;
        let open = owned(&[
            "- docs/ (1)",
            "  docs/intro.md",
            "- src/ (4)",
            "  - app/models/ (2)",
            "    src/app/models/team.rs",
            "    src/app/models/user.rs",
            "  src/application.rs",
            "  src/lib.rs",
        ]);
        self.until("every folder open again", |tab| {
            drawn(tab, Which::Unstaged) == open && drawn(tab, Which::Staged) == owned(&["- src/app/ (1)", "  src/app/view.rs"])
        })
        .await?;

        self.run_entry(&working, "files.show-list", "Show files as a list").await?;
        let flat = owned(&["docs/intro.md", "src/app/models/team.rs", "src/app/models/user.rs", "src/application.rs", "src/lib.rs"]);
        self.until("the lists flat again", |tab| drawn(tab, Which::Unstaged) == flat).await?;
        let remembered = self.tab.read_with(self.cx, |_, cx| crate::settings::settings(cx).file_list_mode)?;
        if remembered != Mode::List {
            return Err(format!("After choosing the list the settings read {remembered:?}").into());
        }
        if git(&root, &["for-each-ref"])? != refs_before {
            return Err("Staging a folder changed a ref".into());
        }
        self.pass();
        Ok(())
    }

    fn type_filter(&mut self, panel: Panel, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, _, cx| tab.ui.file_list.input(panel).clone().update(cx, |field, cx| field.set_text(text, cx)))?;
        Ok(())
    }

    fn filter_focused(&mut self, panel: Panel) -> Result<bool> {
        Ok(self.tab.update(self.cx, |tab, window, cx| tab.ui.file_list.input(panel).focus_handle(cx).is_focused(window))?)
    }

    async fn filter_a_commits_files(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0910");
        let root = init(dir, "Filter-camp")?;
        for folder in ["camp", "trail", "lake", "notes"] {
            fs::create_dir_all(root.join(folder))?;
        }
        fs::write(root.join("README.md"), "Camp\n")?;
        fs::write(root.join("lake/map.txt"), "Lake\n")?;
        git(&root, &["add", "."])?;
        git(&root, &["commit", "-qm", "Pitch the camp"])?;
        let pitch = oid(&root, "HEAD")?;
        for n in 1..=8 {
            fs::write(root.join(format!("camp/tent-{n:02}.txt")), format!("Tent {n}\n"))?;
        }
        for n in 1..=6 {
            fs::write(root.join(format!("trail/marker-{n:02}.txt")), format!("Marker {n}\n"))?;
        }
        fs::write(root.join("lake/map.txt"), "Lake with a dock\n")?;
        fs::write(root.join("lake/dock.txt"), "Dock\n")?;
        fs::write(root.join("README.md"), "Camp, stocked\n")?;
        fs::write(root.join("notes/camp-log.md"), "Day one\n")?;
        git(&root, &["add", "-A"])?;
        git(&root, &["commit", "-qm", "Stock the camp"])?;
        let stock = oid(&root, "HEAD")?;
        let changed: Vec<String> =
            git(&root, &["diff-tree", "--no-commit-id", "--name-only", "-r", &stock])?.lines().map(str::to_owned).collect();
        if changed.len() != 18 {
            return Err(format!("Git lists {changed:?} in Stock the camp").into());
        }
        let before = untouched(&root)?;
        self.open(&root)?;
        self.until("Filter-camp open with two commits", |tab| tab.ui.repo == "Filter-camp" && tab.ui.history.len() == 2).await?;

        self.action(Action::Select(Selection::Commit(pitch.clone())))?;
        self.until("Pitch the camp's two files", |tab| {
            tab.ui.selection == Some(Selection::Commit(pitch.clone()))
                && tab.ui.inspection.file(&pitch, Path::new("README.md")).is_some()
                && tab.ui.inspection.files.len() == 2
        })
        .await?;
        self.focus_desk()?;
        self.key("right")?;
        self.until("the keyboard in the file list", |tab| file_list::here(&tab.ui) == Some(Panel::Commit)).await?;
        let read = self.tab.read_with(self.cx, |tab, _| (file_list::field_shown(&tab.ui, Panel::Commit), crate::find::title(&tab.ui)))?;
        if read != (false, "Filter files…") {
            return Err(format!("With two files (filter field shown, Cmd-F command) reads {read:?}").into());
        }
        self.key("cmd-f")?;
        self.until("the filter field shown", |tab| file_list::field_shown(&tab.ui, Panel::Commit)).await?;
        if !self.filter_focused(Panel::Commit)? {
            return Err("Cmd-F showed the filter field without putting the cursor in it".into());
        }
        self.key("escape")?;
        self.until("the filter field gone", |tab| !file_list::field_shown(&tab.ui, Panel::Commit)).await?;

        self.action(Action::Select(Selection::Commit(stock.clone())))?;
        self.until("Stock the camp's eighteen files", |tab| {
            tab.ui.selection == Some(Selection::Commit(stock.clone()))
                && tab.ui.inspection.file(&stock, Path::new("README.md")).is_some()
                && tab.ui.inspection.files.len() == 18
        })
        .await?;
        if !self.tab.read_with(self.cx, |tab, _| file_list::field_shown(&tab.ui, Panel::Commit))? {
            return Err("A commit with eighteen files shows no filter field".into());
        }
        let matching = |needle: &str| -> Vec<PathBuf> {
            let mut kept: Vec<PathBuf> =
                changed.iter().filter(|path| path.to_lowercase().contains(&needle.to_lowercase())).map(PathBuf::from).collect();
            kept.sort();
            kept
        };
        let camp = matching("camp");
        if camp.len() != 9 || !camp.contains(&PathBuf::from("notes/camp-log.md")) {
            return Err(format!("Git's paths with camp in them are {camp:?}").into());
        }
        self.type_filter(Panel::Commit, "camp")?;
        self.until("only the paths with camp in them", |tab| file_list::shown_paths(&tab.ui, Which::Commit) == camp).await?;
        self.type_filter(Panel::Commit, "TENT-03")?;
        self.until("only tent-03, whatever the case", |tab| file_list::shown_paths(&tab.ui, Which::Commit) == [PathBuf::from("camp/tent-03.txt")])
            .await?;
        self.type_filter(Panel::Commit, "nowhere")?;
        self.until("nothing listed and the list saying so", |tab| {
            file_list::shown_paths(&tab.ui, Which::Commit).is_empty() && file_list::nothing_matches(&tab.ui, Panel::Commit).is_some()
        })
        .await?;
        self.action(Action::FileList(file_list::Action::Show(Mode::Tree)))?;
        self.type_filter(Panel::Commit, "lake")?;
        let lake = owned(&["- lake/ (2)", "  lake/dock.txt", "  lake/map.txt"]);
        self.until("the lake folder and its two files", |tab| drawn(tab, Which::Commit) == lake).await?;
        self.action(Action::FileList(file_list::Action::Show(Mode::List)))?;
        self.tab.update(self.cx, |tab, window, cx| window.focus(&tab.ui.file_list.input(Panel::Commit).focus_handle(cx)))?;
        self.key("escape")?;
        let mut everything: Vec<PathBuf> = changed.iter().map(PathBuf::from).collect();
        everything.sort();
        self.until("all eighteen files again", |tab| {
            tab.ui.file_list.filter(Panel::Commit).is_empty() && file_list::shown_paths(&tab.ui, Which::Commit) == everything
        })
        .await?;
        if self.filter_focused(Panel::Commit)? {
            return Err("Esc left the cursor in the filter field".into());
        }
        if untouched(&root)? != before {
            return Err("Filtering a commit's files changed the repository".into());
        }
        self.pass();
        Ok(())
    }
}
