use super::*;
use crate::{
    commands::{self, Side, Target},
    commit_options::{self, Action as Options},
    jobs,
    view::commit_input,
    notice,
    picked::{self, How},
    prompt::{self, Stop},
    stash,
    view::Tone,
};
use gpui::{ClipboardItem, Focusable, MouseButton, MouseDownEvent, Pixels};
use std::ops::Range;

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };

const SURVEY: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

const NAMES: [&str; 5] = ["alder.txt", "birch bark.txt", "cedar é.txt", "dogwood.txt", "elm.txt"];

fn changed_lines(tab: &Tab) -> Vec<String> {
    let doc = &tab.ui.diff;
    doc.lines
        .iter()
        .filter(|line| matches!(line.kind, crate::diff_doc::Kind::Added | crate::diff_doc::Kind::Deleted))
        .map(|line| doc.text(line).to_owned())
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

fn committing(tab: &Tab) -> Option<&notice::Notice> {
    let id = tab.ui.jobs.write.as_ref()?.notice?;
    tab.ui.notices.get(id).filter(|notice| notice.tone == Tone::Progress)
}

fn hooked(dir: &Path, root: &Path, name: &str) -> Result<PathBuf> {
    let hooks = dir.join(format!("{name}-hooks"));
    fs::create_dir(&hooks)?;
    let path = hooks.to_str().ok_or("The sandbox path is not UTF-8")?;
    git(root, &["config", "--local", "core.hooksPath", path])?;
    Ok(hooks)
}

fn alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "The sandbox path is not UTF-8".into())
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
        self.editing_the_summary(dir).await?;
        self.cancel_a_slow_hook(dir).await?;
        self.failing_hook_output(dir).await?;
        self.skip_hooks(dir).await?;
        self.commit_and_push(dir).await?;
        self.commit_template(dir).await?;
        self.restore_from_a_stash(dir).await?;
        self.switch_carrying_changes(dir).await
    }

    pub(super) async fn asked_about(&mut self, title: &str) -> Result<(Vec<String>, bool)> {
        self.until(&format!("the prompt {title:?} open"), |tab| {
            prompt::shown(&tab.ui).is_some_and(|spec| spec.title == title)
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| (spec.lines, spec.danger)))?;
        shown.ok_or_else(|| format!("The prompt {title:?} closed").into())
    }

    pub(super) async fn noticed(&mut self, title: &str) -> Result<(String, Vec<String>)> {
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

    pub(super) fn titles(&mut self, target: Target) -> Result<Vec<String>> {
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
        let mut wanted: Vec<String> = picks.iter().map(|name| name.to_string()).collect();
        wanted.sort();
        self.until("four files picked by Cmd-click", |tab| picked_paths(tab).as_ref() == Some(&wanted)).await?;
        let target = Target::Files { paths: picks.iter().map(PathBuf::from).collect(), side: Side::Unstaged };
        self.choose(&target, "file.discard")?;
        let (lines, danger) = self.asked_about("Discard changes to 4 files?").await?;
        if !danger || lines[..4] != picks[..] || lines.last().map(String::as_str) != Some("Undo, in the notice that follows, brings them back.") {
            return Err(format!("The discard prompt shows {lines:?}, danger {danger}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The discard prompt did not open with Cancel focused".into());
        }
        self.key(CMD_RETURN)?;
        let (_, offers) = self.noticed("Discarded changes to 4 files").await?;
        if offers != ["Undo"] {
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
        self.noticed("Undid: Discard changes to 4 files").await?;
        git_is(&root, &["status", "--porcelain=v1", "-uall"], &before)?;
        git_is(&root, &["ls-files", "--stage"], &index)?;
        for (name, text) in [
            ("chart.txt", "chart.txt staged and redrawn\n"),
            ("gauge.txt", "gauge.txt reads high\n"),
            ("flow é.txt", "flow é.txt rising\n"),
        ] {
            if fs::read_to_string(root.join(name))? != text {
                return Err(format!("Undo did not bring back {name}").into());
            }
        }
        if root.join("log book.txt").exists() {
            return Err("Undo did not delete log book.txt again".into());
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
        if offers != ["Undo"] {
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
        self.noticed("Undid: Discard all changes").await?;
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
            (tab.ui.renamed_from(&selection, None), tab.ui.diff.stageable_hunks())
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
        self.action(Action::FileHistory(crate::file_history::Action::OpenHere))?;
        self.until("file history lists the commits from before the rename under the old path", |tab| {
            tab.ui.file_history.entries().iter().map(|row| (row.subject.as_str(), row.path.as_path())).eq([
                (subjects[0], Path::new(new)),
                (subjects[1], Path::new(new)),
                (subjects[2], Path::new(old)),
                (subjects[3], Path::new(old)),
            ])
        })
        .await?;

        self.action(Action::FileHistory(crate::file_history::Action::Choose(2)))?;
        self.until("the older revision opens under the old path", |tab| {
            tab.ui.selection == Some(Selection::Commit(added.clone()))
                && tab.ui.diff_open
                && view::centre(&tab.ui) == view::Centre::FileHistory
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

    fn double_click(&mut self, at: gpui::Point<Pixels>) -> Result<()> {
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        let press = MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: gpui::Modifiers::default(),
            click_count: 2,
            first_mouse: false,
        };
        self.tab.update(self.cx, |tab, window, cx| tab.ui.input.update(cx, |field, cx| field.press(&press, window, cx)))?;
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
        self.key("cmd-left")?;
        let last_letter = self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.update(cx, |field, cx| field.bounds_for_range(bistro.end - 1..bistro.end, Bounds::default(), window, cx))
        })?;
        let last_letter = last_letter.ok_or("The summary was not laid out")?;
        self.double_click(gpui::point(last_letter.right() - px(1.), last_letter.center().y))?;
        self.summary_is("double-clicking the right half of bistro's last letter", replaced, bistro.clone())?;

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

    async fn staged_for_commit(&mut self, root: &Path, name: &str, file: &str, text: &str) -> Result<()> {
        fs::write(root.join(file), text)?;
        self.open(root)?;
        self.until(&format!("{name} open with {file} edited"), |tab| tab.ui.repo == name && files(tab, &[], &[file])).await?;
        self.action(Action::StageAll)?;
        self.until(&format!("{file} staged"), |tab| files(tab, &[file], &[])).await
    }

    async fn gone(&mut self, pid: libc::pid_t, what: &str) -> Result<()> {
        let start = Instant::now();
        while alive(pid) {
            if start.elapsed() > Duration::from_secs(5) {
                return Err(format!("{what} (process {pid}) is still running after the cancel").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    async fn cancel_a_slow_hook(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0609");
        let root = init(dir, "Lantern")?;
        record(&root, "wick.txt", "Wick: long\n", "Light the lantern")?;
        let head = oid(&root, "HEAD")?;
        let hooks = hooked(dir, &root, "Lantern")?;
        let (own, child) = (dir.join("lantern-hook.pid"), dir.join("lantern-sleep.pid"));
        gap04::script(
            &hooks.join("pre-commit"),
            &format!(
                "echo 'checking the lantern glass'\n/bin/sleep 120 &\necho $! > '{child}'\necho $$ > '{own}'\nwait\n",
                own = own.display(),
                child = child.display()
            ),
        )?;
        self.staged_for_commit(&root, "Lantern", "wick.txt", "Wick: trimmed\n").await?;
        self.type_commit("Trim the wick")?;
        self.during("the commit naming its hook", |tab| {
            committing(tab).is_some_and(|notice| {
                notice.title == "Running pre-commit…" && notice.detail.starts_with("checking the lantern glass") && notice.job.is_some()
            }) && own.exists()
                && child.exists()
        })
        .await?;
        let read = |path: &Path| -> Result<libc::pid_t> { Ok(fs::read_to_string(path)?.trim().parse()?) };
        let (hook_pid, sleep_pid) = (read(&own)?, read(&child)?);
        if !alive(hook_pid) || !alive(sleep_pid) {
            return Err("The hook was not running while its notice was shown".into());
        }
        self.during("the elapsed time after three seconds", |tab| {
            committing(tab).is_some_and(|notice| notice.detail == "checking the lantern glass\nRunning for 3 s")
        })
        .await?;
        let job = self
            .tab
            .read_with(self.cx, |tab, _| committing(tab).and_then(|notice| notice.job))?
            .ok_or("The commit's notice has no Cancel button")?;
        self.action(Action::Jobs(jobs::Action::Cancel(job)))?;
        self.until("the commit cancelled", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| {
                notice.tone == Tone::Info && notice.title == "Commit cancelled" && notice.detail == "No commit was made."
            })
        })
        .await?;
        self.gone(hook_pid, "The pre-commit hook").await?;
        self.gone(sleep_pid, "The program the hook started").await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only"], "wick.txt\n")?;
        if root.join(".git/index.lock").exists() {
            return Err("The cancelled commit left index.lock behind".into());
        }
        let (summary, _) = self.summary_field()?;
        if summary != "Trim the wick" {
            return Err(format!("After the cancel the summary reads {summary:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn failing_hook_output(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0610");
        let root = init(dir, "Kettle")?;
        record(&root, "brew.txt", "Brew: black\n", "Boil the kettle")?;
        let head = oid(&root, "HEAD")?;
        let hooks = hooked(dir, &root, "Kettle")?;
        gap04::script(
            &hooks.join("pre-commit"),
            "i=1\nwhile [ $i -le 200 ]; do echo \"kettle check $i of 200\"; i=$((i+1)); done\nexit 1\n",
        )?;
        self.staged_for_commit(&root, "Kettle", "brew.txt", "Brew: green\n").await?;
        self.type_commit("Change the brew")?;
        self.until("the hook's failure", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Error && notice.title == "pre-commit stopped the commit")
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().map(|notice| (notice.detail.clone(), notice.output.clone(), notice.expanded, notice::output_label(notice)))
        })?;
        let Some((detail, Some(output), true, "Hide Git output")) = shown else {
            return Err(format!("The failure notice is not expanded with the hook's output: {shown:?}").into());
        };
        if detail != "No commit was made. It exited with code 1." {
            return Err(format!("The failure notice reads {detail:?}").into());
        }
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() != 200 || lines.first() != Some(&"kettle check 1 of 200") || lines.last() != Some(&"kettle check 200 of 200") {
            return Err(format!("The expanded output has {} lines, ending {:?}", lines.len(), lines.last()).into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only"], "brew.txt\n")?;
        let (summary, _) = self.summary_field()?;
        if summary != "Change the brew" {
            return Err(format!("After the failure the summary reads {summary:?}").into());
        }

        gap04::script(&hooks.join("pre-commit"), "echo 'kettle: lid on'\necho 'kettle: 2 cups' >&2\n")?;
        self.key(CMD_RETURN)?;
        self.until("the commit with the hook's words", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Changes committed")
        })
        .await?;
        let told = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().map(|notice| (notice.detail.clone(), notice.output.clone(), notice::output_label(notice)))
        })?;
        let expected = ("Change the brew\npre-commit printed 2 lines.".to_owned(), Some("kettle: lid on\nkettle: 2 cups".to_owned()), "Show output");
        if told.as_ref() != Some(&expected) {
            return Err(format!("The commit's notice reads {told:?}").into());
        }
        git_is(&root, &["log", "-1", "--format=%s"], "Change the brew\n")?;
        self.pass();
        Ok(())
    }

    fn commit_label(&mut self) -> Result<String> {
        Ok(self.tab.read_with(self.cx, |tab, cx| commit_options::commit_label(&tab.ui, cx))?)
    }

    fn draft_is(&mut self, summary: &str, description: &str) -> Result<()> {
        let shown = self.tab.read_with(self.cx, |tab, cx| {
            (tab.ui.input.read(cx).text().to_owned(), tab.ui.description.read(cx).text().to_owned())
        })?;
        if shown != (summary.to_owned(), description.to_owned()) {
            return Err(format!("The commit box reads {shown:?}, not {:?}", (summary, description)).into());
        }
        Ok(())
    }

    async fn skip_hooks(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0611");
        let root = init(dir, "Sluice")?;
        record(&root, "gate.txt", "Gate: shut\n", "Build the sluice")?;
        let head = oid(&root, "HEAD")?;
        let hooks = hooked(dir, &root, "Sluice")?;
        let ran = dir.join("sluice-pre-commit-ran");
        gap04::script(
            &hooks.join("pre-commit"),
            &format!(": > '{}'\necho 'sluice: lint failed'\nexit 1\n", ran.display()),
        )?;
        self.staged_for_commit(&root, "Sluice", "gate.txt", "Gate: open\n").await?;
        let label = self.commit_label()?;
        if label != "Commit 1 file to main" {
            return Err(format!("Before the options the button reads {label:?}").into());
        }
        self.action(Action::CommitOptions(Options::ToggleOpen))?;
        self.action(Action::CommitOptions(Options::ToggleSkipHooks))?;
        let (open, skipping) = self.tab.read_with(self.cx, |tab, _| (tab.ui.commit_options.open, tab.ui.commit_options.skip_hooks))?;
        if !open || !skipping {
            return Err(format!("After ticking Skip hooks the options are open {open} and skipping {skipping}").into());
        }
        let label = self.commit_label()?;
        if label != "Commit 1 file, skipping hooks" {
            return Err(format!("With Skip hooks the button reads {label:?}").into());
        }
        self.type_commit("Open the sluice")?;
        self.until("the commit made past the failing hook", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Changes committed")
                && tab.ui.files.is_empty()
        })
        .await?;
        if ran.exists() {
            return Err("pre-commit ran although Skip hooks was ticked".into());
        }
        git_is(&root, &["log", "-1", "--format=%s"], "Open the sluice\n")?;
        git_is(&root, &["rev-parse", "HEAD^"], &format!("{head}\n"))?;
        git_is(&root, &["show", "HEAD:gate.txt"], "Gate: open\n")?;
        let (skipping, open) = self.tab.read_with(self.cx, |tab, _| (tab.ui.commit_options.skip_hooks, tab.ui.commit_options.open))?;
        if skipping || !open {
            return Err(format!("After the commit Skip hooks is ticked {skipping} and the options open {open}").into());
        }

        let landed = oid(&root, "HEAD")?;
        fs::write(root.join("gate.txt"), "Gate: half open\n")?;
        self.action(Action::Refresh)?;
        self.until("gate.txt edited again", |tab| files(tab, &[], &["gate.txt"])).await?;
        self.action(Action::StageAll)?;
        self.until("gate.txt staged again", |tab| files(tab, &["gate.txt"], &[])).await?;
        let label = self.commit_label()?;
        if label != "Commit 1 file to main" {
            return Err(format!("After the cleared tick the button reads {label:?}").into());
        }
        self.type_commit("Half open")?;
        self.until("the hook stopping the next commit", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Error && notice.title == "pre-commit stopped the commit")
        })
        .await?;
        if !ran.exists() {
            return Err("pre-commit did not run on the next commit".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{landed}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only"], "gate.txt\n")?;
        self.action(Action::CommitOptions(Options::ToggleOpen))?;
        self.pass();
        Ok(())
    }

    async fn commit_and_push(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0612");
        let (root, bare, _) = gap04::published(dir, "Weir")?;
        let before = oid(&bare, "refs/heads/main")?;
        self.staged_for_commit(&root, "Weir", "trail.txt", "FastKraken: git for the impatient\nWeir: raised\n").await?;
        self.until("origin/main tracked", |tab| tab.ui.upstream.as_deref() == Some("origin/main") && !tab.ui.refs.remotes.is_empty())
            .await?;
        self.action(Action::CommitOptions(Options::ToggleOpen))?;
        self.action(Action::CommitOptions(Options::TogglePush))?;
        let label = self.commit_label()?;
        if label != "Commit and push to origin/main" {
            return Err(format!("With Push after committing the button reads {label:?}").into());
        }
        let remembered = self.tab.read_with(self.cx, |tab, cx| {
            let common = tab.repo.as_ref()?.common_dir().to_owned();
            settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.push_after)
        })?;
        if remembered != Some(true) {
            return Err(format!("Push after committing is remembered as {remembered:?}").into());
        }
        self.type_commit("Raise the weir")?;
        self.until("the commit pushed", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Pushed Successfully")
                && tab.ui.files.is_empty()
        })
        .await?;
        let made = oid(&root, "HEAD")?;
        git_is(&root, &["log", "-1", "--format=%s"], "Raise the weir\n")?;
        git_is(&bare, &["rev-parse", "refs/heads/main"], &format!("{made}\n"))?;
        git_is(&bare, &["rev-parse", "refs/heads/main^"], &format!("{before}\n"))?;

        git(&root, &["switch", "-q", "-c", "spillway"])?;
        fs::write(root.join("spill.txt"), "Spillway: open\n")?;
        self.action(Action::Refresh)?;
        self.until("spillway checked out with spill.txt", |tab| {
            tab.ui.branch == "spillway" && tab.ui.upstream.is_none() && files(tab, &[], &["spill.txt"])
        })
        .await?;
        self.action(Action::StageAll)?;
        self.until("spill.txt staged", |tab| files(tab, &["spill.txt"], &[])).await?;
        let label = self.commit_label()?;
        if label != "Commit and publish spillway" {
            return Err(format!("On a branch with no upstream the button reads {label:?}").into());
        }
        self.type_commit("Open the spillway")?;
        self.until("Publish branch offered after the commit", |tab| tab.ui.publish.branch_open() && tab.ui.files.is_empty()).await?;
        git_is(&root, &["log", "-1", "--format=%s"], "Open the spillway\n")?;
        if !git(&bare, &["branch", "--list", "spillway"])?.is_empty() {
            return Err("spillway reached the remote before it was published".into());
        }
        self.action(Action::Publish(crate::publish::Action::Escape))?;
        self.until("the Publish branch form closed", |tab| !tab.ui.publish.branch_open()).await?;
        self.action(Action::CommitOptions(Options::TogglePush))?;
        self.action(Action::CommitOptions(Options::ToggleOpen))?;
        self.pass();
        Ok(())
    }

    async fn commit_template(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0613");
        let root = init(dir, "Ledger")?;
        record(&root, "accounts.txt", "March: open\n", "Open the ledger")?;
        git(&root, &["switch", "-q", "-c", "audit"])?;
        record(&root, "audit.txt", "Audit: passed\n", "Record the audit")?;
        git(&root, &["switch", "-q", "main"])?;
        let template = dir.join("ledger-template.txt");
        fs::write(
            &template,
            "Ledger: \n# One line saying what changed\n\nWhy:\n# Explain why it changed\nChecked by: \n",
        )?;
        git(&root, &["config", "--local", "commit.template", path(&template)?])?;
        fs::write(root.join("accounts.txt"), "March: closed\n")?;
        self.open(&root)?;
        self.until("Ledger open with the template in the commit box", |tab| {
            tab.ui.repo == "Ledger" && tab.ui.commit_options.template.is_some() && files(tab, &[], &["accounts.txt"])
        })
        .await?;
        self.draft_is("Ledger:", "Why:\nChecked by:")?;
        let filling = self.tab.read_with(self.cx, |_, cx| settings::settings(cx).fill_from_template)?;
        if !filling {
            return Err("Filling from the template is off by default".into());
        }
        self.action(Action::StageAll)?;
        self.until("accounts.txt staged", |tab| files(tab, &["accounts.txt"], &[])).await?;
        self.type_commit(" close March")?;
        self.until("the commit from the template", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.tone == Tone::Success && notice.title == "Changes committed")
                && tab.ui.files.is_empty()
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%B"], "Ledger: close March\n\nWhy:\nChecked by:\n\n")?;
        self.draft_is("Ledger:", "Why:\nChecked by:")?;
        self.until("audit listed", |tab| tab.ui.refs.local(std::ffi::OsStr::new("audit")).is_some()).await?;
        self.action(Action::Merge(crate::merge::Action::Start(
            crate::merge::Source::Branch("audit".into()),
            crate::merge::Mode::Squash,
        )))?;
        self.until("audit squashed into the index", |tab| !tab.ui.busy() && files(tab, &["audit.txt"], &[])).await?;
        let squashed = self.tab.read_with(self.cx, |tab, cx| tab.ui.draft(cx))?;
        if squashed.summary != "Squash merge audit into main" || !squashed.description.contains("Record the audit") {
            return Err(format!("After a squash the commit box still holds {squashed:?}").into());
        }
        self.action(Action::CommitOptions(Options::ToggleTemplate))?;
        let filling = self.tab.read_with(self.cx, |_, cx| settings::settings(cx).fill_from_template)?;
        if filling {
            return Err("The template preference did not turn off".into());
        }
        self.action(Action::CommitOptions(Options::ToggleTemplate))?;
        self.pass();
        Ok(())
    }

    async fn restore_from_a_stash(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0614");
        let root = init(dir, "Cache-pool")?;
        for name in ["chart.txt", "gauge.txt", "weir.txt"] {
            fs::write(root.join(name), format!("{name} at rest\n"))?;
        }
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Fill the pool"])?;
        let head = oid(&root, "HEAD")?;
        let untracked = "flow é.txt";
        fs::write(root.join("chart.txt"), "chart.txt redrawn\n")?;
        fs::write(root.join("gauge.txt"), "gauge.txt reads high\n")?;
        fs::write(root.join("weir.txt"), "weir.txt overflows\n")?;
        fs::write(root.join(untracked), "flow é.txt rising\n")?;
        self.open(&root)?;
        self.until("Cache-pool open with four unstaged changes", |tab| {
            tab.ui.repo == "Cache-pool" && files(tab, &[], &["chart.txt", "gauge.txt", "weir.txt", untracked])
        })
        .await?;
        let others = || -> Result<()> {
            for (name, text) in [("gauge.txt", "gauge.txt reads high\n"), ("weir.txt", "weir.txt overflows\n")] {
                if fs::read_to_string(root.join(name))? != text {
                    return Err(format!("{name} changed").into());
                }
            }
            git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
            git_is(&root, &["diff", "--cached", "--name-only"], "")
        };

        self.action(Action::Select(file("chart.txt", false)))?;
        self.until("chart.txt selected", |tab| tab.ui.selection == Some(file("chart.txt", false))).await?;
        self.action(Action::Picked(picked::Action::Click { path: untracked.into(), staged: false, how: How::Toggle }))?;
        let chosen = vec!["chart.txt".to_owned(), untracked.to_owned()];
        self.until("two files picked", |tab| picked_paths(tab).as_ref() == Some(&chosen)).await?;
        self.action(Action::Stash(stash::Action::ToggleOptions))?;
        self.until("the stash form open", |tab| tab.ui.stash.options_open()).await?;
        let offered = self.tab.read_with(self.cx, |tab, _| stash::selected_files(&tab.ui))?;
        if offered != Some(vec![PathBuf::from("chart.txt"), PathBuf::from(untracked)]) {
            return Err(format!("The stash form offers the selected files {offered:?}").into());
        }
        self.action(Action::Stash(stash::Action::ToggleStaged))?;
        self.action(Action::Stash(stash::Action::ToggleFiles))?;
        let ticks = self.tab.read_with(self.cx, |tab, _| (tab.ui.stash.files_only(), tab.ui.stash.staged_only()))?;
        if ticks != (true, false) {
            return Err(format!("Selected files only and Staged changes only read {ticks:?}").into());
        }
        self.type_keys("Survey notes")?;
        self.key("enter")?;
        self.until("the two selected files stashed under their message", |tab| {
            !tab.ui.busy()
                && !tab.ui.stash.options_open()
                && tab.ui.stash.entries.len() == 1
                && tab.ui.stash.entries[0].title == "Survey notes"
                && files(tab, &[], &["gauge.txt", "weir.txt"])
        })
        .await?;
        let said = self.said()?;
        let detail = "Only the selected files. Other changes stay in your files.";
        if said.as_ref().map(|said| (said.1.as_str(), said.2.as_str())) != Some(("Stashed Survey notes", detail)) {
            return Err(format!("Stashing the selected files said {said:?}").into());
        }
        let cleared = self.tab.read_with(self.cx, |tab, _| tab.ui.stash.files_only())?;
        if cleared {
            return Err("Selected files only is still ticked after stashing".into());
        }
        let saved = oid(&root, "refs/stash")?;
        git_is(&root, &["stash", "list", "--format=%H %gs"], &format!("{saved} On main: Survey notes\n"))?;
        git_is(&root, &["show", &format!("{saved}:chart.txt")], "chart.txt redrawn\n")?;
        git_is(&root, &["show", &format!("{saved}^3:{untracked}")], "flow é.txt rising\n")?;
        git_is(&root, &["show", &format!("{saved}:gauge.txt")], "gauge.txt at rest\n")?;
        if fs::read_to_string(root.join("chart.txt"))? != "chart.txt at rest\n" || root.join(untracked).exists() {
            return Err("The stashed files are still changed in the working tree".into());
        }
        others()?;

        self.action(Action::Select(Selection::Stash(saved.clone())))?;
        self.until("the stash selected with its two files", |tab| {
            matches!(&tab.ui.selection, Some(Selection::Stash(oid)) if *oid == saved) && tab.ui.stash.files.len() == 2
        })
        .await?;
        self.action(Action::Stash(stash::Action::OpenFile("chart.txt".into())))?;
        self.until("chart.txt's own diff over the graph", |tab| {
            tab.ui.diff_open
                && tab.ui.stash.open_path() == Some(Path::new("chart.txt"))
                && patch(tab, "+chart.txt redrawn")
                && changed_lines(tab) == ["-chart.txt at rest", "+chart.txt redrawn"]
        })
        .await?;
        let target = Target::File { path: "chart.txt".into(), side: Side::Stash(saved.clone()) };
        let current = self.tab.read_with(self.cx, |tab, _| Target::current(&tab.ui))?;
        if current != target {
            return Err(format!("With chart.txt open the commands act on {current:?}").into());
        }
        let reached = self.tab.read_with(self.cx, |tab, _| commands::targets(&tab.ui))?;
        if !reached.contains(&Target::Stash(saved.clone())) {
            return Err(format!("With chart.txt open the stash's own commands reach only {reached:?}").into());
        }
        let rows = self.menu_rows(&target)?;
        if !rows.iter().any(|row| row.0 == "stash.restore-file" && row.1 == "Restore this file from the stash" && row.2.is_none()) {
            return Err(format!("The menu of the stashed chart.txt lists {rows:?}").into());
        }
        self.choose(&target, "stash.restore-file")?;
        let (detail, _) = self.noticed("Restored chart.txt from the stash").await?;
        if detail != "Other files are unchanged, and the stash stays in the list." {
            return Err(format!("Restoring chart.txt said {detail:?}").into());
        }
        if fs::read_to_string(root.join("chart.txt"))? != "chart.txt redrawn\n" || root.join(untracked).exists() {
            return Err("Restoring chart.txt did not write exactly the stash's chart.txt".into());
        }
        others()?;
        git_is(&root, &["stash", "list", "--format=%H"], &format!("{saved}\n"))?;
        self.until("chart.txt listed as changed again", |tab| files(tab, &[], &["chart.txt", "gauge.txt", "weir.txt"])).await?;

        fs::write(root.join(untracked), "flow é.txt measured by hand\n")?;
        self.action(Action::Refresh)?;
        self.until("the hand-made flow é.txt listed", |tab| {
            files(tab, &[], &["chart.txt", "gauge.txt", "weir.txt", untracked])
        })
        .await?;
        self.action(Action::Stash(stash::Action::OpenFile(untracked.into())))?;
        self.until("the stashed flow é.txt shown as an added file", |tab| {
            tab.ui.stash.open_path() == Some(Path::new(untracked)) && changed_lines(tab) == ["+flow é.txt rising"]
        })
        .await?;
        let flow = Target::File { path: untracked.into(), side: Side::Stash(saved.clone()) };
        self.choose(&flow, "stash.restore-file")?;
        let (lines, danger) = self.asked_about("Replace flow é.txt with the stash's version?").await?;
        if !danger || lines.len() != 2 || !lines[1].contains("the stash stays in the list") {
            return Err(format!("The replace prompt shows {lines:?}, danger {danger}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The replace prompt did not open with Cancel focused".into());
        }
        if fs::read_to_string(root.join(untracked))? != "flow é.txt measured by hand\n" {
            return Err("flow é.txt was replaced before the prompt was answered".into());
        }
        self.key(CMD_RETURN)?;
        self.noticed("Restored flow é.txt from the stash").await?;
        if fs::read_to_string(root.join(untracked))? != "flow é.txt rising\n" {
            return Err("flow é.txt does not match the stash's version".into());
        }
        if fs::read_to_string(root.join("chart.txt"))? != "chart.txt redrawn\n" {
            return Err("Restoring flow é.txt changed chart.txt".into());
        }
        others()?;
        git_is(&root, &["stash", "list", "--format=%H"], &format!("{saved}\n"))?;
        self.pass();
        Ok(())
    }

    pub(super) fn changes_menu(&mut self) -> Result<Vec<(&'static str, String, Option<String>)>> {
        self.action(Action::Menu(Some(view::Menu::Changes { at: gpui::Point::default() })))?;
        let open = self.tab.read_with(self.cx, |tab, _| matches!(tab.ui.menu, Some(view::Menu::Changes { .. })))?;
        if !open {
            return Err("Right-clicking the uncommitted row opened no menu".into());
        }
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::entries_of(&tab.ui, &view::Menu::Changes { at: gpui::Point::default() }, cx)
                .into_iter()
                .map(|entry| (entry.id, entry.title, entry.disabled))
                .collect()
        })?)
    }

    pub(super) fn choose_on_changes(&mut self, id: &str) -> Result<()> {
        let chosen = self.tab.read_with(self.cx, |tab, cx| {
            commands::entries_of(&tab.ui, &view::Menu::Changes { at: gpui::Point::default() }, cx)
                .into_iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| entry.action)
        })?;
        let action = chosen.ok_or_else(|| format!("The uncommitted row's menu has no enabled {id}"))?;
        self.action(Action::Menu(None))?;
        self.action(action)
    }

    async fn switch_carrying_changes(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0615");
        let (root, bare, peer) = super::gap04::published(dir, "Carry-on")?;
        record(&root, "camp.txt", "Camp: by the lake\n", "Pitch the camp")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        git(&root, &["switch", "-q", "-c", "ridge"])?;
        record(&root, "ridge.txt", "Ridge: windy\n", "Walk the ridge")?;
        let ridge = oid(&root, "ridge")?;
        git(&root, &["switch", "-q", "main"])?;
        let main = oid(&root, "main")?;
        fs::write(root.join("camp.txt"), "Camp: by the lake, tent up\n")?;
        fs::write(root.join("trail.txt"), "FastKraken: git for the impatient, and the patient\n")?;
        git(&root, &["add", "--", "trail.txt"])?;
        fs::write(root.join("pack é.txt"), "Pack: water\n")?;
        let carried = " M camp.txt\nM  trail.txt\n?? \"pack \\303\\251.txt\"\n";
        git_is(&root, &["status", "--porcelain=v1"], carried)?;
        self.open(&root)?;
        self.until("Carry-on open on main with its HEAD commit selected", |tab| {
            tab.ui.repo == "Carry-on"
                && tab.ui.branch == "main"
                && tab.ui.selection == Some(Selection::Commit(main.clone()))
                && tab.ui.refs.local(std::ffi::OsStr::new("ridge")).is_some()
                && files(tab, &["trail.txt"], &["camp.txt", "pack é.txt"])
        })
        .await?;

        self.action(Action::Refs(refs::Action::Switch("ridge".into())))?;
        self.until("ridge checked out with the uncommitted row selected", |tab| {
            tab.ui.branch == "ridge"
                && tab.ui.head.as_deref() == Some(ridge.as_str())
                && tab.ui.selection == Some(Selection::Changes)
                && !tab.ui.diff_open
                && files(tab, &["trail.txt"], &["camp.txt", "pack é.txt"])
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{ridge}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], carried)?;

        let rows = self.changes_menu()?;
        let expected = [
            ("repo.stage-all", "Stage all"),
            ("repo.unstage-all", "Unstage all"),
            ("repo.stash", "Stash changes"),
            ("repo.stash-options", "Stash with a message…"),
            ("repo.discard-all", "Discard all changes…"),
            ("repo.browse-files", "Browse files"),
            ("repo.save-patch", "Save all changes as a patch…"),
        ];
        if rows.len() != expected.len()
            || rows.iter().zip(expected).any(|(row, (id, title))| row.0 != id || row.1 != title || row.2.is_some())
        {
            return Err(format!("The uncommitted row's menu lists {rows:?}").into());
        }
        self.choose_on_changes("repo.stage-all")?;
        self.until("everything staged from the row's menu", |tab| {
            !tab.ui.busy() && tab.ui.menu.is_none() && files(tab, &["camp.txt", "pack é.txt", "trail.txt"], &[])
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "M  camp.txt\nA  \"pack \\303\\251.txt\"\nM  trail.txt\n")?;
        self.changes_menu()?;
        self.choose_on_changes("repo.stash")?;
        self.until("the changes stashed from the row's menu", |tab| {
            !tab.ui.busy() && tab.ui.files.is_empty() && tab.ui.stash.entries.len() == 1
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if git(&root, &["stash", "list"])?.lines().count() != 1 {
            return Err("Stashing from the row's menu did not make exactly one stash".into());
        }
        git_is(&root, &["show", "stash@{0}:camp.txt"], "Camp: by the lake, tent up\n")?;
        git_is(&root, &["show", "stash@{0}:pack é.txt"], "Pack: water\n")?;
        let menu = self.changes_menu()?;
        let clean = |id: &str| menu.iter().find(|row| row.0 == id).and_then(|row| row.2.clone());
        if clean("repo.stage-all").as_deref() != Some("Nothing to stage")
            || clean("repo.discard-all").as_deref() != Some("There are no changes to discard")
        {
            return Err(format!("With a clean tree the uncommitted row's menu lists {menu:?}").into());
        }
        self.action(Action::Menu(None))?;

        self.action(Action::Refs(refs::Action::Switch("main".into())))?;
        self.until("main checked out with a clean tree and its HEAD commit selected", |tab| {
            tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(main.as_str())
                && tab.ui.selection == Some(Selection::Commit(main.clone()))
                && tab.ui.files.is_empty()
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;

        fs::write(root.join("notes.txt"), "Notes: start early\n")?;
        self.until("the new note listed", |tab| files(tab, &[], &["notes.txt"])).await?;
        self.action(Action::Branch(crate::branch::Action::Detach { oid: ridge.clone(), tag: None }))?;
        self.until("ridge's commit checked out with the uncommitted row selected", |tab| {
            tab.ui.detached
                && tab.ui.head.as_deref() == Some(ridge.as_str())
                && tab.ui.selection == Some(Selection::Changes)
                && files(tab, &[], &["notes.txt"])
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{ridge}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "?? notes.txt\n")?;

        self.action(Action::Select(Selection::Commit(ridge.clone())))?;
        self.action(Action::Refs(refs::Action::Switch("main".into())))?;
        self.until("main checked out again with the note carried", |tab| {
            tab.ui.branch == "main" && !tab.ui.detached && tab.ui.selection == Some(Selection::Changes)
        })
        .await?;
        git(&peer, &["pull", "-q", "--ff-only", path(&bare)?, "main"])?;
        record(&peer, "summit.txt", "Summit: cairn\n", "Mark the summit")?;
        git(&peer, &["push", "-q", path(&bare)?, "main"])?;
        let summit = oid(&peer, "HEAD")?;
        self.action(Action::Select(Selection::Commit(main.clone())))?;
        self.until("main's commit selected", |tab| tab.ui.selection == Some(Selection::Commit(main.clone()))).await?;
        self.action(Action::Sync(crate::sync::Action::Pull))?;
        self.until("the pull done with the uncommitted row selected", |tab| {
            tab.ui.head.as_deref() == Some(summit.as_str())
                && tab.ui.notices.newest().is_some_and(|notice| notice.title == "Pulled Successfully")
                && tab.ui.selection == Some(Selection::Changes)
                && files(tab, &[], &["notes.txt"])
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{summit}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "?? notes.txt\n")?;

        fs::remove_file(root.join("notes.txt"))?;
        self.until("the tree clean again", |tab| tab.ui.files.is_empty()).await?;
        self.action(Action::Select(Selection::Changes))?;
        self.action(Action::Branch(crate::branch::Action::Detach { oid: ridge.clone(), tag: None }))?;
        self.until("ridge's commit checked out from a clean tree with it selected", |tab| {
            tab.ui.detached
                && tab.ui.head.as_deref() == Some(ridge.as_str())
                && tab.ui.selection == Some(Selection::Commit(ridge.clone()))
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }
}
