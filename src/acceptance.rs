mod gap01;
mod gap02;
mod gap03;
mod gap04;
mod gap05;
mod gap06;
mod merges;
#[cfg(feature = "publish-replay")]
mod publishing;
#[cfg(feature = "publish-replay")]
mod publishing_mock;
mod reversals;
mod stashes;
mod upstream;

use crate::{
    Tab, inspection, refs, repository, settings, storage,
    view::{self, Action, Desk, Selection},
};
use gpui::{
    App, AppContext, Application, AsyncApp, Bounds, EntityInputHandler, Keystroke, WindowBounds,
    WindowOptions, px, size,
};
use serde_json::json;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TRAIL: &str = "FastKraken: git for the impatient\n";
const WAYPOINT: &str = "FastKraken: git for the impatient\nWaypoint: Willow bridge\n";
const NOTE: &str = "Remember to mark the bridge.\n";
const WAYPOINT_COMMIT: &str = "Record Willow waypoint";
const NOTE_COMMIT: &str = "Save bridge note";
const FIRST_WAVE: &[&str] = &[
    "OBS01", "OBS02", "OBS03", "OBS04", "OBS05", "OBS06", "OBS09", "OBS10", "OBS11", "OBS12",
    "OBS21", "OBS22", "OBS23", "EXT08", "EXT01", "EXT02",
];
const SECOND_WAVE: &[&str] = &[
    "OBS07", "OBS08", "EXT05", "EXT06", "EXT07", "OBS13", "OBS17", "OBS18", "OBS19", "OBS20",
    "EXT10", "EXT11", "EXT12",
];

const GAP_WAVES: &[(u8, &[&str])] =
    &[
        (1, &["GAP0101", "GAP0102", "GAP0103", "GAP0104", "GAP0110", "GAP0105", "GAP0106", "GAP0107", "GAP0111", "GAP0108", "GAP0112", "GAP0113", "GAP0114", "GAP0115", "GAP0109"]),
        (2, &["GAP0201", "GAP0202", "GAP0203", "GAP0204", "GAP0205", "GAP0206", "GAP0207", "GAP0208", "GAP0209", "GAP0210", "GAP0211", "GAP0212", "GAP0213", "GAP0214", "GAP0215", "GAP0216", "GAP0217", "GAP0218", "GAP0219", "GAP0220", "GAP0221"]),
        (3, &["GAP0301", "GAP0302", "GAP0303", "GAP0304", "GAP0305", "GAP0306", "GAP0307", "GAP0308", "GAP0309", "GAP0310", "GAP0311", "GAP0312", "GAP0313", "GAP0314", "GAP0315", "GAP0316", "GAP0317", "GAP0318", "GAP0319", "GAP0320", "GAP0321"]),
        (4, &["GAP0401", "GAP0402", "GAP0403", "GAP0404", "GAP0405", "GAP0406", "GAP0407", "GAP0408", "GAP0409", "GAP0410", "GAP0411", "GAP0412", "GAP0413"]),
        (5, &["GAP0501", "GAP0502", "GAP0503", "GAP0504", "GAP0505", "GAP0506", "GAP0507", "GAP0508", "GAP0509", "GAP0510", "GAP0511", "GAP0514", "GAP0512", "GAP0513"]),
        (6, &["GAP0601"]),
    ];

#[derive(Clone, Copy)]
pub enum Wave {
    One,
    Two,
    Three,
    Gap(u8),
    #[cfg(feature = "publish-replay")]
    Publish,
}

impl Wave {
    fn flag(self) -> String {
        match self {
            Self::One => "--acceptance-wave-1".into(),
            Self::Two => "--acceptance-wave-2".into(),
            Self::Three => "--acceptance-wave-3".into(),
            Self::Gap(phase) => format!("--acceptance-gap-{phase:02}"),
            #[cfg(feature = "publish-replay")]
            Self::Publish => "--acceptance-publish".into(),
        }
    }

