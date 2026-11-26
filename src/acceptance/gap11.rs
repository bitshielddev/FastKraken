use super::*;
use crate::{
    commands::{self, Availability, Target},
    journal,
    prompt::{self, Stop},
    refs::{Section, SideRow},
    worktree,
};

const MAIN_TREE: &str = "Worktree-trail";
const RIDGE_TREE: &str = "Worktree-trail-ridge";
const IN_USE: &str = "Checked out in worktree Worktree-trail-ridge";
const CAMP_TREE: &str = "Camp-trail";
const LAKE_TREE: &str = "Camp-trail-lake";
const MAIN_TREE_MEADOW: &str = "Camp-trail-meadow";
const FORD_TREE: &str = "Camp-trail-ford";

fn path(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()).into())
}

fn private_refs(root: &Path, space: &str) -> Result<String> {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)", &format!("refs/fastkraken/undo/{space}/")])
}

impl Replay<'_> {
    pub(super) async fn gap11(&mut self, dir: &Path) -> Result<()> {
        let (root, ridge) = self.list_a_linked_worktree(dir).await?;
        self.refuse_a_branch_in_use(dir, &root, &ridge).await?;
        let camp = self.create_and_remove_a_worktree(dir).await?;
        self.remove_a_dirty_worktree(dir, &camp).await
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
}
