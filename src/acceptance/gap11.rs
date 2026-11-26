use super::*;
use crate::{
    commands::{self, Target},
    journal,
    refs::{Section, SideRow},
    worktree,
};

const MAIN_TREE: &str = "Worktree-trail";
const RIDGE_TREE: &str = "Worktree-trail-ridge";
const IN_USE: &str = "Checked out in worktree Worktree-trail-ridge";

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()).into())
}

fn private_refs(root: &Path, space: &str) -> Result<String> {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/fastkraken/undo/{space}/")])
}

impl Replay<'_> {
    pub(super) async fn gap11(&mut self, dir: &Path) -> Result<()> {
        let (root, ridge) = self.list_a_linked_worktree(dir).await?;
        self.refuse_a_branch_in_use(dir, &root, &ridge).await
    }

    async fn until_tab_count(&mut self, description: &str, count: usize, front: usize) -> Result<()> {
        let started = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            let seen = self.tab.tabs(self.cx)?;
            if seen == (count, front) {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description}: tabs and front are {seen:?}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    fn menu_action(&mut self, target: &Target, id: &str) -> Result<Option<Action>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, target, cx).into_iter().find(|entry| entry.id == id).and_then(|entry| entry.action)
        })?)
    }

    async fn list_a_linked_worktree(&mut self, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        self.start("GAP1101");
        let root = init(dir, MAIN_TREE)?;
        record(&root, "trail.txt", TRAIL, "Start the trail")?;
        git(&root, &["branch", "ridge"])?;
        record(&root, "camp.txt", "Camp by the lake\n", "Pitch the camp")?;
        let ridge = dir.join(RIDGE_TREE);
        git(&root, &["worktree", "add", "-q", path(&ridge)?, "ridge"])?;
        record(&ridge, "ridge.txt", "Ridge path\n", "Climb the ridge")?;
        let ridge_tip = oid(&ridge, "HEAD")?;
        fs::write(ridge.join("ridge.txt"), "Ridge path, washed out\n")?;
        fs::write(ridge.join("summit.txt"), "Summit cairn\n")?;
        let listed = git(&root, &["worktree", "list", "--porcelain"])?;
        let paths: Vec<PathBuf> = listed.lines().filter_map(|line| line.strip_prefix("worktree ")).map(PathBuf::from).collect();
        if paths.len() != 2 {
            return Err(format!("Git lists {listed:?}").into());
        }
        self.open(&root)?;
        self.until("Worktree-trail open with both worktrees listed", |tab| {
            tab.ui.repo == MAIN_TREE && tab.ui.branch == "main" && tab.ui.history.len() == 3 && tab.ui.worktree.list.len() == 2
        })
        .await?;
        self.until("the Worktrees section drawn", |tab| {
            tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Worktrees, count: 2, .. }))
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            let rows: Vec<usize> = tab
                .ui
                .refs
                .rows
                .iter()
                .filter_map(|row| match row {
                    SideRow::Worktree(index) => Some(*index),
                    _ => None,
                })
                .collect();
            let listed: Vec<(PathBuf, String, String, bool)> = tab
                .ui
                .worktree
                .list
                .iter()
                .map(|worktree| (worktree.path.clone(), worktree.name(), worktree.on(), worktree.current))
                .collect();
            (rows, listed)
        })?;
        if shown.0 != [0, 1] {
            return Err(format!("The Worktrees section lists rows {:?}", shown.0).into());
        }
        let wanted = [(MAIN_TREE, "main", true), (RIDGE_TREE, "ridge", false)];
        for ((path, name, on, current), (want_name, want_on, want_current)) in shown.1.iter().zip(wanted) {
            if name != want_name || on != want_on || *current != want_current {
                return Err(format!("The Worktrees section reads {:?}", shown.1).into());
            }
            if !paths.iter().any(|listed| repository::same_place(listed, path)) {
                return Err(format!("{path:?} is not among Git's worktrees {paths:?}").into());
            }
        }
        let ridge_path = shown.1[1].0.clone();
        let chips = self.tab.read_with(self.cx, |tab, _| {
            let on_tip = tab.ui.history.iter().any(|row| row.oid == ridge_tip);
            (on_tip, tab.ui.worktree.at(&ridge_tip).map(worktree::Worktree::name).collect::<Vec<_>>())
        })?;
        if chips != (true, vec![RIDGE_TREE.to_owned()]) {
            return Err(format!("The ridge commit's worktree chips are {chips:?}").into());
        }
        let counted = path(&ridge)?.to_owned();
        let status = git(dir, &["-C", &counted, "status", "--porcelain=v2"])?;
        if status.lines().count() != 2 {
            return Err(format!("Git's status in the worktree reads {status:?}").into());
        }
        let wanted_path = ridge_path.clone();
        self.until("the other worktree's changes counted", move |tab| {
            matches!(tab.ui.worktree.count(&wanted_path), Some(Ok(2)))
        })
        .await?;
        let tip = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.worktree.find(&ridge_path).map(|worktree| worktree::tip(&tab.ui.worktree, worktree))
        })?;
        let expected = format!("{}\nOn ridge\n2 uncommitted changes", ridge_path.display());
        if tip.as_deref() != Some(expected.as_str()) {
            return Err(format!("Hovering the worktree chip gives {tip:?}").into());
        }

        self.action(Action::Worktree(worktree::Action::Open(ridge_path.clone())))?;
        self.until_tab_count("a second tab in front", 2, 1).await?;
        self.until("the ridge worktree open in the new tab", |tab| {
            tab.ui.repo == RIDGE_TREE
                && tab.ui.branch == "ridge"
                && !tab.ui.history.is_empty()
                && tab.ui.worktree.list.iter().any(|worktree| worktree.current && worktree.name() == RIDGE_TREE)
                && tab.ui.worktree.list.iter().any(|worktree| !worktree.current && worktree.name() == MAIN_TREE)
        })
        .await?;
        git_is(&ridge, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;

        self.action(Action::Refs(refs::Action::CreateBranch("ridge-camp".into())))?;
        self.until("ridge-camp made in the ridge worktree", |tab| tab.ui.branch == "ridge-camp" && !tab.ui.busy()).await?;
        let linked_held = private_refs(&root, "worktrees/Worktree-trail-ridge")?;
        if linked_held.lines().count() != 1 || !private_refs(&root, "main")?.is_empty() {
            return Err(format!("After a write in the ridge tab Git holds {:?}", git(&root, &["for-each-ref", "refs/fastkraken/"])?).into());
        }
        self.action(Action::Journal(journal::Action::Undo(None)))?;
        self.until("the ridge tab back on ridge", |tab| tab.ui.branch == "ridge" && !tab.ui.busy()).await?;
        git_is(&ridge, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;
        git_is(&root, &["for-each-ref", "--format=%(refname)", "refs/heads/ridge-camp"], "")?;

        self.key("cmd-1")?;
        self.until_tab_count("the main worktree's tab in front", 2, 0).await?;
        self.until("the main worktree in front", |tab| tab.ui.repo == MAIN_TREE && tab.ui.branch == "main").await?;
        self.action(Action::Refs(refs::Action::CreateBranch("camp".into())))?;
        self.until("camp made in the main worktree", |tab| tab.ui.branch == "camp" && !tab.ui.busy()).await?;
        let main_held = private_refs(&root, "main")?;
        let ids = |held: &str| -> Vec<String> {
            held.lines().filter_map(|line| line.split(' ').next()?.rsplit('/').nth(1).map(str::to_owned)).collect()
        };
        if main_held.lines().count() != 1 || private_refs(&root, "worktrees/Worktree-trail-ridge")? != linked_held || ids(&main_held) != ids(&linked_held) {
            return Err(format!("With both tabs written Git holds {:?}", git(&root, &["for-each-ref", "refs/fastkraken/"])?).into());
        }
        self.action(Action::Journal(journal::Action::Undo(None)))?;
        self.until("the main tab back on main", |tab| tab.ui.branch == "main" && !tab.ui.busy()).await?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["for-each-ref", "--format=%(refname)", "refs/heads/camp"], "")?;

        self.key("cmd-2")?;
        self.until_tab_count("the ridge tab in front", 2, 1).await?;
        self.until("the ridge tab drawn", |tab| tab.ui.repo == RIDGE_TREE).await?;
        self.key("cmd-w")?;
        self.until_tab_count("one tab again", 1, 0).await?;
        self.until("the main worktree in front", |tab| tab.ui.repo == MAIN_TREE).await?;
        self.pass();
        Ok((root, ridge))
    }

    async fn refuse_a_branch_in_use(&mut self, dir: &Path, root: &Path, ridge: &Path) -> Result<()> {
        self.start("GAP1102");
        let bare = dir.join("Worktree-trail.git");
        git(dir, &["init", "-q", "--bare", "-b", "main", path(&bare)?])?;
        git(root, &["remote", "add", "origin", path(&bare)?])?;
        git(root, &["push", "-q", "origin", "ridge"])?;
        self.action(Action::Refresh)?;
        self.until("origin/ridge listed", |tab| {
            tab.ui.refs.items.iter().any(|item| item.kind == refs::Kind::Remote && item.name == "origin/ridge")
                && tab.ui.worktree.holding(std::ffi::OsStr::new("ridge")).is_some()
        })
        .await?;
        let branch = Target::Branch("ridge".into());
        let rows = self.menu_rows(&branch)?;
        let switch = rows.iter().find(|(id, ..)| *id == "branch.switch");
        if switch != Some(&("branch.switch", "Switch to ridge".to_owned(), Some(IN_USE.to_owned()))) {
            return Err(format!("The ridge branch menu reads {rows:?}").into());
        }
        if !rows.iter().any(|row| *row == ("branch.open-worktree", "Open that worktree".to_owned(), None)) {
            return Err(format!("The ridge branch menu offers {rows:?}").into());
        }
        let marked = self.tab.read_with(self.cx, |tab, _| {
            (worktree::in_use(&tab.ui, std::ffi::OsStr::new("ridge")), worktree::in_use(&tab.ui, std::ffi::OsStr::new("main")))
        })?;
        if marked != (Some(IN_USE.to_owned()), None) {
            return Err(format!("The branch marks read {marked:?}").into());
        }

        self.ref_action(refs::Action::Switch("ridge".into()))?;
        self.until("the switch refused with the worktree's name", |tab| {
            tab.ui.notices.newest().is_some_and(|notice| notice.title == "ridge is checked out in worktree Worktree-trail-ridge")
        })
        .await?;
        let said = self.said()?.ok_or("The refusal went away")?;
        if said.3 != ["Open that worktree"] {
            return Err(format!("The refusal reads {said:?}").into());
        }
        git_is(root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(ridge, &["symbolic-ref", "HEAD"], "refs/heads/ridge\n")?;

        let remote = Target::RemoteBranch("origin/ridge".into());
        let rows = self.menu_rows(&remote)?;
        let checkout = rows.iter().find(|(id, ..)| *id == "branch.checkout-remote");
        if checkout != Some(&("branch.checkout-remote", "Check out origin/ridge".to_owned(), Some(IN_USE.to_owned()))) {
            return Err(format!("The origin/ridge menu reads {rows:?}").into());
        }
        if !rows.iter().any(|row| *row == ("branch.open-worktree", "Open that worktree".to_owned(), None)) {
            return Err(format!("The origin/ridge menu offers {rows:?}").into());
        }
        let open = self.menu_action(&remote, "branch.open-worktree")?.ok_or("Open that worktree has no action")?;
        self.action(open)?;
        self.until_tab_count("the ridge worktree in a second tab", 2, 1).await?;
        self.until("the ridge worktree open", |tab| tab.ui.repo == RIDGE_TREE && tab.ui.branch == "ridge").await?;
        git_is(root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        self.key("cmd-w")?;
        self.until_tab_count("one tab again", 1, 0).await?;
        self.until("the main worktree in front", |tab| tab.ui.repo == MAIN_TREE).await?;
        self.pass();
        Ok(())
    }
}
