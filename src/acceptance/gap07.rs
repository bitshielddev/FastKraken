use super::*;
use crate::{
    commands::{self, Side, Target},
    diff_doc::{Blob, PatchState},
    diff_select::{self, Column, Spot},
    lines::{self, How},
    prompt,
    diff_options::{Action as Show, Choice},
    image_diff,
    theme::DIFF_LINE,
    view::{DiffMode, diff_placeholder},
};
use gpui::{Bounds, ClipboardItem, Entity, Focusable, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, point};
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

fn ink(color: u32) -> Option<gpui::Hsla> {
    Some(gpui::rgb(color).into())
}

fn coloured(tab: &Tab, patch_line: &str) -> Option<Vec<(String, Option<gpui::Hsla>)>> {
    let index = row(tab, patch_line)?;
    let line = &tab.ui.diff.lines[index];
    let code = tab.ui.diff.code(line);
    let column = if tab.ui.diff_mode == DiffMode::Split {
        if line.kind == crate::diff_doc::Kind::Deleted { Column::Old } else { Column::New }
    } else {
        Column::Unified
    };
    let mut runs: Vec<(std::ops::Range<usize>, Option<gpui::Hsla>)> = Vec::new();
    for (range, style) in crate::view::diff_highlights(&tab.ui, index, line, column) {
        if style.color.is_none() {
            continue;
        }
        match runs.last_mut() {
            Some((last, color)) if last.end == range.start && *color == style.color => last.end = range.end,
            _ => runs.push((range, style.color)),
        }
    }
    Some(runs.into_iter().map(|(range, color)| (code[range].to_owned(), color)).collect())
}

