use super::*;
use crate::{
    commands::{self, Side, Target},
    view::commit_input,
    notice,
    picked::{self, How},
    prompt::{self, Stop},
    view::Tone,
};
use gpui::{ClipboardItem, Focusable};
use std::ops::Range;

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };

const SURVEY: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

const NAMES: [&str; 5] = ["alder.txt", "birch bark.txt", "cedar é.txt", "dogwood.txt", "elm.txt"];

fn changed_lines(tab: &Tab) -> Vec<String> {
    tab.ui
        .diff
        .iter()
        .map(|line| line.text.as_str())
        .filter(|text| (text.starts_with('+') || text.starts_with('-')) && !text.starts_with("+++") && !text.starts_with("---"))
        .map(str::to_owned)
        .collect()
}

fn renamed_row(tab: &Tab, path: &str, from: &str, staged: bool) -> bool {
    tab.ui.files.iter().any(|row| {
        row.staged == staged
            && row.path == Path::new(path)
            && row.status == "Renamed"
            && row.from.as_deref() == Some(Path::new(from))
    })
}

fn picked_paths(tab: &Tab) -> Option<Vec<String>> {
    picked::files(&tab.ui).map(|(paths, _)| paths.iter().map(|path| path.to_string_lossy().into_owned()).collect())
}

