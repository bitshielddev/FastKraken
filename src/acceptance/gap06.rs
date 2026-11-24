use super::*;
use crate::{
    commands::{self, Side, Target},
    picked::{self, How},
};

const NAMES: [&str; 5] = ["alder.txt", "birch bark.txt", "cedar é.txt", "dogwood.txt", "elm.txt"];

fn picked_paths(tab: &Tab) -> Option<Vec<String>> {
    picked::files(&tab.ui).map(|(paths, _)| paths.iter().map(|path| path.to_string_lossy().into_owned()).collect())
}

impl Replay<'_> {
    pub(super) async fn gap06(&mut self, dir: &Path) -> Result<()> {
        self.stage_a_range(dir).await
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
}