fn settled(tab: &Tab, patch_line: &str) -> bool {
    tab.ui.diff_open && patch(tab, patch_line) && !tab.ui.syntax.pending
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
        copied?;
        self.comment_coloured_from_the_file(dir).await?;
        self.large_file_plain(dir).await?;
        let shown = self.whole_file(dir).await;
        let shown = match shown {
            Ok(()) => self.ignore_whitespace(dir).await,
            failed => failed,
        };
        let shown = match shown {
            Ok(()) => self.find_in_a_diff(dir).await,
            failed => failed,
        };
        self.reset_diff_options()?;
        shown?;
        self.find_commits_without_a_diff(dir).await?;
        let pictured = self.image_diff(dir).await;
        self.action(Action::Images(image_diff::Action::Show(image_diff::Mode::Text)))?;
        pictured
    }

    fn reset_diff_options(&mut self) -> Result<()> {
        for choice in [Choice::WholeFile, Choice::IgnoreWhitespace, Choice::Wrap] {
            self.action(Action::DiffOptions(Show::Set(choice, false)))?;
        }
        Ok(())
    }

    fn scroll_top(&mut self) -> Result<f32> {
        Ok(self.tab.read_with(self.cx, |tab, _| -f32::from(tab.ui.diff_scroll.0.borrow().base_handle.offset().y) / DIFF_LINE)?)
    }

    async fn jump_to(&mut self, key: &str, patch_line: &str) -> Result<()> {
        self.key(key)?;
        let target = self.tab.read_with(self.cx, |tab, _| row(tab, patch_line).and_then(|row| crate::view::diff_visual_of(&tab.ui, row)))?;
        let target = target.ok_or_else(|| format!("The diff has no row {patch_line:?}"))?;
        self.until(&format!("{key} landing on {patch_line:?}"), |tab| {
            tab.ui.diff_options.jumped == Some(target) && tab.ui.diff_scroll.0.borrow().deferred_scroll_to_item.is_none()
        })
        .await?;
        let top = self.scroll_top()?;
        let expected = target.saturating_sub(3) as f32;
        if (top - expected).abs() > 0.5 {
            return Err(format!("{key} scrolled the top of the diff to row {top}, not {expected}, for {patch_line:?} at row {target}").into());
        }
        Ok(())
    }

    async fn whole_file(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0711");
        let root = init(dir, "Whole-harbour")?;
        let long = "anchor chain ".repeat(40).trim_end().to_owned();
        let base: String = (1..=400)
            .map(|n| if n == 250 { format!("{long}\n") } else { format!("line {n}\n") })
            .collect();
        record(&root, "harbour.txt", &base, "Chart the harbour")?;
        let head = oid(&root, "HEAD")?;
        let edited = base
            .replace("line 5\n", "line five\n")
            .replace("line 100\n", "line 100\nline 100b\n")
            .replace("line 180\n", "")
            .replace("line 280\n", "line two-eighty\n");
        fs::write(root.join("harbour.txt"), &edited)?;
        self.open(&root)?;
        self.until("Whole-harbour with the harbour edited", |tab| tab.ui.repo == "Whole-harbour" && files(tab, &[], &["harbour.txt"]))
            .await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;
        self.open_patch("harbour.txt", false, "+line 100b").await?;
        let hunks = self.tab.read_with(self.cx, |tab, _| (tab.ui.diff.hunks.len(), tab.ui.diff.has(" line 50")))?;
        if hunks != (4, false) {
            return Err(format!("With three lines of context the diff has (hunks, line 50 shown) {hunks:?}").into());
        }
        let title = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.whole-file").map(|entry| entry.title)
        })?;
        if title.as_deref() != Some("Show the whole file in diffs") {
            return Err(format!("The whole-file command reads {title:?}").into());
        }

        self.action(Action::DiffOptions(Show::Set(Choice::WholeFile, true)))?;
        self.until("the whole harbour in one hunk", |tab| {
            tab.ui.diff.options.whole_file && patch(tab, " line 50") && tab.ui.diff.hunks.len() == 1
        })
        .await?;
        let (old, new, stageable, saved) = self.tab.read_with(self.cx, |tab, cx| {
            let doc = &tab.ui.diff;
            let side = |kind: crate::diff_doc::Kind| -> Vec<String> {
                doc.lines
                    .iter()
                    .filter(|line| line.kind == crate::diff_doc::Kind::Context || line.kind == kind)
                    .map(|line| doc.code(line).to_owned())
                    .collect()
            };
            (side(crate::diff_doc::Kind::Deleted), side(crate::diff_doc::Kind::Added), doc.stageable, settings::settings(cx).whole_file)
        })?;
        let committed: Vec<String> = git(&root, &["show", "HEAD:harbour.txt"])?.lines().map(str::to_owned).collect();
        let disk: Vec<String> = fs::read_to_string(root.join("harbour.txt"))?.lines().map(str::to_owned).collect();
        if old != committed || new != disk {
            return Err(format!("The whole-file diff holds {} old and {} new lines, not the {} committed and {} on disk", old.len(), new.len(), committed.len(), disk.len()).into());
        }
        if !stageable || !saved {
            return Err(format!("The whole-file diff is stageable {stageable}, remembered {saved}").into());
        }

        let marks = self.tab.read_with(self.cx, |tab, _| crate::diff_options::marks(&tab.ui))?;
        let colours: Vec<u32> = marks.iter().map(|mark| mark.color).collect();
        use crate::theme::{ADDED, DELETED};
        if colours != [DELETED, ADDED, ADDED, DELETED, DELETED, ADDED] {
            return Err(format!("The scrollbar marks the changes with {marks:?}").into());
        }
        let expected = self.tab.read_with(self.cx, |tab, _| {
            ["-line 5", "+line five", "+line 100b", "-line 180", "-line 280", "+line two-eighty"]
                .map(|line| row(tab, line).map(|row| row as f32 / tab.ui.diff.lines.len() as f32))
        })?;
        if marks.iter().zip(expected).any(|(mark, at)| at.is_none_or(|at| (mark.at - at).abs() > 0.0001)) {
            return Err(format!("The scrollbar marks sit at {marks:?}, not {expected:?}").into());
        }

        self.focus_desk()?;
        self.jump_to("alt-down", "-line 5").await?;
        self.jump_to("alt-down", "+line 100b").await?;
        self.jump_to("alt-down", "-line 180").await?;
        self.jump_to("alt-up", "+line 100b").await?;
        let title = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.next-change").map(|entry| (entry.title, entry.disabled))
        })?;
        if title != Some(("Next change".to_owned(), None)) {
            return Err(format!("The next-change command reads {title:?}").into());
        }

        self.pick("+line 100b", How::One, &["+line 100b"]).await?;
        self.key("s")?;
        self.until("harbour.txt on both sides", |tab| files(tab, &["harbour.txt"], &["harbour.txt"])).await?;
        index_is(&root, "harbour.txt", base.replace("line 100\n", "line 100\nline 100b\n").as_bytes())?;
        if added(&root, &["diff", "--", "harbour.txt"])? != ["-line 5", "+line five", "-line 180", "-line 280", "+line two-eighty"] {
            return Err(format!("The unstaged edits after staging one line are {:?}", added(&root, &["diff", "--", "harbour.txt"])?).into());
        }
        if fs::read_to_string(root.join("harbour.txt"))? != edited {
            return Err("Staging a line changed harbour.txt on disk".into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        self.until("the whole unstaged harbour without the staged line", |tab| {
            tab.ui.diff.options.whole_file && patch(tab, "+line five") && !tab.ui.diff.has("+line 100b") && tab.ui.diff.has(" line 100b")
        })
        .await?;

        self.action(Action::DiffOptions(Show::Set(Choice::Wrap, true)))?;
        let long_line = format!(" {long}");
        self.until("the long line wrapped", |tab| {
            tab.ui.diff_options.wrapped.is_some() && crate::view::diff_rows(&tab.ui) > tab.ui.diff.lines.len()
        })
        .await?;
        let wrapped = self.tab.update(self.cx, |tab, window, cx| {
            let ui = &tab.ui;
            let at = row(tab, &long_line)?;
            let visual = crate::view::diff_visual_of(ui, at)?;
            let width = ui.diff_options.columns.unified;
            let mut pieces = Vec::new();
            let mut part = 0;
            while crate::view::diff_doc_row(ui, visual + part, Column::Unified) == Some(at) {
                if crate::view::diff_part(ui, visual + part) != part {
                    return None;
                }
                pieces.push(crate::view::diff_piece(ui, at, part));
                part += 1;
            }
            let code = ui.diff.code(&ui.diff.lines[at]);
            let left = diff_select::origin(ui, Column::Unified);
            let second = pieces.get(1)?.clone();
            let hits = (
                diff_select::hit(ui, window, at, 1, Column::Unified, left - px(20.), diff_select::Unit::Character),
                diff_select::hit(ui, window, at, 1, Column::Unified, left + px(4000.), diff_select::Unit::Character),
            );
            let joined: String = pieces.iter().map(|piece| &code[piece.clone()]).collect();
            let fits = pieces.iter().all(|piece| code[piece.clone()].chars().count() <= width);
            Some((pieces.len(), joined == code, fits, hits == (second.start, second.end), settings::settings(cx).wrap_lines, width))
        })?;
        match wrapped {
            Some((parts, true, true, true, true, width)) if parts >= 2 && width > 20 => {}
            other => return Err(format!("The wrapped long line reads (parts, whole, fits, hits, remembered, width) {other:?}").into()),
        }
        self.jump_to("alt-down", "-line 5").await?;
        self.jump_to("alt-down", "-line 180").await?;
        self.jump_to("alt-down", "-line 280").await?;
        let shifted = self.tab.read_with(self.cx, |tab, _| {
            row(tab, "-line 280").and_then(|at| Some((at, crate::view::diff_visual_of(&tab.ui, at)?)))
        })?;
        if !shifted.is_some_and(|(at, visual)| visual > at) {
            return Err(format!("Below the wrapped line, -line 280 sits at (row, visual row) {shifted:?}").into());
        }
        self.action(Action::DiffOptions(Show::Set(Choice::Wrap, false)))?;
        self.until("one row per line again", |tab| {
            tab.ui.diff_options.wrapped.is_none() && crate::view::diff_rows(&tab.ui) == tab.ui.diff.lines.len()
        })
        .await?;

        self.action(Action::DiffOptions(Show::Set(Choice::WholeFile, false)))?;
        self.until("the harbour back in hunks", |tab| {
            !tab.ui.diff.options.whole_file && patch(tab, "+line five") && !tab.ui.diff.has(" line 50") && tab.ui.diff.hunks.len() == 3
        })
        .await?;
        let saved = self.tab.read_with(self.cx, |_, cx| (settings::settings(cx).whole_file, settings::settings(cx).wrap_lines))?;
        if saved != (false, false) {
            return Err(format!("After switching back the settings read (whole file, wrap) {saved:?}").into());
        }
        self.key("escape")?;
        self.pass();
        Ok(())
    }

    async fn ignore_whitespace(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0712");
        let root = init(dir, "Quiet-tide")?;
        let before = "fn tide() {\n  let high = 1;\n  let low = 2;\n  let slack = 3;\n}\n";
        let after = "fn tide() {\n    let high = 1;\n    let low = 5;\n    let slack = 3;\n}\n";
        record(&root, "tide.rs", before, "Measure the tide")?;
        git(&root, &["add", "--", "tide.rs"])?;
        fs::write(root.join("swell.rs"), before)?;
        git(&root, &["add", "--", "swell.rs"])?;
        git(&root, &["commit", "-qm", "Measure the swell"])?;
        fs::write(root.join("tide.rs"), after)?;
        fs::write(root.join("swell.rs"), before.replace("  let", "\tlet"))?;
        let index = git(&root, &["ls-files", "--stage"])?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Quiet-tide with both files edited", |tab| tab.ui.repo == "Quiet-tide" && files(tab, &[], &["swell.rs", "tide.rs"]))
            .await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;
        self.open_patch("tide.rs", false, "+    let low = 5;").await?;
        let marked = self.tab.read_with(self.cx, |tab, _| ["-  let high = 1;", "+    let high = 1;", "+    let slack = 3;"].map(|line| tab.ui.diff.has(line)))?;
        if marked != [true, true, true] {
            return Err(format!("Before ignoring whitespace the re-indented rows are marked {marked:?}").into());
        }
        let quiet = added(&root, &["diff", "--ignore-all-space", "--", "tide.rs"])?;
        if quiet != ["-  let low = 2;", "+    let low = 5;"] {
            return Err(format!("Git's own diff ignoring whitespace reads {quiet:?}").into());
        }

        self.focus_desk()?;
        self.run_command("view.ignore-whitespace")?;
        self.until("the tide's diff with whitespace ignored", |tab| {
            tab.ui.diff.options.ignore_whitespace && patch(tab, "+    let low = 5;") && !tab.ui.diff.has("+    let high = 1;")
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, cx| {
            let doc = &tab.ui.diff;
            let changed: Vec<String> = doc
                .lines
                .iter()
                .filter(|line| matches!(line.kind, crate::diff_doc::Kind::Added | crate::diff_doc::Kind::Deleted))
                .map(|line| doc.text(line).to_owned())
                .collect();
            let context: Vec<String> = doc
                .lines
                .iter()
                .filter(|line| line.kind == crate::diff_doc::Kind::Context)
                .map(|line| doc.code(line).trim().to_owned())
                .collect();
            (
                changed,
                context,
                doc.stageable,
                lines::side(&tab.ui),
                crate::view::diff_bar_texts(&tab.ui),
                settings::settings(cx).ignore_whitespace,
            )
        })?;
        let (changed, context, stageable, side, bars, saved) = shown;
        if changed != quiet || !context.iter().any(|line| line == "let high = 1;") || !context.iter().any(|line| line == "let slack = 3;") {
            return Err(format!("With whitespace ignored the diff marks {changed:?} and keeps {context:?} as context").into());
        }
        if stageable || side.is_some() || bars != [crate::view::WHITESPACE_HIDDEN] || !saved {
            return Err(format!("With whitespace ignored: stageable {stageable}, line side {side:?}, bars {bars:?}, remembered {saved}").into());
        }
        let low = self.tab.read_with(self.cx, |tab, _| row(tab, "+    let low = 5;"))?.ok_or("The diff has no +    let low = 5; row")?;
        self.action(Action::Lines(lines::Action::Press { row: low, how: How::One }))?;
        self.until("no line picked while whitespace is hidden", |tab| picked(tab).is_empty() && lines::target(&tab.ui).is_none()).await?;
        self.action(Action::StageHunk(0))?;
        self.until("the hunk refused", |tab| tab.ui.notice_text().contains("Whitespace changes are hidden")).await?;
        git_is(&root, &["ls-files", "--stage"], &index)?;
        git_is(&root, &["diff", "--cached", "--name-only"], "")?;

        self.open_patch_unstageable("swell.rs").await?;
        let message = self.tab.read_with(self.cx, |tab, _| (tab.ui.diff.lines.is_empty(), diff_placeholder(&tab.ui, false)))?;
        if message != (true, Some(crate::diff_doc::WHITESPACE_ONLY.to_owned())) {
            return Err(format!("The re-indented swell.rs shows {message:?}").into());
        }
        if git(&root, &["diff", "--ignore-all-space", "--", "swell.rs"])? != "" || git(&root, &["diff", "--", "swell.rs"])?.is_empty() {
            return Err("Git does not see swell.rs as changed only in whitespace".into());
        }

        let title = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.ignore-whitespace").map(|entry| entry.title)
        })?;
        if title.as_deref() != Some("Show whitespace changes") {
            return Err(format!("The whitespace command reads {title:?}").into());
        }
        self.run_command("view.ignore-whitespace")?;
        self.until("swell.rs re-indented again", |tab| !tab.ui.diff.options.ignore_whitespace && patch(tab, "+\tlet high = 1;")).await?;
        self.open_patch("tide.rs", false, "+    let high = 1;").await?;
        self.action(Action::StageHunk(0))?;
        self.until("tide.rs staged whole", |tab| files(tab, &["tide.rs"], &["swell.rs"])).await?;
        index_is(&root, "tide.rs", after.as_bytes())?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        if fs::read_to_string(root.join("tide.rs"))? != after {
            return Err("Staging changed tide.rs on disk".into());
        }
        self.key("escape")?;
        self.pass();
        Ok(())
    }

    async fn open_patch_unstageable(&mut self, path: &str) -> Result<()> {
        self.action(Action::Select(file(path, false)))?;
        self.action(Action::OpenDiff)?;
        self.until(&format!("the unstaged diff of {path}"), |tab| {
            tab.ui.selection == Some(file(path, false)) && tab.ui.diff_open && !tab.ui.loading_diff() && tab.diff.is_some()
        })
        .await
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
            let hit = |x: Pixels, unit| diff_select::hit(&tab.ui, window, tide, 0, Column::Unified, x, unit);
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

const REEF: &str = "/*\n Reef survey notes.\n The north wall is steep.\n Coral grows on the ledge.\n Fish gather at dawn.\n Tide pools fill at noon.\n Divers keep to the buoys.\n Currents turn at slack water.\n Visibility is best in spring.\n Mark every anchor point.\n*/\nfn survey() -> u32 {\n    42\n}\n";

impl Replay<'_> {
    async fn comment_coloured_from_the_file(&mut self, dir: &Path) -> Result<()> {
        use crate::theme::{SYNTAX_COMMENT, SYNTAX_KEYWORD};
        if SYNTAX_KEYWORD == SYNTAX_COMMENT {
            return Err("Keywords and comments share a colour".into());
        }
        self.start("GAP0709");
        let root = init(dir, "Colour-reef")?;
        record(&root, "reef.rs", REEF, "Survey the reef")?;
        record(&root, "reef.txt", REEF, "Keep a plain copy")?;
        let edited = REEF.replace(" Divers keep to the buoys.", " Divers stay by the buoys.");
        fs::write(root.join("reef.rs"), &edited)?;
        fs::write(root.join("reef.txt"), &edited)?;
        self.open(&root)?;
        self.until("Colour-reef with the survey edited", |tab| {
            tab.ui.repo == "Colour-reef" && files(tab, &[], &["reef.rs", "reef.txt"])
        })
        .await?;
        if git(&root, &["show", "HEAD:reef.rs"])?.lines().next() != Some("/*") {
            return Err("reef.rs does not open its comment on the first line".into());
        }
        let shown = git(&root, &["diff", "--", "reef.rs"])?;
        let header = shown.lines().find(|line| line.starts_with("@@ ")).unwrap_or_default().to_owned();
        if !header.starts_with("@@ -4,7 +4,7 @@") || shown.lines().any(|line| line.ends_with("/*")) {
            return Err(format!("The hunk of reef.rs is {header:?}, or it shows the comment's opening line").into());
        }
        let on = self.cx.update(|cx| settings::settings(cx).colour_code)?;
        if !on {
            return Err("Colouring code is not on by default".into());
        }
        self.action(Action::DiffMode(DiffMode::Unified))?;
        let before = self.tab.read_with(self.cx, |tab, _| tab.ui.syntax.highlighted)?;
        self.open_patch("reef.rs", false, "+ Divers stay by the buoys.").await?;
        self.until("the unstaged survey coloured", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.").is_some_and(|runs| !runs.is_empty())
        })
        .await?;
        let comment = |text: &str| vec![(text.to_owned(), ink(SYNTAX_COMMENT))];
        let (added, deleted, inside, after) = self.tab.read_with(self.cx, |tab, _| {
            (
                coloured(tab, "+ Divers stay by the buoys."),
                coloured(tab, "- Divers keep to the buoys."),
                coloured(tab, "  Fish gather at dawn."),
                coloured(tab, " fn survey() -> u32 {"),
            )
        })?;
        if added != Some(comment(" Divers stay by the buoys."))
            || deleted != Some(comment(" Divers keep to the buoys."))
            || inside != Some(comment(" Fish gather at dawn."))
        {
            return Err(format!("Rows inside the comment are coloured {added:?}, {deleted:?} and {inside:?}").into());
        }
        if after.is_some() {
            return Err("The hunk reaches past the comment".into());
        }
        let closing = self.tab.read_with(self.cx, |tab, _| coloured(tab, "  Mark every anchor point."))?;
        if closing != Some(comment(" Mark every anchor point.")) {
            return Err(format!("The last row inside the comment is coloured {closing:?}").into());
        }
        let first = self.tab.read_with(self.cx, |tab, _| tab.ui.syntax.highlighted)?;
        if first < before + 2 {
            return Err(format!("Colouring the survey read {} files, not both sides", first - before).into());
        }

        self.open_patch("reef.txt", false, "+ Divers stay by the buoys.").await?;
        self.until("the plain copy settled", |tab| settled(tab, "+ Divers stay by the buoys.")).await?;
        let plain = self.tab.read_with(self.cx, |tab, _| coloured(tab, "+ Divers stay by the buoys."))?;
        if plain != Some(Vec::new()) {
            return Err(format!("A .txt file was coloured {plain:?}").into());
        }

        self.action(Action::Select(file("reef.rs", false)))?;
        self.until("the survey coloured again from the cache", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
        })
        .await?;
        let again = self.tab.read_with(self.cx, |tab, _| tab.ui.syntax.highlighted)?;
        if again != first {
            return Err(format!("Showing the survey again coloured {} more files instead of using the cache", again - first).into());
        }

        let toggle = self.tab.read_with(self.cx, |tab, cx| command_action(tab, "view.colour-code", &Target::Repo, cx))?;
        let toggle = toggle.ok_or("The palette has no command to stop colouring code")?;
        let title = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.colour-code").map(|entry| entry.title)
        })?;
        if title.as_deref() != Some("Show code without colours") {
            return Err(format!("The colour command reads {title:?}").into());
        }
        self.action(toggle.clone())?;
        let off = self.tab.read_with(self.cx, |tab, cx| {
            (settings::settings(cx).colour_code, tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()), coloured(tab, "+ Divers stay by the buoys."))
        })?;
        if off != (false, true, Some(Vec::new())) {
            return Err(format!("After turning colours off: setting, every row plain, the edited row: {off:?}").into());
        }
        self.action(Action::Select(file("reef.txt", false)))?;
        self.until("the plain copy with colours off", |tab| settled(tab, "+ Divers stay by the buoys.")).await?;
        self.action(Action::Select(file("reef.rs", false)))?;
        self.until("the survey shown with colours off", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && tab.ui.selection == Some(file("reef.rs", false))
        })
        .await?;
        let still = self.tab.read_with(self.cx, |tab, _| tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()))?;
        if !still {
            return Err("A diff opened with colours off was coloured".into());
        }
        self.action(toggle.clone())?;
        self.until("the survey coloured after turning colours on", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
        })
        .await?;
        let on = self.cx.update(|cx| settings::settings(cx).colour_code)?;
        if !on {
            return Err("Turning colours back on did not reach the settings".into());
        }

        self.action(Action::Select(file("reef.txt", false)))?;
        self.until("the plain copy before a toggle during a load", |tab| settled(tab, "+ Divers stay by the buoys.")).await?;
        self.action(Action::Select(file("reef.rs", false)))?;
        self.action(toggle.clone())?;
        self.until("the survey loaded after colours went off during its load", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && tab.ui.selection == Some(file("reef.rs", false))
        })
        .await?;
        let off_during_load = self.tab.read_with(self.cx, |tab, cx| {
            (settings::settings(cx).colour_code, tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()))
        })?;
        if off_during_load != (false, true) {
            return Err(format!("Turning colours off while the survey loaded: setting, every row plain: {off_during_load:?}").into());
        }
        self.action(Action::Select(file("reef.txt", false)))?;
        self.until("the plain copy before turning colours on during a load", |tab| settled(tab, "+ Divers stay by the buoys.")).await?;
        self.action(Action::Select(file("reef.rs", false)))?;
        self.action(toggle)?;
        self.until("the survey coloured after colours came on during its load", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
        })
        .await?;

        let mirrored = self.tab.update(self.cx, |tab, _, cx| {
            settings::Settings::update(cx, |s| s.colour_code = false);
            crate::tabs::mirror(tab, cx);
            let title = commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.colour-code").map(|entry| entry.title);
            (title, tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()))
        })?;
        if mirrored != (Some("Colour code by language".to_owned()), true) {
            return Err(format!("After colours went off in another tab, the command and every row plain: {mirrored:?}").into());
        }
        self.tab.update(self.cx, |tab, _, cx| {
            settings::Settings::update(cx, |s| s.colour_code = true);
            crate::tabs::mirror(tab, cx);
        })?;
        self.until("the survey coloured after colours came on in another tab", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
        })
        .await?;
        let title = self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == "view.colour-code").map(|entry| entry.title)
        })?;
        if title.as_deref() != Some("Show code without colours") {
            return Err(format!("After colours came on in another tab, the command reads {title:?}").into());
        }

        self.action(Action::StageFile)?;
        self.until("the survey staged", |tab| files(tab, &["reef.rs"], &["reef.txt"])).await?;
        self.open_patch("reef.rs", true, "+ Divers stay by the buoys.").await?;
        self.until("the staged survey coloured", |tab| {
            settled(tab, "+ Divers stay by the buoys.") && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
        })
        .await?;
        index_is(&root, "reef.rs", edited.as_bytes())?;

        self.type_commit("Note the divers")?;
        self.until("the survey committed", |tab| {
            files(tab, &[], &["reef.txt"]) && tab.ui.history.first().is_some_and(|row| row.subject == "Note the divers")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        self.select_commit(&head).await?;
        self.open_file("reef.rs").await?;
        self.action(Action::DiffMode(DiffMode::Split))?;
        self.until("the committed survey coloured side by side", |tab| {
            settled(tab, "+ Divers stay by the buoys.")
                && coloured(tab, "+ Divers stay by the buoys.") == Some(comment(" Divers stay by the buoys."))
                && coloured(tab, "- Divers keep to the buoys.") == Some(comment(" Divers keep to the buoys."))
        })
        .await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;
        git_is(&root, &["show", "--format=%s", "-s", "HEAD"], "Note the divers\n")?;
        if fs::read_to_string(root.join("reef.rs"))? != edited {
            return Err("reef.rs changed on disk".into());
        }
        self.pass();
        Ok(())
    }

    async fn large_file_plain(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0710");
        let root = init(dir, "Plain-shoal")?;
        let large: String = (0..24_000).map(|n| format!("let depth_{n:05} = {n};\n")).collect();
        let small = "fn sound() -> u32 {\n    7\n}\n";
        let wide = format!("fn chart() {{}}\nlet line = \"{}\";\n", "x".repeat(2_500));
        fs::write(root.join("shoal.rs"), &large)?;
        fs::write(root.join("sound.rs"), small)?;
        fs::write(root.join("wide.rs"), &wide)?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Sound the shoal"])?;
        let large_edited = large.replace("let depth_12000 = 12000;", "let depth_12000 = 12001;");
        fs::write(root.join("shoal.rs"), &large_edited)?;
        fs::write(root.join("sound.rs"), small.replace("    7", "    8"))?;
        let wide_edited = wide.replace("fn chart() {}", "fn charted() {}").replace("xxx\"", "xxy\"");
        fs::write(root.join("wide.rs"), &wide_edited)?;
        let size: u64 = git(&root, &["cat-file", "-s", "HEAD:shoal.rs"])?.trim().parse()?;
        if size <= crate::syntax::FILE_LIMIT || fs::metadata(root.join("shoal.rs"))?.len() <= crate::syntax::FILE_LIMIT {
            return Err(format!("shoal.rs is {size} bytes, not over the 512 KiB cap").into());
        }
        self.open(&root)?;
        self.until("Plain-shoal with three files edited", |tab| {
            tab.ui.repo == "Plain-shoal" && files(tab, &[], &["shoal.rs", "sound.rs", "wide.rs"])
        })
        .await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;

        let counts = |tab: &Tab| (tab.ui.syntax.highlighted, tab.ui.syntax.too_large);
        let both_sides_skipped = |from: (usize, usize), to: (usize, usize)| {
            to.0 == from.0 && to.1 > from.1 && (to.1 - from.1) % 2 == 0
        };
        let before = self.tab.read_with(self.cx, |tab, _| counts(tab))?;
        self.open_patch("shoal.rs", false, "+let depth_12000 = 12001;").await?;
        self.until("the large unstaged file settled", |tab| settled(tab, "+let depth_12000 = 12001;")).await?;
        let (after, plain) = self.tab.read_with(self.cx, |tab, _| {
            (counts(tab), tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()))
        })?;
        if !both_sides_skipped(before, after) || !plain {
            return Err(format!("Over the cap: coloured and skipped went from {before:?} to {after:?}, every row plain {plain}").into());
        }
        let row_plain = self.tab.read_with(self.cx, |tab, _| coloured(tab, "+let depth_12000 = 12001;"))?;
        if row_plain != Some(Vec::new()) {
            return Err(format!("The large file's edited row is coloured {row_plain:?}").into());
        }

        self.open_patch("sound.rs", false, "+    8").await?;
        self.until("the small file coloured", |tab| {
            settled(tab, "+    8") && coloured(tab, " fn sound() -> u32 {").is_some_and(|runs| !runs.is_empty())
        })
        .await?;
        let control = self.tab.read_with(self.cx, |tab, _| counts(tab))?;
        if control.0 < after.0 + 1 || control.1 != after.1 {
            return Err(format!("The small file moved the counts from {after:?} to {control:?}").into());
        }

        self.open_patch("wide.rs", false, "+fn charted() {}").await?;
        self.until("the file with a long line coloured", |tab| {
            settled(tab, "+fn charted() {}") && coloured(tab, "+fn charted() {}").is_some_and(|runs| !runs.is_empty())
        })
        .await?;
        let long = self.tab.read_with(self.cx, |tab, _| {
            let doc = &tab.ui.diff;
            doc.lines
                .iter()
                .filter(|line| line.kind == crate::diff_doc::Kind::Added && doc.code(line).len() > crate::syntax::LINE_LIMIT)
                .map(|line| line.syntax.is_empty())
                .collect::<Vec<_>>()
        })?;
        if long != [true] {
            return Err(format!("The long added line is plain: {long:?}").into());
        }

        self.action(Action::Select(file("shoal.rs", false)))?;
        self.until("the large file selected", |tab| tab.ui.selection == Some(file("shoal.rs", false))).await?;
        self.action(Action::StageFile)?;
        self.until("the large file staged", |tab| files(tab, &["shoal.rs"], &["sound.rs", "wide.rs"])).await?;
        let staged_before = self.tab.read_with(self.cx, |tab, _| counts(tab))?;
        self.open_patch("shoal.rs", true, "+let depth_12000 = 12001;").await?;
        self.until("the large staged file settled", |tab| settled(tab, "+let depth_12000 = 12001;")).await?;
        let (staged_after, staged_plain) = self.tab.read_with(self.cx, |tab, _| {
            (counts(tab), tab.ui.diff.lines.iter().all(|line| line.syntax.is_empty()))
        })?;
        if !both_sides_skipped(staged_before, staged_after) || !staged_plain {
            return Err(format!("The staged large file moved the counts from {staged_before:?} to {staged_after:?}, plain {staged_plain}").into());
        }
        let staged_size: u64 = git(&root, &["cat-file", "-s", ":shoal.rs"])?.trim().parse()?;
        if staged_size <= crate::syntax::FILE_LIMIT {
            return Err("The staged shoal.rs is under the cap".into());
        }
        index_is(&root, "shoal.rs", large_edited.as_bytes())?;
        git_is(&root, &["diff", "--name-only"], "sound.rs\nwide.rs\n")?;
        self.pass();
        Ok(())
    }
}

