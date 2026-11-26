use super::*;
use crate::{
    commands::{self, Availability, Target},
    commands::Side,
    journal, notice,
    prompt::{self, Stop},
    refs::{Section, SideRow},
    submodule, sync, worktree,
};

const MAIN_TREE: &str = "Worktree-trail";
const RIDGE_TREE: &str = "Worktree-trail-ridge";
const IN_USE: &str = "Checked out in worktree Worktree-trail-ridge";
const CAMP_TREE: &str = "Camp-trail";
const LAKE_TREE: &str = "Camp-trail-lake";
const MAIN_TREE_MEADOW: &str = "Camp-trail-meadow";
const FORD_TREE: &str = "Camp-trail-ford";
const SUMMIT: &str = "Summit-trail";
const SUMMIT_MAPS: &str = "Summit-maps";
const SUMMIT_PEER: &str = "Summit-peer";
const BASE_CAMP: &str = "Base-camp";
const BASE_ATLAS: &str = "Base-atlas";
const BASE_MIRROR: &str = "Base-atlas-mirror.git";
const ATLAS: &str = "vendor/atlas";

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()).into())
}

fn scripted(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = repository::bundled_command(root)?;
    let output = repository::with_system_tools(&mut command).args(args).output()?;
    if !output.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn private_refs(root: &Path, space: &str) -> Result<String> {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/fastkraken/undo/{space}/")])
}

impl Replay<'_> {
    pub(super) async fn gap11(&mut self, dir: &Path) -> Result<()> {
        let (root, ridge) = self.list_a_linked_worktree(dir).await?;
        self.refuse_a_branch_in_use(dir, &root, &ridge).await?;
        let camp = self.create_and_remove_a_worktree(dir).await?;
        self.remove_a_dirty_worktree(dir, &camp).await?;
        let (summit, maps) = self.a_submodule_with_new_commits(dir).await?;
        self.update_a_submodule(&summit).await?;
        self.pull_a_moved_submodule(dir, &summit, &maps).await?;
        self.add_and_remove_a_submodule(dir).await
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

    fn worktree_prompt(&mut self) -> Result<Option<(String, Vec<String>, bool, Option<(String, bool)>, bool, String)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            if !matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Worktree(_))) {
                return None;
            }
            let spec = prompt::shown(&tab.ui)?;
            let field = tab.ui.prompt.field().map(|field| field.read(cx).text().to_owned()).unwrap_or_default();
            let tick = spec.tick.map(|(label, _)| (label.to_string(), tab.ui.prompt.ticked()));
            Some((spec.title, spec.lines, spec.danger, tick, spec.fixed_tick, field))
        })?)
    }

    async fn until_worktree_prompt(&mut self, title: &str) -> Result<(String, Vec<String>, bool, Option<(String, bool)>, bool, String)> {
        let wanted = title.to_owned();
        self.until(&format!("the prompt {title:?}"), move |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Worktree(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == wanted)
        })
        .await?;
        self.worktree_prompt()?.ok_or_else(|| "The worktree prompt closed".into())
    }

    fn type_second(&mut self, text: &str) -> Result<()> {
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            let Some(second) = tab.ui.prompt.second().cloned() else { return false };
            second.update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx));
            second.read(cx).text() == text
        })?;
        if !typed {
            return Err(format!("The prompt's second field did not take {text:?}").into());
        }
        Ok(())
    }

    fn type_first(&mut self, text: &str) -> Result<()> {
        let typed = self.tab.update(self.cx, |tab, window, cx| {
            let Some(field) = tab.ui.prompt.field().cloned() else { return false };
            field.update(cx, |field, cx| field.replace_text_in_range(None, text, window, cx));
            field.read(cx).text() == text
        })?;
        if !typed {
            return Err(format!("The prompt's field did not take {text:?}").into());
        }
        Ok(())
    }

    async fn until_said(&mut self, title: &str) -> Result<Vec<String>> {
        let wanted = title.to_owned();
        self.until(&format!("the notice {title:?}"), move |tab| {
            !tab.ui.busy() && !tab.ui.prompt.open() && tab.ui.notices.newest().is_some_and(|notice| notice.title == wanted)
        })
        .await?;
        Ok(self.said()?.map(|said| said.3).unwrap_or_default())
    }

    fn listed_path(&mut self, name: &str) -> Result<PathBuf> {
        let wanted = name.to_owned();
        let found = self.tab.read_with(self.cx, move |tab, _| {
            tab.ui.worktree.list.iter().find(|worktree| worktree.name() == wanted).map(|worktree| worktree.path.clone())
        })?;
        found.ok_or_else(|| format!("{name} is not listed").into())
    }

    fn git_lists(root: &Path, folder: &Path) -> Result<Option<String>> {
        let listed = git(root, &["worktree", "list", "--porcelain"])?;
        Ok(listed
            .split("\n\n")
            .find(|record| {
                record.lines().next().and_then(|line| line.strip_prefix("worktree ")).is_some_and(|path| repository::same_place(Path::new(path), folder))
            })
            .map(str::to_owned))
    }

    async fn create_and_remove_a_worktree(&mut self, dir: &Path) -> Result<PathBuf> {
        self.start("GAP1103");
        let root = init(dir, CAMP_TREE)?;
        record(&root, "trail.txt", TRAIL, "Start the camp trail")?;
        git(&root, &["branch", "lake"])?;
        record(&root, "camp.txt", "Camp by the lake\n", "Pitch the camp")?;
        let (lake_tip, main_tip) = (oid(&root, "lake")?, oid(&root, "main")?);
        self.open(&root)?;
        self.until("Camp-trail open with one worktree", |tab| {
            tab.ui.repo == CAMP_TREE
                && tab.ui.branch == "main"
                && tab.ui.history.len() == 2
                && tab.ui.worktree.list.len() == 1
                && tab.ui.refs.lists(refs::Kind::Branch, "lake")
                && tab.ui.refs.local(std::ffi::OsStr::new("main")).is_some()
                && !tab.ui.busy()
        })
        .await?;
        let hidden = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Worktrees, .. }))
        })?;
        if hidden {
            return Err("The Worktrees section shows with only one worktree".into());
        }

        let lake = Target::Branch("lake".into());
        let rows = self.menu_rows(&lake)?;
        if !rows.iter().any(|row| *row == ("worktree.new", "New worktree from lake…".to_owned(), None)) {
            return Err(format!("The lake branch menu reads {rows:?}").into());
        }
        self.choose(&lake, "worktree.new")?;
        let asked = self.until_worktree_prompt("New worktree from lake").await?;
        let folder = dir.join(LAKE_TREE);
        let real_dir = fs::canonicalize(dir)?;
        if Path::new(&asked.5) != real_dir.join(LAKE_TREE) && Path::new(&asked.5) != folder {
            return Err(format!("The folder offered is {:?}, not beside the repository", asked.5).into());
        }
        if asked.2 || asked.3 != Some(("Create a new branch from lake".to_owned(), false)) || asked.4 {
            return Err(format!("The new worktree prompt reads {asked:?}").into());
        }
        if folder.exists() {
            return Err("The folder exists before the worktree is created".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let offers = self.until_said("Created worktree Camp-trail-lake").await?;
        if offers != ["Open in a new tab"] {
            return Err(format!("Creating the worktree offered {offers:?}").into());
        }
        let offered = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.notices.newest().and_then(|notice| notice.actions.first().map(|(_, action)| action.clone()))
        })?;
        let open_lake = offered.ok_or("The creation notice has no offer")?;
        let recorded = Self::git_lists(&root, &folder)?.ok_or("Git does not list the new worktree")?;
        if !recorded.contains("branch refs/heads/lake") || !recorded.contains(&format!("HEAD {lake_tip}")) {
            return Err(format!("Git records the worktree as {recorded:?}").into());
        }
        if fs::read_to_string(folder.join("trail.txt"))? != TRAIL || folder.join("camp.txt").exists() {
            return Err("The new worktree's folder does not hold lake's files".into());
        }
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.until("the Worktrees section with both worktrees", |tab| {
            tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Worktrees, count: 2, .. }))
                && tab.ui.worktree.list.iter().any(|worktree| worktree.name() == LAKE_TREE && worktree.branch.as_deref() == Some(std::ffi::OsStr::new("lake")))
        })
        .await?;
        let undo = self.tab.read_with(self.cx, |tab, _| journal::availability(&tab.ui, journal::Way::Undo))?;
        let refused = Availability::Disabled(
            "Creating worktree Camp-trail-lake cannot be undone. Remove it from the Worktrees section instead.".to_owned(),
        );
        if undo != refused {
            return Err(format!("Undo after creating a worktree reads {undo:?}").into());
        }

        let section = Target::Section(Section::Worktrees);
        let rows = self.menu_rows(&section)?;
        let wanted = [
            ("worktree.new", "New worktree…".to_owned(), None),
            ("worktree.prune", "Remove missing worktrees".to_owned(), Some("No worktree's folder is missing".to_owned())),
        ];
        if !wanted.iter().all(|row| rows.contains(row)) {
            return Err(format!("The Worktrees section menu reads {rows:?}").into());
        }
        self.choose(&section, "worktree.new")?;
        let asked = self.until_worktree_prompt("New worktree from main").await?;
        let meadow = dir.join(MAIN_TREE_MEADOW);
        if asked.3 != Some(("Create a new branch from main".to_owned(), true))
            || !asked.4
            || !asked.1.contains(&"main is checked out in this tab, so the new worktree needs a branch of its own.".to_owned())
        {
            return Err(format!("The prompt for a worktree from the checked-out branch reads {asked:?}").into());
        }
        if Path::new(&asked.5) != real_dir.join("Camp-trail-main") && Path::new(&asked.5) != dir.join("Camp-trail-main") {
            return Err(format!("The folder offered for main is {:?}", asked.5).into());
        }
        let typed_folder = path(&meadow)?.to_owned();
        self.tab.update(self.cx, |tab, window, cx| {
            if let Some(field) = tab.ui.prompt.field().cloned() {
                field.update(cx, |field, cx| {
                    field.set_text("", cx);
                    field.replace_text_in_range(None, &typed_folder, window, cx);
                });
            }
        })?;
        self.type_second("meadow")?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until_said("Created worktree Camp-trail-meadow").await?;
        let recorded = Self::git_lists(&root, &meadow)?.ok_or("Git does not list the meadow worktree")?;
        if !recorded.contains("branch refs/heads/meadow") || !recorded.contains(&format!("HEAD {main_tip}")) {
            return Err(format!("Git records the meadow worktree as {recorded:?}").into());
        }
        git_is(&root, &["rev-parse", "meadow"], &format!("{main_tip}\n"))?;
        git_is(&root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;

        self.until("all three worktrees listed", |tab| tab.ui.worktree.list.len() == 3).await?;
        let lake_path = self.listed_path(LAKE_TREE)?;
        self.action(open_lake)?;
        self.until_tab_count("the lake worktree in a second tab", 2, 1).await?;
        self.until("the lake worktree open", |tab| tab.ui.repo == LAKE_TREE && tab.ui.branch == "lake").await?;
        self.key("cmd-1")?;
        self.until_tab_count("the main worktree in front", 2, 0).await?;
        self.until("the main worktree drawn", |tab| tab.ui.repo == CAMP_TREE).await?;
        self.choose(&Target::Worktree(lake_path.clone()), "worktree.remove")?;
        self.until_said("Worktree Camp-trail-lake is open in another tab").await?;
        if !folder.is_dir() || Self::git_lists(&root, &folder)?.is_none() {
            return Err("Removing a worktree open in another tab touched it".into());
        }
        self.key("cmd-2")?;
        self.until_tab_count("the lake tab in front", 2, 1).await?;
        self.until("the lake tab drawn", |tab| tab.ui.repo == LAKE_TREE).await?;
        self.key("cmd-w")?;
        self.until_tab_count("one tab again", 1, 0).await?;
        self.until("the main worktree in front", |tab| tab.ui.repo == CAMP_TREE && !tab.ui.busy()).await?;

        let menu = self.menu_rows(&Target::Worktree(lake_path.clone()))?;
        let ids: Vec<&str> = menu.iter().map(|row| row.0).collect();
        if !["worktree.open", "worktree.lock", "worktree.remove"].iter().all(|id| ids.contains(id)) || ids.contains(&"worktree.unlock") {
            return Err(format!("The lake worktree's menu reads {menu:?}").into());
        }
        self.choose(&Target::Worktree(lake_path.clone()), "worktree.remove")?;
        let asked = self.until_worktree_prompt("Remove worktree Camp-trail-lake?").await?;
        let expected_lines = vec![
            format!("Its folder {} is deleted, with any ignored files in it.", lake_path.display()),
            "Branch lake is kept unless you delete it too. Its commits are also on other branches or tags.".to_owned(),
        ];
        if asked.2 || asked.1 != expected_lines || asked.3 != Some(("Also delete branch lake".to_owned(), false)) {
            return Err(format!("The remove prompt for a clean worktree reads {asked:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until_said("Removed worktree Camp-trail-lake").await?;
        if folder.exists() || Self::git_lists(&root, &folder)?.is_some() {
            return Err("The lake worktree's folder or Git's record is still there".into());
        }
        git_is(&root, &["rev-parse", "lake"], &format!("{lake_tip}\n"))?;

        self.until("the lake worktree gone from the list", |tab| tab.ui.worktree.list.len() == 2).await?;
        let meadow_path = self.listed_path(MAIN_TREE_MEADOW)?;
        self.choose(&Target::Worktree(meadow_path), "worktree.remove")?;
        self.until_worktree_prompt("Remove worktree Camp-trail-meadow?").await?;
        self.action(Action::Prompt(prompt::Action::Toggle))?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let offers = self.until_said("Removed worktree Camp-trail-meadow and branch meadow").await?;
        if offers.first().map(String::as_str) != Some("Undo") {
            return Err(format!("Removing a worktree and its branch offered {offers:?}").into());
        }
        if meadow.exists() || Self::git_lists(&root, &meadow)?.is_some() {
            return Err("The meadow worktree's folder or Git's record is still there".into());
        }
        git_is(&root, &["for-each-ref", "refs/heads/meadow"], "")?;
        self.until("the Worktrees section gone", |tab| {
            tab.ui.worktree.list.len() == 1
                && !tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Worktrees, .. }))
        })
        .await?;
        self.action(Action::Journal(journal::Action::Undo(None)))?;
        self.until("branch meadow back", |tab| !tab.ui.busy() && tab.ui.refs.local(std::ffi::OsStr::new("meadow")).is_some()).await?;
        git_is(&root, &["rev-parse", "meadow"], &format!("{main_tip}\n"))?;
        if meadow.exists() {
            return Err("Undo brought back the meadow folder".into());
        }

        let gone = dir.join("Camp-trail-gone");
        git(&root, &["worktree", "add", "-q", "-b", "gone", path(&gone)?])?;
        fs::remove_dir_all(&gone)?;
        self.action(Action::Refresh)?;
        self.until("the missing worktree marked", |tab| {
            tab.ui.worktree.list.iter().any(|worktree| worktree.name() == "Camp-trail-gone" && worktree.prunable.is_some())
        })
        .await?;
        let gone_path = self.listed_path("Camp-trail-gone")?;
        let menu = self.menu_rows(&Target::Worktree(gone_path))?;
        let remove = menu.iter().find(|row| row.0 == "worktree.remove").and_then(|row| row.2.clone());
        if remove.as_deref() != Some("Its folder is missing. Use Remove missing worktrees") {
            return Err(format!("The missing worktree's menu reads {menu:?}").into());
        }
        self.choose(&Target::Section(Section::Worktrees), "worktree.prune")?;
        self.until_said("Removed missing worktree Camp-trail-gone").await?;
        if Self::git_lists(&root, &gone)?.is_some() {
            return Err("Git still lists the missing worktree".into());
        }
        let gone_tip = oid(&root, "gone")?;
        if gone_tip != main_tip {
            return Err("Pruning the missing worktree moved its branch".into());
        }
        self.until("one worktree again", |tab| tab.ui.worktree.list.len() == 1).await?;
        self.pass();
        Ok(root)
    }

    async fn remove_a_dirty_worktree(&mut self, dir: &Path, root: &Path) -> Result<()> {
        self.start("GAP1104");
        let ford = dir.join(FORD_TREE);
        git(root, &["worktree", "add", "-q", "-b", "ford", path(&ford)?, "lake"])?;
        fs::write(ford.join("trail.txt"), "FastKraken: git for the impatient\nThe ford is flooded\n")?;
        fs::write(ford.join("ford.txt"), "Stepping stones\n")?;
        let counted = path(&ford)?.to_owned();
        let status = git(dir, &["-C", &counted, "status", "--porcelain=v1"])?;
        if status.lines().count() != 2 {
            return Err(format!("Git's status in the ford worktree reads {status:?}").into());
        }
        self.action(Action::Refresh)?;
        self.until("the ford worktree listed", |tab| tab.ui.worktree.list.iter().any(|worktree| worktree.name() == FORD_TREE)).await?;
        let ford_path = self.listed_path(FORD_TREE)?;
        let target = Target::Worktree(ford_path.clone());

        self.choose(&target, "worktree.lock")?;
        let asked = self.until_worktree_prompt("Lock worktree Camp-trail-ford").await?;
        if asked.2 || !asked.5.is_empty() {
            return Err(format!("The lock prompt reads {asked:?}").into());
        }
        self.type_first("Waiting for the water to drop")?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until_said("Locked worktree Camp-trail-ford").await?;
        let recorded = Self::git_lists(root, &ford)?.ok_or("Git does not list the ford worktree")?;
        if !recorded.contains("locked Waiting for the water to drop") {
            return Err(format!("Git records the locked worktree as {recorded:?}").into());
        }
        self.until("the ford worktree shown locked", |tab| {
            tab.ui.worktree.list.iter().any(|worktree| worktree.name() == FORD_TREE && worktree.locked.is_some())
        })
        .await?;
        let menu = self.menu_rows(&target)?;
        let remove = menu.iter().find(|row| row.0 == "worktree.remove").and_then(|row| row.2.clone());
        if remove.as_deref() != Some("It is locked. Unlock it first") || !menu.iter().any(|row| row.0 == "worktree.unlock" && row.2.is_none()) {
            return Err(format!("The locked worktree's menu reads {menu:?}").into());
        }
        self.choose(&target, "worktree.unlock")?;
        self.until_said("Unlocked worktree Camp-trail-ford").await?;
        let recorded = Self::git_lists(root, &ford)?.ok_or("Git does not list the ford worktree")?;
        if recorded.contains("locked") {
            return Err(format!("Git still records the worktree as locked: {recorded:?}").into());
        }
        self.until("the ford worktree shown unlocked", |tab| {
            tab.ui.worktree.list.iter().any(|worktree| worktree.name() == FORD_TREE && worktree.locked.is_none())
        })
        .await?;

        self.choose(&target, "worktree.remove")?;
        let asked = self.until_worktree_prompt("Remove worktree Camp-trail-ford?").await?;
        let first = format!("Its folder {} is deleted with its 2 uncommitted changes.", ford_path.display());
        if !asked.2 || asked.1.first() != Some(&first) || asked.1.get(1).map(String::as_str) != Some("They have not been committed, so they cannot be brought back.") {
            return Err(format!("The remove prompt for a dirty worktree reads {asked:?}").into());
        }
        let confirm = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| spec.confirm.to_string()))?;
        if confirm.as_deref() != Some("Remove worktree") {
            return Err(format!("The dirty remove prompt's button reads {confirm:?}").into());
        }
        self.until("the dirty remove prompt drawn", |tab| tab.ui.prompt.open()).await?;
        if self.prompt_focus()? != Some(Stop::Cancel) {
            return Err(format!("The dirty remove prompt opened with {:?} focused", self.prompt_focus()?).into());
        }
        self.key("enter")?;
        self.until("the prompt cancelled", |tab| !tab.ui.prompt.open()).await?;
        if fs::read_to_string(ford.join("ford.txt"))? != "Stepping stones\n" || !ford.join("trail.txt").exists() {
            return Err("Cancelling touched the ford worktree's files".into());
        }
        if git(dir, &["-C", &counted, "status", "--porcelain=v1"])? != status || Self::git_lists(root, &ford)?.is_none() {
            return Err("Cancelling changed the ford worktree".into());
        }

        self.choose(&target, "worktree.remove")?;
        self.until_worktree_prompt("Remove worktree Camp-trail-ford?").await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until_said("Removed worktree Camp-trail-ford").await?;
        if ford.exists() || Self::git_lists(root, &ford)?.is_some() {
            return Err("The dirty worktree's folder or Git's record is still there".into());
        }
        git_is(root, &["rev-parse", "ford"], &format!("{}\n", oid(root, "lake")?))?;
        git_is(root, &["symbolic-ref", "HEAD"], "refs/heads/main\n")?;
        git_is(root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn a_submodule_with_new_commits(&mut self, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        self.start("GAP1105");
        let config = global_config(dir);
        let mut text = fs::read_to_string(&config).unwrap_or_default();
        text.push_str("[protocol \"file\"]\n\tallow = always\n");
        fs::write(&config, text)?;
        let maps = init(dir, SUMMIT_MAPS)?;
        record(&maps, "map.txt", "Ridge map\n", "Draw the ridge map")?;
        let root = init(dir, SUMMIT)?;
        record(&root, "trail.txt", TRAIL, "Start the summit trail")?;
        scripted(&root, &["submodule", "add", "-q", path(&maps)?, "maps"])?;
        git(&root, &["commit", "-qm", "Add the maps"])?;
        let inside = root.join("maps");
        Self::configure(&inside)?;
        let start = Self::recorded(&root)?;
        record(&inside, "camp.txt", "Camp\n", "Mark the camp")?;
        record(&inside, "lake.txt", "Lake\n", "Mark the lake")?;
        let checked = oid(&inside, "HEAD")?;
        self.open(&root)?;
        self.until("Summit-trail open with its submodule moved", |tab| {
            tab.ui.repo == SUMMIT
                && tab.ui.files.len() == 1
                && tab.ui.files[0].path == Path::new("maps")
                && !tab.ui.files[0].staged
                && tab.ui.files[0].submodule.is_some_and(|flags| flags.moved && !flags.modified && !flags.untracked)
                && tab.ui.submodule.list.len() == 1
                && tab.ui.submodule.list[0].checked_out.is_some()
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            let flags = tab.ui.files[0].submodule.unwrap_or_default();
            let module = tab.ui.submodule.list[0].clone();
            let section = tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Submodules, count: 1, .. }))
                && tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Submodule(0)));
            (submodule::describe(flags, false), submodule::state_text(&tab.ui, &module), module, section)
        })?;
        if shown.0 != "new commits" || shown.1 != "New commits" || !shown.3 {
            return Err(format!("The submodule row says {:?}, its sidebar row {:?}, section shown {}", shown.0, shown.1, shown.3).into());
        }
        if shown.2.recorded.as_deref() != Some(start.as_str()) || shown.2.checked_out.as_deref() != Some(checked.as_str()) || !shown.2.populated {
            return Err(format!("The submodule is listed as {:?}", shown.2).into());
        }
        let file_target = Target::File { path: "maps".into(), side: Side::Unstaged };
        let rows = self.menu_rows(&file_target)?;
        let wanted = [
            ("submodule.open", "Open submodule".to_owned(), None),
            ("submodule.update", "Update to the recorded commit".to_owned(), None),
        ];
        if !wanted.iter().all(|row| rows.contains(row)) || rows.iter().any(|row| row.0 == "file.discard") {
            return Err(format!("The submodule row's menu reads {rows:?}").into());
        }

        self.action(Action::Select(file("maps", false)))?;
        self.until("the submodule panel read", |tab| {
            view::centre(&tab.ui) == view::Centre::Diff
                && submodule::showing(&tab.ui).is_some()
                && submodule::loaded(&tab.ui).is_some_and(|details| details.ahead_count == 2)
        })
        .await?;
        let read = self.tab.read_with(self.cx, |tab, _| submodule::loaded(&tab.ui).cloned())?.ok_or("The panel has no details")?;
        let subjects: Vec<String> = read.ahead.iter().filter_map(|commit| commit.subject.clone()).collect();
        let logged = git(&inside, &["log", "--format=%s", &format!("{start}..HEAD")])?;
        let logged: Vec<String> = logged.lines().map(str::to_owned).collect();
        if read.from.as_ref().map(|commit| commit.oid.as_str()) != Some(start.as_str())
            || read.to.as_ref().map(|commit| commit.oid.as_str()) != Some(checked.as_str())
            || subjects != logged
            || logged != ["Mark the lake", "Mark the camp"]
            || read.behind_count != 0
            || submodule::ahead_title(&read) != "2 commits checked out that this repository does not record"
        {
            return Err(format!("The submodule panel reads {read:?}; Git logs {logged:?}").into());
        }

        fs::write(inside.join("map.txt"), "Ridge map, redrawn\n")?;
        self.until("the submodule's modified content seen", |tab| {
            tab.ui.files.iter().any(|row| row.path == Path::new("maps") && row.submodule.is_some_and(|flags| flags.moved && flags.modified))
                && submodule::loaded(&tab.ui).is_some_and(|details| details.changes == Some(1))
        })
        .await?;
        let summary = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.files.iter().find(|row| !row.staged).and_then(|row| row.submodule).map(|flags| submodule::describe(flags, false))
        })?;
        if summary.as_deref() != Some("new commits, modified content") {
            return Err(format!("The dirty submodule row says {summary:?}").into());
        }
        self.action(Action::StagePath("maps".into()))?;
        self.until_said("Staged the new commit of submodule maps").await?;
        let said = self.said()?.ok_or("No notice after staging the submodule")?;
        if said.2 != "Only its commit is recorded, not the uncommitted changes inside it. Commit those in the submodule first if they belong with it."
            || said.3 != ["Open submodule"]
        {
            return Err(format!("Staging the dirty submodule said {said:?}").into());
        }
        if Self::recorded(&root)? != checked {
            return Err("Staging did not record the submodule's checked-out commit".into());
        }
        git_is(&inside, &["status", "--porcelain=v1"], " M map.txt\n")?;
        self.until("the staged submodule listed", |tab| {
            tab.ui.files.iter().any(|row| row.staged && row.path == Path::new("maps") && row.submodule.is_some())
                && tab.ui.files.iter().any(|row| !row.staged && row.submodule.is_some_and(|flags| flags.modified && !flags.moved))
        })
        .await?;
        self.action(Action::Select(file("maps", true)))?;
        self.until("the staged submodule's panel", |tab| {
            submodule::loaded(&tab.ui).is_some_and(|details| details.staged && details.ahead_count == 2)
        })
        .await?;
        self.action(Action::UnstagePath("maps".into()))?;
        self.until("the submodule unstaged", |tab| tab.ui.files.iter().all(|row| !row.staged)).await?;
        if Self::recorded(&root)? != start {
            return Err("Unstaging did not restore the recorded commit".into());
        }
        git(&inside, &["checkout", "--", "map.txt"])?;
        self.until("the submodule clean inside again", |tab| {
            tab.ui.files.len() == 1 && tab.ui.files[0].submodule.is_some_and(|flags| flags.moved && !flags.modified)
        })
        .await?;

        self.action(Action::Submodule(submodule::Action::Open("maps".into())))?;
        self.until_tab_count("the submodule in a second tab", 2, 1).await?;
        self.until("the submodule open", |tab| tab.ui.repo == "maps" && tab.ui.history.len() == 3).await?;
        self.key("cmd-w")?;
        self.until_tab_count("one tab again", 1, 0).await?;
        self.until("Summit-trail in front", |tab| tab.ui.repo == SUMMIT).await?;
        self.pass();
        Ok((root, maps))
    }

    fn configure(root: &Path) -> Result<()> {
        for (key, value) in [
            ("user.name", "Cedar Contributor"),
            ("user.email", "cedar@example.invalid"),
            ("core.hooksPath", "/dev/null"),
            ("commit.gpgSign", "false"),
        ] {
            git(root, &["config", "--local", key, value])?;
        }
        Ok(())
    }

    fn recorded(root: &Path) -> Result<String> {
        let listed = git(root, &["ls-files", "--stage", "--", "maps"])?;
        let mut fields = listed.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some("160000"), Some(oid)) => Ok(oid.to_owned()),
            _ => Err(format!("Git's index lists maps as {listed:?}").into()),
        }
    }

    async fn update_a_submodule(&mut self, root: &Path) -> Result<()> {
        self.start("GAP1106");
        let inside = root.join("maps");
        let recorded = Self::recorded(root)?;
        let ahead = oid(&inside, "HEAD")?;
        let refs = git(root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags"])?;
        let head = oid(root, "HEAD")?;
        self.action(Action::Select(file("maps", false)))?;
        self.until("the moved submodule's panel", |tab| submodule::loaded(&tab.ui).is_some_and(|details| details.ahead_count == 2)).await?;
        self.action(Action::Submodule(submodule::Action::Update("maps".into())))?;
        let offers = self.until_said("Updated submodule maps").await?;
        let said = self.said()?.ok_or("No notice after the update")?;
        let detail = format!(
            "It is at {}, the commit this repository records. It was at {}.",
            notice::short(&recorded),
            notice::short(&ahead)
        );
        if said.2 != detail || !offers.is_empty() {
            return Err(format!("Updating the submodule said {said:?}").into());
        }
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{recorded}\n"))?;
        if Self::recorded(root)? != recorded || oid(root, "HEAD")? != head {
            return Err("Updating the submodule changed the repository's index or HEAD".into());
        }
        git_is(root, &["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/tags"], &refs)?;
        git_is(&inside, &["cat-file", "-t", &ahead], "commit\n")?;
        git_is(root, &["status", "--porcelain=v1"], "")?;
        self.until("the submodule row gone and the sidebar in sync", |tab| {
            tab.ui.files.is_empty()
                && tab.ui.submodule.list.first().is_some_and(|module| module.in_sync() && submodule::state_text(&tab.ui, module) == "In sync")
        })
        .await?;
        let target = Target::Submodule("maps".into());
        let rows = self.menu_rows(&target)?;
        if !rows.contains(&("submodule.update", "Update to the recorded commit".to_owned(), Some("It is at the recorded commit".to_owned()))) {
            return Err(format!("The in-sync submodule's menu reads {rows:?}").into());
        }

        scripted(root, &["submodule", "deinit", "-q", "-f", "maps"])?;
        self.action(Action::Refresh)?;
        self.until("the submodule listed as not initialised", |tab| {
            tab.ui.submodule.list.first().is_some_and(|module| !module.populated && submodule::state_text(&tab.ui, module) == "Not initialised")
        })
        .await?;
        let rows = self.menu_rows(&target)?;
        let wanted = [
            ("submodule.update", "Initialise".to_owned(), None),
            ("submodule.open", "Open submodule".to_owned(), Some("It is not initialised yet".to_owned())),
        ];
        if !wanted.iter().all(|row| rows.contains(row)) {
            return Err(format!("The uninitialised submodule's menu reads {rows:?}").into());
        }
        if inside.join("map.txt").exists() {
            return Err("Deinitialising left the submodule's files".into());
        }
        self.choose(&target, "submodule.update")?;
        self.until_said("Initialised submodule maps").await?;
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{recorded}\n"))?;
        if fs::read_to_string(inside.join("map.txt"))? != "Ridge map\n" {
            return Err("Initialising did not check out the submodule's files".into());
        }
        self.until("the submodule in sync again", |tab| {
            tab.ui.files.is_empty() && tab.ui.submodule.list.first().is_some_and(submodule::Module::in_sync)
        })
        .await?;
        self.pass();
        Ok(())
    }

    fn move_pointer(dir: &Path, maps: &Path, peer: &Path, subject: &str) -> Result<String> {
        let name = format!("{}.txt", subject.to_lowercase().replace(' ', "-"));
        record(maps, &name, &format!("{subject}\n"), subject)?;
        let tip = oid(maps, "HEAD")?;
        if !peer.exists() {
            git(dir, &["clone", "-q", path(&dir.join("Summit-trail.git"))?, path(peer)?])?;
            Self::configure(peer)?;
        }
        git(peer, &["pull", "-q", "--ff-only"])?;
        git(peer, &["update-index", "--cacheinfo", &format!("160000,{tip},maps")])?;
        git(peer, &["commit", "-qm", &format!("Move the maps to {}", subject.to_lowercase())])?;
        git(peer, &["push", "-q", "origin", "main"])?;
        Ok(tip)
    }

    async fn pull_a_moved_submodule(&mut self, dir: &Path, root: &Path, maps: &Path) -> Result<()> {
        self.start("GAP1107");
        let inside = root.join("maps");
        let bare = dir.join("Summit-trail.git");
        git(dir, &["init", "-q", "--bare", "-b", "main", path(&bare)?])?;
        git(root, &["remote", "add", "origin", path(&bare)?])?;
        git(root, &["push", "-q", "-u", "origin", "main"])?;
        git(root, &["branch", "before-the-pass"])?;
        self.action(Action::Refresh)?;
        self.until("Summit-trail tracking origin/main", |tab| tab.ui.upstream.as_deref() == Some("origin/main")).await?;
        let peer = dir.join(SUMMIT_PEER);
        let before = oid(&inside, "HEAD")?;
        let tip = Self::move_pointer(dir, maps, &peer, "Mark the pass")?;
        let pulled = oid(&peer, "HEAD")?;
        let pulled_for = pulled.clone();
        self.action(Action::Sync(sync::Action::Pull))?;
        self.until("the strip after the pull", move |tab| {
            tab.ui.head.as_deref() == Some(pulled_for.as_str()) && tab.ui.submodule.behind == [PathBuf::from("maps")]
        })
        .await?;
        if Self::recorded(root)? != tip {
            return Err("The pull did not move the recorded submodule commit".into());
        }
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{before}\n"))?;
        if submodule::strip_text(1) != "1 submodule is not at the commit this branch expects." {
            return Err(format!("The strip reads {:?}", submodule::strip_text(1)).into());
        }
        let section = Target::Section(Section::Submodules);
        let rows = self.menu_rows(&section)?;
        if !rows.contains(&("submodule.update-all", "Update submodules".to_owned(), None)) {
            return Err(format!("The Submodules section menu reads {rows:?}").into());
        }
        self.action(Action::Submodule(submodule::Action::UpdateAll))?;
        self.until_said("Updated submodule maps").await?;
        self.until("the strip gone", |tab| tab.ui.submodule.behind.is_empty() && tab.ui.files.is_empty()).await?;
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{tip}\n"))?;
        git_is(root, &["rev-parse", "HEAD"], &format!("{pulled}\n"))?;

        self.action(Action::Submodule(submodule::Action::ToggleAutomatic))?;
        let remembered = self.tab.read_with(self.cx, |tab, cx| {
            let common = tab.repo.as_ref().map(|repo| repo.common_dir().to_owned());
            let stored = common.and_then(|common| settings::memory(cx).and_then(|memory| memory.repo(&common)).and_then(|view| view.update_submodules));
            (tab.ui.submodule.automatic, stored)
        })?;
        if remembered != (true, Some(true)) {
            return Err(format!("Turning on automatic updates gives {remembered:?}").into());
        }
        let next = Self::move_pointer(dir, maps, &peer, "Mark the summit")?;
        let pulled = oid(&peer, "HEAD")?;
        let pulled_for = pulled.clone();
        self.action(Action::Sync(sync::Action::Pull))?;
        let wanted = next.clone();
        self.until("the submodule updated without asking", move |tab| {
            tab.ui.head.as_deref() == Some(pulled_for.as_str())
                && tab.ui.submodule.behind.is_empty()
                && tab.ui.submodule.list.first().is_some_and(|module| module.in_sync() && module.recorded.as_deref() == Some(wanted.as_str()))
                && tab.ui.notices.newest().is_some_and(|notice| notice.title == "Updated submodule maps")
        })
        .await?;
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{next}\n"))?;
        git_is(root, &["status", "--porcelain=v1"], "")?;
        self.action(Action::Submodule(submodule::Action::ToggleAutomatic))?;
        let off = self.tab.read_with(self.cx, |tab, _| tab.ui.submodule.automatic)?;
        if off {
            return Err("Automatic submodule updates stayed on".into());
        }

        self.action(Action::Refs(refs::Action::Switch("before-the-pass".into())))?;
        self.until("the strip after switching to an older pointer", |tab| {
            tab.ui.branch == "before-the-pass" && tab.ui.submodule.behind == [PathBuf::from("maps")]
        })
        .await?;
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{next}\n"))?;
        if Self::recorded(root)? == next {
            return Err("The older branch records the newest maps commit".into());
        }
        self.action(Action::Submodule(submodule::Action::Dismiss))?;
        self.until("the strip dismissed", |tab| tab.ui.submodule.behind.is_empty()).await?;
        git_is(&inside, &["rev-parse", "HEAD"], &format!("{next}\n"))?;
        self.pass();
        Ok(())
    }

    fn submodule_prompt(&mut self) -> Result<Option<(String, Vec<String>, bool, String)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            if !matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Submodule(_))) {
                return None;
            }
            let spec = prompt::shown(&tab.ui)?;
            let field = tab.ui.prompt.field().map(|field| field.read(cx).text().to_owned()).unwrap_or_default();
            Some((spec.title, spec.lines, spec.danger, field))
        })?)
    }

    async fn until_submodule_prompt(&mut self, title: &str) -> Result<(String, Vec<String>, bool, String)> {
        let wanted = title.to_owned();
        self.until(&format!("the prompt {title:?}"), move |tab| {
            matches!(tab.ui.prompt.asked(), Some(prompt::Kind::Submodule(_)))
                && prompt::shown(&tab.ui).is_some_and(|spec| spec.title == wanted)
        })
        .await?;
        self.submodule_prompt()?.ok_or_else(|| "The submodule prompt closed".into())
    }

    fn prompt_problems(&mut self, first: &str, second: &str) -> Result<(Option<String>, Option<String>)> {
        let (first, second) = (first.to_owned(), second.to_owned());
        Ok(self.tab.read_with(self.cx, move |tab, _| {
            let Some(spec) = prompt::shown(&tab.ui) else { return (None, None) };
            let one = spec.field.as_ref().and_then(|field| (field.validate)(&first, &tab.ui));
            let two = spec.second.as_ref().and_then(|field| (field.validate)(&second, &tab.ui));
            (one, two)
        })?)
    }

    fn undo_reads(&mut self) -> Result<Availability> {
        Ok(self.tab.read_with(self.cx, |tab, _| journal::availability(&tab.ui, journal::Way::Undo))?)
    }

    fn declared_in(root: &Path, key: &str) -> Result<String> {
        Ok(git(root, &["config", "--file", ".gitmodules", "--get", key])?.trim_end().to_owned())
    }

    async fn add_and_remove_a_submodule(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1108");
        let config = global_config(dir);
        let allowed = fs::read_to_string(&config).unwrap_or_default();
        if !allowed.contains("[protocol \"file\"]") {
            fs::write(&config, format!("{allowed}[protocol \"file\"]\n\tallow = always\n"))?;
        }
        let atlas = init(dir, BASE_ATLAS)?;
        record(&atlas, "atlas.txt", "Atlas of the valley\n", "Draw the atlas")?;
        let atlas_tip = oid(&atlas, "HEAD")?;
        let root = init(dir, BASE_CAMP)?;
        record(&root, "trail.txt", TRAIL, "Start the base camp")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Base-camp open without submodules", |tab| {
            tab.ui.repo == BASE_CAMP && tab.ui.history.len() == 1 && tab.ui.head.is_some() && tab.ui.submodule.list.is_empty() && !tab.ui.busy()
        })
        .await?;
        let section_shown = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Submodules, .. }))
        })?;
        if section_shown {
            return Err("The Submodules section shows before any submodule is added".into());
        }
        let rows = self.menu_rows(&Target::Repo)?;
        if !rows.contains(&("submodule.add", "Add submodule…".to_owned(), None)) {
            return Err(format!("The repository menu reads {rows:?}").into());
        }
        self.choose(&Target::Repo, "submodule.add")?;
        let asked = self.until_submodule_prompt("Add submodule").await?;
        if asked.2 || !asked.3.is_empty() {
            return Err(format!("The add prompt reads {asked:?}").into());
        }
        let problems = self.prompt_problems("https://cedar:secret@example.invalid/atlas.git", "trail.txt")?;
        if problems
            != (
                Some("Use a repository URL without embedded credentials, a query or a fragment.".to_owned()),
                Some("trail.txt already exists and is not a folder.".to_owned()),
            )
        {
            return Err(format!("The add prompt's checks read {problems:?}").into());
        }
        let address = path(&atlas)?.to_owned();
        self.type_first(&address)?;
        self.type_second(ATLAS)?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let offers = self.until_said("Added submodule vendor/atlas").await?;
        let said = self.said()?.ok_or("No notice after adding the submodule")?;
        let detail = format!(
            "It is at {}, cloned from {address}. It is staged with .gitmodules: commit them to record it.",
            notice::short(&atlas_tip)
        );
        if offers != ["Open submodule"] || said.2 != detail {
            return Err(format!("Adding the submodule said {said:?}").into());
        }
        git_is(&root, &["diff", "--cached", "--name-status"], "A\t.gitmodules\nA\tvendor/atlas\n")?;
        if Self::declared_in(&root, "submodule.vendor/atlas.url")? != address || Self::declared_in(&root, "submodule.vendor/atlas.path")? != ATLAS {
            return Err("Git's .gitmodules does not declare the new submodule".into());
        }
        git_is(&root, &["ls-files", "--stage", "--", ATLAS], &format!("160000 {atlas_tip} 0\t{ATLAS}\n"))?;
        let modules = root.join(".git/modules");
        if !modules.join(ATLAS).is_dir() || fs::read_to_string(root.join(ATLAS).join("atlas.txt"))? != "Atlas of the valley\n" {
            return Err("The new submodule's Git data or files are missing".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.until("the Submodules section with the new submodule", |tab| {
            tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Submodules, count: 1, .. }))
                && tab.ui.submodule.list.first().is_some_and(|module| module.path == Path::new(ATLAS) && module.in_sync())
                && tab.ui.files.iter().any(|row| row.staged && row.path == Path::new(ATLAS) && row.submodule.is_some())
                && tab.ui.files.iter().any(|row| row.staged && row.path == Path::new(".gitmodules"))
        })
        .await?;
        let undo = self.undo_reads()?;
        let refused = Availability::Disabled(
            "Adding submodule vendor/atlas cannot be undone. Remove it from the Submodules section instead.".to_owned(),
        );
        if undo != refused {
            return Err(format!("Undo after adding a submodule reads {undo:?}").into());
        }
        let section = Target::Section(Section::Submodules);
        let rows = self.menu_rows(&section)?;
        if !rows.contains(&("submodule.add", "Add submodule…".to_owned(), None)) {
            return Err(format!("The Submodules section menu reads {rows:?}").into());
        }
        git(&root, &["commit", "-qm", "Add the atlas"])?;
        self.action(Action::Refresh)?;
        self.until("the added submodule committed", |tab| tab.ui.files.is_empty() && tab.ui.history.len() == 2).await?;

        let mirror = dir.join(BASE_MIRROR);
        git(dir, &["clone", "-q", "--bare", &address, path(&mirror)?])?;
        let target = Target::Submodule(ATLAS.into());
        let rows = self.menu_rows(&target)?;
        let wanted = [
            ("submodule.set-url", "Change address…".to_owned(), None),
            ("submodule.remove", "Remove submodule…".to_owned(), None),
        ];
        if !wanted.iter().all(|row| rows.contains(row)) {
            return Err(format!("The submodule's menu reads {rows:?}").into());
        }
        self.choose(&target, "submodule.set-url")?;
        let asked = self.until_submodule_prompt("Change the address of submodule vendor/atlas").await?;
        if asked.3 != address || asked.2 {
            return Err(format!("The address prompt reads {asked:?}").into());
        }
        let moved_to = path(&mirror)?.to_owned();
        self.tab.update(self.cx, |tab, _, cx| {
            if let Some(field) = tab.ui.prompt.field().cloned() {
                field.update(cx, |field, cx| field.set_text("", cx));
            }
        })?;
        self.type_first(&moved_to)?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until_said("Changed the address of submodule vendor/atlas").await?;
        if Self::declared_in(&root, "submodule.vendor/atlas.url")? != moved_to {
            return Err("Changing the address did not change .gitmodules".into());
        }
        git_is(&root, &["config", "--get", "submodule.vendor/atlas.url"], &format!("{moved_to}\n"))?;
        git_is(&root.join(ATLAS), &["remote", "get-url", "origin"], &format!("{moved_to}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], " M .gitmodules\n")?;
        git_is(&root, &["ls-files", "--stage", "--", ATLAS], &format!("160000 {atlas_tip} 0\t{ATLAS}\n"))?;
        git(&root, &["commit", "-qam", "Fetch the atlas from its mirror"])?;

        scripted(&root, &["submodule", "deinit", "-q", "-f", "--", ATLAS])?;
        self.open(&root)?;
        self.until("the offer to initialise after opening", |tab| {
            tab.ui.repo == BASE_CAMP && tab.ui.submodule.uninitialised == [PathBuf::from(ATLAS)] && tab.ui.submodule.behind.is_empty()
        })
        .await?;
        if submodule::uninitialised_text(1) != "1 submodule is not initialised, so its folder is empty." {
            return Err(format!("The offer reads {:?}", submodule::uninitialised_text(1)).into());
        }
        self.action(Action::Submodule(submodule::Action::InitialiseAll))?;
        self.until_said("Initialised submodule vendor/atlas").await?;
        self.until("the offer gone and the submodule listed as initialised", |tab| {
            tab.ui.submodule.uninitialised.is_empty() && tab.ui.files.is_empty() && tab.ui.submodule.list.first().is_some_and(submodule::Module::in_sync)
        })
        .await?;
        git_is(&root.join(ATLAS), &["rev-parse", "HEAD"], &format!("{atlas_tip}\n"))?;

        let inside = root.join(ATLAS);
        Self::configure(&inside)?;
        record(&inside, "ford.txt", "Ford\n", "Mark the ford")?;
        let ford = oid(&inside, "HEAD")?;
        fs::write(inside.join("atlas.txt"), "Atlas, redrawn\n")?;
        self.action(Action::Refresh)?;
        self.until("the changed submodule seen", move |tab| {
            tab.ui.files.iter().any(|row| row.path == Path::new(ATLAS) && row.submodule.is_some_and(|flags| flags.moved && flags.modified))
                && tab.ui.submodule.list.first().is_some_and(|module| module.populated && module.checked_out.as_deref() == Some(ford.as_str()))
        })
        .await?;
        self.choose(&target, "submodule.remove")?;
        let asked = self.until_submodule_prompt("Remove submodule vendor/atlas?").await?;
        let lines = [
            "Its folder vendor/atlas and its Git data are deleted, with any commits and changes made in it that are not pushed. They cannot be brought back.",
            "It has 1 uncommitted change now, and 1 commit in it is on no remote.",
            "The removal and the change to .gitmodules are staged for you to commit.",
        ];
        if !asked.2 || asked.1 != lines {
            return Err(format!("The remove prompt reads {asked:?}").into());
        }
        self.action(Action::Prompt(prompt::Action::Cancel))?;
        self.until("the remove prompt closed", |tab| !tab.ui.prompt.open()).await?;
        if !modules.join(ATLAS).is_dir() || !inside.join("ford.txt").is_file() {
            return Err("Cancelling the removal touched the submodule".into());
        }
        self.choose(&target, "submodule.remove")?;
        self.until_submodule_prompt("Remove submodule vendor/atlas?").await?;
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        let offers = self.until_said("Removed submodule vendor/atlas").await?;
        let said = self.said()?.ok_or("No notice after removing the submodule")?;
        if !offers.is_empty()
            || said.2 != "Its folder and its Git data are deleted. The removal and the change to .gitmodules are staged: commit them to record it."
        {
            return Err(format!("Removing the submodule said {said:?}").into());
        }
        git_is(&root, &["diff", "--cached", "--name-status"], "M\t.gitmodules\nD\tvendor/atlas\n")?;
        git_is(&root, &["ls-files", "--", ATLAS], "")?;
        if fs::read_to_string(root.join(".gitmodules"))?.contains("atlas") {
            return Err("The removed submodule is still declared in .gitmodules".into());
        }
        if root.join(ATLAS).exists() {
            return Err("The removed submodule's folder is still there".into());
        }
        let left: Vec<_> = fs::read_dir(&modules).map(|entries| entries.flatten().map(|entry| entry.file_name()).collect()).unwrap_or_default();
        if !left.is_empty() {
            return Err(format!("Removing the submodule left {left:?} under modules/").into());
        }
        if fs::read_to_string(root.join(".git/config"))?.contains("[submodule") {
            return Err("The removed submodule is still registered in the repository's settings".into());
        }
        let head = oid(&root, "HEAD")?;
        self.until("the Submodules section gone", |tab| {
            tab.ui.submodule.list.is_empty()
                && !tab.ui.refs.rows.iter().any(|row| matches!(row, SideRow::Header { section: Section::Submodules, .. }))
                && tab.ui.files.iter().any(|row| row.staged && row.path == Path::new(ATLAS))
        })
        .await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        let undo = self.undo_reads()?;
        if undo != Availability::Disabled("Removing submodule vendor/atlas cannot be undone.".to_owned()) {
            return Err(format!("Undo after removing a submodule reads {undo:?}").into());
        }
        git_is(&atlas, &["rev-list", "--count", "HEAD"], "1\n")?;
        self.pass();
        Ok(())
    }
}
