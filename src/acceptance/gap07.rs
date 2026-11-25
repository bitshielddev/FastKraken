use super::*;
use crate::{
    commands::{Side, Target},
    diff_doc::{Blob, PatchState},
    lines::{self, How},
    prompt,
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

fn row(tab: &Tab, patch_line: &str) -> Option<usize> {
    let doc = &tab.ui.diff;
    doc.lines.iter().position(|line| doc.text(line) == patch_line)
}

fn picked(tab: &Tab) -> Vec<String> {
    let doc = &tab.ui.diff;
    lines::current(&tab.ui)
        .map(|pick| pick.rows.iter().map(|&row| doc.text(&doc.lines[row]).to_owned()).collect())
        .unwrap_or_default()
}

fn index_of(root: &Path, path: &str) -> Result<Vec<u8>> {
    let output = repository::bundled_command(root)?.args(["show", &format!(":{path}")]).output()?;
    if !output.status.success() {
        return Err(format!("git show :{path}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(output.stdout)
}

fn index_is(root: &Path, path: &str, expected: &[u8]) -> Result<()> {
    let actual = index_of(root, path)?;
    if actual != expected {
        return Err(format!(
            "The index holds {:?} for {path}, not {:?}",
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(expected)
        )
        .into());
    }
    Ok(())
}

fn added(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git(root, args)?
        .lines()
        .filter(|line| (line.starts_with('+') && !line.starts_with("+++")) || (line.starts_with('-') && !line.starts_with("---")))
        .map(str::to_owned)
        .collect())
}

fn bytes(seed: u8, length: usize) -> Vec<u8> {
    (0..length).map(|i| if i % 64 == 0 { 0 } else { seed.wrapping_add(i as u8) }).collect()
}

impl Replay<'_> {
    pub(super) async fn gap07(&mut self, dir: &Path) -> Result<()> {
        self.stage_one_line(dir).await?;
        self.unstage_one_line(dir).await?;
        self.discard_one_line(dir).await?;
        self.line_at_the_end(dir).await?;
        self.binary_sizes(dir).await?;
        self.mode_only(dir).await
    }

    async fn pick(&mut self, patch_line: &str, how: How, expected: &[&str]) -> Result<()> {
        let at = self.tab.read_with(self.cx, |tab, _| row(tab, patch_line))?;
        let at = at.ok_or_else(|| format!("The diff has no row {patch_line:?}"))?;
        self.action(Action::Lines(lines::Action::Press { row: at, how }))?;
        self.until(&format!("{expected:?} picked"), |tab| picked(tab) == expected).await
    }

    async fn open_patch(&mut self, path: &str, staged: bool, line: &str) -> Result<()> {
        self.action(Action::Select(file(path, staged)))?;
        self.action(Action::OpenDiff)?;
        self.until(&format!("the {} diff of {path} showing {line:?}", if staged { "staged" } else { "unstaged" }), |tab| {
            tab.ui.selection == Some(file(path, staged)) && tab.ui.diff_open && patch(tab, line) && tab.ui.diff.stageable
        })
        .await
    }

    async fn stage_one_line(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0701");
        let root = init(dir, "Line-ford")?;
        record(&root, "trail.txt", "north\nsouth\n", "Mark the trail")?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("trail.txt"), "north\neast\nwest\nsouth\n")?;
        self.open(&root)?;
        self.until("Line-ford with the trail edited", |tab| tab.ui.repo == "Line-ford" && files(tab, &[], &["trail.txt"]))
            .await?;
        self.open_patch("trail.txt", false, "+west").await?;
        let hunks = self.tab.read_with(self.cx, |tab, _| tab.ui.diff.stageable_hunks())?;
        if hunks != 1 {
            return Err(format!("The trail's diff has {hunks} stageable hunks, not one").into());
        }
        let context = self.tab.read_with(self.cx, |tab, _| row(tab, " north"))?.ok_or("The diff has no context row")?;
        self.action(Action::Lines(lines::Action::Press { row: context, how: How::One }))?;
        self.until("a context row left unpicked", |tab| picked(tab).is_empty()).await?;
        self.pick("+east", How::One, &["+east"]).await?;
        self.pick("+west", How::Range, &["+east", "+west"]).await?;
        self.pick("+east", How::Toggle, &["+west"]).await?;
        self.action(Action::Activity(crate::activity::Action::Toggle))?;
        self.until("the activity log hiding the diff and its pick", |tab| {
            !tab.ui.diff_open && tab.ui.activity.open() && lines::target(&tab.ui).is_none()
        })
        .await?;
        self.action(Action::Activity(crate::activity::Action::Close))?;
        self.until("the pick still hidden with the diff closed", |tab| {
            !tab.ui.activity.open() && !tab.ui.diff_open && lines::target(&tab.ui).is_none()
        })
        .await?;
        self.action(Action::OpenDiff)?;
        self.until("the pick back with its diff", |tab| tab.ui.diff_open && picked(tab) == ["+west"]).await?;
        let target = Target::Lines { path: "trail.txt".into(), side: Side::Unstaged, count: 1 };
        let shown = self.tab.read_with(self.cx, |tab, _| lines::target(&tab.ui))?;
        if shown.as_ref() != Some(&target) {
            return Err(format!("The pick's target is {shown:?}").into());
        }
        let titles = self.titles(target)?;
        if !titles.iter().any(|title| title == "Stage line") || !titles.iter().any(|title| title == "Discard line…") {
            return Err(format!("The menu of the picked line reads {titles:?}").into());
        }
        self.key("s")?;
        self.until("trail.txt on both sides", |tab| files(tab, &["trail.txt"], &["trail.txt"])).await?;
        index_is(&root, "trail.txt", b"north\nwest\nsouth\n")?;
        git_is(&root, &["diff", "--cached", "--numstat"], "1\t0\ttrail.txt\n")?;
        if added(&root, &["diff", "--", "trail.txt"])? != ["+east"] {
            return Err("The unstaged edit is not just +east".into());
        }
        if fs::read_to_string(root.join("trail.txt"))? != "north\neast\nwest\nsouth\n" {
            return Err("Staging a line changed trail.txt on disk".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.until("the pick cleared by the new diff", |tab| picked(tab).is_empty() && !tab.ui.diff.has("+west")).await?;
        self.pass();
        Ok(())
    }

    async fn unstage_one_line(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0702");
        let root = init(dir, "Ember-camp")?;
        record(&root, "camp.txt", "fire\nwater\n", "Pitch the camp")?;
        let edited = "fire\nwood\nsmoke\nwater\n";
        fs::write(root.join("camp.txt"), edited)?;
        git(&root, &["add", "--", "camp.txt"])?;
        self.open(&root)?;
        self.until("Ember-camp with the camp staged", |tab| tab.ui.repo == "Ember-camp" && files(tab, &["camp.txt"], &[]))
            .await?;
        self.open_patch("camp.txt", true, "+smoke").await?;
        self.pick("+wood", How::One, &["+wood"]).await?;
        self.key("escape")?;
        self.until("Esc clearing the pick and keeping the diff open", |tab| picked(tab).is_empty() && tab.ui.diff_open).await?;
        self.pick("+smoke", How::One, &["+smoke"]).await?;
        let titles = self.titles(Target::Lines { path: "camp.txt".into(), side: Side::Staged, count: 1 })?;
        if !titles.iter().any(|title| title == "Unstage line") || titles.iter().any(|title| title.starts_with("Stage") || title.starts_with("Discard")) {
            return Err(format!("The menu of the picked staged line reads {titles:?}").into());
        }
        self.key("u")?;
        self.until("camp.txt on both sides", |tab| files(tab, &["camp.txt"], &["camp.txt"])).await?;
        index_is(&root, "camp.txt", b"fire\nwood\nwater\n")?;
        if added(&root, &["diff", "--cached", "--", "camp.txt"])? != ["+wood"] {
            return Err("The staged change is not just +wood".into());
        }
        if added(&root, &["diff", "--", "camp.txt"])? != ["+smoke"] {
            return Err("The unstaged change is not just +smoke".into());
        }
        if fs::read_to_string(root.join("camp.txt"))? != edited {
            return Err("Unstaging a line changed camp.txt on disk".into());
        }
        self.pass();
        Ok(())
    }

    async fn discard_one_line(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0703");
        let root = init(dir, "Vale-map")?;
        record(&root, "map.txt", "hill\nriver\nvalley\n", "Draw the map")?;
        fs::write(root.join("map.txt"), "hill\nridge\nriver\nvale\n")?;
        git(&root, &["add", "--", "map.txt"])?;
        fs::write(root.join("map.txt"), "hill\nridge\nriver\nvale\nlake\nshore\n")?;
        let index = git(&root, &["ls-files", "--stage", "--", "map.txt"])?;
        self.open(&root)?;
        self.until("Vale-map with staged and unstaged edits", |tab| {
            tab.ui.repo == "Vale-map" && files(tab, &["map.txt"], &["map.txt"])
        })
        .await?;
        self.open_patch("map.txt", false, "+shore").await?;
        let (lake, shore) = self.tab.read_with(self.cx, |tab, _| (row(tab, "+lake"), row(tab, "+shore")))?;
        let (lake, shore) = lake.zip(shore).ok_or("The diff has no +lake and +shore rows")?;
        self.action(Action::Lines(lines::Action::Press { row: lake, how: How::One }))?;
        self.action(Action::Lines(lines::Action::DragTo(shore)))?;
        self.until("a drag picking both lines", |tab| picked(tab) == ["+lake", "+shore"]).await?;
        self.action(Action::Lines(lines::Action::DragTo(lake)))?;
        self.action(Action::Lines(lines::Action::Release))?;
        self.until("the drag back to +lake", |tab| picked(tab) == ["+lake"] && !tab.ui.lines.dragging()).await?;
        self.action(Action::Lines(lines::Action::AskDiscard))?;
        self.until("the discard prompt", |tab| tab.ui.prompt.open()).await?;
        let spec = self.tab.read_with(self.cx, |tab, _| prompt::shown(&tab.ui).map(|spec| (spec.title.clone(), spec.confirm.to_string())))?;
        let wanted = ("Discard this line of map.txt?".to_owned(), "Discard line".to_owned());
        if spec.as_ref() != Some(&wanted) {
            return Err(format!("The discard prompt reads {spec:?}").into());
        }
        if fs::read_to_string(root.join("map.txt"))? != "hill\nridge\nriver\nvale\nlake\nshore\n" {
            return Err("map.txt changed before the discard was confirmed".into());
        }
        self.action(Action::Prompt(prompt::Action::Confirm))?;
        self.until("the line discarded", |tab| !tab.ui.prompt.open() && patch(tab, "+shore") && !tab.ui.diff.has("+lake"))
            .await?;
        if fs::read_to_string(root.join("map.txt"))? != "hill\nridge\nriver\nvale\nshore\n" {
            return Err("Discarding +lake did not leave the other edits in map.txt".into());
        }
        git_is(&root, &["ls-files", "--stage", "--", "map.txt"], &index)?;
        if added(&root, &["diff", "--cached", "--", "map.txt"])? != ["+ridge", "-valley", "+vale"] {
            return Err("The staged edits changed".into());
        }
        self.pass();
        Ok(())
    }

    async fn line_at_the_end(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0704");
        let root = init(dir, "Last-cairn")?;
        record(&root, "end.txt", "one\ntwo", "Stack the cairn")?;
        let edited: &[u8] = b"one\ntwo\nthree\nfour";
        fs::write(root.join("end.txt"), edited)?;
        self.open(&root)?;
        self.until("Last-cairn with the end edited", |tab| tab.ui.repo == "Last-cairn" && files(tab, &[], &["end.txt"]))
            .await?;
        self.open_patch("end.txt", false, "+four").await?;
        let marks = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.diff.lines.iter().filter(|line| line.kind == crate::diff_doc::Kind::NoNewline).count()
        })?;
        if marks != 2 {
            return Err(format!("The diff notes {marks} missing final newlines, not two").into());
        }
        self.pick("+four", How::One, &["+four"]).await?;
        self.key("s")?;
        self.until("end.txt on both sides", |tab| files(tab, &["end.txt"], &["end.txt"])).await?;
        index_is(&root, "end.txt", b"one\ntwo\nfour")?;
        self.open_patch("end.txt", false, "+three").await?;
        self.pick("+three", How::One, &["+three"]).await?;
        self.key("s")?;
        self.until("end.txt fully staged", |tab| files(tab, &["end.txt"], &[])).await?;
        index_is(&root, "end.txt", edited)?;
        if fs::read(root.join("end.txt"))? != edited {
            return Err("Staging lines changed end.txt on disk".into());
        }
        git_is(&root, &["diff", "--", "end.txt"], "")?;
        self.pass();
        Ok(())
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