fn reef() -> String {
    (1..=300)
        .map(|n| match n {
            20 => "kelp forest at the north wall\n".to_owned(),
            150 => "the KELP thins here\n".to_owned(),
            151 => "Kelp and kelp again\n".to_owned(),
            200 => format!("{} kelp\n", "x".repeat(400)),
            280 => "last kelp bed\n".to_owned(),
            n => format!("reef line {n}\n"),
        })
        .collect()
}

fn grep_count(root: &Path, args: &[&str]) -> Result<usize> {
    let output = repository::bundled_command(root)?.args(["grep"]).args(args).output()?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).lines().count()),
        Some(1) => Ok(0),
        _ => Err(format!("git grep {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into()),
    }
}

fn found(tab: &Tab) -> Option<(String, String)> {
    let hit = tab.ui.find.current()?;
    let doc = &tab.ui.diff;
    let line = doc.lines.get(hit.row)?;
    Some((doc.text(line).to_owned(), doc.code(line).get(hit.range.clone())?.to_owned()))
}

fn painted(tab: &Tab, row: usize, column: Column) -> (usize, usize) {
    let (found, current) = crate::find::styles();
    let Some(line) = tab.ui.diff.lines.get(row) else { return (0, 0) };
    let runs = crate::view::diff_highlights(&tab.ui, row, line, column);
    let count = |style: gpui::HighlightStyle| runs.iter().filter(|(_, run)| run.background_color == style.background_color).count();
    (count(found), count(current))
}