impl Replay<'_> {
    pub(super) async fn gap06(&mut self, dir: &Path) -> Result<()> {
        self.stage_a_range(dir).await?;
        self.discard_and_restore(dir).await?;
        self.discard_all(dir).await?;
        self.ignore_by_extension(dir).await?;
        self.ignore_tracked(dir).await?;
        self.staged_rename(dir).await?;
        self.history_across_a_rename(dir).await?;
        self.editing_the_summary(dir).await
    }

    async fn asked_about(&mut self, title: &str) -> Result<(Vec<String>, bool)> {
        self.until(&format!("the prompt {title:?} open"), |tab| {
            prompt::shown(&tab.ui).is_some_and(|spec| spec.title == title)
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| (spec.lines, spec.danger)))?;
        shown.ok_or_else(|| format!("The prompt {title:?} closed").into())
    }

    async fn noticed(&mut self, title: &str) -> Result<(String, Vec<String>)> {
        self.until(&format!("the notice {title:?}"), |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == title)
        })
        .await?;
        match self.said()? {
            Some((Tone::Success, _, detail, offers)) => Ok((detail, offers)),
            said => Err(format!("The notice {title:?} reads {said:?}").into()),
        }
    }

    fn header(&mut self) -> Result<String> {
        Ok(self.tab.read_with(self.cx, |tab, _| crate::discard::summary(&tab.ui))?)
    }

    fn titles(&mut self, target: Target) -> Result<Vec<String>> {
        Ok(self
            .tab
            .read_with(self.cx, |tab, cx| commands::menu(&tab.ui, &target, cx).into_iter().map(|entry| entry.title).collect())?)
    }

    async fn stage_a_range(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0601");
        let root = init(dir, "Range-ford")?;
        for name in NAMES {
            fs::write(root.join(name), "base\n")?;
        }
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Plant the grove"])?;
        let head = oid(&root, "HEAD")?;
        for name in NAMES {
            fs::write(root.join(name), format!("{name} grew\n"))?;
        }
        self.open(&root)?;
        self.until("Range-ford open with five unstaged files", |tab| {
            tab.ui.repo == "Range-ford" && files(tab, &[], &NAMES)
        })
        .await?;
        let worktree = || -> Result<()> {
            for name in NAMES {
                if fs::read_to_string(root.join(name))? != format!("{name} grew\n") {
                    return Err(format!("Staging changed {name} in the working tree").into());
                }
            }
            git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))
        };

        self.action(Action::Select(file(NAMES[1], false)))?;
        self.until("birch bark.txt selected", |tab| tab.ui.selection == Some(file(NAMES[1], false))).await?;
        let click = Action::Picked(picked::Action::Click { path: NAMES[3].into(), staged: false, how: How::Range });
        self.action(click)?;
        let range: Vec<String> = NAMES[1..4].iter().map(|name| name.to_string()).collect();
        self.until("three files picked by Shift-click", |tab| {
            picked_paths(tab).as_ref() == Some(&range) && tab.ui.selection == Some(file(NAMES[3], false))
        })
        .await?;
        let target = Target::Files { paths: NAMES[1..4].iter().map(PathBuf::from).collect(), side: Side::Unstaged };
        let titles = self.titles(target)?;
        if !titles.iter().any(|title| title == "Stage 3 files") || !titles.iter().any(|title| title == "Discard 3 files…") {
            return Err(format!("The menu of the picked rows reads {titles:?}").into());
        }

        self.key("s")?;
        self.until("exactly the three picked files staged by S", |tab| {
            files(tab, &NAMES[1..4], &[NAMES[0], NAMES[4]])
        })
        .await?;
        git_is(&root, &["-c", "core.quotePath=false", "diff", "--cached", "--name-only"], "birch bark.txt\ncedar é.txt\ndogwood.txt\n")?;
        git_is(&root, &["diff", "--name-only"], "alder.txt\nelm.txt\n")?;
        worktree()?;

        self.action(Action::Select(file(NAMES[1], true)))?;
        self.until("birch bark.txt selected under Staged", |tab| tab.ui.selection == Some(file(NAMES[1], true))).await?;
        self.key("cmd-a")?;
        self.until("Cmd-A picks every staged file", |tab| picked_paths(tab).as_ref() == Some(&range)).await?;
        self.action(Action::Select(file(NAMES[1], true)))?;
        self.until("a plain click picks one file again", |tab| picked_paths(tab).is_none()).await?;
        self.key("shift-down")?;
        self.key("shift-down")?;
        self.until("Shift-Down twice picks the staged range", |tab| picked_paths(tab).as_ref() == Some(&range)).await?;
        self.key("shift-up")?;
        let two: Vec<String> = NAMES[1..3].iter().map(|name| name.to_string()).collect();
        self.until("Shift-Up shrinks the range", |tab| picked_paths(tab).as_ref() == Some(&two)).await?;
        self.key("u")?;
        self.until("the two picked files unstaged by U", |tab| files(tab, &[NAMES[3]], &[NAMES[0], NAMES[1], NAMES[2], NAMES[4]]))
            .await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "dogwood.txt\n")?;
        worktree()?;
        self.pass();
        Ok(())
    }

    async fn discard_and_restore(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0602");
        let root = init(dir, "Spill-way")?;
        for name in ["chart.txt", "gauge.txt", "log book.txt", "weir.txt"] {
            fs::write(root.join(name), format!("{name} at the gauge\n"))?;
        }
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Survey the spillway"])?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("chart.txt"), "chart.txt staged\n")?;
        git(&root, &["add", "--", "chart.txt"])?;
        fs::write(root.join("chart.txt"), "chart.txt staged and redrawn\n")?;
        fs::write(root.join("gauge.txt"), "gauge.txt reads high\n")?;
        fs::write(root.join("weir.txt"), "weir.txt overflows\n")?;
        fs::remove_file(root.join("log book.txt"))?;
        fs::write(root.join("flow é.txt"), "flow é.txt rising\n")?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let before = git(&root, &["status", "--porcelain=v1", "-uall"])?;
        let picks = ["chart.txt", "gauge.txt", "log book.txt", "flow é.txt"];
        self.open(&root)?;
        self.until("Spill-way open with five unstaged changes and one staged", |tab| {
            tab.ui.repo == "Spill-way" && files(tab, &["chart.txt"], &["chart.txt", "gauge.txt", "log book.txt", "weir.txt", "flow é.txt"])
        })
        .await?;

        self.action(Action::Select(file(picks[0], false)))?;
        self.until("chart.txt selected", |tab| tab.ui.selection == Some(file(picks[0], false))).await?;
        for name in &picks[1..] {
            self.action(Action::Picked(picked::Action::Click { path: name.into(), staged: false, how: How::Toggle }))?;
        }
        let wanted: Vec<String> = picks.iter().map(|name| name.to_string()).collect();
        self.until("four files picked by Cmd-click", |tab| picked_paths(tab).as_ref() == Some(&wanted)).await?;
        let target = Target::Files { paths: picks.iter().map(PathBuf::from).collect(), side: Side::Unstaged };
        self.choose(&target, "file.discard")?;
        let (lines, danger) = self.asked_about("Discard changes to 4 files?").await?;
        if !danger || lines[..4] != wanted[..] || lines.last().map(String::as_str) != Some("Restore, in the notice that follows, brings them back.") {
            return Err(format!("The discard prompt shows {lines:?}, danger {danger}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The discard prompt did not open with Cancel focused".into());
        }
        self.key(CMD_RETURN)?;
        let (_, offers) = self.noticed("Discarded changes to 4 files").await?;
        if offers != ["Restore"] {
            return Err(format!("The discard notice offers {offers:?}").into());
        }
        for name in ["gauge.txt", "log book.txt"] {
            if fs::read_to_string(root.join(name))? != format!("{name} at the gauge\n") {
                return Err(format!("{name} does not match HEAD after the discard").into());
            }
        }
        if fs::read_to_string(root.join("chart.txt"))? != "chart.txt staged\n" {
            return Err("chart.txt lost its staged content".into());
        }
        if root.join("flow é.txt").exists() {
            return Err("The untracked flow é.txt is still there".into());
        }
        if fs::read_to_string(root.join("weir.txt"))? != "weir.txt overflows\n" {
            return Err("The unpicked weir.txt was discarded too".into());
        }
        git_is(&root, &["ls-files", "--stage"], &index)?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["stash", "list"], "")?;
        self.until("the list shows what is left", |tab| files(tab, &["chart.txt"], &["weir.txt"])).await?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.noticed("Restored changes to 4 files").await?;
        git_is(&root, &["status", "--porcelain=v1", "-uall"], &before)?;
        git_is(&root, &["ls-files", "--stage"], &index)?;
        for (name, text) in [
            ("chart.txt", "chart.txt staged and redrawn\n"),
            ("gauge.txt", "gauge.txt reads high\n"),
            ("flow é.txt", "flow é.txt rising\n"),
        ] {
            if fs::read_to_string(root.join(name))? != text {
                return Err(format!("Restore did not bring back {name}").into());
            }
        }
        if root.join("log book.txt").exists() {
            return Err("Restore did not delete log book.txt again".into());
        }
        self.until("the list shows every change again", |tab| {
            files(tab, &["chart.txt"], &["chart.txt", "gauge.txt", "log book.txt", "weir.txt", "flow é.txt"])
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn discard_all(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0603");
        let root = init(dir, "Clear-water")?;
        fs::write(root.join(".gitignore"), "*.log\n")?;
        fs::write(root.join("dam.txt"), "Dam: stone\n")?;
        fs::write(root.join("reed.txt"), "Reed: green\n")?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Build the dam"])?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("dam.txt"), "Dam: raised\n")?;
        git(&root, &["add", "--", "dam.txt"])?;
        fs::write(root.join("reed.txt"), "Reed: cut\n")?;
        fs::write(root.join("pool.txt"), "Pool: deep\n")?;
        fs::create_dir_all(root.join("build"))?;
        fs::write(root.join("build/side.txt"), "Side channel\n")?;
        fs::write(root.join("build/out.log"), "Built\n")?;
        fs::write(root.join("debug.log"), "Debugged\n")?;
        let before = git(&root, &["status", "--porcelain=v1", "-uall"])?;
        self.open(&root)?;
        self.until("Clear-water open with four changes", |tab| {
            tab.ui.repo == "Clear-water" && files(tab, &["dam.txt"], &["reed.txt", "pool.txt", "build/side.txt"])
        })
        .await?;
        let header = self.header()?;
        if header != "4 changes on main" {
            return Err(format!("The panel header reads {header:?}").into());
        }
        let offered = self.tab.read_with(self.cx, |tab, cx| {
            commands::find("repo.discard-all")
                .and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx))
                .map(|entry| (entry.title, entry.action.is_some()))
        })?;
        if offered != Some(("Discard all changes…".to_owned(), true)) {
            return Err(format!("The header's discard button runs {offered:?}").into());
        }

        self.action(Action::Run("repo.discard-all"))?;
        let (lines, danger) = self.asked_about("Discard all changes?").await?;
        let expected = "Changes to 2 files will be removed, staged ones included, and 2 untracked files will be deleted.";
        if !danger || lines.first().map(String::as_str) != Some(expected) {
            return Err(format!("The discard-all prompt shows {lines:?}, danger {danger}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The discard-all prompt did not open with Cancel focused".into());
        }
        self.key(CMD_RETURN)?;
        let (_, offers) = self.noticed("Discarded all changes").await?;
        if offers != ["Restore"] {
            return Err(format!("The discard-all notice offers {offers:?}").into());
        }
        git_is(&root, &["status", "--porcelain=v1", "-uall"], "")?;
        git_is(&root, &["stash", "list"], "")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        if fs::read_to_string(root.join("debug.log"))? != "Debugged\n" || fs::read_to_string(root.join("build/out.log"))? != "Built\n" {
            return Err("Discard all touched an ignored file".into());
        }
        self.until("the list is empty", |tab| tab.ui.files.is_empty()).await?;
        let header = self.header()?;
        if header != "No changes on main" {
            return Err(format!("After discarding, the panel header reads {header:?}").into());
        }

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.noticed("Restored the discarded changes").await?;
        git_is(&root, &["status", "--porcelain=v1", "-uall"], &before)?;
        git_is(&root, &["show", ":dam.txt"], "Dam: raised\n")?;
        self.pass();
        Ok(())
    }

    async fn ignore_by_extension(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0604");
        let root = init(dir, "Log-jam")?;
        fs::write(root.join(".gitignore"), "build/")?;
        fs::write(root.join("mill.txt"), "Mill: turning\n")?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Start the mill"])?;
        let head = oid(&root, "HEAD")?;
        fs::create_dir_all(root.join("logs"))?;
        fs::write(root.join("first.log"), "first\n")?;
        fs::write(root.join("logs/second.log"), "second\n")?;
        fs::write(root.join("notes.txt"), "notes\n")?;
        self.open(&root)?;
        self.until("Log-jam open with three untracked files", |tab| {
            tab.ui.repo == "Log-jam" && files(tab, &[], &["first.log", "logs/second.log", "notes.txt"])
        })
        .await?;
        let target = Target::File { path: "first.log".into(), side: Side::Unstaged };
        self.choose_under(&target, "file.ignore-menu", "Ignore all .log files")?;
        let (detail, _) = self.noticed("Ignored all .log files").await?;
        if detail != "Added *.log to .gitignore." {
            return Err(format!("The ignore notice says {detail:?}").into());
        }
        if fs::read_to_string(root.join(".gitignore"))? != "build/\n*.log\n" {
            return Err(format!(".gitignore reads {:?}", fs::read_to_string(root.join(".gitignore"))?).into());
        }
        self.until("the log files left the list", |tab| files(tab, &[], &[".gitignore", "notes.txt"])).await?;
        git_is(&root, &["check-ignore", "--", "first.log", "logs/second.log"], "first.log\nlogs/second.log\n")?;
        git_is(&root, &["ls-files"], ".gitignore\nmill.txt\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.pass();
        Ok(())
    }

    async fn ignore_tracked(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0605");
        let root = init(dir, "Key-stone")?;
        fs::write(root.join("app.local"), "port = 80\n")?;
        fs::write(root.join("readme.txt"), "Read me\n")?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Set the keystone"])?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("app.local"), "port = 8080\n")?;
        self.open(&root)?;
        self.until("Key-stone open with app.local edited", |tab| {
            tab.ui.repo == "Key-stone" && files(tab, &[], &["app.local"])
        })
        .await?;
        let target = Target::File { path: "app.local".into(), side: Side::Unstaged };
        self.choose_under(&target, "file.ignore-menu", "Ignore app.local")?;
        let (lines, _) = self.asked_about("Ignore and stop tracking app.local?").await?;
        if lines.first().map(String::as_str) != Some("/app.local is added to .gitignore.") {
            return Err(format!("The stop-tracking prompt shows {lines:?}").into());
        }
        git_is(&root, &["ls-files"], "app.local\nreadme.txt\n")?;
        self.key(CMD_RETURN)?;
        self.noticed("Ignored app.local").await?;
        git_is(&root, &["ls-files"], "readme.txt\n")?;
        git_is(&root, &["diff", "--cached", "--name-status"], "D\tapp.local\n")?;
        if fs::read_to_string(root.join("app.local"))? != "port = 8080\n" {
            return Err("app.local is no longer on disk with its edit".into());
        }
        if fs::read_to_string(root.join(".gitignore"))? != "/app.local\n" {
            return Err(format!(".gitignore reads {:?}", fs::read_to_string(root.join(".gitignore"))?).into());
        }
        git_is(&root, &["check-ignore", "--no-index", "--", "app.local"], "app.local\n")?;
        self.until("app.local listed as a staged deletion", |tab| files(tab, &["app.local"], &[".gitignore"])).await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.pass();
        Ok(())
    }

    async fn staged_rename(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0606");
        let root = init(dir, "Mill-race")?;
        let (old, new) = ("race notes.txt", "channels/race é.txt");
        fs::write(root.join(old), SURVEY)?;
        fs::write(root.join("wheel.txt"), "Wheel\n")?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Dig the race"])?;
        let head = oid(&root, "HEAD")?;
        fs::create_dir(root.join("channels"))?;
        git(&root, &["mv", "--", old, new])?;
        let edited = SURVEY.replace("four", "FOUR");
        fs::write(root.join(new), &edited)?;
        git(&root, &["add", "--", new])?;
        self.open(&root)?;
        self.until("Mill-race open with one staged row for the rename", |tab| {
            tab.ui.repo == "Mill-race" && files(tab, &[new], &[]) && renamed_row(tab, new, old, true)
        })
        .await?;
        let listed = git(&root, &["-c", "core.quotePath=false", "status", "--porcelain=v2", "-z"])?;
        if !listed.starts_with("2 R. ") || !listed.contains(&format!(" {new}\0{old}\0")) {
            return Err(format!("git status reads {listed:?}").into());
        }

        self.action(Action::Select(file(new, true)))?;
        self.until("the staged rename's diff shows only the edited lines", |tab| {
            patch(tab, "+FOUR") && changed_lines(tab) == ["-four", "+FOUR"]
        })
        .await?;
        let (from, hunks) = self.tab.read_with(self.cx, |tab, _| {
            let selection = tab.ui.selection.clone().unwrap_or(Selection::Changes);
            (tab.ui.renamed_from(&selection, None), tab.ui.diff.iter().filter(|line| line.hunk.is_some()).count())
        })?;
        if from.as_deref() != Some(Path::new(old)) {
            return Err(format!("The diff header names {from:?} as where the file came from").into());
        }
        if hunks != 0 {
            return Err(format!("The staged rename offers {hunks} hunk buttons").into());
        }

        self.key("u")?;
        self.until("U unstages both halves of the rename", |tab| files(tab, &[], &[old, new])).await?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        git_is(&root, &["ls-files"], &format!("{old}\nwheel.txt\n"))?;
        if fs::read_to_string(root.join(new))? != edited || root.join(old).exists() {
            return Err("Unstaging the rename changed the files on disk".into());
        }
        self.action(Action::StageAll)?;
        self.until("staging both halves pairs them again", |tab| {
            files(tab, &[new], &[]) && renamed_row(tab, new, old, true)
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.pass();
        Ok(())
    }

    async fn history_across_a_rename(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0607");
        let root = init(dir, "Field-book")?;
        let (old, new) = ("notes.txt", "docs/field é.md");
        record(&root, old, SURVEY, "Start the field book")?;
        let second = SURVEY.replace("two", "TWO");
        record(&root, old, &second, "Add the second line")?;
        let added = oid(&root, "HEAD")?;
        fs::create_dir(root.join("docs"))?;
        git(&root, &["mv", "--", old, new])?;
        record(&root, new, &second.replace("nine", "NINE"), "Move the field book")?;
        let moved = oid(&root, "HEAD")?;
        record(&root, new, &second.replace("nine", "NINE").replace("ten", "TEN"), "Fill in the moved book")?;
        let tip = oid(&root, "HEAD")?;
        let subjects = ["Fill in the moved book", "Move the field book", "Add the second line", "Start the field book"];
        git_is(&root, &["log", "--follow", "--format=%s", "--", new], &format!("{}\n", subjects.join("\n")))?;
        self.open(&root)?;
        self.until("Field-book open at its tip", |tab| {
            tab.ui.repo == "Field-book" && tab.ui.selection == Some(Selection::Commit(tip.clone()))
        })
        .await?;
        self.open_file(new).await?;
        self.action(Action::Inspect(inspection::Action::History))?;
        self.until("file history lists the commits from before the rename under the old path", |tab| {
            tab.ui.inspection.history.as_ref().is_some_and(|history| {
                history.iter().map(|row| (row.subject.as_str(), row.path.as_path())).eq([
                    (subjects[0], Path::new(new)),
                    (subjects[1], Path::new(new)),
                    (subjects[2], Path::new(old)),
                    (subjects[3], Path::new(old)),
                ])
            })
        })
        .await?;

        self.action(Action::Inspect(inspection::Action::OpenRevision { oid: added.clone(), path: old.into() }))?;
        self.until("the older revision opens under the old path", |tab| {
            tab.ui.selection == Some(Selection::Commit(added.clone()))
                && tab.ui.diff_open
                && tab.ui.inspection.path.as_deref() == Some(Path::new(old))
                && patch(tab, "+TWO")
                && changed_lines(tab) == ["-two", "+TWO"]
        })
        .await?;
        git_is(&root, &["show", &format!("{added}:{old}")], &second)?;

        self.select_commit(&moved).await?;
        self.until("the commit that moved the file lists one renamed row", |tab| {
            tab.ui.inspection.files.len() == 1
                && committed_file(tab, new, "Renamed")
                && tab.ui.inspection.files[0].from.as_deref() == Some(Path::new(old))
        })
        .await?;
        self.open_file(new).await?;
        self.until("the moved file's patch shows only the edited lines", |tab| {
            changed_lines(tab) == ["-nine", "+NINE"]
        })
        .await?;
        let listed = git(&root, &["-c", "core.quotePath=false", "show", "--format=", "-M", "--name-status", &moved])?;
        if !listed.starts_with('R') || !listed.ends_with(&format!("\t{old}\t{new}\n")) {
            return Err(format!("git show lists {listed:?}").into());
        }
        self.pass();
        Ok(())
    }

    fn summary_field(&mut self) -> Result<(String, Range<usize>)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let field = tab.ui.input.read(cx);
            (field.text().to_owned(), field.selected_range())
        })?)
    }

    fn summary_is(&mut self, after: &str, text: &str, selected: Range<usize>) -> Result<()> {
        let (shown, at) = self.summary_field()?;
        if shown != text || at != selected {
            return Err(format!("After {after} the summary reads {shown:?} with {at:?} selected, not {text:?} with {selected:?}").into());
        }
        Ok(())
    }

    fn summary_menu_shown(&mut self) -> Result<bool> {
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        Ok(self.tab.read_with(self.cx, |tab, cx| commit_input::menu_shown_for(tab.ui.input.entity_id(), cx))?)
    }

    async fn editing_the_summary(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0608");
        let root = init(dir, "Quill")?;
        record(&root, "menu.txt", "Soup\n", "Write the menu")?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("menu.txt"), "Soup\nBread\n")?;
        self.open(&root)?;
        self.until("Quill open with menu.txt edited", |tab| tab.ui.repo == "Quill" && files(tab, &[], &["menu.txt"]))
            .await?;
        self.key("cmd-shift-m")?;
        let focused = self.tab.update(self.cx, |tab, window, cx| tab.ui.input.focus_handle(cx).is_focused(window))?;
        if !focused {
            return Err("Cmd-Shift-M did not put the cursor in the commit summary".into());
        }
        let typed = "fix the café's menu";
        let end = typed.len();
        let word = "Fix the ".len().."Fix the café's".len();
        let bistro = "Fix the ".len().."Fix the bistro".len();
        let replaced = "fix the bistro menu";
        self.type_keys(typed)?;
        self.summary_is("typing", typed, end..end)?;
        self.key("alt-left")?;
        self.key("alt-left")?;
        self.summary_is("Option-Left twice", typed, word.start..word.start)?;
        self.key("alt-shift-right")?;
        self.summary_is("Option-Shift-Right", typed, word.clone())?;
        self.type_keys("bistro")?;
        self.summary_is("typing over the word", replaced, bistro.end..bistro.end)?;
        self.key("cmd-left")?;
        self.summary_is("Cmd-Left", replaced, 0..0)?;
        self.key("ctrl-e")?;
        self.summary_is("Control-E", replaced, replaced.len()..replaced.len())?;
        self.key("alt-backspace")?;
        self.summary_is("Option-Backspace", "fix the bistro ", bistro.end + 1..bistro.end + 1)?;
        self.key("cmd-z")?;
        self.summary_is("Cmd-Z", replaced, replaced.len()..replaced.len())?;
        self.key("cmd-z")?;
        self.summary_is("a second Cmd-Z", typed, word.clone())?;
        self.key("cmd-shift-z")?;
        self.summary_is("Cmd-Shift-Z", replaced, replaced.len()..replaced.len())?;

        let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
        self.cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(" at noon\nsharp".to_owned())))?;
        let pasted = self.key("cmd-v");
        self.cx.update(|cx| cx.write_to_clipboard(saved.clone().unwrap_or_else(|| ClipboardItem::new_string(String::new()))))?;
        pasted?;
        let lunch = "fix the bistro menu at noon sharp";
        self.summary_is("pasting two lines", lunch, lunch.len()..lunch.len())?;
        self.key("cmd-z")?;
        self.summary_is("Cmd-Z after the paste", replaced, replaced.len()..replaced.len())?;
        self.key("cmd-backspace")?;
        self.summary_is("Cmd-Backspace", "", 0..0)?;
        self.key("cmd-z")?;
        self.summary_is("Cmd-Z after Cmd-Backspace", replaced, replaced.len()..replaced.len())?;

        self.key("alt-left")?;
        self.key("alt-left")?;
        self.key("alt-shift-right")?;
        self.summary_is("selecting bistro", replaced, bistro.clone())?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.update(cx, |field, cx| field.open_menu(gpui::point(px(400.), px(300.)), window, cx))
        })?;
        if !self.summary_menu_shown()? {
            return Err("The summary's menu did not open".into());
        }
        let offered = self.tab.read_with(self.cx, |tab, cx| {
            tab.ui.input.read(cx).menu_items(true).into_iter().map(|item| (item.label, item.enabled)).collect::<Vec<_>>()
        })?;
        if offered != [("Cut", true), ("Copy", true), ("Paste", true), ("Delete", true), ("Select All", true)] {
            return Err(format!("The summary's menu offers {offered:?}").into());
        }
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.update(cx, |field, cx| field.choose(commit_input::FieldCommand::Delete, window, cx))
        })?;
        if self.summary_menu_shown()? {
            return Err("The summary's menu stayed open after Delete".into());
        }
        self.summary_is("Delete from the menu", "fix the  menu", bistro.start..bistro.start)?;
        self.key("cmd-z")?;
        self.summary_is("Cmd-Z after Delete", replaced, bistro.clone())?;

        let clipboard = self.cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?;
        if clipboard != saved.and_then(|item| item.text()) {
            return Err("The clipboard was not put back".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        self.tab.update(self.cx, |tab, _, cx| tab.ui.input.update(cx, |field, cx| field.clear(cx)))?;
        self.key("escape")?;
        self.pass();
        Ok(())
    }
}