    pub fn from_flag(flag: &str) -> Option<Self> {
        if let Some(phase) = flag.strip_prefix("--acceptance-gap-") {
            return phase.parse().ok().map(Self::Gap);
        }
        [
            Self::One,
            Self::Two,
            Self::Three,
            #[cfg(feature = "publish-replay")]
            Self::Publish,
        ]
        .into_iter()
        .find(|wave| wave.flag() == flag)
    }

    fn scenarios(self) -> &'static [&'static str] {
        match self {
            Self::One => FIRST_WAVE,
            Self::Two => SECOND_WAVE,
            Self::Three => &["OBS14", "OBS15", "OBS16", "EXT14", "EXT15"],
            Self::Gap(phase) => GAP_WAVES
                .iter()
                .find(|(number, _)| *number == phase)
                .map_or(&[], |(_, ids)| *ids),
            #[cfg(feature = "publish-replay")]
            Self::Publish => &["PUB01", "PUB02", "PUB03", "PUB04", "PUB05", "PUB06", "PUB07", "PUB08"],
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = repository::bundled_command(root)?.args(args).output()?;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_is(root: &Path, args: &[&str], expected: &str) -> Result<()> {
    let actual = git(root, args)?;
    if actual != expected {
        return Err(format!("git {args:?}: expected {expected:?}, got {actual:?}").into());
    }
    Ok(())
}

fn init(dir: &Path, name: &str) -> Result<PathBuf> {
    let root = dir.join(name);
    fs::create_dir(&root)?;
    git(&root, &["init", "-q", "-b", "main"])?;
    for (key, value) in [
        ("user.name", "Cedar Contributor"),
        ("user.email", "cedar@example.invalid"),
        ("core.hooksPath", "/dev/null"),
        ("commit.gpgSign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git(&root, &["config", "--local", key, value])?;
    }
    Ok(root)
}

fn record(root: &Path, path: &str, contents: &str, subject: &str) -> Result<()> {
    fs::write(root.join(path), contents)?;
    git(root, &["add", "--", path])?;
    git(root, &["commit", "-qm", subject])?;
    Ok(())
}

fn oid(root: &Path, name: &str) -> Result<String> {
    Ok(git(root, &["rev-parse", "--verify", name])?
        .trim_end()
        .to_owned())
}

fn file(path: &str, staged: bool) -> Selection {
    Selection::File {
        path: path.into(),
        staged,
    }
}

fn files(tab: &Tab, staged: &[&str], unstaged: &[&str]) -> bool {
    tab.ui.files.len() == staged.len() + unstaged.len()
        && [(true, staged), (false, unstaged)]
            .iter()
            .all(|(side, names)| {
                names.iter().all(|name| {
                    tab.ui
                        .files
                        .iter()
                        .any(|row| row.staged == *side && row.path == Path::new(name))
                })
            })
}

fn patch(tab: &Tab, line: &str) -> bool {
    !tab.ui.loading_diff && tab.diff.is_some() && tab.ui.diff.iter().any(|row| row.text == line)
}

fn committed_file(tab: &Tab, name: &str, status: &str) -> bool {
    tab.ui
        .inspection
        .files
        .iter()
        .any(|row| row.path == Path::new(name) && row.status == status)
}

struct Replay<'a> {
    tab: crate::tabs::Front,
    cx: &'a mut AsyncApp,
    active: &'static str,
    passed: Vec<&'static str>,
}

impl Replay<'_> {
    fn start(&mut self, id: &'static str) {
        self.active = id;
    }

    fn pass(&mut self) {
        println!("{}", json!({"scenario": self.active, "status": "passed"}));
        self.passed.push(self.active);
    }

    fn action(&mut self, action: Action) -> Result<()> {
        self.tab
            .update(self.cx, |tab, window, cx| tab.dispatch(action, window, cx))?;
        Ok(())
    }

    fn open(&mut self, root: &Path) -> Result<()> {
        self.tab
            .update(self.cx, |tab, _, cx| tab.open(root.to_owned(), cx))?;
        Ok(())
    }

    async fn until(&mut self, description: &str, predicate: impl Fn(&Tab) -> bool) -> Result<()> {
        let start = Instant::now();
        loop {
            self.cx
                .update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            if self
                .tab
                .read_with(self.cx, |tab, _| tab.settled() && predicate(tab))?
            {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                let state = self.tab.read_with(self.cx, |tab, _| {
                    format!(
                        "branch={}, selection={:?}, files={:?}, notice={:?}",
                        tab.ui.branch, tab.ui.selection, tab.ui.files, tab.ui.notice_text()
                    )
                })?;
                return Err(format!("Timed out waiting for {description} ({state})").into());
            }
            self.cx
                .background_executor()
                .timer(Duration::from_millis(12))
                .await;
        }
    }

    async fn during(&mut self, description: &str, predicate: impl Fn(&Tab) -> bool) -> Result<()> {
        let start = Instant::now();
        loop {
            self.cx
                .update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            if self.tab.read_with(self.cx, |tab, _| predicate(tab))? {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                let notices = self.tab.read_with(self.cx, |tab, _| tab.ui.notice_text())?;
                return Err(format!("Timed out waiting for {description} (notice={notices:?})").into());
            }
            self.cx
                .background_executor()
                .timer(Duration::from_millis(12))
                .await;
        }
    }

    fn key(&mut self, key: &str) -> Result<()> {
        let stroke = Keystroke::parse(key)?;
        self.cx.update_window(self.tab.into(), |_, window, cx| {
            window.draw(cx).clear();
            window.dispatch_keystroke(stroke, cx);
        })?;
        Ok(())
    }

    fn type_commit(&mut self, subject: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.input.update(cx, |field, cx| {
                field.replace_text_in_range(None, subject, window, cx);
            })
        })?;
        self.key(if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        })
    }

    fn type_ref(&mut self, value: &str) -> Result<()> {
        let entered = self.tab.update(self.cx, |tab, window, cx| {
            if tab.ui.refs.draft.is_none() {
                return false;
            }
            tab.ui.refs.input.update(cx, |field, cx| {
                field.replace_text_in_range(None, value, window, cx);
                field.text() == value
            })
        })?;
        if !entered {
            return Err("Ref draft did not accept the supplied name".into());
        }
        Ok(())
    }

    async fn gap(&mut self, phase: u8, dir: &Path) -> Result<()> {
        match phase {
            1 => self.gap01(dir).await,
            2 => self.gap02(dir).await,
            3 => self.gap03(dir).await,
            4 => self.gap04(dir).await,
            5 => self.gap05(dir).await,
            6 => self.gap06(dir).await,
            _ => Err(format!("No replay is registered for gap phase {phase}.").into()),
        }
    }

    fn ref_action(&mut self, action: refs::Action) -> Result<()> {
        self.action(Action::Refs(action))
    }

    async fn ref_list(&mut self) -> Result<()> {
        if !self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.refs.list_open)?
        {
            self.ref_action(refs::Action::ToggleList)?;
        }
        self.until("LOCAL contains main", |tab| {
            tab.ui.refs.list_open
                && tab
                    .ui
                    .refs
                    .items
                    .iter()
                    .any(|r| r.kind == refs::Kind::Branch && r.name == "main")
        })
        .await
    }

    async fn select_commit(&mut self, hash: &str) -> Result<()> {
        self.action(Action::Select(Selection::Commit(hash.to_owned())))?;
        self.until("selected commit patch", |tab| {
            tab.ui.selection == Some(Selection::Commit(hash.to_owned()))
                && !tab.ui.loading_diff
                && tab.diff.is_some()
        })
        .await
    }

    async fn open_file(&mut self, name: &str) -> Result<()> {
        self.until("changed path listed", |tab| {
            tab.ui
                .inspection
                .files
                .iter()
                .any(|row| row.path == Path::new(name))
        })
        .await?;
        self.action(Action::Inspect(inspection::Action::OpenFile(name.into())))?;
        self.until("committed file patch", |tab| {
            tab.ui.inspection.path.as_deref() == Some(Path::new(name))
                && !tab.ui.loading_diff
                && tab.diff.is_some()
        })
        .await
    }

    async fn cedar(&mut self, dir: &Path) -> Result<()> {
        self.start("OBS01");
        let root = init(dir, "Cedar")?;
        record(&root, "README.md", "# Cedar\n", "Start Cedar guide")?;
        record(&root, "trail.txt", TRAIL, "Add trail description")?;
        let trail = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "feature/map"])?;
        record(
            &root,
            "map.txt",
            "Map: Follow the creek east.\n",
            "Add creek map",
        )?;
        git(&root, &["switch", "-q", "main"])?;
        fs::write(root.join("trail.txt"), WAYPOINT)?;
        fs::write(root.join("notes.txt"), NOTE)?;
        self.open(&root)?;
        self.until("Cedar graph and two unstaged paths", |tab| {
            tab.ui.repo == "Cedar"
                && tab.ui.branch == "main"
                && files(tab, &[], &["trail.txt", "notes.txt"])
                && [
                    "Start Cedar guide",
                    "Add trail description",
                    "Add creek map",
                ]
                .iter()
                .all(|name| tab.ui.history.iter().any(|row| row.subject == *name))
        })
        .await?;
        git_is(
            &root,
            &["status", "--porcelain=v1"],
            " M trail.txt\n?? notes.txt\n",
        )?;
        self.pass();

        self.start("OBS02");
        self.select_commit(&trail).await?;
        self.open_file("trail.txt").await?;
        self.until("Cedar author and committed tagline", |tab| {
            tab.ui.inspection.author == "Cedar Contributor"
                && patch(tab, "+FastKraken: git for the impatient")
                && !patch(tab, "+Waypoint: Willow bridge")
        })
        .await?;
        git_is(&root, &["show", "HEAD:trail.txt"], TRAIL)?;
        self.pass();

        self.start("OBS03");
        self.action(Action::ViewChanges)?;
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("unstaged waypoint patch", |tab| {
            tab.ui.selection == Some(file("trail.txt", false))
                && patch(tab, "+Waypoint: Willow bridge")
        })
        .await?;
        self.pass();

        self.start("OBS04");
        self.action(Action::StageFile)?;
        self.until("only the waypoint staged", |tab| {
            files(tab, &["trail.txt"], &["notes.txt"])
        })
        .await?;
        git_is(
            &root,
            &["status", "--porcelain=v1"],
            "M  trail.txt\n?? notes.txt\n",
        )?;
        git_is(&root, &["show", ":trail.txt"], WAYPOINT)?;
        self.pass();

        self.start("OBS05");
        self.action(Action::UnstageAll)?;
        self.until("both pending paths and empty index", |tab| {
            files(tab, &[], &["trail.txt", "notes.txt"])
        })
        .await?;
        self.action(Action::Select(file("trail.txt", false)))?;
        self.until("unstaged waypoint still present", |tab| {
            patch(tab, "+Waypoint: Willow bridge")
        })
        .await?;
        git_is(&root, &["show", ":trail.txt"], TRAIL)?;
        if fs::read_to_string(root.join("trail.txt"))? != WAYPOINT {
            return Err("Unstaging lost working bytes".into());
        }
        self.pass();

        self.start("OBS06");
        self.action(Action::StageFile)?;
        self.until("waypoint staged for first commit", |tab| {
            files(tab, &["trail.txt"], &["notes.txt"])
        })
        .await?;
        self.type_commit(WAYPOINT_COMMIT)?;
        self.until("selected waypoint commit and remaining note", |tab| {
            files(tab, &[], &["notes.txt"])
                && tab.ui.history.iter().any(|row| {
                    row.subject == WAYPOINT_COMMIT
                        && tab.ui.selection == Some(Selection::Commit(row.oid.clone()))
                })
                && tab.ui.inspection.files.len() == 1
                && committed_file(tab, "trail.txt", "Modified")
        })
        .await?;
        git_is(
            &root,
            &["log", "-1", "--format=%s", "main"],
            &format!("{WAYPOINT_COMMIT}\n"),
        )?;
        git_is(
            &root,
            &["diff", "--name-status", "HEAD^", "HEAD"],
            "M\ttrail.txt\n",
        )?;
        git_is(&root, &["status", "--porcelain=v1"], "?? notes.txt\n")?;
        self.pass();

        self.start("OBS09");
        self.action(Action::StageAll)?;
        self.until("only note staged", |tab| files(tab, &["notes.txt"], &[]))
            .await?;
        self.type_commit(NOTE_COMMIT)?;
        self.until("new tip with one added note", |tab| {
            files(tab, &[], &[])
                && tab.ui.history.iter().any(|row| {
                    row.subject == NOTE_COMMIT
                        && tab.ui.selection == Some(Selection::Commit(row.oid.clone()))
                })
                && tab.ui.inspection.files.len() == 1
                && committed_file(tab, "notes.txt", "Added")
        })
        .await?;
        git_is(
            &root,
            &["log", "-1", "--format=%s", "main"],
            &format!("{NOTE_COMMIT}\n"),
        )?;
        git_is(
            &root,
            &["diff", "--name-status", "HEAD^", "HEAD"],
            "A\tnotes.txt\n",
        )?;
        git_is(&root, &["show", "HEAD:notes.txt"], NOTE)?;
        self.pass();
        self.cedar_refs(&root).await
    }

    async fn cedar_refs(&mut self, root: &Path) -> Result<()> {
        let tip = oid(root, "main")?;
        self.start("OBS10");
        self.ref_action(refs::Action::New(refs::Kind::Branch))?;
        self.type_ref("draft-cancel")?;
        self.key("escape")?;
        self.until("dismissed draft and unchanged main", |tab| {
            tab.ui.refs.draft.is_none()
                && tab.ui.branch == "main"
                && !tab
                    .ui
                    .history
                    .iter()
                    .any(|r| r.refs.contains("draft-cancel"))
        })
        .await?;
        git_is(root, &["branch", "--list", "draft-cancel"], "")?;
        git_is(root, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        self.pass();

        self.start("OBS11");
        self.ref_action(refs::Action::New(refs::Kind::Branch))?;
        self.type_ref("review-walk")?;
        self.key("enter")?;
        self.until("new branch checked out at selected tip", |tab| {
            tab.ui.branch == "review-walk"
                && tab.ui.selection == Some(Selection::Commit(tip.clone()))
        })
        .await?;
        git_is(root, &["symbolic-ref", "--short", "HEAD"], "review-walk\n")?;
        git_is(root, &["rev-parse", "review-walk"], &format!("{tip}\n"))?;
        self.pass();

        self.start("OBS12");
        self.ref_list().await?;
        self.ref_action(refs::Action::Switch("main".into()))?;
        self.until("main checked out with Save bridge note selected", |tab| {
            tab.ui.branch == "main" && tab.ui.selection == Some(Selection::Commit(tip.clone()))
        })
        .await?;
        git_is(root, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        git_is(root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        self.pass();

        self.start("OBS21");
        git(root, &["switch", "-qc", "feature/detour"])?;
        record(
            root,
            "trail.txt",
            "FastKraken: git for the impatient\nWaypoint: Ridge pass\n",
            "Record ridge detour",
        )?;
        git(root, &["switch", "-q", "main"])?;
        record(
            root,
            "signpost.txt",
            "Follow the east signpost.\n",
            "Add east signpost",
        )?;
        let signpost = oid(root, "HEAD")?;
        git(
            root,
            &[
                "merge",
                "--no-ff",
                "-qm",
                "Merge feature/detour",
                "feature/detour",
            ],
        )?;
        let merge = oid(root, "HEAD")?;
        self.until("detour merge in graph", |tab| {
            tab.ui
                .history
                .iter()
                .any(|r| r.oid == merge && r.parents.first() == Some(&signpost))
        })
        .await?;
        self.select_commit(&signpost).await?;
        self.ref_action(refs::Action::New(refs::Kind::Tag))?;
        self.type_ref("signpost-v1")?;
        self.ref_action(refs::Action::Submit)?;
        self.until("tag label on earlier signpost", |tab| {
            tab.ui.branch == "main"
                && tab.ui.refs.show_labels
                && tab
                    .ui
                    .history
                    .iter()
                    .any(|r| r.oid == signpost && r.refs.contains("signpost-v1"))
        })
        .await?;
        git_is(
            root,
            &["rev-parse", "refs/tags/signpost-v1"],
            &format!("{signpost}\n"),
        )?;
        git_is(root, &["rev-parse", "main"], &format!("{merge}\n"))?;
        self.pass();
        Ok(())
    }

    async fn logbook(&mut self, dir: &Path) -> Result<()> {
        self.start("OBS22");
        let logbook_dir = dir.join("logbook");
        fs::create_dir(&logbook_dir)?;
        let root = init(&logbook_dir, "Cedar")?;
        record(&root, "README.md", "# Cedar\n", "Start Cedar guide")?;
        git(&root, &["switch", "-qc", "feature/logbook"])?;
        let mut original = String::from("North: Cedar gate\n");
        for i in 1..=20 {
            original.push_str(&format!("Survey marker {i:02}\n"));
        }
        original.push_str("South: East gate\n");
        record(&root, "logbook.txt", &original, "Add survey logbook")?;
        self.open(&root)?;
        self.until("clean logbook branch", |tab| {
            tab.ui.branch == "feature/logbook"
                && tab.ui.changed_count() == 0
                && tab.watcher.is_some()
                && tab
                    .ui
                    .history
                    .iter()
                    .any(|r| r.subject == "Add survey logbook")
        })
        .await?;
        let north = original.replace("North: Cedar gate", "North: River gate");
        let both = north.replace("South: East gate", "South: Orchard gate");
        fs::write(root.join("logbook.txt"), &both)?;
        self.until("watcher publishes one changed path", |tab| {
            tab.ui.changed_count() == 1 && files(tab, &[], &["logbook.txt"])
        })
        .await?;
        self.action(Action::ViewChanges)?;
        self.until("change opened from the banner", |tab| {
            tab.ui.selection == Some(file("logbook.txt", false))
                && files(tab, &[], &["logbook.txt"])
                && patch(tab, "+North: River gate")
                && patch(tab, "+South: Orchard gate")
        })
        .await?;
        git_is(&root, &["status", "--porcelain=v1"], " M logbook.txt\n")?;
        self.pass();

        self.start("OBS23");
        self.until("two independently stageable hunks", |tab| {
            tab.ui.diff.iter().filter(|r| r.hunk.is_some()).count() == 2
        })
        .await?;
        let north_hunk = self
            .tab
            .read_with(self.cx, |tab, _| {
                let mut hunk = None;
                for line in &tab.ui.diff {
                    if line.hunk.is_some() {
                        hunk = line.hunk;
                    }
                    if line.text == "+North: River gate" {
                        return hunk;
                    }
                }
                None
            })?
            .ok_or("North edit did not have a stageable patch header")?;
        self.action(Action::StageHunk(north_hunk))?;
        self.until("logbook path on both sides, one distinct change", |tab| {
            files(tab, &["logbook.txt"], &["logbook.txt"]) && tab.ui.changed_count() == 1
        })
        .await?;
        self.action(Action::Select(file("logbook.txt", true)))?;
        self.until("only north staged", |tab| {
            patch(tab, "+North: River gate") && !patch(tab, "+South: Orchard gate")
        })
        .await?;
        self.action(Action::Select(file("logbook.txt", false)))?;
        self.until("only south unstaged", |tab| {
            patch(tab, "+South: Orchard gate") && !patch(tab, "+North: River gate")
        })
        .await?;
        git_is(&root, &["show", "HEAD:logbook.txt"], &original)?;
        git_is(&root, &["show", ":logbook.txt"], &north)?;
        if fs::read_to_string(root.join("logbook.txt"))? != both {
            return Err("Hunk staging altered the file on disk".into());
        }
        self.pass();
        Ok(())
    }

    async fn notebook(&mut self, dir: &Path) -> Result<()> {
        self.start("EXT08");
        let root = init(dir, "Notebook")?;
        record(&root, "notes.txt", "Notes\n", "Start notebook")?;
        let start = oid(&root, "HEAD")?;
        record(&root, "checklist.txt", "Checklist\n", "Add checklist")?;
        record(&root, "notes.txt", "Notes\nRevision\n", "Revise notebook")?;
        let tip = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Notebook tip", |tab| {
            tab.ui.repo == "Notebook" && tab.ui.selection == Some(Selection::Commit(tip.clone()))
        })
        .await?;
        self.open_file("notes.txt").await?;
        self.action(Action::Inspect(inspection::Action::History))?;
        self.until(
            "file history lists only Revise notebook and Start notebook",
            |tab| {
                tab.ui.inspection.history.as_ref().is_some_and(|history| {
                    history
                        .iter()
                        .map(|r| r.subject.as_str())
                        .eq(["Revise notebook", "Start notebook"])
                })
            },
        )
        .await?;
        git_is(
            &root,
            &["log", "--format=%s", "--", "notes.txt"],
            "Revise notebook\nStart notebook\n",
        )?;
        self.pass();

        self.start("EXT01");
        self.select_commit(&start).await?;
        self.ref_list().await?;
        self.ref_action(refs::Action::ToggleLabels)?;
        self.ref_action(refs::Action::New(refs::Kind::Branch))?;
        self.type_ref("review-notes")?;
        self.ref_action(refs::Action::ToggleCheckout)?;
        self.ref_action(refs::Action::Submit)?;
        self.until("new branch listed with graph labels hidden", |tab| {
            !tab.ui.refs.show_labels
                && tab.ui.refs.lists(refs::Kind::Branch, "review-notes")
                && tab.ui.refs.items.iter().any(|r| {
                    r.kind == refs::Kind::Branch
                        && r.name == "review-notes"
                        && r.oid.as_ref() == Some(&start)
                })
        })
        .await?;
        self.select_commit(&tip).await?;
        self.ref_action(refs::Action::Inspect(
            refs::Kind::Branch,
            "review-notes".into(),
        ))?;
        self.until("branch inspection selects Start notebook on main", |tab| {
            tab.ui.selection == Some(Selection::Commit(start.clone())) && tab.ui.branch == "main"
        })
        .await?;
        git_is(&root, &["symbolic-ref", "--short", "HEAD"], "main\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(
            &root,
            &["rev-parse", "refs/heads/review-notes"],
            &format!("{start}\n"),
        )?;
        self.pass();

        self.start("EXT02");
        self.select_commit(&tip).await?;
        self.ref_action(refs::Action::New(refs::Kind::Tag))?;
        self.type_ref("release candidate")?;
        let invalid = self
            .tab
            .read_with(self.cx, |tab, cx| tab.ui.refs.validation(cx).is_some())?;
        if !invalid {
            return Err("Tag input did not show a validation error for a space".into());
        }
        self.ref_action(refs::Action::Submit)?;
        self.until("invalid form stays open", |tab| {
            tab.ui.refs.draft.is_some() && !tab.ui.refs.show_labels
        })
        .await?;
        self.key("escape")?;
        self.ref_list().await?;
        self.until("TAGS does not contain the invalid name", |tab| {
            !tab.ui
                .refs
                .items
                .iter()
                .any(|r| r.kind == refs::Kind::Tag && r.name == "release candidate")
        })
        .await?;
        git_is(&root, &["tag", "--list"], "")?;
        self.pass();
        Ok(())
    }
}

