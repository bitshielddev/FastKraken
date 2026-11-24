use super::*;
use crate::{
    branch,
    commands::{self, Cursor, Key, Target},
    stash, sync,
    notice,
    prompt::{self, Stop},
    view::Tone,
};
use gpui::{Focusable, Point};
use std::ffi::OsStr;

type Shown = (String, Vec<String>, bool);

type Said = (Tone, String, String, Vec<String>);

const CMD_RETURN: &str = if cfg!(target_os = "macos") { "cmd-enter" } else { "ctrl-enter" };
const SELECT_ALL: &str = if cfg!(target_os = "macos") { "cmd-a" } else { "ctrl-a" };

impl Replay<'_> {
    pub(super) async fn gap02(&mut self, dir: &Path) -> Result<()> {
        self.delete_merged(dir).await?;
        self.delete_unmerged(dir).await?;
        self.delete_checked_out(dir).await?;
        self.rename(dir).await?;
        self.checkout_remote(dir).await?;
        self.checkout_diverged(dir).await?;
        self.checkout_tag(dir).await?;
        self.hard_reset(dir).await?;
        self.mixed_reset(dir).await?;
        self.merge_remote(dir).await?;
        self.merge_nothing(dir).await?;
        self.tag_and_push(dir).await?;
        self.delete_tag(dir).await?;
        self.delete_moved_remote(dir).await?;
        self.fast_forward_other(dir).await?;
        self.force_after_amend(dir).await?;
        self.force_stale(dir).await?;
        self.rejected_push(dir).await?;
        self.stash_staged(dir).await?;
        self.drop_stash(dir).await?;
        self.pop_older(dir).await
    }

    fn shown(&mut self) -> Result<Option<Shown>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Branch(_) | prompt::Kind::Tag(_) | prompt::Kind::Tracking(_) | prompt::Kind::Sync(_) | prompt::Kind::Stash(_)))
                .then(|| prompt::shown(&tab.ui))
                .flatten()
                .map(|spec| (spec.title, spec.lines, spec.danger))
        })?)
    }

    fn said(&mut self) -> Result<Option<Said>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().map(|n| {
                let offers = n.actions.iter().map(|(label, _)| label.to_string()).collect();
                (n.tone, n.title.clone(), n.detail.clone(), offers)
            })
        })?)
    }

    async fn choose_on(&mut self, target: &Target, id: &str) -> Result<()> {
        self.until("the branch listed", |tab| tab.ui.refs.oid_of(target).is_some()).await?;
        self.choose(target, id)
    }

    fn choose_under(&mut self, target: &Target, id: &str, title: &str) -> Result<()> {
        self.action(Action::Menu(Some(view::Menu::Object { target: target.clone(), at: Point::default() })))?;
        let chosen = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| entry.children.into_iter().find(|child| child.title == title))
                .and_then(|child| child.action)
        })?;
        let action = chosen.ok_or_else(|| format!("The {id} submenu of {target:?} has no enabled {title}"))?;
        self.action(action)
    }

    fn newest_notice(&mut self) -> Result<u64> {
        let id = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().map(|n| n.id))?;
        id.ok_or_else(|| "There is no notice".into())
    }

    pub(super) fn choose(&mut self, target: &Target, id: &str) -> Result<()> {
        self.action(Action::Menu(Some(view::Menu::Object { target: target.clone(), at: Point::default() })))?;
        let chosen = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx).into_iter().find(|entry| entry.id == id).and_then(|entry| entry.action)
        })?;
        let action = chosen.ok_or_else(|| format!("The menu of {target:?} has no enabled {id}"))?;
        self.action(action)
    }

    async fn asked(&mut self, title: &str) -> Result<Shown> {
        self.until("the branch prompt open", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Branch(_) | prompt::Kind::Tag(_) | prompt::Kind::Tracking(_) | prompt::Kind::Sync(_) | prompt::Kind::Stash(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == title)
        })
        .await?;
        self.shown()?.ok_or_else(|| "The branch prompt closed".into())
    }

    async fn delete_merged(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0201");
        let root = init(dir, "Pruning")?;
        record(&root, "trail.txt", TRAIL, "Start the pruning trail")?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        git(&root, &["branch", "feature/merged"])?;
        let tip = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Pruning open on main", |tab| tab.ui.repo == "Pruning" && tab.ui.branch == "main")
            .await?;
        let merged = Target::Branch("feature/merged".into());
        self.choose_on(&merged, "branch.delete").await?;
        let shown = self.asked("Delete branch feature/merged?").await?;
        if shown.1 != ["Its commits are also on other branches or tags."] || shown.2 {
            return Err(format!("The delete prompt for a merged branch shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("The ordinary delete prompt did not open with Delete branch focused".into());
        }
        self.key("enter")?;
        let short = notice::short(&tip).to_owned();
        self.until("the branch deleted", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.refs.oid_of(&merged).is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted branch feature/merged")
        })
        .await?;
        let said = self.said()?;
        let expected = (Tone::Success, "Deleted branch feature/merged".to_owned(), format!("was {short}"), vec!["Restore".to_owned()]);
        if said.as_ref() != Some(&expected) {
            return Err(format!("Deleting feature/merged said {said:?}").into());
        }
        git_is(&root, &["for-each-ref", "refs/heads/feature/merged"], "")?;
        git_is(&root, &["rev-parse", "main"], &format!("{tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn delete_unmerged(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0202");
        let root = init(dir, "Ridge")?;
        record(&root, "trail.txt", TRAIL, "Start the ridge trail")?;
        let main = oid(&root, "main")?;
        git(&root, &["switch", "-qc", "feature/ridge"])?;
        record(&root, "ridge.txt", "Ridge: marked\n", "Mark the ridge")?;
        record(&root, "ridge.txt", "Ridge: marked\nSummit: reached\n", "Climb the ridge")?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["config", "branch.feature/ridge.remote", "origin"])?;
        git(&root, &["config", "branch.feature/ridge.merge", "refs/heads/feature/ridge"])?;
        let tip = oid(&root, "feature/ridge")?;
        let (climb, mark) = (oid(&root, "feature/ridge")?, oid(&root, "feature/ridge^")?);
        self.open(&root)?;
        self.until("Ridge open on main", |tab| tab.ui.repo == "Ridge" && tab.ui.branch == "main").await?;
        let ridge = Target::Branch("feature/ridge".into());
        let lines = vec![
            "2 commits are only on this branch. They will no longer appear in the graph.".to_owned(),
            format!("{}  Climb the ridge", notice::short(&climb)),
            format!("{}  Mark the ridge", notice::short(&mark)),
        ];
        let still = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "refs/heads/feature/ridge"], &format!("{tip}\n"))
                .map_err(|e| format!("{what}: {e}").into())
        };

        self.choose_on(&ridge, "branch.delete").await?;
        let shown = self.asked("Delete branch feature/ridge?").await?;
        if shown.1 != lines || !shown.2 {
            return Err(format!("The delete prompt for an unmerged branch shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The danger delete prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("Return cancelled the delete", |tab| !tab.ui.prompt.open()).await?;
        still("Return on the danger prompt")?;

        self.choose_on(&ridge, "branch.delete").await?;
        self.asked("Delete branch feature/ridge?").await?;
        self.key(CMD_RETURN)?;
        self.until("the unmerged branch deleted", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.refs.oid_of(&ridge).is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted branch feature/ridge")
        })
        .await?;
        git_is(&root, &["for-each-ref", "refs/heads/feature/ridge"], "")?;
        let config = git(&root, &["config", "--local", "--list"])?;
        if config.contains("branch.feature/ridge.") {
            return Err(format!("The deleted branch's upstream setting is still there: {config}").into());
        }
        git_is(&root, &["rev-parse", "main"], &format!("{main}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;

        let offered = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().filter(|n| n.actions.len() == 1 && n.actions[0].0.as_ref() == "Restore").map(|n| n.id)
        })?;
        let offered = offered.ok_or("The delete notice offers no Restore")?;
        self.cx.background_executor().timer(Duration::from_millis(5500)).await;
        self.until("the delete notice still shown", |tab| tab.ui.notices.get(offered).is_some()).await?;

        git(&root, &["switch", "-qc", "detour"])?;
        record(&root, "trail.txt", "FastKraken: git for the impatient\nDetour: river crossing\n", "Take the detour")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        self.action(Action::Refresh)?;
        self.begin_merge("detour").await?;
        self.until("the merge of detour stopped", |tab| {
            tab.ui.operation.active() && !tab.ui.busy() && !tab.ui.prompt.open()
        })
        .await?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the branch restored", |tab| {
            tab.ui.refs.oid_of(&ridge) == Some(tip.as_str())
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Restored branch feature/ridge")
        })
        .await?;
        if !self.tab.read_with(self.cx, |tab, _| tab.ui.operation.active())? {
            return Err("Restoring the branch ended the merge".into());
        }
        let said = self.said()?;
        let expected = (Tone::Success, "Restored branch feature/ridge".to_owned(), format!("at {}", notice::short(&tip)), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Restoring feature/ridge said {said:?}").into());
        }
        still("Restore")?;
        git_is(&root, &["config", "branch.feature/ridge.remote"], "origin\n")?;
        git_is(&root, &["config", "branch.feature/ridge.merge"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git(&root, &["merge", "--abort"])?;
        self.action(Action::Refresh)?;
        self.until("the merge of detour aborted", |tab| !tab.ui.operation.active() && !tab.ui.busy()).await?;
        self.pass();
        Ok(())
    }

    async fn delete_checked_out(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0203");
        let root = dir.join("Pruning");
        let tip = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Pruning open on main", |tab| {
            tab.ui.repo == "Pruning" && tab.ui.branch == "main" && tab.ui.refs.branch("main").is_some()
        })
        .await?;
        let main = Target::Branch("main".into());
        let reason = "Switch to another branch first";
        let rows = self.menu_rows(&main)?;
        let row = rows.iter().position(|row| row.0 == "branch.delete");
        let Some(row) = row.filter(|row| rows[*row].1 == "Delete main…" && rows[*row].2.as_deref() == Some(reason)) else {
            return Err(format!("The menu of main lists {rows:?}").into());
        };
        let enabled = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &main, cx).into_iter().any(|entry| entry.id == "branch.delete" && entry.action.is_some())
        })?;
        if enabled {
            return Err("Delete main… can be chosen".into());
        }
        self.action(Action::Menu(Some(view::Menu::Object { target: main.clone(), at: Point::default() })))?;
        for _ in 0..=row {
            self.action(Action::MenuKey(Key::Down))?;
        }
        let cursor = self.tab.read_with(self.cx, |tab, _| tab.ui.menu_cursor)?;
        if cursor != (Cursor { row: Some(row), open: false, sub: None }) {
            return Err(format!("Down {} times highlighted {cursor:?}", row + 1).into());
        }
        self.action(Action::MenuKey(Key::Enter))?;
        self.until("the reason in an information notice", |tab| {
            tab.ui.menu.is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.title == reason)
        })
        .await?;
        self.action(Action::Branch(branch::Action::Delete("main".into())))?;
        let asked = self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())?;
        if asked {
            return Err("Deleting the checked-out branch opened a prompt".into());
        }
        git_is(&root, &["rev-parse", "refs/heads/main"], &format!("{tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    fn answers(&mut self) -> Result<(String, Vec<(String, bool)>)> {
        let read = self.tab.read_with(self.cx, |tab, _| {
            let spec = prompt::shown(&tab.ui)?;
            let choices = spec.choices.iter().map(|c| (c.label.to_string(), c.danger)).collect();
            Some((spec.confirm.to_string(), choices))
        })?;
        read.ok_or_else(|| "No prompt is open".into())
    }

    fn field(&mut self) -> Result<(String, Option<String>)> {
        let read = self.tab.read_with(self.cx, |tab, cx| {
            let text = tab.ui.prompt.field()?.read(cx).text().to_owned();
            let spec = prompt::shown(&tab.ui)?;
            let problem = (spec.field?.validate)(&text, &tab.ui);
            Some((text, problem))
        })?;
        read.ok_or_else(|| "The prompt has no field".into())
    }

    async fn rename(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0204");
        let root = init(dir, "Survey")?;
        record(&root, "trail.txt", TRAIL, "Start the survey trail")?;
        git(&root, &["switch", "-qc", "feature/map"])?;
        record(&root, "map.txt", "Map: north ridge\n", "Draw the north ridge")?;
        git(&root, &["switch", "-q", "main"])?;
        let remote = dir.join("Survey-origin.git");
        git(dir, &["init", "-q", "--bare", remote.to_str().ok_or("Scratch path is not UTF-8")?])?;
        git(&root, &["remote", "add", "origin", remote.to_str().ok_or("Scratch path is not UTF-8")?])?;
        git(&root, &["push", "-q", "-u", "origin", "feature/map"])?;
        let tip = oid(&root, "feature/map")?;
        let head = oid(&root, "main")?;
        self.open(&root)?;
        self.until("Survey open with feature/map tracking origin", |tab| {
            tab.ui.repo == "Survey"
                && tab.ui.branch == "main"
                && tab.ui.refs.branch("feature/map").is_some_and(|item| item.upstream.as_deref() == Some("origin/feature/map"))
        })
        .await?;
        let renamed = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "refs/heads/feature/map"], &format!("{tip}\n"))?;
            git_is(&root, &["for-each-ref", "--format=%(refname)", "refs/heads"], "refs/heads/feature/map\nrefs/heads/main\n")
                .map_err(|e| format!("{what}: {e}").into())
        };

        self.action(Action::Select(Selection::Commit(head.clone())))?;
        self.run_command("branch.rename")?;
        let shown = self.asked("Rename main").await?;
        if !shown.1.is_empty() || shown.2 {
            return Err(format!("Renaming main, which has no upstream, shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Field) {
            return Err("The rename prompt did not put the cursor in its field".into());
        }
        self.type_keys("x")?;
        self.key("escape")?;
        self.until("Esc closed the rename prompt", |tab| !tab.ui.prompt.open()).await?;
        renamed("Esc")?;

        let map = Target::Branch("feature/map".into());
        self.choose_on(&map, "branch.rename").await?;
        let shown = self.asked("Rename feature/map").await?;
        let line = "origin/feature/map keeps its name. Push the branch afterwards to publish it under the new name.";
        if shown.1 != [line] || shown.2 {
            return Err(format!("Renaming feature/map shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Field) {
            return Err("The rename prompt did not put the cursor in its field".into());
        }
        let field = self.field()?;
        if field != ("feature/map".to_owned(), Some(String::new())) {
            return Err(format!("The rename field opened as {field:?}").into());
        }
        self.key("enter")?;
        if self.field()?.0 != "feature/map" {
            return Err(format!("Return on the unchanged name left {:?}", self.field()?).into());
        }
        for (typed, problem) in [("main", "A branch named main already exists"), ("map board", "Branch names cannot contain spaces")] {
            self.key(SELECT_ALL)?;
            self.type_keys(typed)?;
            let field = self.field()?;
            if field != (typed.to_owned(), Some(problem.to_owned())) {
                return Err(format!("Typing {typed:?} gave {field:?}").into());
            }
            self.key("enter")?;
            if self.field()?.0 != typed {
                return Err(format!("Return on {typed:?} left {:?}", self.field()?).into());
            }
        }
        let open = self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open() && !tab.ui.busy())?;
        if !open {
            return Err("Return on a name that cannot be used closed the prompt".into());
        }
        renamed("Return on a name that cannot be used")?;

        self.key(SELECT_ALL)?;
        self.type_keys("trail/map")?;
        if self.field()? != ("trail/map".to_owned(), None) {
            return Err(format!("Typing trail/map gave {:?}", self.field()?).into());
        }
        self.key("enter")?;
        self.until("the branch renamed", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.refs.branch("trail/map").is_some()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Renamed feature/map to trail/map")
        })
        .await?;
        let said = self.said()?;
        let detail = "origin/feature/map keeps its name. Push trail/map to publish it under the new name.";
        let expected = (Tone::Success, "Renamed feature/map to trail/map".to_owned(), detail.to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Renaming feature/map said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/heads/trail/map"], &format!("{tip}\n"))?;
        git_is(&root, &["for-each-ref", "refs/heads/feature/map"], "")?;
        git_is(&root, &["for-each-ref", "--format=%(upstream:short)", "refs/heads/trail/map"], "origin/feature/map\n")?;
        git_is(&root, &["config", "branch.trail/map.merge"], "refs/heads/feature/map\n")?;
        git_is(&remote, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"], &format!("refs/heads/feature/map {tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    fn crossing(dir: &Path, name: &str, branches: &[&str]) -> Result<(PathBuf, PathBuf)> {
        let root = init(dir, name)?;
        record(&root, "trail.txt", TRAIL, "Start the crossing")?;
        let origin = dir.join(format!("{name}-origin.git"));
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", path])?;
        git(&root, &["remote", "add", "origin", path])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        for branch in branches {
            git(&root, &["branch", branch])?;
            git(&root, &["push", "-q", "origin", branch])?;
            git(&root, &["branch", "-D", branch])?;
        }
        Ok((root, origin))
    }

    async fn checkout_remote(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0205");
        let (root, origin) = Self::crossing(dir, "Crossing", &["feature/bridge", "feature/ford"])?;
        let fork = dir.join("Crossing-fork.git");
        let fork_path = fork.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", fork_path])?;
        git(&root, &["remote", "add", "team/fork", fork_path])?;
        git(&root, &["switch", "-q", "-c", "survey/east"])?;
        record(&root, "east.txt", "East: surveyed\n", "Survey the east bank")?;
        git(&root, &["push", "-q", "team/fork", "survey/east"])?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["branch", "-D", "survey/east"])?;
        let base = oid(&root, "main")?;
        let east = oid(&root, "refs/remotes/team/fork/survey/east")?;
        self.open(&root)?;
        let (bridge, ford) = (Target::RemoteBranch("origin/feature/bridge".into()), Target::RemoteBranch("origin/feature/ford".into()));
        let surveyed = Target::RemoteBranch("team/fork/survey/east".into());
        self.until("Crossing open with its remote branches", |tab| {
            tab.ui.repo == "Crossing"
                && tab.ui.branch == "main"
                && [&bridge, &ford, &surveyed].iter().all(|target| tab.ui.refs.oid_of(target).is_some())
                && tab.ui.history.iter().any(|row| row.oid == base && row.refs.contains("origin/feature/bridge"))
        })
        .await?;
        let tracked = |local: &str, upstream: &str, tip: &str| -> Result<()> {
            git_is(&root, &["symbolic-ref", "HEAD"], &format!("refs/heads/{local}\n"))?;
            git_is(&root, &["rev-parse", &format!("refs/heads/{local}")], &format!("{tip}\n"))?;
            git_is(&root, &["for-each-ref", "--format=%(upstream:short)", &format!("refs/heads/{local}")], &format!("{upstream}\n"))?;
            git_is(&root, &["status", "--porcelain=v1"], "")
        };

        let rows = self.menu_rows(&bridge)?;
        if !rows.iter().any(|row| row.0 == "branch.checkout-remote" && row.1 == "Check out origin/feature/bridge" && row.2.is_none()) {
            return Err(format!("The menu of origin/feature/bridge lists {rows:?}").into());
        }
        let rows = self.menu_rows(&Target::RemoteBranch("origin/main".into()))?;
        if !rows.iter().any(|row| row.0 == "branch.checkout-remote" && row.2.as_deref() == Some("main is already checked out")) {
            return Err(format!("The menu of origin/main lists {rows:?}").into());
        }

        self.action(view::label_click(&base, &bridge, 2))?;
        self.until("feature/bridge checked out", |tab| {
            tab.ui.branch == "feature/bridge" && tab.ui.notices.newest().is_some_and(|n| n.title == "Switched to feature/bridge")
        })
        .await?;
        let said = self.said()?;
        let expected = (Tone::Success, "Switched to feature/bridge".to_owned(), "A new branch that tracks origin/feature/bridge.".to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Checking out origin/feature/bridge said {said:?}").into());
        }
        tracked("feature/bridge", "origin/feature/bridge", &base)?;
        self.until("the window tracking origin/feature/bridge", |tab| {
            tab.ui.upstream.as_deref() == Some("origin/feature/bridge")
        })
        .await?;

        self.action(refs::sidebar_click(refs::Kind::Remote, false, OsStr::new("team/fork/survey/east"), 2))?;
        self.until("survey/east checked out", |tab| tab.ui.branch == "survey/east").await?;
        tracked("survey/east", "team/fork/survey/east", &east)?;

        self.ref_list().await?;
        self.find_text("feature/ford")?;
        let (found, create, at) = self.tab.read_with(self.cx, |tab, cx| refs::State::switcher_rows(&tab.ui, cx))?;
        let remotes = self.tab.read_with(self.cx, |tab, cx| refs::State::switcher_remotes(&tab.ui, cx))?;
        if !found.is_empty() || create.as_deref() != Some("feature/ford") || remotes != ["origin/feature/ford"] || at != Some(1) {
            return Err(format!("Typing feature/ford showed {found:?}, {create:?}, {remotes:?} and the highlight on {at:?}").into());
        }
        self.find_text("o")?;
        let remotes = self.tab.read_with(self.cx, |tab, cx| refs::State::switcher_remotes(&tab.ui, cx))?;
        if remotes != ["origin/feature/bridge", "origin/feature/ford", "origin/main", "team/fork/survey/east"] {
            return Err(format!("Typing o listed the remote branches {remotes:?}").into());
        }
        self.find_text("feature/ford")?;
        self.key("enter")?;
        self.until("feature/ford checked out from the switcher", |tab| {
            tab.ui.branch == "feature/ford" && !tab.ui.refs.list_open
        })
        .await?;
        tracked("feature/ford", "origin/feature/ford", &base)?;
        git_is(&origin, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"], &format!(
            "refs/heads/feature/bridge {base}\nrefs/heads/feature/ford {base}\nrefs/heads/main {base}\n"
        ))?;
        self.pass();
        Ok(())
    }

    fn find_text(&mut self, text: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.refs.find.update(cx, |field, cx| {
                field.set_text("", cx);
                field.replace_text_in_range(None, text, window, cx);
            })
        })?;
        Ok(())
    }

    async fn checkout_diverged(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0206");
        let (root, origin) = Self::crossing(dir, "Ridgeway", &["feature/ridge"])?;
        git(&root, &["switch", "-q", "--detach", "origin/feature/ridge"])?;
        record(&root, "ridge.txt", "Ridge: cairn on the remote\n", "Build the remote cairn")?;
        git(&root, &["push", "-q", "origin", "HEAD:refs/heads/feature/ridge"])?;
        let remote_tip = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "-c", "feature/ridge", "main"])?;
        git(&root, &["branch", "-q", "--set-upstream-to=origin/feature/ridge"])?;
        record(&root, "ridge.txt", "Ridge: local note\n", "Note the local ridge")?;
        let local_tip = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        let base = oid(&root, "main")?;
        self.open(&root)?;
        let ridge = Target::RemoteBranch("origin/feature/ridge".into());
        self.until("Ridgeway open with both ridges", |tab| {
            tab.ui.repo == "Ridgeway"
                && tab.ui.branch == "main"
                && tab.ui.refs.oid_of(&ridge) == Some(remote_tip.as_str())
                && tab.ui.refs.local(OsStr::new("feature/ridge")).and_then(|item| item.oid.as_deref()) == Some(local_tip.as_str())
        })
        .await?;
        let untouched = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "refs/heads/feature/ridge", "refs/remotes/origin/feature/ridge"], &format!("{local_tip}\n{remote_tip}\n"))
                .and_then(|()| git_is(&root, &["for-each-ref", "--format=%(upstream:short)", "refs/heads/feature/ridge"], "origin/feature/ridge\n"))
                .and_then(|()| git_is(&origin, &["rev-parse", "refs/heads/feature/ridge"], &format!("{remote_tip}\n")))
                .map_err(|e| format!("{what}: {e}").into())
        };
        let lines = vec![
            "feature/ridge already exists, with 1 commit that origin/feature/ridge does not have. It is also 1 commit behind.".to_owned(),
            format!("{}  Note the local ridge", notice::short(&local_tip)),
            "Resetting feature/ridge to origin/feature/ridge takes this commit off feature/ridge.".to_owned(),
        ];
        let three = (
            "Switch to feature/ridge as it is".to_owned(),
            vec![("Create another branch…".to_owned(), false), ("Reset feature/ridge to origin/feature/ridge".to_owned(), true)],
        );

        self.choose_on(&ridge, "branch.checkout-remote").await?;
        let shown = self.asked("Check out origin/feature/ridge").await?;
        if shown.1 != lines || shown.2 {
            return Err(format!("The diverged prompt shows {shown:?}").into());
        }
        if self.answers()? != three {
            return Err(format!("The diverged prompt offers {:?}", self.answers()?).into());
        }
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("The diverged prompt did not open with Switch as it is focused".into());
        }
        self.key("tab")?;
        self.key("tab")?;
        if self.prompt_focus()? != Some(Stop::Choice(1)) {
            return Err(format!("Tab twice focused {:?}, not Reset", self.prompt_focus()?).into());
        }
        self.key("enter")?;
        self.until("Return on Reset cancelled", |tab| !tab.ui.prompt.open()).await?;
        untouched("Return on Reset")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;

        self.choose_on(&ridge, "branch.checkout-remote").await?;
        self.asked("Check out origin/feature/ridge").await?;
        self.key("tab")?;
        if self.prompt_focus()? != Some(Stop::Choice(0)) {
            return Err(format!("Tab focused {:?}, not Create another branch", self.prompt_focus()?).into());
        }
        self.key("enter")?;
        let shown = self.asked("Create a branch from origin/feature/ridge").await?;
        if shown.1 != ["The new branch starts at origin/feature/ridge and tracks it. feature/ridge stays as it is."] || shown.2 {
            return Err(format!("The new branch prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Field) {
            return Err("The new branch prompt did not put the cursor in its field".into());
        }
        self.key("enter")?;
        self.type_keys("feature/ridge")?;
        if self.field()? != ("feature/ridge".to_owned(), Some("A branch named feature/ridge already exists".to_owned())) {
            return Err(format!("Typing feature/ridge gave {:?}", self.field()?).into());
        }
        self.key("enter")?;
        let open = self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())?;
        if !open {
            return Err("Return on a taken name closed the prompt".into());
        }
        self.key(SELECT_ALL)?;
        self.type_keys("ridge/remote")?;
        self.key("enter")?;
        self.until("ridge/remote checked out", |tab| {
            !tab.ui.prompt.open() && tab.ui.branch == "ridge/remote"
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Switched to ridge/remote")
        })
        .await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/ridge/remote\n")?;
        git_is(&root, &["rev-parse", "refs/heads/ridge/remote"], &format!("{remote_tip}\n"))?;
        git_is(&root, &["for-each-ref", "--format=%(upstream:short)", "refs/heads/ridge/remote"], "origin/feature/ridge\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        untouched("Create another branch")?;

        self.choose_on(&ridge, "branch.checkout-remote").await?;
        self.asked("Check out origin/feature/ridge").await?;
        self.key("enter")?;
        self.until("feature/ridge checked out as it is", |tab| {
            !tab.ui.prompt.open() && tab.ui.branch == "feature/ridge"
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Switched to feature/ridge")
        })
        .await?;
        let said = self.said()?;
        let detail = "It has 1 commit that origin/feature/ridge does not, and is 1 commit behind.";
        if said.as_ref().map(|said| said.2.as_str()) != Some(detail) {
            return Err(format!("Switching as it is said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        untouched("Switch as it is")?;
        let rows = self.menu_rows(&ridge)?;
        if !rows.iter().any(|row| row.0 == "branch.checkout-remote" && row.2.as_deref() == Some("feature/ridge is already checked out")) {
            return Err(format!("With feature/ridge checked out, the menu of origin/feature/ridge lists {rows:?}").into());
        }

        git(&root, &["switch", "-q", "main"])?;
        self.action(Action::Refresh)?;
        self.until("back on main", |tab| tab.ui.branch == "main").await?;
        self.choose_on(&ridge, "branch.checkout-remote").await?;
        self.asked("Check out origin/feature/ridge").await?;
        self.key("tab")?;
        self.key("tab")?;
        self.key("space")?;
        self.until("feature/ridge reset to origin/feature/ridge", |tab| {
            !tab.ui.prompt.open() && tab.ui.branch == "feature/ridge"
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Reset feature/ridge to origin/feature/ridge")
        })
        .await?;
        let said = self.said()?;
        let detail = format!("Now on feature/ridge. It was at {}.", notice::short(&local_tip));
        if said.as_ref().map(|said| said.2.as_str()) != Some(detail.as_str()) {
            return Err(format!("The reset said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/ridge\n")?;
        git_is(&root, &["rev-parse", "refs/heads/feature/ridge"], &format!("{remote_tip}\n"))?;
        git_is(&root, &["for-each-ref", "--format=%(upstream:short)", "refs/heads/feature/ridge"], "origin/feature/ridge\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        git_is(&origin, &["rev-parse", "refs/heads/feature/ridge", "refs/heads/main"], &format!("{remote_tip}\n{base}\n"))?;
        self.pass();
        Ok(())
    }

    fn detached_panel(&mut self) -> Result<(Option<&'static str>, Option<Action>)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let button = commands::find("repo.new-branch")
                .and_then(|command| commands::entry(command, &tab.ui, &Target::Repo, cx))
                .and_then(|entry| entry.action);
            (view::detached_warning(&tab.ui), button)
        })?)
    }

    async fn checkout_tag(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0207");
        let root = init(dir, "Signpost")?;
        record(&root, "trail.txt", TRAIL, "Start the signpost trail")?;
        git(&root, &["tag", "-a", "-m", "First signpost", "v1.0"])?;
        let (base, tag_object) = (oid(&root, "HEAD")?, oid(&root, "refs/tags/v1.0")?);
        record(&root, "trail.txt", WAYPOINT, WAYPOINT_COMMIT)?;
        let tip = oid(&root, "HEAD")?;
        let origin = dir.join("Signpost-origin.git");
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", path])?;
        git(&root, &["remote", "add", "origin", path])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        self.open(&root)?;
        let v1 = Target::Tag("v1.0".into());
        self.until("Signpost open on main with v1.0 and an upstream", |tab| {
            tab.ui.repo == "Signpost"
                && tab.ui.branch == "main"
                && tab.ui.upstream.as_deref() == Some("origin/main")
                && tab.ui.refs.oid_of(&v1) == Some(base.as_str())
                && tab.ui.history.iter().any(|row| row.oid == base && row.refs.contains("tag: v1.0"))
        })
        .await?;
        let unmoved = |what: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "refs/heads/main", "refs/tags/v1.0"], &format!("{tip}\n{tag_object}\n"))
                .and_then(|()| git_is(&root, &["for-each-ref", "--format=%(refname)", "refs/heads"], "refs/heads/main\n"))
                .and_then(|()| git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{tip}\n")))
                .map_err(|e| format!("{what}: {e}").into())
        };
        let detached_at = |at: &str| -> Result<()> {
            git_is(&root, &["rev-parse", "--symbolic-full-name", "HEAD"], "HEAD\n")?;
            git_is(&root, &["rev-parse", "HEAD"], &format!("{at}\n"))?;
            git_is(&root, &["status", "--porcelain=v1"], "")
        };
        if self.detached_panel()?.0.is_some() {
            return Err("The commit panel warns about a detached HEAD while on main".into());
        }

        let rows = self.menu_rows(&v1)?;
        if !rows.iter().any(|row| row.0 == "tag.checkout" && row.1 == "Check out the commit at v1.0" && row.2.is_none()) {
            return Err(format!("The menu of v1.0 lists {rows:?}").into());
        }
        self.action(view::label_click(&base, &v1, 2))?;
        self.until("HEAD detached at v1.0", |tab| {
            tab.ui.detached
                && tab.ui.head.as_deref() == Some(base.as_str())
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Checked out the commit at v1.0")
        })
        .await?;
        let said = self.said()?;
        let detail = format!("You are not on a branch. HEAD is at {}.", notice::short(&base));
        let expected = (Tone::Success, "Checked out the commit at v1.0".to_owned(), detail, vec!["Create branch here…".to_owned()]);
        if said.as_ref() != Some(&expected) {
            return Err(format!("Checking out v1.0 said {said:?}").into());
        }
        detached_at(&base)?;
        if fs::read_to_string(root.join("trail.txt"))? != TRAIL {
            return Err("The working tree does not hold v1.0's files".into());
        }
        unmoved("Checking out v1.0")?;

        self.action(Action::Select(Selection::Changes))?;
        self.until("the commit panel shown", |tab| tab.ui.selection == Some(Selection::Changes)).await?;
        let (warning, button) = self.detached_panel()?;
        if warning != Some("You are not on a branch. A commit made here is easy to lose.") {
            return Err(format!("The commit panel warns {warning:?}").into());
        }
        if !matches!(&button, Some(Action::Refs(refs::Action::NewAt(refs::Kind::Branch, at))) if *at == base) {
            return Err(format!("The warning's Create branch here… runs {button:?}").into());
        }
        let reasons = self.tab.read_with(self.cx, |tab, cx| {
            commands::listed(&tab.ui, &Target::Repo, &["repo.pull", "repo.push"], cx)
                .into_iter()
                .map(|entry| entry.disabled)
                .collect::<Vec<_>>()
        })?;
        let expected = [Some(crate::sync::DETACHED_PULL.to_owned()), Some(crate::sync::DETACHED_PUSH.to_owned())];
        if reasons != expected {
            return Err(format!("While detached, Pull and Push give the reasons {reasons:?}").into());
        }
        let already = "The commit at v1.0 is already checked out";
        let rows = self.menu_rows(&v1)?;
        if !rows.iter().any(|row| row.0 == "tag.checkout" && row.2.as_deref() == Some(already)) {
            return Err(format!("While on v1.0 the menu of v1.0 lists {rows:?}").into());
        }
        self.action(refs::sidebar_click(refs::Kind::Tag, false, OsStr::new("v1.0"), 2))?;
        self.until("the reason in an information notice", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Info && n.title == already)
        })
        .await?;
        detached_at(&base)?;

        self.select_commit(&tip).await?;
        let commit = Target::Commit(tip.clone());
        let rows = self.menu_rows(&commit)?;
        let switch = rows.iter().position(|row| row.0 == "branch.switch" && row.1 == "Switch to main" && row.2.is_none());
        let checkout = rows.iter().position(|row| row.0 == "commit.checkout" && row.1 == "Check out this commit" && row.2.is_none());
        if !matches!((switch, checkout), (Some(s), Some(c)) if c == s + 1) {
            return Err(format!("The menu of main's tip lists {rows:?}").into());
        }
        self.choose(&commit, "commit.checkout")?;
        self.until("HEAD detached at main's tip", |tab| {
            tab.ui.detached
                && tab.ui.head.as_deref() == Some(tip.as_str())
                && tab.ui.notices.newest().is_some_and(|n| n.title.starts_with("Checked out ") && n.title != "Checked out the commit at v1.0")
        })
        .await?;
        let said = self.said()?;
        let title = format!("Checked out {}", notice::short(&tip));
        let expected = (Tone::Success, title, "You are not on a branch.".to_owned(), vec!["Create branch here…".to_owned()]);
        if said.as_ref() != Some(&expected) {
            return Err(format!("Checking out main's tip said {said:?}").into());
        }
        detached_at(&tip)?;
        unmoved("Checking out main's tip")?;

        let (_, button) = self.detached_panel()?;
        self.action(button.ok_or("The warning's Create branch here… is dimmed")?)?;
        self.type_ref("signpost/fix")?;
        self.ref_action(refs::Action::Submit)?;
        self.until("on the new branch", |tab| tab.ui.branch == "signpost/fix" && !tab.ui.detached).await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/signpost/fix\n")?;
        git_is(&root, &["rev-parse", "refs/heads/signpost/fix"], &format!("{tip}\n"))?;
        if self.detached_panel()?.0.is_some() {
            return Err("The detached-HEAD warning stayed after creating a branch".into());
        }
        self.pass();
        Ok(())
    }

    fn reset_shown(&mut self) -> Result<Option<Shown>> {
        Ok(self.tab.read_with(self.cx, |tab, _| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Reverse(_)))
                .then(|| prompt::shown(&tab.ui))
                .flatten()
                .map(|spec| (spec.title, spec.lines, spec.danger))
        })?)
    }

    async fn reset_asked(&mut self, title: &str) -> Result<Shown> {
        self.until("the reset prompt open", |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Reverse(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == title)
        })
        .await?;
        self.reset_shown()?.ok_or_else(|| "The reset prompt closed".into())
    }

    pub(super) fn choose_in(&mut self, target: &Target, parent: &str, id: &str) -> Result<()> {
        self.action(Action::Menu(Some(view::Menu::Object { target: target.clone(), at: Point::default() })))?;
        let chosen = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx)
                .into_iter()
                .find(|entry| entry.id == parent)
                .and_then(|entry| entry.children.into_iter().find(|child| child.id == id))
                .and_then(|child| child.action)
        })?;
        let action = chosen.ok_or_else(|| format!("The {parent} submenu of {target:?} has no enabled {id}"))?;
        self.action(action)
    }

    async fn hard_reset(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0208");
        let root = init(dir, "Quarry")?;
        record(&root, "quarry.txt", "Quarry: open\n", "Start the quarry")?;
        let origin = dir.join("Quarry-origin.git");
        let path = origin.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", path])?;
        git(&root, &["remote", "add", "origin", path])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        record(&root, "stones.txt", "Stone: one\n", "Cut the first stone")?;
        record(&root, "stones.txt", "Stone: one\nStone: two\n", "Cut the second stone")?;
        let (start, first, second) = (oid(&root, "main~2")?, oid(&root, "main~1")?, oid(&root, "main")?);
        fs::write(root.join("quarry.txt"), "Quarry: flooded\n")?;
        fs::write(root.join("stones.txt"), "Stone: one\nStone: cracked\n")?;
        git(&root, &["add", "--", "stones.txt"])?;
        fs::write(root.join("notes.txt"), "Ask about the crane\n")?;
        self.open(&root)?;
        self.until("Quarry open with its changes", |tab| {
            tab.ui.repo == "Quarry"
                && tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(second.as_str())
                && tab.ui.history.iter().any(|row| row.oid == start)
                && files(tab, &["stones.txt"], &["quarry.txt", "notes.txt"])
        })
        .await?;
        let untouched = |what: &str| -> Result<()> {
            let check = || -> Result<()> {
                git_is(&root, &["rev-parse", "main"], &format!("{second}\n"))?;
                git_is(&root, &["status", "--porcelain=v1"], " M quarry.txt\nM  stones.txt\n?? notes.txt\n")?;
                git_is(&root, &["show", ":stones.txt"], "Stone: one\nStone: cracked\n")?;
                if fs::read(root.join("quarry.txt"))? != b"Quarry: flooded\n" {
                    return Err("quarry.txt changed".into());
                }
                Ok(())
            };
            check().map_err(|e| format!("{what}: {e}").into())
        };

        self.select_commit(&start).await?;
        let commit = Target::Commit(start.clone());
        let rows = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &commit, cx)
                .into_iter()
                .find(|entry| entry.id == "commit.reset")
                .map(|entry| {
                    let children: Vec<_> = entry
                        .children
                        .iter()
                        .map(|child| (child.id, child.title.clone(), child.danger, child.action.is_some()))
                        .collect();
                    (entry.title, children)
                })
        })?;
        let expected = (
            "Reset main to this commit".to_owned(),
            vec![
                ("commit.reset-soft", "Soft: keep all changes staged…".to_owned(), false, true),
                ("commit.reset-mixed", "Mixed: keep the files, clear the staging area…".to_owned(), false, true),
                ("commit.reset-hard", "Hard: discard all changes…".to_owned(), true, true),
            ],
        );
        if rows.as_ref() != Some(&expected) {
            return Err(format!("The commit's reset submenu is {rows:?}").into());
        }

        let title = "Reset main and discard all changes?";
        let lines = vec![
            format!("{}  Start the quarry", notice::short(&start)),
            "2 commits leave main. Changes in 2 files will be discarded. This cannot be undone.".to_owned(),
            format!("{}  Cut the second stone", notice::short(&second)),
            format!("{}  Cut the first stone", notice::short(&first)),
            "Your untracked file is kept.".to_owned(),
        ];
        self.choose_in(&commit, "commit.reset", "commit.reset-hard")?;
        let shown = self.reset_asked(title).await?;
        if shown.1 != lines || !shown.2 {
            return Err(format!("The hard reset prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The hard reset prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("Return cancelled the hard reset", |tab| !tab.ui.prompt.open()).await?;
        untouched("Return on the hard reset prompt")?;

        self.choose_in(&commit, "commit.reset", "commit.reset-hard")?;
        self.reset_asked(title).await?;
        self.key(CMD_RETURN)?;
        let short = notice::short(&start).to_owned();
        self.until("main reset to Start the quarry", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.head.as_deref() == Some(start.as_str())
                && files(tab, &[], &["notes.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == format!("Reset main to {short}"))
        })
        .await?;
        let said = self.said()?;
        let detail = format!("2 commits left main. Your files match {short}. Your untracked file was kept.");
        let expected = (Tone::Success, format!("Reset main to {short}"), detail, Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("The hard reset said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "main"], &format!("{start}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only", &start], "")?;
        git_is(&root, &["diff", "--name-only"], "")?;
        git_is(&root, &["ls-files"], "quarry.txt\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "?? notes.txt\n")?;
        if fs::read(root.join("quarry.txt"))? != b"Quarry: open\n" || root.join("stones.txt").exists() {
            return Err("The files do not match Start the quarry after the hard reset".into());
        }
        if fs::read(root.join("notes.txt"))? != b"Ask about the crane\n" {
            return Err("The untracked notes.txt did not survive the hard reset".into());
        }

        record(&root, "quarry.txt", "Quarry: open\nWrong stone\n", "Cut a wrong stone")?;
        let wrong = oid(&root, "main")?;
        self.action(Action::Refresh)?;
        self.until("the wrong stone committed", |tab| tab.ui.head.as_deref() == Some(wrong.as_str())).await?;
        let main = Target::Branch("main".into());
        self.choose(&main, "branch.reset-to-upstream")?;
        let shown = self.reset_asked("Reset main to origin/main?").await?;
        let lines = vec![
            format!("origin/main is at {short}  Start the quarry"),
            "1 commit leaves main. This cannot be undone.".to_owned(),
            format!("{}  Cut a wrong stone", notice::short(&wrong)),
            "Your untracked file is kept.".to_owned(),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The reset to origin/main prompt shows {shown:?}").into());
        }
        self.key(CMD_RETURN)?;
        self.until("main reset to origin/main", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.head.as_deref() == Some(start.as_str())
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Reset main to origin/main")
        })
        .await?;
        git_is(&root, &["rev-parse", "main", "refs/remotes/origin/main"], &format!("{start}\n{start}\n"))?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{start}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "?? notes.txt\n")?;
        if fs::read(root.join("notes.txt"))? != b"Ask about the crane\n" {
            return Err("The untracked notes.txt did not survive the reset to origin/main".into());
        }
        let rows = self.menu_rows(&main)?;
        let dimmed = rows.iter().any(|row| {
            row.0 == "branch.reset-to-upstream"
                && row.1 == "Reset main to origin/main…"
                && row.2.as_deref() == Some("main already matches origin/main")
        });
        if !dimmed {
            return Err(format!("After the reset the menu of main lists {rows:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn mixed_reset(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0209");
        let root = init(dir, "Kiln")?;
        record(&root, "kiln.txt", "Kiln: cold\n", "Light the kiln")?;
        record(&root, "pots.txt", "Pots: fired\n", "Fire the first pots")?;
        fs::write(root.join("kiln.txt"), "Kiln: hot\n")?;
        git(&root, &["add", "--", "kiln.txt"])?;
        record(&root, "pots.txt", "Pots: fired\nGlaze: blue\n", "Glaze the pots")?;
        let start = oid(&root, "main~2")?;
        fs::write(root.join("glaze.txt"), "Glaze: cobalt\n")?;
        git(&root, &["add", "--", "glaze.txt"])?;
        self.open(&root)?;
        self.until("Kiln open with a staged file", |tab| {
            tab.ui.repo == "Kiln"
                && tab.ui.branch == "main"
                && tab.ui.history.iter().any(|row| row.oid == start)
                && files(tab, &["glaze.txt"], &[])
        })
        .await?;
        self.select_commit(&start).await?;

        let commit = Target::Commit(start.clone());
        self.action(Action::Menu(Some(view::Menu::Object { target: commit.clone(), at: Point::default() })))?;
        let row = self.menu_rows(&commit)?.iter().position(|row| row.0 == "commit.reset");
        let row = row.ok_or("The commit's menu has no reset submenu")?;
        for _ in 0..=row {
            self.action(Action::MenuKey(Key::Down))?;
        }
        for key in [Key::Right, Key::Down] {
            self.action(Action::MenuKey(key))?;
        }
        let cursor = self.tab.read_with(self.cx, |tab, _| tab.ui.menu_cursor)?;
        if cursor != (Cursor { row: Some(row), open: true, sub: Some(1) }) {
            return Err(format!("The keys left the menu at {cursor:?}").into());
        }
        self.action(Action::MenuKey(Key::Enter))?;
        let shown = self.reset_asked("Reset main and clear the staging area?").await?;
        let lines = vec![
            format!("{}  Light the kiln", notice::short(&start)),
            "2 commits leave main. Their changes stay in your files, unstaged.".to_owned(),
            "The staged changes to 1 file become unstaged. Your files do not change.".to_owned(),
        ];
        if shown.1 != lines || shown.2 {
            return Err(format!("The mixed reset prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("The mixed reset prompt did not open with Reset branch focused".into());
        }
        self.key("enter")?;
        let short = notice::short(&start).to_owned();
        self.until("main reset to Light the kiln", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.head.as_deref() == Some(start.as_str())
                && files(tab, &[], &["kiln.txt", "pots.txt", "glaze.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == format!("Reset main to {short}"))
        })
        .await?;
        let said = self.said()?;
        let detail = "2 commits left main. Their changes are in your files, unstaged.".to_owned();
        let expected = (Tone::Success, format!("Reset main to {short}"), detail, Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("The mixed reset said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "main"], &format!("{start}\n"))?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        git_is(&root, &["ls-files", "--stage"], &git(&root, &["ls-tree", "-r", "--format=%(objectmode) %(objectname) 0\t%(path)", &start])?)?;
        git_is(&root, &["status", "--porcelain=v1"], " M kiln.txt\n?? glaze.txt\n?? pots.txt\n")?;
        for (path, text) in [("kiln.txt", "Kiln: hot\n"), ("pots.txt", "Pots: fired\nGlaze: blue\n"), ("glaze.txt", "Glaze: cobalt\n")] {
            if fs::read_to_string(root.join(path))? != text {
                return Err(format!("{path} changed in the mixed reset").into());
            }
        }

        self.key(if cfg!(target_os = "macos") { "cmd-p" } else { "ctrl-p" })?;
        self.until("the palette open", |tab| tab.ui.palette.open()).await?;
        self.type_keys("mixed")?;
        let (rows, _) = self.palette_rows()?;
        let title = "Reset main to this commit › Mixed: keep the files, clear the staging area…";
        if !rows.iter().any(|row| row.0 == "commit.reset-mixed" && row.1 == title) {
            return Err(format!("\"mixed\" lists {rows:?}").into());
        }
        self.key("escape")?;
        self.until("the palette closed", |tab| !tab.ui.palette.open()).await?;
        self.pass();
        Ok(())
    }

    async fn merged(&mut self, title: &str, detail: &str) -> Result<()> {
        self.until("the merge finished", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == title && n.detail == detail)
        })
        .await?;
        let said = self.said()?;
        let expected = (Tone::Success, title.to_owned(), detail.to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("The merge said {said:?}").into());
        }
        Ok(())
    }

    async fn merge_remote(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0210");
        let (root, origin) = Self::crossing(dir, "Confluence", &[])?;
        git(&root, &["switch", "-q", "-c", "feature/weir"])?;
        record(&root, "weir.txt", "Weir: stone\n", "Build the weir")?;
        let weir = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "river.txt", "River: surveyed\n", "Survey the river")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        let river = oid(&root, "refs/remotes/origin/main")?;
        git(&root, &["switch", "-q", "feature/weir"])?;
        self.open(&root)?;
        let upstream = Target::RemoteBranch("origin/main".into());
        self.until("Confluence open on feature/weir", |tab| {
            tab.ui.repo == "Confluence"
                && tab.ui.branch == "feature/weir"
                && tab.ui.refs.oid_of(&upstream) == Some(river.as_str())
        })
        .await?;
        let rows = self.menu_rows(&upstream)?;
        if !rows.iter().any(|row| row.0 == "branch.merge-remote" && row.1 == "Merge origin/main into feature/weir" && row.2.is_none()) {
            return Err(format!("The menu of origin/main lists {rows:?}").into());
        }
        self.choose_on(&upstream, "branch.merge-remote").await?;
        self.until("feature/weir merged", |tab| tab.ui.head.as_deref().is_some_and(|head| head != weir)).await?;
        let head = oid(&root, "HEAD")?;
        let detail = format!("Created merge commit {}.", notice::short(&head));
        self.merged("Merged origin/main into feature/weir", &detail).await?;
        git_is(&root, &["show", "-s", "--format=%P", "HEAD"], &format!("{weir} {river}\n"))?;
        git_is(&root, &["log", "-1", "--format=%s"], "Merge remote-tracking branch 'origin/main' into feature/weir\n")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/weir\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        if fs::read_to_string(root.join("river.txt"))? != "River: surveyed\n" {
            return Err("The merge did not bring river.txt into the working tree".into());
        }
        git_is(&root, &["rev-parse", "refs/heads/main", "refs/remotes/origin/main"], &format!("{river}\n{river}\n"))?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{river}\n"))?;
        self.until("the graph selects the merge commit", |tab| {
            tab.ui.selection == Some(Selection::Commit(head.clone()))
                && tab.ui.history.iter().any(|row| row.oid == head && row.parents == [weir.clone(), river.clone()])
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn merge_nothing(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0211");
        let (root, origin) = Self::crossing(dir, "Eddy", &[])?;
        let start = oid(&root, "main")?;
        git(&root, &["tag", "-a", "-m", "First survey", "v1.0"])?;
        record(&root, "eddy.txt", "Eddy: calm\n", "Chart the eddy")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        let charted = oid(&root, "main")?;
        git(&root, &["switch", "-q", "-c", "feature/pool"])?;
        record(&root, "pool.txt", "Pool: deep\n", "Dig the pool")?;
        let pool = oid(&root, "HEAD")?;
        let reflog = git(&root, &["reflog", "--format=%H", "refs/heads/feature/pool"])?;
        self.open(&root)?;
        let (upstream, tag) = (Target::RemoteBranch("origin/main".into()), Target::Tag("v1.0".into()));
        self.until("Eddy open on feature/pool", |tab| {
            tab.ui.repo == "Eddy"
                && tab.ui.branch == "feature/pool"
                && tab.ui.refs.oid_of(&upstream) == Some(charted.as_str())
                && tab.ui.refs.oid_of(&tag) == Some(start.as_str())
                && tab.ui.history.iter().any(|row| row.oid == start)
        })
        .await?;
        let unchanged = || -> Result<()> {
            git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/feature/pool\n")?;
            git_is(&root, &["rev-parse", "HEAD"], &format!("{pool}\n"))?;
            git_is(&root, &["reflog", "--format=%H", "refs/heads/feature/pool"], &reflog)?;
            git_is(&root, &["status", "--porcelain=v1"], "")?;
            git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{charted}\n"))?;
            if root.join(".git/MERGE_HEAD").exists() {
                return Err("A merge with nothing to do left a merge in progress".into());
            }
            Ok(())
        };

        self.choose_on(&upstream, "branch.merge-remote").await?;
        self.merged("Already up to date", "feature/pool already contains origin/main.").await?;
        unchanged()?;
        let rows = self.menu_rows(&tag)?;
        if !rows.iter().any(|row| row.0 == "tag.merge" && row.1 == "Merge v1.0 into feature/pool" && row.2.is_none()) {
            return Err(format!("The menu of v1.0 lists {rows:?}").into());
        }
        self.choose(&tag, "tag.merge")?;
        self.merged("Already up to date", "feature/pool already contains v1.0.").await?;
        unchanged()?;
        let commit = Target::Commit(start.clone());
        let rows = self.menu_rows(&commit)?;
        if !rows.iter().any(|row| row.0 == "commit.merge" && row.1 == "Merge this commit into feature/pool" && row.2.is_none()) {
            return Err(format!("The menu of the first commit lists {rows:?}").into());
        }
        self.choose(&commit, "commit.merge")?;
        let detail = format!("feature/pool already contains {}.", notice::short(&start));
        self.merged("Already up to date", &detail).await?;
        unchanged()?;
        let rows = self.menu_rows(&Target::Commit(pool.clone()))?;
        if !rows.iter().any(|row| row.0 == "commit.merge" && row.2.as_deref() == Some("This commit is the tip of feature/pool")) {
            return Err(format!("The menu of feature/pool's tip lists {rows:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn tag_and_push(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0212");
        let (root, origin) = Self::crossing(dir, "Summit", &[])?;
        let camp = oid(&root, "HEAD")?;
        record(&root, "summit.txt", "Summit: reached\n", "Reach the summit")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        git(&root, &["tag", "base-camp", &camp])?;
        let summit = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Summit open on main", |tab| {
            tab.ui.repo == "Summit"
                && tab.ui.branch == "main"
                && tab.ui.head.as_deref() == Some(summit.as_str())
                && tab.ui.refs.remotes == ["origin"]
                && tab.ui.refs.tag(OsStr::new("base-camp")).is_some()
        })
        .await?;
        self.action(Action::Select(Selection::Commit(summit.clone())))?;
        self.ref_action(refs::Action::New(refs::Kind::Tag))?;
        self.until("the New tag dialog open at the summit", |tab| {
            tab.ui.refs.draft_target() == Some((refs::Kind::Tag, summit.as_str()))
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.refs.draft_push())? != Some(false) {
            return Err("The New tag dialog's Push to origin tick did not start cleared".into());
        }
        self.type_ref("v1.0")?;
        self.tab.update(self.cx, |tab, window, cx| {
            let message = tab.ui.refs.message.clone();
            window.focus(&message.focus_handle(cx));
            message.update(cx, |field, cx| field.replace_text_in_range(None, "Reach the summit", window, cx));
        })?;
        self.key("enter")?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.refs.message.update(cx, |field, cx| {
                field.replace_text_in_range(None, "\nThe ridge route is open.", window, cx)
            })
        })?;
        let message = self.tab.read_with(self.cx, |tab, cx| {
            (tab.ui.refs.draft.is_some(), tab.ui.refs.message.read(cx).text().to_owned())
        })?;
        if message != (true, "Reach the summit\n\nThe ridge route is open.".to_owned()) {
            return Err(format!("Return in the tag message left {message:?}").into());
        }
        self.ref_action(refs::Action::TogglePush)?;
        let ticked = self.tab.read_with(self.cx, |tab, cx| {
            (tab.ui.refs.draft_push(), settings::settings(cx).push_new_tags)
        })?;
        if ticked != (Some(true), true) {
            return Err(format!("Ticking Push to origin left the tick and setting at {ticked:?}").into());
        }
        self.tab.update(self.cx, |tab, window, cx| window.focus(&tab.ui.refs.input.focus_handle(cx)))?;
        self.key("enter")?;
        self.until("the tag created and pushed", |tab| {
            tab.ui.refs.draft.is_none()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Pushed v1.0 to origin")
        })
        .await?;
        let short = notice::short(&summit).to_owned();
        let said = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.items().iter().map(|n| (n.tone, n.text())).collect::<Vec<_>>()
        })?;
        let created = (Tone::Success, format!("Tag created — v1.0 at {short}, annotated"));
        if !said.contains(&created) || said.last() != Some(&(Tone::Success, "Pushed v1.0 to origin".to_owned())) {
            return Err(format!("Creating and pushing v1.0 said {said:?}").into());
        }
        let object = oid(&root, "refs/tags/v1.0")?;
        git_is(&root, &["cat-file", "-t", "refs/tags/v1.0"], "tag\n")?;
        git_is(&origin, &["cat-file", "-t", "refs/tags/v1.0"], "tag\n")?;
        git_is(&origin, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/tags"], &format!("refs/tags/v1.0 {object}\n"))?;
        git_is(&origin, &["rev-parse", "refs/tags/v1.0^{commit}"], &format!("{summit}\n"))?;
        git_is(&origin, &["tag", "-l", "--format=%(contents)", "v1.0"], "Reach the summit\n\nThe ridge route is open.\n\n")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{summit}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        let date = git(&root, &["tag", "-l", "--format=%(taggerdate:short)", "v1.0"])?;
        let tip = format!("Annotated tag by Cedar Contributor on {}: Reach the summit", date.trim_end());
        self.until("the tag listed with its annotation", |tab| {
            tab.ui.refs.tag(OsStr::new("v1.0")).is_some_and(|item| item.object == object && crate::tag::tooltip(item) == tip)
                && tab.ui.refs.lists(refs::Kind::Tag, "v1.0")
        })
        .await?;
        let light = self.tab.read_with(self.cx, |tab, _| tab.ui.refs.tag(OsStr::new("base-camp")).map(crate::tag::tooltip))?;
        if light != Some(format!("Lightweight tag at {}", notice::short(&camp))) {
            return Err(format!("base-camp's tooltip reads {light:?}").into());
        }

        self.ref_action(refs::Action::New(refs::Kind::Tag))?;
        self.until("the New tag dialog open again", |tab| tab.ui.refs.draft.is_some()).await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.refs.draft_push())? != Some(true) {
            return Err("The New tag dialog forgot the Push to origin tick".into());
        }
        self.ref_action(refs::Action::TogglePush)?;
        self.key("escape")?;
        self.until("the New tag dialog closed", |tab| tab.ui.refs.draft.is_none()).await?;
        if self.cx.update(|cx| settings::settings(cx).push_new_tags)? {
            return Err("Clearing Push to origin was not remembered".into());
        }

        let camp_tag = Target::Tag("base-camp".into());
        let rows = self.menu_rows(&camp_tag)?;
        let offered = |id: &str, title: &str| rows.iter().any(|row| row.0 == id && row.1 == title && row.2.is_none());
        if !offered("tag.push", "Push base-camp to origin") || !offered("tag.annotate", "Annotate base-camp…") {
            return Err(format!("The menu of base-camp lists {rows:?}").into());
        }
        if self.menu_rows(&Target::Tag("v1.0".into()))?.iter().any(|row| row.0 == "tag.annotate") {
            return Err("The annotated v1.0 offers Annotate".into());
        }
        self.choose(&camp_tag, "tag.annotate")?;
        let shown = self.asked("Annotate base-camp").await?;
        let line = format!(
            "base-camp becomes an annotated tag on the same commit, {}, with this message, your name and today's date.",
            notice::short(&camp)
        );
        if shown.1 != [line] || shown.2 || self.prompt_focus()? != Some(Stop::Field) {
            return Err(format!("Annotating base-camp shows {shown:?}").into());
        }
        self.key("enter")?;
        if !self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())? {
            return Err("Return with no message closed the Annotate prompt".into());
        }
        self.type_keys("camp is set")?;
        self.key("enter")?;
        self.until("base-camp annotated", |tab| {
            !tab.ui.prompt.open()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Annotated base-camp")
                && tab.ui.refs.tag(OsStr::new("base-camp")).is_some_and(|item| item.annotation.is_some())
        })
        .await?;
        let said = self.said()?;
        let expected = (Tone::Success, "Annotated base-camp".to_owned(), format!("It still marks {}.", notice::short(&camp)), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Annotating base-camp said {said:?}").into());
        }
        git_is(&root, &["cat-file", "-t", "refs/tags/base-camp"], "tag\n")?;
        git_is(&root, &["rev-parse", "refs/tags/base-camp^{commit}"], &format!("{camp}\n"))?;
        git_is(&root, &["tag", "-l", "--format=%(contents)", "base-camp"], "camp is set\n\n")?;
        let camp_object = oid(&root, "refs/tags/base-camp")?;

        self.choose(&camp_tag, "tag.push")?;
        self.until("base-camp pushed", |tab| tab.ui.notices.newest().is_some_and(|n| n.title == "Pushed base-camp to origin"))
            .await?;
        git_is(&origin, &["cat-file", "-t", "refs/tags/base-camp"], "tag\n")?;
        git_is(&origin, &["rev-parse", "refs/tags/base-camp"], &format!("{camp_object}\n"))?;
        self.choose(&camp_tag, "tag.push")?;
        self.until("the second push of base-camp", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.text() == "origin already has base-camp — Nothing was sent.")
        })
        .await?;

        git(&root, &["push", "-q", "origin", &format!("{summit}:refs/tags/v0.9")])?;
        git(&root, &["tag", "-a", "-m", "Camp marker", "v0.9", &camp])?;
        self.action(Action::Refresh)?;
        let early = Target::Tag("v0.9".into());
        self.until("v0.9 listed", |tab| tab.ui.refs.oid_of(&early) == Some(camp.as_str())).await?;
        self.choose(&early, "tag.push")?;
        self.until("the push of v0.9 refused", |tab| tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error))
            .await?;
        let said = self.said()?;
        let expected = (Tone::Error, "Push Failed".to_owned(), "origin already has a different v0.9.".to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Pushing a v0.9 the remote has differently said {said:?}").into());
        }
        git_is(&origin, &["rev-parse", "refs/tags/v0.9"], &format!("{summit}\n"))?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{summit}\n"))?;
        self.pass();
        Ok(())
    }

    async fn delete_tag(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0213");
        let (root, origin) = Self::crossing(dir, "Ford", &[])?;
        let mirror = dir.join("Ford-mirror.git");
        let mirror_path = mirror.to_str().ok_or("Scratch path is not UTF-8")?;
        git(dir, &["init", "-q", "--bare", "-b", "main", mirror_path])?;
        git(&root, &["remote", "add", "mirror", mirror_path])?;
        git(&root, &["tag", "-a", "-m", "Mark the ford", "v2.0"])?;
        for remote in ["origin", "mirror"] {
            git(&root, &["push", "-q", remote, "main", "refs/tags/v2.0"])?;
        }
        let (commit, object) = (oid(&root, "HEAD")?, oid(&root, "refs/tags/v2.0")?);
        let short = notice::short(&commit).to_owned();
        let tag = Target::Tag("v2.0".into());
        self.open(&root)?;
        self.until("Ford open with v2.0 and two remotes", |tab| {
            tab.ui.repo == "Ford" && tab.ui.refs.remotes == ["mirror", "origin"] && tab.ui.refs.oid_of(&tag) == Some(commit.as_str())
        })
        .await?;
        let has = |at: &Path, expected: &str| git_is(at, &["for-each-ref", "--format=%(objectname)", "refs/tags"], expected);
        let kept = format!("{object}\n");
        let submenus = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &tag, cx)
                .into_iter()
                .filter(|entry| !entry.children.is_empty())
                .map(|entry| (entry.title, entry.children.into_iter().map(|child| child.title).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        })?;
        let modes = crate::merge::Mode::ALL.iter().map(|mode| mode.title().to_owned()).collect();
        let expected = vec![
            ("Merge v2.0 into main".to_owned(), modes),
            ("Push v2.0 to".to_owned(), vec!["mirror".to_owned(), "origin".to_owned(), "All remotes".to_owned()]),
            ("Delete v2.0 from".to_owned(), vec!["mirror…".to_owned(), "origin…".to_owned(), "All remotes…".to_owned()]),
        ];
        if submenus != expected {
            return Err(format!("The menu of v2.0 has the submenus {submenus:?}").into());
        }

        self.choose(&tag, "tag.delete")?;
        let shown = self.asked("Delete tag v2.0?").await?;
        let lines = vec![
            format!("The commit it marks, {short}, stays."),
            "Its message goes with it: Mark the ford".to_owned(),
            "mirror and origin keep any copy they have until you delete it there too.".to_owned(),
        ];
        if shown.1 != lines || shown.2 {
            return Err(format!("The tag delete prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("The tag delete prompt did not open with Delete tag focused".into());
        }
        self.key("enter")?;
        self.until("v2.0 deleted here", |tab| {
            !tab.ui.prompt.open() && tab.ui.refs.oid_of(&tag).is_none() && tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted tag v2.0")
        })
        .await?;
        let said = self.said()?;
        let offers = vec!["Restore".to_owned(), "Delete from all remotes too".to_owned()];
        let expected = (Tone::Success, "Deleted tag v2.0".to_owned(), format!("was {short}"), offers.clone());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Deleting v2.0 said {said:?}").into());
        }
        has(&root, "")?;
        has(&origin, &kept)?;
        has(&mirror, &kept)?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{commit}\n"))?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("v2.0 restored", |tab| {
            tab.ui.refs.oid_of(&tag) == Some(commit.as_str()) && tab.ui.notices.newest().is_some_and(|n| n.title == "Restored tag v2.0")
        })
        .await?;
        has(&root, &kept)?;
        git_is(&root, &["tag", "-l", "--format=%(contents:subject)", "v2.0"], "Mark the ford\n")?;

        self.choose_under(&tag, "tag.delete-remote", "origin…")?;
        let shown = self.asked("Delete v2.0 from origin?").await?;
        let lines = vec!["This removes the tag for everyone who uses origin.".to_owned(), "v2.0 stays in this repository.".to_owned()];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The remote tag delete prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The remote tag delete prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("the prompt cancelled", |tab| !tab.ui.prompt.open()).await?;
        has(&origin, &kept)?;
        self.choose_under(&tag, "tag.delete-remote", "origin…")?;
        self.asked("Delete v2.0 from origin?").await?;
        self.key(CMD_RETURN)?;
        self.until("v2.0 deleted from origin", |tab| tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted v2.0 from origin"))
            .await?;
        has(&origin, "")?;
        has(&mirror, &kept)?;
        has(&root, &kept)?;

        self.choose(&tag, "tag.delete")?;
        self.asked("Delete tag v2.0?").await?;
        self.key("enter")?;
        self.until("v2.0 deleted here again", |tab| {
            tab.ui.refs.oid_of(&tag).is_none() && tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted tag v2.0")
        })
        .await?;
        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 1)))?;
        let shown = self.asked("Delete v2.0 from all remotes?").await?;
        let lines = vec!["This removes the tag from mirror and origin, for everyone who uses them.".to_owned()];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The all-remotes delete prompt shows {shown:?}").into());
        }
        self.key(CMD_RETURN)?;
        self.until("v2.0 deleted from both remotes", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted v2.0 from mirror and origin")
        })
        .await?;
        has(&root, "")?;
        has(&origin, "")?;
        has(&mirror, "")?;
        git_is(&root, &["rev-parse", "HEAD", "refs/remotes/origin/main"], &format!("{commit}\n{commit}\n"))?;
        git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{commit}\n"))?;
        git_is(&mirror, &["rev-parse", "refs/heads/main"], &format!("{commit}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    fn peer_push(dir: &Path, origin: &Path, branch: &str, subject: &str) -> Result<String> {
        let peer = dir.join(format!("{}-peer", origin.file_stem().and_then(OsStr::to_str).unwrap_or("peer")));
        if !peer.exists() {
            git(dir, &["clone", "-q", origin.to_str().ok_or("Scratch path is not UTF-8")?, peer.to_str().ok_or("Scratch path is not UTF-8")?])?;
        }
        git(&peer, &["fetch", "-q", "origin"])?;
        git(&peer, &["switch", "-q", "-C", branch, &format!("origin/{branch}")])?;
        let who = ["-c", "user.name=Peer", "-c", "user.email=peer@example.invalid", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false"];
        git(&peer, &[&who[..], &["commit", "-q", "--allow-empty", "-m", subject]].concat())?;
        git(&peer, &["push", "-q", "origin", branch])?;
        oid(&peer, "HEAD")
    }

    async fn delete_moved_remote(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0214");
        let (root, origin) = Self::crossing(dir, "Sluice", &[])?;
        git(&root, &["switch", "-q", "-c", "feature/gate"])?;
        record(&root, "gate.txt", "Gate: oak\n", "Hang the gate")?;
        git(&root, &["push", "-q", "-u", "origin", "feature/gate"])?;
        git(&root, &["switch", "-q", "main"])?;
        let (gate, main) = (oid(&root, "feature/gate")?, oid(&root, "main")?);
        let moved = Self::peer_push(dir, &origin, "feature/gate", "Open the sluice")?;
        let (short, moved_short) = (notice::short(&gate).to_owned(), notice::short(&moved).to_owned());
        let remote = Target::RemoteBranch("origin/feature/gate".into());
        let local = Target::Branch("feature/gate".into());
        self.open(&root)?;
        self.until("Sluice open with origin/feature/gate as last fetched", |tab| {
            tab.ui.repo == "Sluice" && tab.ui.branch == "main" && tab.ui.refs.oid_of(&remote) == Some(gate.as_str())
        })
        .await?;
        let on_origin = |expected: &str| {
            git_is(&origin, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"], expected)
        };
        let both = format!("refs/heads/feature/gate {moved}\nrefs/heads/main {main}\n");
        on_origin(&both)?;
        let rows = self.menu_rows(&remote)?;
        for (id, title) in [
            ("branch.delete-remote", "Delete origin/feature/gate…"),
            ("branch.delete-both", "Delete feature/gate and origin/feature/gate…"),
        ] {
            if !rows.iter().any(|row| row.0 == id && row.1 == title && row.2.is_none()) {
                return Err(format!("The menu of origin/feature/gate lists {rows:?}").into());
            }
        }

        let tick = |replay: &mut Self| -> Result<Option<(String, bool)>> {
            Ok(replay.tab.read_with(replay.cx, |tab, _| {
                prompt::shown(&tab.ui).and_then(|spec| spec.tick).map(|(label, ticked)| (label.to_string(), ticked))
            })?)
        };
        self.choose(&remote, "branch.delete-remote")?;
        let shown = self.asked("Delete origin/feature/gate?").await?;
        let lines = vec![
            "This removes the branch for everyone who uses origin.".to_owned(),
            format!("It is deleted only while origin still has it at {short}, where it was when you last fetched."),
            "Deleting feature/gate too takes 1 commit more out of the graph.".to_owned(),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The remote delete prompt shows {shown:?}").into());
        }
        if tick(self)? != Some(("Also delete feature/gate here".to_owned(), false)) {
            return Err(format!("The remote delete prompt offers {:?}", tick(self)?).into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The remote delete prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("the prompt cancelled", |tab| !tab.ui.prompt.open()).await?;
        on_origin(&both)?;

        self.choose(&remote, "branch.delete-remote")?;
        self.asked("Delete origin/feature/gate?").await?;
        self.key(CMD_RETURN)?;
        self.until("the delete refused", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title == "Delete Failed")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Error,
            "Delete Failed".to_owned(),
            "origin/feature/gate changed on the remote. Fetch and look again.".to_owned(),
            vec!["Fetch".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("The refused delete said {said:?}").into());
        }
        let output = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().and_then(|n| n.output.clone()))?;
        if !output.is_some_and(|output| output.contains("failed to push")) {
            return Err("The refused delete keeps no Git output to show".into());
        }
        on_origin(&both)?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/feature/gate", "refs/heads/feature/gate"], &format!("{gate}\n{gate}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("origin fetched", |tab| !tab.ui.busy() && tab.ui.refs.oid_of(&remote) == Some(moved.as_str())).await?;
        self.choose(&local, "branch.delete-both")?;
        let shown = self.asked("Delete origin/feature/gate?").await?;
        let lines = vec![
            "This removes the branch for everyone who uses origin.".to_owned(),
            "1 commit is only on origin/feature/gate. It will no longer appear in the graph.".to_owned(),
            format!("{moved_short}  Open the sluice"),
            format!("It is deleted only while origin still has it at {moved_short}, where it was when you last fetched."),
            "Deleting feature/gate too takes 1 commit more out of the graph.".to_owned(),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The delete-both prompt shows {shown:?}").into());
        }
        if tick(self)? != Some(("Also delete feature/gate here".to_owned(), true)) {
            return Err(format!("The delete-both prompt offers {:?}", tick(self)?).into());
        }
        self.key(CMD_RETURN)?;
        self.until("both deleted", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == "Deleted feature/gate and origin/feature/gate")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Success,
            "Deleted feature/gate and origin/feature/gate".to_owned(),
            format!("feature/gate was {short}, origin/feature/gate was {moved_short}."),
            vec!["Restore feature/gate".to_owned(), "Restore origin/feature/gate".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("Deleting both said {said:?}").into());
        }
        on_origin(&format!("refs/heads/main {main}\n"))?;
        git_is(&root, &["for-each-ref", "refs/heads/feature/gate", "refs/remotes/origin/feature/gate"], "")?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 1)))?;
        self.until("origin/feature/gate restored", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == "Restored origin/feature/gate")
        })
        .await?;
        on_origin(&both)?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/feature/gate"], &format!("{moved}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn fast_forward_other(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0215");
        let (root, origin) = Self::crossing(dir, "Lock", &[])?;
        git(&root, &["branch", "--unset-upstream", "main"])?;
        git(&root, &["switch", "-q", "-c", "feature/lock"])?;
        record(&root, "lock.txt", "Lock: gates\n", "Build the lock")?;
        let lock = oid(&root, "feature/lock")?;
        let main = oid(&root, "main")?;
        Self::peer_push(dir, &origin, "main", "Dredge the channel")?;
        let ahead = Self::peer_push(dir, &origin, "main", "Fill the basin")?;
        git(&root, &["fetch", "-q", "origin"])?;
        fs::write(root.join("lock.txt"), "Lock: gates, half painted\n")?;
        fs::write(root.join("notes.txt"), "Paint the second gate\n")?;
        let status = git(&root, &["status", "--porcelain=v1"])?;
        let branch = Target::Branch("main".into());
        self.open(&root)?;
        self.until("Lock open on feature/lock with origin/main ahead", |tab| {
            tab.ui.repo == "Lock"
                && tab.ui.branch == "feature/lock"
                && tab.ui.refs.oid_of(&Target::RemoteBranch("origin/main".into())) == Some(ahead.as_str())
                && tab.ui.refs.branch("main").is_some()
        })
        .await?;
        let tooltip = |replay: &mut Self| -> Result<String> {
            Ok(replay.tab.read_with(replay.cx, |tab, _| tab.ui.refs.branch("main").map(crate::tracking::tooltip))?.unwrap_or_default())
        };
        if tooltip(self)? != "Tracks no remote branch" {
            return Err(format!("main's tooltip reads {:?}", tooltip(self)?).into());
        }
        let rows = self.menu_rows(&branch)?;
        if rows.iter().any(|row| row.0 == "branch.fast-forward") || !rows.iter().any(|row| row.0 == "branch.set-upstream" && row.1 == "Set upstream of main…" && row.2.is_none()) {
            return Err(format!("The menu of main, with no upstream, lists {rows:?}").into());
        }

        self.choose(&branch, "branch.set-upstream")?;
        let shown = self.asked("Set upstream of main").await?;
        let lines = vec![
            "Which remote branch should main push to and pull from?".to_owned(),
            "It tracks no remote branch now.".to_owned(),
            "Remote branches named main: origin/main.".to_owned(),
        ];
        if shown.1 != lines || shown.2 {
            return Err(format!("The upstream prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Field) || self.field()? != ("origin/main".to_owned(), None) {
            return Err(format!("The upstream prompt opened with {:?} in {:?}", self.field()?, self.prompt_focus()?).into());
        }
        self.key(SELECT_ALL)?;
        self.type_keys("origin/nowhere")?;
        let problem = "There is no remote branch origin/nowhere. Fetch first, or name one such as origin/main.";
        if self.field()? != ("origin/nowhere".to_owned(), Some(problem.to_owned())) {
            return Err(format!("Typing origin/nowhere gave {:?}", self.field()?).into());
        }
        self.key("enter")?;
        if !self.tab.read_with(self.cx, |tab, _| tab.ui.prompt.open())? {
            return Err("Return on a remote branch that is not there closed the prompt".into());
        }
        self.key(SELECT_ALL)?;
        self.type_keys("origin/main")?;
        self.key("enter")?;
        self.until("main tracks origin/main", |tab| {
            !tab.ui.busy()
                && tab.ui.notices.newest().is_some_and(|n| n.title == "main tracks origin/main")
                && tab.ui.refs.branch("main").is_some_and(|item| item.upstream.as_deref() == Some("origin/main") && item.behind == 2)
        })
        .await?;
        let said = self.said()?;
        let expected = (Tone::Success, "main tracks origin/main".to_owned(), "It is 2 commits behind origin/main.".to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Setting the upstream said {said:?}").into());
        }
        git_is(&root, &["config", "branch.main.remote"], "origin\n")?;
        git_is(&root, &["config", "branch.main.merge"], "refs/heads/main\n")?;
        if tooltip(self)? != "Tracks origin/main. 2 commits behind" {
            return Err(format!("main's tooltip reads {:?}", tooltip(self)?).into());
        }

        let counts = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.refs.branch("main").map(|item| crate::tracking::counts(item, false)).unwrap_or_default()
        })?;
        let button = ("branch.fast-forward", "↓2".to_owned(), "2 commits behind origin/main. Click to fast-forward".to_owned());
        if counts != [button] {
            return Err(format!("main's sidebar counts are {counts:?}").into());
        }
        let rows = self.menu_rows(&branch)?;
        for (id, title, disabled) in [
            ("branch.fast-forward", "Fast-forward main to origin/main", None),
            ("branch.pull", "Pull main from origin/main", None),
            ("branch.push", "Push main to origin/main", Some("main has no commits that origin/main lacks".to_owned())),
        ] {
            if !rows.iter().any(|row| row.0 == id && row.1 == title && row.2 == disabled) {
                return Err(format!("The menu of main lists {rows:?}").into());
            }
        }
        self.tab.update(self.cx, |tab, window, cx| commands::run_for(tab, "branch.fast-forward", branch.clone(), window, cx))?;
        self.until("main fast-forwarded", |tab| {
            !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == "Fast-forwarded main")
        })
        .await?;
        let said = self.said()?;
        let detail = format!("2 commits from origin/main. Now at {}.", notice::short(&ahead));
        let expected = (Tone::Success, "Fast-forwarded main".to_owned(), detail, Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Fast-forwarding said {said:?}").into());
        }
        let unchanged = |root: &Path| -> Result<()> {
            git_is(root, &["symbolic-ref", "HEAD"], "refs/heads/feature/lock\n")?;
            git_is(root, &["rev-parse", "refs/heads/feature/lock"], &format!("{lock}\n"))?;
            git_is(root, &["status", "--porcelain=v1"], &status)?;
            if fs::read_to_string(root.join("lock.txt"))? != "Lock: gates, half painted\n" {
                return Err("The fast-forward changed lock.txt".into());
            }
            Ok(())
        };
        git_is(&root, &["rev-parse", "refs/heads/main"], &format!("{ahead}\n"))?;
        git_is(&root, &["reflog", "-1", "--format=%gs", "refs/heads/main"], &format!("fetch --porcelain --no-tags --no-write-fetch-head --no-recurse-submodules --no-prune -- . {ahead}:refs/heads/main: fast-forward\n"))?;
        unchanged(&root)?;
        if main == ahead {
            return Err("The fixture did not move main's remote branch".into());
        }

        let newer = Self::peer_push(dir, &origin, "main", "Open the upper gate")?;
        self.until("main shown up to date", |tab| tab.ui.refs.branch("main").is_some_and(|item| item.behind == 0)).await?;
        self.choose(&branch, "branch.pull")?;
        self.until("main pulled", |tab| !tab.ui.busy() && tab.ui.notices.newest().is_some_and(|n| n.title == "Pulled main")).await?;
        let said = self.said()?;
        let expected = (Tone::Success, "Pulled main".to_owned(), "Fast-forwarded main by 1 commit from origin/main.".to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("Pulling main said {said:?}").into());
        }
        git_is(&root, &["rev-parse", "refs/heads/main", "refs/remotes/origin/main"], &format!("{newer}\n{newer}\n"))?;
        unchanged(&root)?;
        self.pass();
        Ok(())
    }

    fn counts(tab: &Tab) -> Option<(usize, usize)> {
        tab.ui.refs.current(&tab.ui.branch).filter(|item| item.upstream.is_some()).map(|item| (item.ahead, item.behind))
    }

    async fn push_and_force(&mut self, branch: &str, upstream: &str) -> Result<Shown> {
        self.action(Action::Sync(sync::Action::Push))?;
        self.asked(&format!("{branch} is behind {upstream}")).await?;
        self.action(Action::Prompt(prompt::Action::Choose(0)))?;
        self.asked(&format!("Force push {branch} to {upstream}?")).await
    }

    async fn force_after_amend(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0216");
        let (root, origin) = Self::crossing(dir, "Ferry", &[])?;
        record(&root, "ferry.txt", "Ferry: raft\n", "Rig the ferry")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        let pushed = oid(&root, "main")?;
        let short = notice::short(&pushed).to_owned();
        self.open(&root)?;
        self.until("Ferry open on main, level with origin/main", |tab| {
            tab.ui.repo == "Ferry" && tab.ui.head.as_deref() == Some(pushed.as_str()) && Self::counts(tab) == Some((0, 0))
        })
        .await?;
        let warning = |replay: &mut Self| -> Result<Option<String>> {
            Ok(replay.tab.read_with(replay.cx, |tab, _| view::amend_warning(&tab.ui))?)
        };
        if warning(self)?.is_some() {
            return Err("The commit panel warns before Amend is ticked".into());
        }
        self.action(Action::ToggleAmend)?;
        self.until("the last message loaded for amending", |tab| {
            tab.ui.amend.as_ref().is_some_and(|amend| amend.original.is_some())
        })
        .await?;
        let expected = "This commit is already on origin/main. Amending it means a force push.";
        if warning(self)?.as_deref() != Some(expected) {
            return Err(format!("Under the Amend tick the panel says {:?}", warning(self)?).into());
        }
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui.input.update(cx, |field, cx| field.set_text("Rig the ferry with oars", cx));
        })?;
        self.action(Action::Commit)?;
        self.until("the last commit amended", |tab| {
            tab.ui.amend.is_none() && tab.ui.head.as_deref() != Some(pushed.as_str()) && Self::counts(tab) == Some((1, 1))
        })
        .await?;
        let amended = oid(&root, "main")?;
        git_is(&root, &["log", "-1", "--format=%s", "main"], "Rig the ferry with oars\n")?;
        if warning(self)?.is_some() {
            return Err("The warning stayed after the amend".into());
        }
        let on_origin = |expected: &str| git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{expected}\n"));

        self.action(Action::Sync(sync::Action::Push))?;
        let shown = self.asked("main is behind origin/main").await?;
        let lines = vec![
            "origin/main has 1 commit that main does not. Pull to bring them in, then push.".to_owned(),
            "Or force push to replace them with your 1 commit.".to_owned(),
        ];
        if shown.1 != lines || shown.2 {
            return Err(format!("The behind prompt shows {shown:?}").into());
        }
        let choices = self.tab.read_with(self.cx, |tab, _| {
            prompt::shown(&tab.ui).map(|spec| spec.choices.iter().map(|c| (c.label.to_string(), c.danger)).collect::<Vec<_>>())
        })?;
        if choices != Some(vec![("Force push…".to_owned(), true)]) || self.prompt_focus()? != Some(Stop::Confirm) {
            return Err(format!("The behind prompt offers {choices:?} with {:?} focused", self.prompt_focus()?).into());
        }
        self.action(Action::Prompt(prompt::Action::Choose(0)))?;
        let shown = self.asked("Force push main to origin/main?").await?;
        let lines = vec![
            "This replaces 1 commit on origin/main with your 1. Anyone who already pulled it will have to recover by hand.".to_owned(),
            format!("{short}  Rig the ferry"),
            format!("It goes ahead only while origin still has main at {short}, where it was when you last fetched."),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The force push prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The force push prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("the force push cancelled", |tab| !tab.ui.prompt.open()).await?;
        on_origin(&pushed)?;

        self.push_and_force("main", "origin/main").await?;
        self.key(CMD_RETURN)?;
        self.until("main force pushed", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == "Force pushed main") && Self::counts(tab) == Some((0, 0))
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Success,
            "Force pushed main".to_owned(),
            "Replaced 1 commit on origin/main with 1 from main.".to_owned(),
            vec!["Restore origin/main".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("The force push said {said:?}").into());
        }
        on_origin(&amended)?;
        git_is(&root, &["rev-parse", "refs/remotes/origin/main", "refs/heads/main"], &format!("{amended}\n{amended}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn force_stale(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0217");
        let (root, origin) = Self::crossing(dir, "Weir", &[])?;
        record(&root, "weir.txt", "Weir: stone\n", "Raise the weir")?;
        git(&root, &["push", "-q", "origin", "main"])?;
        let pushed = oid(&root, "main")?;
        git(&root, &["commit", "-q", "--amend", "-m", "Raise the weir higher"])?;
        let amended = oid(&root, "main")?;
        let moved = Self::peer_push(dir, &origin, "main", "Open the sluices")?;
        let (short, moved_short) = (notice::short(&pushed).to_owned(), notice::short(&moved).to_owned());
        let on_origin = || git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{moved}\n"));
        self.open(&root)?;
        self.until("Weir open with main and origin/main apart", |tab| {
            tab.ui.repo == "Weir" && tab.ui.head.as_deref() == Some(amended.as_str()) && Self::counts(tab) == Some((1, 1))
        })
        .await?;

        let shown = self.push_and_force("main", "origin/main").await?;
        let lines = vec![
            "This replaces 1 commit on origin/main with your 1. Anyone who already pulled it will have to recover by hand.".to_owned(),
            format!("{short}  Raise the weir"),
            format!("It goes ahead only while origin still has main at {short}, where it was when you last fetched."),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The force push prompt shows {shown:?}").into());
        }
        self.key(CMD_RETURN)?;
        self.until("the force push refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title == "Force Push Failed")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Error,
            "Force Push Failed".to_owned(),
            "origin/main changed since your last fetch.".to_owned(),
            vec!["Fetch".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("The stale force push said {said:?}").into());
        }
        let output = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().and_then(|n| n.output.clone()))?;
        if !output.as_deref().is_some_and(|output| output.contains("main: [rejected] (stale info)")) {
            return Err(format!("The stale force push keeps {output:?} as Git output").into());
        }
        on_origin()?;
        git_is(&root, &["rev-parse", "refs/heads/main", "refs/remotes/origin/main"], &format!("{amended}\n{pushed}\n"))?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("origin fetched", |tab| {
            tab.ui.refs.oid_of(&Target::RemoteBranch("origin/main".into())) == Some(moved.as_str()) && Self::counts(tab) == Some((1, 2))
        })
        .await?;
        let shown = self.push_and_force("main", "origin/main").await?;
        let lines = vec![
            "This replaces 2 commits on origin/main with your 1. Anyone who already pulled them will have to recover by hand.".to_owned(),
            format!("{moved_short}  Open the sluices"),
            format!("{short}  Raise the weir"),
            format!("It goes ahead only while origin still has main at {moved_short}, where it was when you last fetched."),
        ];
        if shown.1 != lines || !shown.2 {
            return Err(format!("The force push prompt after the fetch shows {shown:?}").into());
        }
        self.key("escape")?;
        self.until("the force push cancelled", |tab| !tab.ui.prompt.open()).await?;
        on_origin()?;
        git_is(&root, &["rev-parse", "refs/heads/main"], &format!("{amended}\n"))?;
        self.pass();
        Ok(())
    }

    async fn rejected_push(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0218");
        let (root, origin) = Self::crossing(dir, "Pier", &[])?;
        git(&root, &["branch", "notes"])?;
        record(&root, "pier.txt", "Pier: planks\n", "Lay the planks")?;
        let local = oid(&root, "main")?;
        let moved = Self::peer_push(dir, &origin, "main", "Paint the posts")?;
        let on_origin = |expected: &str| git_is(&origin, &["rev-parse", "refs/heads/main"], &format!("{expected}\n"));
        self.open(&root)?;
        self.until("Pier open on main, one commit ahead of what was fetched", |tab| {
            tab.ui.repo == "Pier" && tab.ui.head.as_deref() == Some(local.as_str()) && Self::counts(tab) == Some((1, 0))
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.sync.strategy())? != sync::Strategy::FastForwardIfPossible {
            return Err("Pull is not set to fast-forward if possible".into());
        }

        self.action(Action::Sync(sync::Action::Push))?;
        self.until("the push rejected", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title == "Push Rejected")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Error,
            "Push Rejected".to_owned(),
            "origin/main has 1 commit that main does not.".to_owned(),
            vec!["Pull".to_owned(), "Force push…".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("The rejected push said {said:?}").into());
        }
        let output = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().and_then(|n| n.output.clone()))?;
        if !output.as_deref().is_some_and(|output| output.contains("main: [rejected] (fetch first)")) {
            return Err(format!("The rejected push keeps {output:?} as Git output").into());
        }
        on_origin(&moved)?;
        git_is(&root, &["rev-parse", "refs/heads/main", "refs/remotes/origin/main"], &format!("{local}\n{moved}\n"))?;

        let offered = self.newest_notice()?;
        git(&root, &["switch", "-q", "notes"])?;
        self.action(Action::Refresh)?;
        self.until("notes checked out", |tab| tab.ui.branch == "notes").await?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the offered Pull refused", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.tone == Tone::Error && n.title == "Pull Failed")
        })
        .await?;
        let said = self.said()?;
        let detail = "This Pull was for main, but notes is checked out now. Switch back to main to pull it.";
        if said.as_ref().map(|said| said.2.as_str()) != Some(detail) {
            return Err(format!("The offered Pull on notes said {said:?}").into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/notes\n")?;
        git_is(&root, &["rev-parse", "refs/heads/main"], &format!("{local}\n"))?;

        git(&root, &["switch", "-q", "main"])?;
        self.action(Action::Refresh)?;
        self.until("main checked out, one commit behind", |tab| tab.ui.branch == "main" && Self::counts(tab) == Some((1, 1))).await?;
        self.action(Action::Sync(sync::Action::Push))?;
        let shown = self.asked("main is behind origin/main").await?;
        if shown.1.first().map(String::as_str) != Some("origin/main has 1 commit that main does not. Pull to bring them in, then push.") || shown.2 {
            return Err(format!("The behind prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Confirm) {
            return Err("The behind prompt did not open with Pull focused".into());
        }
        self.key("enter")?;
        self.until("main pulled", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == "Pulled Successfully") && Self::counts(tab) == Some((2, 0))
        })
        .await?;
        git_is(&root, &["show", "-s", "--format=%P", "main"], &format!("{local} {moved}\n"))?;
        let merged = oid(&root, "main")?;
        self.until("the merge shown as main's tip", |tab| tab.ui.head.as_deref() == Some(merged.as_str())).await?;
        self.action(Action::Sync(sync::Action::Push))?;
        self.until("main pushed", |tab| {
            tab.ui.notices.newest().is_some_and(|n| n.title == "Pushed Successfully") && Self::counts(tab) == Some((0, 0))
        })
        .await?;
        let said = self.said()?;
        if said.as_ref().map(|said| said.2.as_str()) != Some("2 commits to origin/main.") {
            return Err(format!("The push after the pull said {said:?}").into());
        }
        let merged = oid(&root, "main")?;
        on_origin(&merged)?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    fn stash_form(&mut self) -> Result<(String, bool, bool)> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let text = tab.ui.stash.message().map(|field| field.read(cx).text().to_owned()).unwrap_or_default();
            (text, tab.ui.stash.staged_only(), tab.ui.stash.options_open())
        })?)
    }

    fn stash_field_focused(&mut self) -> Result<bool> {
        Ok(self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.stash.message().is_some_and(|field| field.focus_handle(cx).is_focused(window))
        })?)
    }

    fn stash_list(root: &Path) -> Result<String> {
        git(root, &["stash", "list", "--format=%H %gs"])
    }

    async fn stash_staged(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0219");
        let root = init(dir, "Lantern")?;
        record(&root, "lantern.txt", "Wick: long\n", "Hang the lantern")?;
        record(&root, "notes.txt", "Notes\n", "Start the notes")?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("lantern.txt"), "Wick: trimmed\n")?;
        git(&root, &["add", "--", "lantern.txt"])?;
        fs::write(root.join("notes.txt"), "Notes\nOil is low\n")?;
        fs::write(root.join("sketch.txt"), "A lantern sketch\n")?;
        self.open(&root)?;
        self.until("Lantern open with one staged and two unstaged files", |tab| {
            tab.ui.repo == "Lantern" && files(tab, &["lantern.txt"], &["notes.txt", "sketch.txt"])
        })
        .await?;
        let unchanged = |what: &str| -> Result<()> {
            git_is(&root, &["stash", "list"], "")
                .and_then(|_| git_is(&root, &["status", "--porcelain=v1"], "M  lantern.txt\n M notes.txt\n?? sketch.txt\n"))
                .map_err(|e| format!("{what}: {e}").into())
        };

        self.action(Action::Stash(stash::Action::ToggleOptions))?;
        self.until("the stash form open", |tab| tab.ui.stash.options_open()).await?;
        if !self.stash_field_focused()? {
            return Err("The stash form did not put the cursor in its message field".into());
        }
        self.type_keys("Trim the wick")?;
        self.key("escape")?;
        self.until("Esc closed the stash form", |tab| !tab.ui.stash.options_open()).await?;
        unchanged("Esc")?;
        self.action(Action::Stash(stash::Action::ToggleOptions))?;
        self.until("the stash form open again", |tab| tab.ui.stash.options_open()).await?;
        if self.stash_form()? != ("Trim the wick".to_owned(), false, true) || !self.stash_field_focused()? {
            return Err(format!("The stash form opened again as {:?}", self.stash_form()?).into());
        }
        self.action(Action::Stash(stash::Action::ToggleStaged))?;
        if self.stash_form()? != ("Trim the wick".to_owned(), true, true) {
            return Err(format!("Ticking Staged changes only left the form as {:?}", self.stash_form()?).into());
        }
        unchanged("Ticking")?;
        self.key("enter")?;
        self.until("the staged change stashed under its message", |tab| {
            !tab.ui.stash.options_open()
                && tab.ui.stash.entries.len() == 1
                && tab.ui.stash.entries[0].title == "Trim the wick"
                && files(tab, &[], &["notes.txt", "sketch.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Stashed Trim the wick")
        })
        .await?;
        let said = self.said()?;
        let detail = "Only the staged changes. Unstaged edits and untracked files stay in your files.";
        let expected = (Tone::Success, "Stashed Trim the wick".to_owned(), detail.to_owned(), Vec::new());
        if said.as_ref() != Some(&expected) {
            return Err(format!("The staged stash said {said:?}").into());
        }
        if self.stash_form()? != (String::new(), false, false) {
            return Err(format!("After stashing the form holds {:?}", self.stash_form()?).into());
        }
        let saved = oid(&root, "refs/stash")?;
        git_is(&root, &["stash", "list", "--format=%gs"], "On main: Trim the wick\n")?;
        git_is(&root, &["show", &format!("{saved}:lantern.txt")], "Wick: trimmed\n")?;
        git_is(&root, &["show", &format!("{saved}^2:lantern.txt")], "Wick: trimmed\n")?;
        git_is(&root, &["show", &format!("{saved}:notes.txt")], "Notes\n")?;
        if git(&root, &["rev-parse", "--verify", "--quiet", &format!("{saved}^3")]).is_ok() {
            return Err("The staged-only stash took the untracked sketch".into());
        }
        git_is(&root, &["status", "--porcelain=v1"], " M notes.txt\n?? sketch.txt\n")?;
        if fs::read_to_string(root.join("notes.txt"))? != "Notes\nOil is low\n"
            || fs::read_to_string(root.join("sketch.txt"))? != "A lantern sketch\n"
            || fs::read_to_string(root.join("lantern.txt"))? != "Wick: long\n"
        {
            return Err("The staged-only stash changed the files it should have left".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;

        self.action(Action::Stash(stash::Action::ToggleOptions))?;
        self.until("the stash form open with nothing staged", |tab| tab.ui.stash.options_open()).await?;
        self.action(Action::Stash(stash::Action::ToggleStaged))?;
        self.key("enter")?;
        let refused = self.tab.read_with(self.cx, |tab, _| tab.ui.stash.options_open() && !tab.ui.busy())?;
        if !refused {
            return Err("Return with Staged changes only and nothing staged did not leave the form open".into());
        }
        self.key("escape")?;
        self.until("the stash form closed", |tab| !tab.ui.stash.options_open()).await?;
        git_is(&root, &["stash", "list", "--format=%H"], &format!("{saved}\n"))?;
        self.pass();
        Ok(())
    }

    async fn drop_stash(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0220");
        let root = init(dir, "Cairn")?;
        record(&root, "trail.txt", TRAIL, "Start the cairn trail")?;
        let mut saved = Vec::new();
        for (name, message) in [("first.txt", "First cairn"), ("second.txt", "Second cairn"), ("third.txt", "Third cairn")] {
            fs::write(root.join(name), format!("{message}\n"))?;
            git(&root, &["stash", "push", "-q", "--include-untracked", "-m", message])?;
            saved.push(oid(&root, "refs/stash")?);
        }
        let (first, second, third) = (saved[0].clone(), saved[1].clone(), saved[2].clone());
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Cairn open with three stashes", |tab| {
            tab.ui.repo == "Cairn"
                && tab.ui.stash.entries.iter().map(|entry| entry.title.as_str()).collect::<Vec<_>>()
                    == ["Third cairn", "Second cairn", "First cairn"]
        })
        .await?;
        let all = format!("{third} On main: Third cairn\n{second} On main: Second cairn\n{first} On main: First cairn\n");
        let target = Target::Stash(second.clone());
        let rows = self.menu_rows(&target)?;
        let ids: Vec<_> = rows.iter().map(|row| row.0).collect();
        if ids != ["stash.apply", "stash.pop", "stash.rename", "stash.drop"] || rows.iter().any(|row| row.2.is_some()) {
            return Err(format!("The menu of an older stash lists {rows:?}").into());
        }

        self.choose(&target, "stash.drop")?;
        let shown = self.asked("Drop stash Second cairn?").await?;
        if shown.1.len() != 2
            || !shown.1[0].starts_with(&format!("stash@{{1}}, {}, saved ", notice::short(&second)))
            || shown.1[1] != "Its changes leave the stash list. Restore, in the notice that follows, puts it back."
            || !shown.2
        {
            return Err(format!("The drop prompt shows {shown:?}").into());
        }
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err("The drop prompt did not open with Cancel focused".into());
        }
        self.key("enter")?;
        self.until("Return cancelled the drop", |tab| !tab.ui.prompt.open()).await?;
        git_is(&root, &["stash", "list", "--format=%H %gs"], &all)?;

        self.choose(&target, "stash.drop")?;
        self.asked("Drop stash Second cairn?").await?;
        self.key(CMD_RETURN)?;
        self.until("the middle stash dropped", |tab| {
            tab.ui.stash.entries.len() == 2 && tab.ui.notices.newest().is_some_and(|n| n.title == "Dropped stash Second cairn")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Success,
            "Dropped stash Second cairn".to_owned(),
            format!("was stash@{{1}}, {}", notice::short(&second)),
            vec!["Restore".to_owned()],
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("Dropping the stash said {said:?}").into());
        }
        let left = format!("{third} On main: Third cairn\n{first} On main: First cairn\n");
        git_is(&root, &["stash", "list", "--format=%H %gs"], &left)?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;

        let offered = self.newest_notice()?;
        self.action(Action::Notice(notice::Action::Take(offered, 0)))?;
        self.until("the stash restored", |tab| {
            tab.ui.stash.entries.len() == 3 && tab.ui.notices.newest().is_some_and(|n| n.title == "Restored stash Second cairn")
        })
        .await?;
        let said = self.said()?;
        let expected = (
            Tone::Success,
            "Restored stash Second cairn".to_owned(),
            "It is back as the newest stash, stash@{0}.".to_owned(),
            Vec::new(),
        );
        if said.as_ref() != Some(&expected) {
            return Err(format!("Restoring the stash said {said:?}").into());
        }
        let restored = format!("{second} On main: Second cairn\n{left}");
        if Self::stash_list(&root)? != restored {
            return Err(format!("After Restore the stash list is {:?}", Self::stash_list(&root)?).into());
        }
        git_is(&root, &["show", &format!("{second}^3:second.txt")], "Second cairn\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn pop_older(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0221");
        let root = init(dir, "Satchel")?;
        record(&root, "trail.txt", TRAIL, "Pack the satchel")?;
        fs::write(root.join("map.txt"), "Map: the low pass\n")?;
        git(&root, &["stash", "push", "-q", "--include-untracked", "-m", "Folded map"])?;
        let older = oid(&root, "refs/stash")?;
        fs::write(root.join("trail.txt"), WAYPOINT)?;
        git(&root, &["stash", "push", "-q", "-m", "Waypoint notes"])?;
        let newer = oid(&root, "refs/stash")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Satchel open with two stashes", |tab| tab.ui.repo == "Satchel" && tab.ui.stash.entries.len() == 2)
            .await?;
        let target = Target::Stash(older.clone());
        self.action(Action::Select(Selection::Stash(older.clone())))?;
        self.until("the older stash selected", |tab| {
            matches!(&tab.ui.selection, Some(Selection::Stash(oid)) if *oid == older) && !tab.ui.stash.files.is_empty()
        })
        .await?;
        let rows = self.menu_rows(&target)?;
        if rows.iter().find(|row| row.0 == "stash.pop").map(|row| row.2.clone()) != Some(None) {
            return Err(format!("Pop is not offered for the older stash: {rows:?}").into());
        }

        self.choose(&target, "stash.pop")?;
        self.until("the older stash popped", |tab| {
            tab.ui.stash.entries.len() == 1
                && tab.ui.stash.entries[0].oid == newer
                && files(tab, &[], &["map.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Popped stash Folded map")
        })
        .await?;
        git_is(&root, &["stash", "list", "--format=%H %gs"], &format!("{newer} On main: Waypoint notes\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "?? map.txt\n")?;
        if fs::read_to_string(root.join("map.txt"))? != "Map: the low pass\n" || fs::read_to_string(root.join("trail.txt"))? != TRAIL {
            return Err("Popping the older stash did not restore exactly its changes".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;

        self.key("alt-shift-cmd-s")?;
        self.until("the latest stash popped", |tab| {
            tab.ui.stash.entries.is_empty()
                && files(tab, &[], &["trail.txt", "map.txt"])
                && tab.ui.notices.newest().is_some_and(|n| n.title == "Popped stash Waypoint notes")
        })
        .await?;
        git_is(&root, &["stash", "list"], "")?;
        git_is(&root, &["status", "--porcelain=v1"], " M trail.txt\n?? map.txt\n")?;
        if fs::read_to_string(root.join("trail.txt"))? != WAYPOINT {
            return Err("Pop latest stash did not restore the waypoint".into());
        }
        self.pass();
        Ok(())
    }
}
