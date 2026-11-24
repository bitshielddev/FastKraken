use super::*;
use crate::{
    commands::{self, Side, Target},
    notice,
    picked::{self, How},
    prompt::{self, Stop},
    view::Tone,
};

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };

const NAMES: [&str; 5] = ["alder.txt", "birch bark.txt", "cedar é.txt", "dogwood.txt", "elm.txt"];

fn picked_paths(tab: &Tab) -> Option<Vec<String>> {
    picked::files(&tab.ui).map(|(paths, _)| paths.iter().map(|path| path.to_string_lossy().into_owned()).collect())
}

impl Replay<'_> {
    pub(super) async fn gap06(&mut self, dir: &Path) -> Result<()> {
        self.stage_a_range(dir).await?;
        self.discard_and_restore(dir).await?;
        self.discard_all(dir).await?;
        self.ignore_by_extension(dir).await?;
        self.ignore_tracked(dir).await
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
}