fn on_screen(tab: &Tab) -> bool {
    let ui = &tab.ui;
    let Some(hit) = ui.find.current() else { return false };
    let Some(at) = crate::view::diff_visual_of(ui, hit.row) else { return false };
    ui.diff_scroll.0.borrow().deferred_scroll_to_item.is_none()
        && crate::diff_options::viewport(ui).is_some_and(|(top, shown)| at >= top && at < top + shown)
}

impl Replay<'_> {
    fn type_find(&mut self, query: &str) -> Result<()> {
        self.tab.update(self.cx, |tab, _, cx| tab.ui.find.input.update(cx, |field, cx| field.set_text(query, cx)))?;
        Ok(())
    }

    fn find_command(&mut self, id: &str) -> Result<Option<(String, Option<String>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &Target::Repo, cx).into_iter().find(|entry| entry.id == id).map(|entry| (entry.title, entry.disabled))
        })?)
    }

    async fn find_step(&mut self, key: &str, counter: &str, line: &str, matched: &str) -> Result<usize> {
        self.key(key)?;
        self.until(&format!("{key} reading {counter} on {line:?}"), |tab| {
            tab.ui.find.counter().as_deref() == Some(counter)
                && found(tab).is_some_and(|(text, word)| text == line && word == matched)
                && on_screen(tab)
        })
        .await?;
        Ok(self.scroll_top()? as usize)
    }

    async fn find_in_a_diff(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0713");
        let root = init(dir, "Find-reef")?;
        record(&root, "README.md", "Reef notes\n", "Start the reef notes")?;
        let contents = reef();
        fs::write(root.join("reef.txt"), &contents)?;
        let total = grep_count(&root, &["--no-index", "-i", "-o", "-e", "kelp", "--", "reef.txt"])?;
        let exact = grep_count(&root, &["--no-index", "-o", "-e", "Kelp", "--", "reef.txt"])?;
        if (total, exact) != (6, 1) {
            return Err(format!("git grep counts {total} kelp ignoring case and {exact} Kelp").into());
        }
        self.open(&root)?;
        self.until("Find-reef with reef.txt new", |tab| tab.ui.repo == "Find-reef" && files(tab, &[], &["reef.txt"])).await?;
        self.action(Action::DiffMode(DiffMode::Unified))?;
        self.action(Action::Select(file("reef.txt", false)))?;
        self.action(Action::OpenDiff)?;
        self.until("the new reef.txt diff", |tab| tab.ui.diff_open && patch(tab, "+last kelp bed")).await?;
        let title = self.find_command("repo.find")?;
        if title.as_ref().map(|(title, _)| title.as_str()) != Some("Find in diff…") {
            return Err(format!("With a diff open the Cmd-F command reads {title:?}").into());
        }
        let next = self.find_command("view.find-next")?;
        if next.as_ref().and_then(|(_, why)| why.as_deref()) != Some("Open the find bar with ⌘F first") {
            return Err(format!("Before finding, Find next reads {next:?}").into());
        }

        self.focus_desk()?;
        self.key("cmd-f")?;
        self.until("the find bar over the diff", |tab| crate::find::shown(&tab.ui) == Some(crate::find::Surface::Diff)).await?;
        let focused = self.tab.update(self.cx, |tab, window, cx| {
            (tab.ui.find.input.focus_handle(cx).is_focused(window), tab.ui.search.open(), tab.ui.diff_open)
        })?;
        if focused != (true, false, true) {
            return Err(format!("After Cmd-F over a diff (find field focused, commit search open, diff open) reads {focused:?}").into());
        }
        self.type_find("kelp")?;
        self.until("the counter reading 1 of 6", |tab| tab.ui.find.counter().as_deref() == Some("1 of 6")).await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.find.hits().len())? != total {
            return Err(format!("The find bar counts a total other than git grep's {total}").into());
        }
        let first = self.tab.read_with(self.cx, |tab, _| {
            let twice = row(tab, "+Kelp and kelp again")?;
            let at = tab.ui.find.current()?.row;
            Some((found(tab)?, painted(tab, at, Column::Unified), painted(tab, twice, Column::Unified)))
        })?;
        let expected = (("+kelp forest at the north wall".to_owned(), "kelp".to_owned()), (0, 1), (2, 0));
        if first.as_ref() != Some(&expected) {
            return Err(format!("The first match reads (match, its highlights, line 151's highlights) {first:?}").into());
        }
        let start = self.scroll_top()? as usize;

        let after = self.find_step("enter", "2 of 6", "+the KELP thins here", "KELP").await?;
        if after <= start {
            return Err(format!("Return moved to line 150 but the diff's top row stayed at {after} (from {start})").into());
        }
        self.find_step("cmd-g", "3 of 6", "+Kelp and kelp again", "Kelp").await?;
        self.find_step("cmd-g", "4 of 6", "+Kelp and kelp again", "kelp").await?;
        let long = format!("+{} kelp", "x".repeat(400));
        self.find_step("cmd-g", "5 of 6", &long, "kelp").await?;
        let across = self.tab.read_with(self.cx, |tab, _| -f32::from(tab.ui.diff_scroll.0.borrow().base_handle.offset().x))?;
        if across <= 0. {
            return Err("The match at the end of the long line did not scroll the diff sideways".into());
        }
        self.find_step("shift-enter", "4 of 6", "+Kelp and kelp again", "kelp").await?;
        self.find_step("cmd-shift-g", "3 of 6", "+Kelp and kelp again", "Kelp").await?;
        self.find_step("cmd-g", "4 of 6", "+Kelp and kelp again", "kelp").await?;
        self.find_step("cmd-g", "5 of 6", &long, "kelp").await?;
        self.find_step("cmd-g", "6 of 6", "+last kelp bed", "kelp").await?;
        let wrapped = self.find_step("cmd-g", "1 of 6", "+kelp forest at the north wall", "kelp").await?;
        if wrapped > 20 {
            return Err(format!("Wrapping round to the first match left the top row at {wrapped}").into());
        }
        self.find_step("cmd-shift-g", "6 of 6", "+last kelp bed", "kelp").await?;

        self.type_find("Kelp")?;
        self.until("an upper-case letter matching case", |tab| {
            tab.ui.find.counter().as_deref() == Some("1 of 1") && found(tab).is_some_and(|(_, word)| word == "Kelp")
        })
        .await?;
        if self.tab.read_with(self.cx, |tab, _| tab.ui.find.hits().len())? != exact {
            return Err(format!("Matching case counts other than git grep's {exact}").into());
        }
        self.type_find("coral")?;
        self.until("no results for coral", |tab| tab.ui.find.counter().as_deref() == Some("No results")).await?;
        let refused = self.find_command("view.find-next")?;
        if refused.as_ref().and_then(|(_, why)| why.as_deref()) != Some("Nothing matches") {
            return Err(format!("With nothing found, Find next reads {refused:?}").into());
        }
        self.type_find("kelp")?;
        self.until("kelp found again", |tab| tab.ui.find.hits().len() == 6 && tab.ui.find.current().is_some()).await?;

        self.action(Action::DiffMode(DiffMode::Split))?;
        self.until("the split diff still finding", |tab| tab.ui.diff_mode == DiffMode::Split && tab.ui.find.hits().len() == 6).await?;
        self.action(Action::Find(crate::find::Action::Step(1)))?;
        let counter = self.tab.read_with(self.cx, |tab, _| tab.ui.find.counter())?;
        let line = counter.as_deref().and_then(|counter| counter.split(' ').next()?.parse::<usize>().ok()).unwrap_or(0);
        self.until("the split match on screen", on_screen).await?;
        let split = self.tab.read_with(self.cx, |tab, _| {
            let at = tab.ui.find.current()?.row;
            let others = tab.ui.find.hits().iter().filter(|hit| hit.row == at).count() - 1;
            Some((painted(tab, at, Column::New), others))
        })?;
        if !split.is_some_and(|(painted, others)| painted == (others, 1)) || line == 0 {
            return Err(format!("In split view match {counter:?} is painted (others, current) and has others {split:?}").into());
        }
        self.action(Action::DiffMode(DiffMode::Unified))?;

        git(&root, &["add", "--", "reef.txt"])?;
        git(&root, &["commit", "-qm", "Plant the kelp"])?;
        let head = oid(&root, "HEAD")?;
        fs::write(root.join("reef.txt"), contents.replace("reef line 100\n", "reef line one hundred\n"))?;
        self.until("reef.txt edited after the commit", |tab| {
            files(tab, &[], &["reef.txt"]) && patch(tab, "+reef line one hundred") && tab.ui.diff_open
        })
        .await?;
        self.until("the edited hunk with no kelp in it", |tab| tab.ui.find.counter().as_deref() == Some("No results")).await?;
        self.action(Action::DiffOptions(Show::Set(Choice::WholeFile, true)))?;
        self.until("the whole reef.txt finding six again", |tab| {
            tab.ui.diff.options.whole_file && patch(tab, " last kelp bed") && tab.ui.find.counter().as_deref() == Some("1 of 6")
        })
        .await?;
        self.find_step("cmd-shift-g", "6 of 6", " last kelp bed", "kelp").await?;

        self.action(Action::Select(Selection::Commit(head.clone())))?;
        self.until("the commit selected with the find bar gone", |tab| {
            tab.ui.selection == Some(Selection::Commit(head.clone())) && !tab.ui.diff_open && crate::find::shown(&tab.ui).is_none()
        })
        .await?;
        self.action(Action::Inspect(inspection::Action::AllChanges))?;
        self.until("the whole commit open", |tab| tab.ui.diff_open && tab.ui.inspection.path.is_none() && patch(tab, "+last kelp bed")).await?;
        let committed = grep_count(&root, &["-i", "-o", "-e", "kelp", &head, "--", "reef.txt"])?;
        self.focus_desk()?;
        self.key("cmd-f")?;
        self.until("the remembered query finding in the whole commit", |tab| {
            crate::find::shown(&tab.ui).is_some() && tab.ui.find.counter().as_deref() == Some(&format!("1 of {committed}"))
        })
        .await?;
        self.find_step("enter", "2 of 6", "+the KELP thins here", "KELP").await?;
        self.action(Action::Text(crate::diff_select::Action::SelectAll))?;
        let selected = self.tab.read_with(self.cx, |tab, _| {
            let hit = tab.ui.find.current()?.clone();
            let line = tab.ui.diff.lines.get(hit.row)?;
            let runs = crate::view::diff_highlights(&tab.ui, hit.row, line, Column::Unified);
            runs.into_iter().find(|(range, _)| range.start <= hit.range.start && hit.range.start < range.end).map(|(_, style)| style)
        })?;
        let readable = (Some(gpui::rgb(crate::theme::TEXT_SELECTION).into()), Some(gpui::rgb(crate::theme::TEXT).into()));
        if selected.map(|style| (style.background_color, style.color)) != Some(readable) {
            return Err(format!("The current match under a text selection is drawn {selected:?}").into());
        }
        self.action(Action::Text(crate::diff_select::Action::Clear))?;

        self.key("escape")?;
        self.until("the find bar closed and the diff still open", |tab| {
            crate::find::shown(&tab.ui).is_none() && tab.ui.diff_open && crate::view::centre(&tab.ui) == crate::view::Centre::Diff
        })
        .await?;
        let cleared = self.tab.read_with(self.cx, |tab, _| {
            let at = row(tab, "+Kelp and kelp again")?;
            Some(painted(tab, at, Column::Unified))
        })?;
        if cleared != Some((0, 0)) {
            return Err(format!("After closing the bar line 151 is still painted {cleared:?}").into());
        }
        self.key("escape")?;
        self.until("the diff closed by the second Esc", |tab| !tab.ui.diff_open).await?;
        self.pass();
        Ok(())
    }

    async fn find_commits_without_a_diff(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0714");
        let root = init(dir, "Find-log")?;
        record(&root, "log.txt", "first\n", "Chart the lagoon")?;
        record(&root, "log.txt", "first\nsecond\n", "Sound the channel")?;
        let wanted = git(&root, &["log", "-i", "--grep=lagoon", "--format=%H"])?;
        let wanted = wanted.trim().to_owned();
        let tip = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Find-log open on its tip", |tab| tab.ui.repo == "Find-log" && tab.ui.history.len() == 2).await?;
        self.action(Action::Select(Selection::Commit(tip.clone())))?;
        self.until("the tip selected with no diff open", |tab| {
            tab.ui.selection == Some(Selection::Commit(tip.clone())) && !tab.ui.diff_open && crate::view::centre(&tab.ui) == crate::view::Centre::History
        })
        .await?;
        let title = self.find_command("repo.find")?;
        if title != Some(("Find commits…".to_owned(), None)) {
            return Err(format!("With no diff open the Cmd-F command reads {title:?}").into());
        }

        self.focus_desk()?;
        self.key("cmd-f")?;
        self.until("commit search open", |tab| tab.ui.search.open()).await?;
        let focus = self.tab.update(self.cx, |tab, window, cx| {
            (tab.ui.search.input.focus_handle(cx).is_focused(window), crate::find::shown(&tab.ui), tab.ui.find.input.focus_handle(cx).is_focused(window))
        })?;
        if focus != (true, None, false) {
            return Err(format!("After Cmd-F with no diff (toolbar field focused, find bar, find field focused) reads {focus:?}").into());
        }
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.search.input.update(cx, |field, cx| field.replace_text_in_range(None, "lagoon", window, cx))
        })?;
        self.until("lagoon searched", |tab| {
            tab.ui.search.visible() && !tab.ui.search.loading() && crate::view::centre(&tab.ui) == crate::view::Centre::Search
        })
        .await?;
        self.key("enter")?;
        self.until("the lagoon commit chosen from the results", |tab| tab.ui.selection == Some(Selection::Commit(wanted.clone()))).await?;
        self.action(Action::Search(crate::search::Action::Close))?;
        self.until("commit search closed", |tab| !tab.ui.search.open()).await?;

        self.action(Action::Help(crate::help::Action::Shortcuts))?;
        self.until("the shortcuts sheet", |tab| tab.ui.help.open()).await?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.help.filter.update(cx, |field, cx| field.set_text("find", cx));
            window.focus(&tab.ui.modal_focus);
        })?;
        self.key("cmd-f")?;
        let filter = self.tab.update(self.cx, |tab, window, cx| {
            let focused = tab.ui.help.filter.focus_handle(cx).is_focused(window);
            (focused, tab.ui.help.filter.read(cx).selected_range(), tab.ui.search.open())
        })?;
        if filter != (true, 0..4, false) {
            return Err(format!("Cmd-F over the shortcuts sheet (filter focused, selection, commit search open) reads {filter:?}").into());
        }
        self.key("escape")?;
        self.until("the shortcuts sheet closed", |tab| !tab.ui.help.open()).await?;

        let shortcuts = Some(crate::preferences::Section::Shortcuts);
        self.action(Action::Preferences(crate::preferences::Action::Open(shortcuts)))?;
        self.until("the shortcuts in Preferences", |tab| tab.ui.preferences.shows_filter()).await?;
        self.tab.update(self.cx, |tab, _, cx| tab.ui.preferences.filter.update(cx, |field, cx| field.set_text("find", cx)))?;
        self.focus_desk()?;
        self.key("cmd-f")?;
        let preferences = self.tab.update(self.cx, |tab, window, cx| {
            let focused = tab.ui.preferences.filter.focus_handle(cx).is_focused(window);
            (focused, tab.ui.preferences.filter.read(cx).selected_range(), tab.ui.preferences.open(), tab.ui.search.open())
        })?;
        if preferences != (true, 0..4, true, false) {
            return Err(format!(
                "Cmd-F over the Preferences shortcuts (filter focused, selection, Preferences open, commit search open) reads {preferences:?}"
            )
            .into());
        }
        self.action(Action::Preferences(crate::preferences::Action::Close))?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open()).await?;

        self.action(Action::Select(Selection::Changes))?;
        self.until("the working tree selected", |tab| tab.ui.selection == Some(Selection::Changes)).await?;
        self.action(Action::CommitOptions(crate::commit_options::Action::OpenCoauthors))?;
        self.until("the co-author picker", |tab| tab.ui.commit_options.picking()).await?;
        self.tab.update(self.cx, |tab, _, cx| tab.ui.commit_options.filter.update(cx, |field, cx| field.set_text("ada", cx)))?;
        self.focus_desk()?;
        self.key("cmd-f")?;
        let picker = self.tab.update(self.cx, |tab, window, cx| {
            let focused = tab.ui.commit_options.filter.focus_handle(cx).is_focused(window);
            (focused, tab.ui.commit_options.filter.read(cx).selected_range(), tab.ui.commit_options.picking(), tab.ui.search.open())
        })?;
        if picker != (true, 0..3, true, false) {
            return Err(format!("Cmd-F over the co-author picker (filter focused, selection, picker open, commit search open) reads {picker:?}").into());
        }
        self.action(Action::CommitOptions(crate::commit_options::Action::CloseCoauthors))?;
        self.until("the co-author picker closed", |tab| !tab.ui.commit_options.picking()).await?;
        self.pass();
        Ok(())
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in bytes {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend(crc32(&body).to_be_bytes());
}

fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut header = width.to_be_bytes().to_vec();
    header.extend(height.to_be_bytes());
    header.extend([8, 6, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut out, b"IHDR", &header);
    out
}

fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            raw.extend([seed ^ x as u8, y as u8, seed.wrapping_mul(3), 255]);
        }
    }
    let mut stream = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (index, block) in blocks.iter().enumerate() {
        stream.push((index + 1 == blocks.len()) as u8);
        stream.extend((block.len() as u16).to_le_bytes());
        stream.extend((!(block.len() as u16)).to_le_bytes());
        stream.extend_from_slice(block);
    }
    stream.extend(adler32(&raw).to_be_bytes());
    let mut out = png_header(width, height);
    png_chunk(&mut out, b"IDAT", &stream);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

fn bmp(width: u32, height: u32) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let pixels = row * height;
    let mut out = b"BM".to_vec();
    out.extend((54 + pixels).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((width as i32).to_le_bytes());
    out.extend((height as i32).to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(pixels.to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend(2835u32.to_le_bytes());
    out.extend([0u8; 8]);
    for y in 0..height {
        for x in 0..row {
            out.push(if x < width * 3 { (x * 40 + y * 90) as u8 } else { 0 });
        }
    }
    out
}

fn svg(width: u32, height: u32) -> String {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">\n  <rect width=\"{width}\" height=\"{height}\" fill=\"navy\"/>\n</svg>\n")
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seen {
    Absent,
    Drawn(u32, u32, u64),
    Refused(u32, u32, u64, &'static str),
}

fn seen(held: &image_diff::Held, side: usize) -> Option<Seen> {
    if held.request.sides[side].is_none() {
        return Some(Seen::Absent);
    }
    let picture = held.picture(side)?;
    let (width, height) = picture.size?;
    let bytes = picture.bytes?;
    Some(match image_diff::notice(picture) {
        Some(notice) => Seen::Refused(width, height, bytes, notice),
        None if picture.shown() => Seen::Drawn(width, height, bytes),
        None => return None,
    })
}

fn pictured(tab: &Tab, path: &str, expected: [Seen; 2], labels: [&str; 2]) -> bool {
    let Some(held) = image_diff::showing(&tab.ui) else { return false };
    held.request.path == Path::new(path)
        && held.settled()
        && image_diff::labels(&held.request) == labels
        && (0..2).all(|side| seen(held, side) == Some(expected[side]))
        && crate::find::here(&tab.ui).is_none()
        && !crate::diff_options::showing(&tab.ui)
}

fn length(root: &Path, path: &str) -> Result<u64> {
    Ok(fs::metadata(root.join(path))?.len())
}

impl Replay<'_> {
    async fn image_diff(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0715");
        let root = init(dir, "Image-cove")?;
        fs::write(root.join("chart.png"), png(40, 30, 11))?;
        fs::write(root.join("logo.svg"), svg(24, 16))?;
        fs::write(root.join("old.bmp"), bmp(3, 2))?;
        git(&root, &["add", "--", "."])?;
        git(&root, &["commit", "-qm", "Chart the cove"])?;
        let base = oid(&root, "HEAD")?;
        let old_chart = git(&root, &["cat-file", "-s", "HEAD:chart.png"])?.trim().parse::<u64>()?;
        let old_logo = git(&root, &["cat-file", "-s", "HEAD:logo.svg"])?.trim().parse::<u64>()?;
        let old_bmp = git(&root, &["cat-file", "-s", "HEAD:old.bmp"])?.trim().parse::<u64>()?;
        fs::write(root.join("chart.png"), png(64, 48, 23))?;
        fs::write(root.join("logo.svg"), svg(32, 16))?;
        fs::write(root.join("buoy.png"), png(10, 20, 5))?;
        fs::write(root.join("poster.png"), png_header(20000, 20000))?;
        fs::remove_file(root.join("old.bmp"))?;
        let (new_chart, new_logo, buoy, poster) =
            (length(&root, "chart.png")?, length(&root, "logo.svg")?, length(&root, "buoy.png")?, length(&root, "poster.png")?);
        self.open(&root)?;
        self.until("Image-cove with its pictures changed", |tab| {
            tab.ui.repo == "Image-cove" && files(tab, &[], &["chart.png", "logo.svg", "old.bmp", "buoy.png", "poster.png"])
        })
        .await?;

        self.action(Action::Select(file("chart.png", false)))?;
        let chart = [Seen::Drawn(40, 30, old_chart), Seen::Drawn(64, 48, new_chart)];
        self.until("the unstaged chart drawn before and after with both sizes", |tab| {
            tab.ui.diff_open && pictured(tab, "chart.png", chart, ["Before", "After"])
        })
        .await?;
        let captions = self.tab.read_with(self.cx, |tab, _| {
            let held = image_diff::showing(&tab.ui)?;
            Some([0, 1].map(|side| held.picture(side).map(|picture| image_diff::caption(picture, true))))
        })?;
        let expected = [
            Some(format!("40 × 30 · {}", crate::conflict::size(old_chart))),
            Some(format!("64 × 48 · {}", crate::conflict::size(new_chart))),
        ];
        if captions != Some(expected.clone()) {
            return Err(format!("The chart's captions read {captions:?}, not {expected:?}").into());
        }

        self.action(Action::Select(file("buoy.png", false)))?;
        self.until("the untracked buoy drawn as added", |tab| {
            pictured(tab, "buoy.png", [Seen::Absent, Seen::Drawn(10, 20, buoy)], ["", "Added"])
        })
        .await?;
        self.action(Action::Select(file("old.bmp", false)))?;
        self.until("the deleted bitmap drawn as deleted", |tab| {
            pictured(tab, "old.bmp", [Seen::Drawn(3, 2, old_bmp), Seen::Absent], ["Deleted", ""])
        })
        .await?;
        self.action(Action::Select(file("poster.png", false)))?;
        self.until("the huge poster sized but not drawn", |tab| {
            pictured(tab, "poster.png", [Seen::Absent, Seen::Refused(20000, 20000, poster, "Too large to preview")], ["", "Added"])
        })
        .await?;

        self.action(Action::Select(file("logo.svg", false)))?;
        self.until("the logo shown as text with an image toggle", |tab| {
            patch(tab, "+<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"16\">")
                && image_diff::offers_toggle(&tab.ui)
                && image_diff::showing(&tab.ui).is_none()
                && crate::find::here(&tab.ui).is_some()
        })
        .await?;
        let title = self.find_command("view.image-or-text")?;
        if title != Some(("Show the image".to_owned(), None)) {
            return Err(format!("For an SVG diff the image command reads {title:?}").into());
        }
        let action = self.tab.read_with(self.cx, |tab, cx| command_action(tab, "view.image-or-text", &Target::Repo, cx))?;
        self.action(action.ok_or("The image command has no action")?)?;
        self.until("the logo drawn before and after", |tab| {
            tab.ui.images.mode == image_diff::Mode::Image
                && pictured(tab, "logo.svg", [Seen::Drawn(24, 16, old_logo), Seen::Drawn(32, 16, new_logo)], ["Before", "After"])
        })
        .await?;
        let title = self.find_command("view.image-or-text")?;
        if title != Some(("Show the image as text".to_owned(), None)) {
            return Err(format!("With the logo drawn the image command reads {title:?}").into());
        }
        self.action(Action::Images(image_diff::Action::Show(image_diff::Mode::Text)))?;
        self.until("the logo back as text", |tab| {
            image_diff::showing(&tab.ui).is_none() && patch(tab, "-<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"16\">")
        })
        .await?;

        self.action(Action::Select(file("chart.png", false)))?;
        self.until("the chart selected again", |tab| pictured(tab, "chart.png", chart, ["Before", "After"])).await?;
        self.action(Action::StageFile)?;
        self.until("the chart staged", |tab| files(tab, &["chart.png"], &["logo.svg", "old.bmp", "buoy.png", "poster.png"])).await?;
        sized(&root, ":chart.png", new_chart)?;
        self.action(Action::Select(file("chart.png", true)))?;
        self.until("the staged chart drawn from the index", |tab| {
            tab.ui.selection == Some(file("chart.png", true)) && pictured(tab, "chart.png", chart, ["Before", "After"])
        })
        .await?;

        self.action(Action::StageAll)?;
        self.until("every change staged", |tab| {
            files(tab, &["chart.png", "logo.svg", "old.bmp", "buoy.png", "poster.png"], &[])
        })
        .await?;
        self.type_commit("Redraw the cove")?;
        self.until("the picture commit made", |tab| {
            tab.ui.files.is_empty() && tab.ui.history.first().is_some_and(|row| row.subject == "Redraw the cove")
        })
        .await?;
        let head = oid(&root, "HEAD")?;
        sized(&root, "HEAD:chart.png", new_chart)?;
        self.select_commit(&head).await?;
        self.open_file("chart.png").await?;
        self.until("the committed chart drawn before and after", |tab| {
            pictured(tab, "chart.png", chart, ["Before", "After"])
        })
        .await?;
        self.action(Action::CloseDiff)?;
        self.until("the diff closed and its pictures let go", |tab| !tab.ui.diff_open && tab.ui.images.holding() == 0).await?;
        self.action(Action::OpenDiff)?;
        self.until("the reopened chart drawn again", |tab| pictured(tab, "chart.png", chart, ["Before", "After"])).await?;
        self.action(Action::CloseDiff)?;
        self.until("the diff closed again", |tab| !tab.ui.diff_open && tab.ui.images.holding() == 0).await?;

        git(&root, &["switch", "-qc", "side", &base])?;
        fs::write(root.join("chart.png"), png(50, 50, 31))?;
        git(&root, &["commit", "-qam", "Chart the cove from the side"])?;
        let side_chart = git(&root, &["cat-file", "-s", "side:chart.png"])?.trim().parse::<u64>()?;
        git(&root, &["switch", "-q", "main"])?;
        self.until("the side branch seen", |tab| tab.ui.branch == "main").await?;
        self.begin_merge("side").await?;
        self.until("the merge stopped on the chart", |tab| {
            tab.ui.operation.active() && tab.ui.operation.conflicted() == [Path::new("chart.png")]
        })
        .await?;
        self.action(Action::Merge(crate::merge::Action::Open("chart.png".into())))?;
        self.until("both versions of the chart drawn in the chooser", |tab| {
            crate::view::centre(&tab.ui) == crate::view::Centre::Chooser
                && image_diff::conflict_preview(&tab.ui).is_some_and(|held| {
                    held.settled()
                        && seen(held, 0) == Some(Seen::Drawn(64, 48, new_chart))
                        && seen(held, 1) == Some(Seen::Drawn(50, 50, side_chart))
                        && held.picture(0).map(|picture| image_diff::caption(picture, false)).as_deref() == Some("64 × 48")
                })
        })
        .await?;
        self.action(Action::Conflict(crate::conflict::Action::Take("chart.png".into(), crate::merge::Side::Incoming)))?;
        self.until("the chart resolved with the side's version and the previews let go", |tab| {
            tab.ui.operation.conflicted().is_empty()
                && tab.ui.operation.resolved() == [Path::new("chart.png")]
                && tab.ui.images.chooser.is_none()
                && tab.ui.images.holding() == 0
        })
        .await?;
        let side = oid(&root, "side:chart.png")?;
        git_is(&root, &["ls-files", "--stage", "--", "chart.png"], &format!("100644 {side} 0\tchart.png\n"))?;
        if fs::read(root.join("chart.png"))? != png(50, 50, 31) {
            return Err("chart.png does not hold the side's chart".into());
        }
        self.pass();
        Ok(())
    }
}
