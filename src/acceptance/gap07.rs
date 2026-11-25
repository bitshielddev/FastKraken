use super::*;
use crate::{
    diff_doc::{Blob, PatchState},
    view::diff_placeholder,
};
use std::os::unix::fs::PermissionsExt;

fn shown(tab: &Tab, path: &str, message: &str) -> bool {
    let open = match &tab.ui.selection {
        Some(Selection::File { path: selected, .. }) => selected == Path::new(path),
        Some(Selection::Commit(_)) => tab.ui.inspection.path.as_deref() == Some(Path::new(path)),
        _ => false,
    };
    open && tab.diff.is_some()
        && tab.ui.diff_open
        && tab.ui.diff.lines.is_empty()
        && diff_placeholder(&tab.ui, false).as_deref() == Some(message)
}

fn sized(root: &Path, object: &str, expected: u64) -> Result<()> {
    let size: u64 = git(root, &["cat-file", "-s", object])?.trim().parse()?;
    if size != expected {
        return Err(format!("git cat-file -s {object} reads {size}, not {expected}").into());
    }
    Ok(())
}

fn bytes(seed: u8, length: usize) -> Vec<u8> {
    (0..length).map(|i| if i % 64 == 0 { 0 } else { seed.wrapping_add(i as u8) }).collect()
}

impl Replay<'_> {
    pub(super) async fn gap07(&mut self, dir: &Path) -> Result<()> {
        self.binary_sizes(dir).await?;
        self.mode_only(dir).await
    }

    async fn binary_sizes(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0705");
        let root = init(dir, "Byte-harbour")?;
        fs::write(root.join("chart.bin"), bytes(7, 3000))?;
        fs::write(root.join("log.txt"), "Harbour log\n")?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Chart the harbour"])?;
        fs::write(root.join("chart.bin"), bytes(9, 4500))?;
        fs::write(root.join("buoy.bin"), bytes(3, 1200))?;
        self.open(&root)?;
        self.until("Byte-harbour with the chart edited and a new buoy", |tab| {
            tab.ui.repo == "Byte-harbour" && files(tab, &[], &["chart.bin", "buoy.bin"])
        })
        .await?;
        sized(&root, "HEAD:chart.bin", 3000)?;
        if fs::metadata(root.join("chart.bin"))?.len() != 4500 {
            return Err("chart.bin is not 4,500 bytes on disk".into());
        }
        let both = "Binary file · 3.0 KB → 4.5 KB";
        self.action(Action::Select(file("chart.bin", false)))?;
        self.until("the unstaged chart naming both sizes", |tab| shown(tab, "chart.bin", both)).await?;
        let state = self.tab.read_with(self.cx, |tab, _| tab.ui.diff.state.clone())?;
        if state != (PatchState::Binary { old: Blob::Bytes(3000), new: Blob::Bytes(4500) }) {
            return Err(format!("The unstaged chart's diff is {state:?}").into());
        }

        self.action(Action::Select(file("buoy.bin", false)))?;
        self.until("the untracked buoy naming its size", |tab| {
            shown(tab, "buoy.bin", "Binary file added · 1.2 KB")
        })
        .await?;

        self.action(Action::Select(file("chart.bin", false)))?;
        self.until("the chart selected again", |tab| shown(tab, "chart.bin", both)).await?;
        self.action(Action::StageFile)?;
        self.until("the chart staged", |tab| files(tab, &["chart.bin"], &["buoy.bin"])).await?;
        sized(&root, ":chart.bin", 4500)?;
        self.action(Action::Select(file("chart.bin", true)))?;
        self.until("the staged chart naming both sizes", |tab| {
            tab.ui.selection == Some(file("chart.bin", true)) && shown(tab, "chart.bin", both)
        })
        .await?;

        self.action(Action::Select(file("buoy.bin", false)))?;
        self.until("the buoy selected", |tab| tab.ui.selection == Some(file("buoy.bin", false))).await?;
        self.action(Action::StageFile)?;
        self.until("both binaries staged", |tab| files(tab, &["chart.bin", "buoy.bin"], &[])).await?;
        self.type_commit("Redraw the chart and add a buoy")?;
        self.until("the binary commit made", |tab| {
            tab.ui.files.is_empty() && tab.ui.history.first().is_some_and(|row| row.subject == "Redraw the chart and add a buoy")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        sized(&root, "HEAD:chart.bin", 4500)?;
        sized(&root, "HEAD:buoy.bin", 1200)?;
        self.select_commit(&head).await?;
        self.action(Action::OpenDiff)?;
        self.until("the whole commit noting each binary file", |tab| {
            let doc = &tab.ui.diff;
            let notes: Vec<&str> = doc
                .lines
                .iter()
                .filter(|line| line.kind == crate::diff_doc::Kind::Note)
                .map(|line| doc.text(line))
                .collect();
            doc.state == PatchState::Text && notes == ["Binary file added · 1.2 KB", both]
        })
        .await?;
        self.open_file("chart.bin").await?;
        self.until("the committed chart naming both sizes", |tab| shown(tab, "chart.bin", both)).await?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn mode_only(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0706");
        let root = init(dir, "Mode-pier")?;
        let script = root.join("launch.sh");
        fs::write(&script, ": launch the tender\n")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644))?;
        git(&root, &["add", "--", "launch.sh"])?;
        git(&root, &["commit", "-qm", "Write the launch script"])?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        self.open(&root)?;
        self.until("Mode-pier with the script changed", |tab| {
            tab.ui.repo == "Mode-pier" && files(tab, &[], &["launch.sh"])
        })
        .await?;
        git_is(&root, &["diff", "--summary"], " mode change 100644 => 100755 launch.sh\n")?;
        let message = "Only the file mode changed: 100644 → 100755";
        self.action(Action::Select(file("launch.sh", false)))?;
        self.until("the unstaged script naming both modes", |tab| shown(tab, "launch.sh", message)).await?;

        self.action(Action::StageFile)?;
        self.until("the mode change staged", |tab| files(tab, &["launch.sh"], &[])).await?;
        git_is(&root, &["diff", "--cached", "--summary"], " mode change 100644 => 100755 launch.sh\n")?;
        self.action(Action::Select(file("launch.sh", true)))?;
        self.until("the staged script naming both modes", |tab| {
            tab.ui.selection == Some(file("launch.sh", true)) && shown(tab, "launch.sh", message)
        })
        .await?;

        self.type_commit("Make the launch script runnable")?;
        self.until("the mode commit made", |tab| {
            tab.ui.files.is_empty() && tab.ui.history.first().is_some_and(|row| row.subject == "Make the launch script runnable")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        git_is(&root, &["show", "--format=", "--summary", "HEAD"], " mode change 100644 => 100755 launch.sh\n")?;
        self.select_commit(&head).await?;
        self.open_file("launch.sh").await?;
        self.until("the committed script naming both modes", |tab| shown(tab, "launch.sh", message)).await?;
        let state = self.tab.read_with(self.cx, |tab, _| tab.ui.diff.state.clone())?;
        if state != (PatchState::ModeOnly { old: 0o100644, new: 0o100755 }) {
            return Err(format!("The committed script's diff is {state:?}").into());
        }
        if fs::read_to_string(&script)? != ": launch the tender\n" {
            return Err("launch.sh changed on disk".into());
        }
        self.pass();
        Ok(())
    }
}