pub(crate) fn replaying() -> bool {
    static REPLAYING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REPLAYING.get_or_init(|| std::env::var_os(CHILD).is_some())
}

pub(crate) fn sandbox() -> Option<PathBuf> {
    std::env::var_os(CHILD).map(PathBuf::from).filter(|path| path.is_absolute())
}

fn global_config(sandbox: &Path) -> PathBuf {
    sandbox.join("gitconfig")
}

const CHILD: &str = "FASTKRAKEN_ACCEPTANCE_PROCESS";

fn hidden(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1180.), px(760.)), cx))),
        show: false,
        focus: false,
        ..Default::default()
    }
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!(
        "{}",
        json!({"result":"failed", "error":message.to_string()})
    );
    std::process::exit(1)
}

pub fn run(wave: Wave) {
    let Some(sandbox) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let sandbox = tempfile::Builder::new()
            .prefix("fastkraken-acceptance-")
            .tempdir()
            .unwrap_or_else(|e| fail(e));
        let mut child =
            std::process::Command::new(std::env::current_exe().unwrap_or_else(|e| fail(e)));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        #[cfg(feature = "publish-replay")]
        if matches!(wave, Wave::Publish) {
            child.env("GIT_ALLOW_PROTOCOL", "file");
        }
        let global = global_config(sandbox.path());
        fs::write(&global, "").unwrap_or_else(|e| fail(e));
        let status = child
            .arg(wave.flag())
            .env(CHILD, sandbox.path())
            .env(storage::STATE_DIR, sandbox.path().join("state"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global)
            .status()
            .unwrap_or_else(|e| fail(e));
        drop(sandbox);
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        return;
    };
    let Some(state) = storage::override_directory().filter(|dir| dir.starts_with(&sandbox)) else {
        fail("The acceptance replay keeps FastKraken's state inside its sandbox.");
    };
    fs::create_dir_all(sandbox.join("Applications/Terminal.app")).unwrap_or_else(|e| fail(e));
    Application::new().run(move |cx: &mut App| {
        view::bindings(cx);
        settings::load(Some(state), cx);
        let handle = cx
            .open_window(hidden(cx), |window, cx| cx.new(|cx| crate::tabs::Workspace::new(window, cx, None)))
            .unwrap_or_else(|err| fail(err));
        cx.spawn(async move |cx| {
            let mut replay = Replay { tab: crate::tabs::Front(handle), cx, active: "setup", passed: Vec::new() };
            let scenarios = wave.scenarios();
            let result: Result<()> = async {
                match wave {
                    Wave::One => {
                        replay.cedar(&sandbox).await?;
                        replay.logbook(&sandbox).await?;
                        replay.notebook(&sandbox).await
                    }
                    Wave::Two => {
                        replay.stashes(&sandbox).await?;
                        replay.merges(&sandbox).await?;
                        replay.reversals(&sandbox).await
                    }
                    Wave::Three => replay.upstream(&sandbox).await,
                    Wave::Gap(phase) => replay.gap(phase, &sandbox).await,
                    #[cfg(feature = "publish-replay")]
                    Wave::Publish => replay.publishing(&sandbox).await,
                }
            }.await;
            if let Err(err) = result {
                println!("{}", json!({"scenario":replay.active, "status":"failed", "error":err.to_string()}));
                for id in scenarios.iter().filter(|id| **id != replay.active && !replay.passed.contains(id)) {
                    println!("{}", json!({"scenario":id, "status":"not_run"}));
                }
                fail(err);
            }
            if replay.passed.as_slice() != scenarios { fail("Incomplete scenario replay"); }
            println!("{}", json!({"result":"complete", "passed":replay.passed.len(), "driver":"offscreen_gpui_actions"}));
            replay.cx.update(|cx| cx.quit()).unwrap_or_else(|err| fail(err));
        }).detach();
    });
}
