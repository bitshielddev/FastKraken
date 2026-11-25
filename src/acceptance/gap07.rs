use super::*;
use crate::{
    commands::{self, Side, Target},
    diff_doc::{Blob, PatchState},
    diff_select::{self, Column, Spot},
    lines::{self, How},
    prompt,
    view::{DiffMode, diff_placeholder},
};
use gpui::{Bounds, ClipboardItem, Entity, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, point};
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

fn body_lines(patch: &str) -> Vec<String> {
    patch
        .lines()
        .skip_while(|line| !line.starts_with("@@ "))
        .filter(|line| !line.starts_with("@@ ") && !line.starts_with('\\'))
        .map(|line| line.get(1..).unwrap_or_default().to_owned())
        .collect()
}

fn command_action(tab: &Tab, id: &str, target: &Target, cx: &App) -> Option<Action> {
    commands::menu(&tab.ui, target, cx).into_iter().find(|entry| entry.id == id)?.action
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
        self.mode_only(dir).await?;
        let saved = self.cx.update(|cx| cx.read_from_clipboard())?;
        let copied = self.copy_from_a_diff(dir).await;
        let copied = match copied {
            Ok(()) => self.copy_part_of_a_message(dir).await,
            failed => failed,
        };
        self.cx.update(|cx| cx.write_to_clipboard(saved.unwrap_or_else(|| ClipboardItem::new_string(String::new()))))?;
        copied
    }

    fn clipboard(&mut self) -> Result<Option<String>> {
        Ok(self.cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))?)
    }

    async fn clipboard_is(&mut self, after: &str, expected: &str) -> Result<()> {
        self.until(&format!("the clipboard filled after {after}"), |_| true).await?;
        let copied = self.clipboard()?;
        if copied.as_deref() != Some(expected) {
            return Err(format!("After {after} the clipboard holds {copied:?}, not {expected:?}").into());
        }
        Ok(())
    }

    fn select_text(&mut self, column: Column, from: Spot, to: Spot) -> Result<()> {
        self.action(Action::Text(diff_select::Action::Press { column, spot: from, clicks: 1, extend: false }))?;
        self.action(Action::Text(diff_select::Action::DragTo { spot: to, edge: diff_select::Edge::Inside }))?;
        self.action(Action::Text(diff_select::Action::Release))
    }

    async fn copy_from_a_diff(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0707");
        let root = init(dir, "Copy-cove")?;
        record(&root, "cove.txt", "anchor\nrope\nsail\n", "Moor in the cove")?;
        let head = oid(&root, "HEAD")?;
        let edited = "anchor\ntide rope\nsail\nmast pole\n";
        fs::write(root.join("cove.txt"), edited)?;
        self.open(&root)?;
        self.until("Copy-cove with the cove edited", |tab| tab.ui.repo == "Copy-cove" && files(tab, &[], &["cove.txt"])).await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;
        self.open_patch("cove.txt", false, "+mast pole").await?;
        let rows = self.tab.read_with(self.cx, |tab, _| {
            [" anchor", "-rope", "+tide rope", " sail", "+mast pole"].map(|line| row(tab, line))
        })?;
        let [Some(anchor), Some(rope), Some(tide), Some(sail), Some(mast)] = rows else {
            return Err(format!("The cove's diff rows are {rows:?}").into());
        };
        let reach = self.tab.update(self.cx, |tab, window, _| {
            let left = diff_select::origin(&tab.ui, Column::Unified);
            let hit = |x: Pixels, unit| diff_select::hit(&tab.ui, window, tide, Column::Unified, x, unit);
            (hit(left - px(30.), diff_select::Unit::Character), hit(left + px(4000.), diff_select::Unit::Character))
        })?;
        if reach != (0, "tide rope".len()) {
            return Err(format!("Pointing before and after the text of +tide rope reaches offsets {reach:?}").into());
        }
        self.focus_desk()?;
        self.select_text(Column::Unified, Spot { row: anchor, at: 2 }, Spot { row: tide, at: 4 })?;
        self.key("cmd-c")?;
        self.until("the copy confirmed", |tab| tab.ui.notices.newest().is_some_and(|n| n.text() == "Copied text")).await?;
        let head_lines: Vec<String> = git(&root, &["show", "HEAD:cove.txt"])?.lines().map(str::to_owned).collect();
        let disk_lines: Vec<String> = fs::read_to_string(root.join("cove.txt"))?.lines().map(str::to_owned).collect();
        let expected = format!("{}\n{}\n{}", &head_lines[0][2..], head_lines[1], &disk_lines[1][..4]);
        self.clipboard_is("dragging from \"anchor\" into \"+tide rope\" and pressing Cmd-C", &expected).await?;
        if expected != "chor\nrope\ntide" {
            return Err(format!("The expected copy was {expected:?}").into());
        }

        self.action(Action::Text(diff_select::Action::Press { column: Column::Unified, spot: Spot { row: tide, at: 6 }, clicks: 2, extend: false }))?;
        self.action(Action::Text(diff_select::Action::Release))?;
        self.key("cmd-c")?;
        self.clipboard_is("double-clicking \"rope\"", "rope").await?;
        self.action(Action::Text(diff_select::Action::Press { column: Column::Unified, spot: Spot { row: mast, at: 1 }, clicks: 3, extend: false }))?;
        self.action(Action::Text(diff_select::Action::Release))?;
        self.key("cmd-c")?;
        self.clipboard_is("triple-clicking \"+mast pole\"", "mast pole").await?;
        self.action(Action::Text(diff_select::Action::Press { column: Column::Unified, spot: Spot { row: anchor, at: 3 }, clicks: 1, extend: false }))?;
        self.action(Action::Text(diff_select::Action::Press { column: Column::Unified, spot: Spot { row: rope, at: 2 }, clicks: 1, extend: true }))?;
        self.action(Action::Text(diff_select::Action::Release))?;
        self.key("cmd-c")?;
        self.clipboard_is("Shift-pressing in \"-rope\"", "hor\nro").await?;

        self.key("cmd-a")?;
        self.key("cmd-c")?;
        let whole = body_lines(&git(&root, &["diff", "--", "cove.txt"])?).join("\n");
        self.clipboard_is("Cmd-A then Cmd-C", &whole).await?;
        if whole != "anchor\nrope\ntide rope\nsail\nmast pole" {
            return Err(format!("Git's diff body reads {whole:?}").into());
        }
        let picked = self.tab.read_with(self.cx, |tab, _| tab.ui.picked.is_some() || lines::current(&tab.ui).is_some())?;
        if picked {
            return Err("Cmd-A in the diff picked files or lines instead of selecting text".into());
        }

        self.action(Action::DiffMode(DiffMode::Split))?;
        self.until("the cove's diff side by side", |tab| tab.ui.diff_mode == DiffMode::Split && diff_select::current(&tab.ui).is_none()).await?;
        self.focus_desk()?;
        self.select_text(Column::Old, Spot { row: anchor, at: 0 }, Spot { row: mast, at: 4 })?;
        self.key("cmd-c")?;
        let before = git(&root, &["show", "HEAD:cove.txt"])?;
        self.clipboard_is("dragging down the Before side", before.trim_end_matches('\n')).await?;
        self.select_text(Column::New, Spot { row: tide, at: 0 }, Spot { row: mast, at: 99 })?;
        self.key("cmd-c")?;
        self.clipboard_is("dragging down the After side", "tide rope\nsail\nmast pole").await?;
        if !edited.ends_with("tide rope\nsail\nmast pole\n") {
            return Err("The After side does not match the file".into());
        }
        self.action(Action::DiffMode(DiffMode::Unified))?;

        let target = self.tab.read_with(self.cx, |tab, _| diff_select::code_target(&tab.ui, tide, 2))?;
        let target = target.ok_or("The +tide rope row has no menu")?;
        let titles = self.titles(target.clone())?;
        for title in ["Copy", "Copy line", "Copy hunk as patch", "Copy path", "Select all"] {
            if !titles.iter().any(|shown| shown == title) {
                return Err(format!("The menu of a code row reads {titles:?}, without {title:?}").into());
            }
        }
        let edit = self.tab.read_with(self.cx, |tab, cx| command_action(tab, "file.edit", &target, cx))?;
        if !matches!(&edit, Some(Action::Handoff(crate::handoff::Action::Edit { path, line: Some(2) })) if path == Path::new("cove.txt")) {
            return Err(format!("The menu of a code row opens the editor with {edit:?}, not at line 2 of cove.txt").into());
        }
        for (id, notice, expected) in [("text.copy-line", "Copied line", "tide rope"), ("file.copy-path", "Copied path", "cove.txt")] {
            let run = self.tab.read_with(self.cx, |tab, cx| command_action(tab, id, &target, cx))?;
            let copied = self.copied(run.ok_or(format!("{id} is not offered"))?, notice).await?;
            if copied.as_deref() != Some(expected) {
                return Err(format!("{id} copied {copied:?}, not {expected:?}").into());
            }
        }
        let run = self.tab.read_with(self.cx, |tab, cx| command_action(tab, "text.copy-hunk", &target, cx))?;
        let patch = self.copied(run.ok_or("Copy hunk as patch is not offered")?, "Copied hunk").await?.unwrap_or_default();
        if !patch.starts_with("diff --git a/cove.txt b/cove.txt\n") || !patch.contains("\n+tide rope\n") || !patch.ends_with("+mast pole\n") {
            return Err(format!("The hunk was copied as {patch:?}").into());
        }
        let file = dir.join("cove-hunk.patch");
        fs::write(&file, &patch)?;
        git(&root, &["apply", "--check", "--cached", &file.to_string_lossy()])?;
        fs::remove_file(&file)?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        if fs::read_to_string(root.join("cove.txt"))? != edited {
            return Err("Copying changed cove.txt on disk".into());
        }

        self.select_text(Column::Unified, Spot { row: sail, at: 0 }, Spot { row: sail, at: 2 })?;
        self.key("escape")?;
        self.until("Esc clearing the text selection and keeping the diff open", |tab| {
            diff_select::current(&tab.ui).is_none() && tab.ui.diff_open
        })
        .await?;

        let tides: String = (1..=300).map(|n| format!("tide {n}\n")).collect();
        fs::write(root.join("tides.txt"), &tides)?;
        self.until("tides.txt listed", |tab| files(tab, &[], &["cove.txt", "tides.txt"])).await?;
        self.open_patch("tides.txt", false, "+tide 300").await?;
        self.focus_desk()?;
        let first = self.tab.read_with(self.cx, |tab, _| row(tab, "+tide 1"))?.ok_or("No first tide row")?;
        self.action(Action::Text(diff_select::Action::Press { column: Column::Unified, spot: Spot { row: first, at: 0 }, clicks: 1, extend: false }))?;
        self.action(Action::Text(diff_select::Action::DragTo { spot: Spot { row: first + 3, at: 2 }, edge: diff_select::Edge::Below }))?;
        let scrolled = |tab: &Tab| f32::from(tab.ui.diff_scroll.0.borrow().base_handle.offset().y);
        self.until("dragging below the diff scrolling it down", |tab| {
            scrolled(tab) <= -200. && diff_select::current(&tab.ui).is_some_and(|chosen| chosen.head.row > first + 20)
        })
        .await?;
        self.action(Action::Text(diff_select::Action::Release))?;
        let stopped = self.tab.read_with(self.cx, |tab, _| (scrolled(tab), diff_select::current(&tab.ui).map(|chosen| chosen.head)))?;
        self.cx.background_executor().timer(Duration::from_millis(200)).await;
        let later = self.tab.read_with(self.cx, |tab, _| (scrolled(tab), diff_select::current(&tab.ui).map(|chosen| chosen.head)))?;
        if stopped != later {
            return Err(format!("The diff kept scrolling after the release: {stopped:?} then {later:?}").into());
        }
        let reached = stopped.1.ok_or("The scrolled selection was lost")?.row - first + 1;
        self.key("cmd-c")?;
        let expected: Vec<&str> = tides.lines().take(reached).collect();
        self.clipboard_is("dragging past the bottom of the diff", &expected.join("\n")).await?;
        self.pass();
        Ok(())
    }

    fn range_bounds(&mut self, field: &Entity<crate::view::CommitInput>, range: std::ops::Range<usize>) -> Result<Bounds<Pixels>> {
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        let bounds = self.tab.update(self.cx, |_, window, cx| {
            field.update(cx, |field, cx| field.bounds_for_range(range, Bounds::default(), window, cx))
        })?;
        Ok(bounds.ok_or("The message was not laid out")?)
    }

    fn click(&mut self, field: &Entity<crate::view::CommitInput>, at: Point<Pixels>, clicks: usize) -> Result<()> {
        let press = MouseDownEvent { button: MouseButton::Left, position: at, modifiers: gpui::Modifiers::default(), click_count: clicks, first_mouse: false };
        let release = MouseUpEvent { button: MouseButton::Left, position: at, modifiers: gpui::Modifiers::default(), click_count: clicks };
        self.tab.update(self.cx, |_, window, cx| {
            field.update(cx, |field, cx| {
                field.press(&press, window, cx);
                field.release(&release, window, cx);
            })
        })?;
        Ok(())
    }

    fn reads(&mut self, field: &Entity<crate::view::CommitInput>) -> Result<(String, std::ops::Range<usize>)> {
        Ok(self.tab.read_with(self.cx, |_, cx| {
            let field = field.read(cx);
            (field.text().to_owned(), field.selected_range())
        })?)
    }

    async fn copy_part_of_a_message(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0708");
        let root = init(dir, "Ford-notes")?;
        let body = "The river rose at dawn.\n\nSee https://example.invalid/ford for the map.";
        fs::write(root.join("ford.txt"), "stones\n")?;
        git(&root, &["add", "--", "ford.txt"])?;
        git(&root, &["commit", "-qm", "Mark the ford", "-m", body])?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Ford-notes open", |tab| tab.ui.repo == "Ford-notes" && tab.ui.history.iter().any(|row| row.oid == head)).await?;
        self.action(Action::Select(Selection::Commit(head.clone())))?;
        self.until("the commit's message in the details panel", |tab| {
            tab.ui.inspection.body == body && !tab.ui.panel_hidden
        })
        .await?;
        let (subject, message) = self.tab.read_with(self.cx, |tab, _| (tab.ui.message.subject.clone(), tab.ui.message.body.clone()))?;
        self.until("the message readers filled", |_| true).await?;
        let shown = (self.reads(&subject)?.0, self.reads(&message)?.0);
        if shown != ("Mark the ford".to_owned(), body.to_owned()) {
            return Err(format!("The details panel shows {shown:?}").into());
        }
        let found = self.tab.read_with(self.cx, |_, cx| message.read(cx).links().into_iter().map(str::to_owned).collect::<Vec<_>>())?;
        if found != ["https://example.invalid/ford"] {
            return Err(format!("The message's links are {found:?}").into());
        }
        let river = body.find("river").ok_or("no river")?;
        let letter = self.range_bounds(&message, river + 1..river + 2)?;
        self.click(&message, letter.center(), 2)?;
        if self.reads(&message)?.1 != (river..river + "river".len()) {
            return Err(format!("Double-clicking \"river\" selected {:?}", self.reads(&message)?.1).into());
        }
        self.key("alt-shift-right")?;
        self.key("cmd-c")?;
        self.clipboard_is("selecting \"river rose\" in the message", "river rose").await?;
        let offered: Vec<(&str, bool)> = self.tab.read_with(self.cx, |_, cx| {
            message.read(cx).menu_items(true).into_iter().map(|item| (item.label, item.enabled)).collect()
        })?;
        if offered != [("Copy", true), ("Select All", true)] {
            return Err(format!("The message's menu offers {offered:?}").into());
        }
        self.type_keys("x")?;
        self.key("backspace")?;
        self.key("enter")?;
        self.key("cmd-v")?;
        self.key("cmd-x")?;
        if self.reads(&message)?.0 != body {
            return Err(format!("Typing changed the message to {:?}", self.reads(&message)?.0).into());
        }
        self.key("cmd-a")?;
        self.key("cmd-c")?;
        self.clipboard_is("Cmd-A and Cmd-C in the message", body).await?;

        let title = self.range_bounds(&subject, 0..1)?;
        self.click(&subject, title.center(), 1)?;
        self.key("cmd-a")?;
        self.key("cmd-c")?;
        self.clipboard_is("Cmd-A and Cmd-C in the subject", "Mark the ford").await?;

        let link = body.find("https://").ok_or("no link")?;
        let opened = crate::handoff::opened().len();
        let letter = self.range_bounds(&message, link + 10..link + 11)?;
        self.click(&message, letter.center(), 1)?;
        self.until("the link opened through the handoff", |_| {
            crate::handoff::opened().get(opened..) == Some(&["https://example.invalid/ford".to_owned()][..])
        })
        .await?;
        let plain = self.range_bounds(&message, river..river + 1)?;
        self.click(&message, point(plain.left() + px(1.), plain.center().y), 1)?;
        if crate::handoff::opened().len() != opened + 1 {
            return Err("Clicking plain words opened a link".into());
        }

        let message_text = git(&root, &["log", "-1", "--format=%B", &head])?;
        if !message_text.contains("river rose") || !message_text.starts_with("Mark the ford\n") {
            return Err(format!("Git reads the message as {message_text:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.key("escape")?;
        self.pass();
        Ok(())
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
