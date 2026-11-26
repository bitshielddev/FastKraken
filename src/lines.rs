use crate::{
    Tab,
    commands::{Side, Target},
    diff_doc::{DiffDoc, Kind},
    kit, prompt,
    repository::Diff,
    theme::*,
    view::{self, Desk, DiscardAsk, Selection, Ui},
};
use gpui::{Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent, Window, div, prelude::*, rgb};
use crate::theme::px;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum How {
    One,
    Toggle,
    Range,
}

impl How {
    fn of(modifiers: &gpui::Modifiers) -> Self {
        if modifiers.shift {
            Self::Range
        } else if modifiers.platform {
            Self::Toggle
        } else {
            Self::One
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Press { row: usize, how: How },
    DragTo(usize),
    Release,
    Clear,
    Stage,
    Unstage,
    AskDiscard,
}

impl Action {
    pub(crate) fn picks(&self) -> bool {
        matches!(self, Self::Press { .. } | Self::DragTo(_) | Self::Release | Self::Clear)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pick {
    selection: Selection,
    fingerprint: u64,
    pub(crate) rows: BTreeSet<usize>,
    anchor: usize,
}

#[derive(Clone, Debug)]
struct Drag {
    anchor: usize,
    base: BTreeSet<usize>,
}

#[derive(Default)]
pub(crate) struct State {
    pick: Option<Pick>,
    drag: Option<Drag>,
}

impl State {
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

pub(crate) fn changed(doc: &DiffDoc, row: usize) -> bool {
    doc.lines
        .get(row)
        .is_some_and(|line| line.hunk.is_some() && matches!(line.kind, Kind::Added | Kind::Deleted))
}

fn span(doc: &DiffDoc, from: usize, to: usize) -> BTreeSet<usize> {
    let (low, high) = if from <= to { (from, to) } else { (to, from) };
    (low..=high).filter(|&row| changed(doc, row)).collect()
}

fn pressed(doc: &DiffDoc, current: Option<(&BTreeSet<usize>, usize)>, row: usize, how: How) -> (BTreeSet<usize>, Drag) {
    match how {
        How::One if current.is_some_and(|(rows, _)| rows.len() == 1 && rows.contains(&row)) => {
            (BTreeSet::new(), Drag { anchor: row, base: BTreeSet::new() })
        }
        How::One => (BTreeSet::from([row]), Drag { anchor: row, base: BTreeSet::new() }),
        How::Toggle => {
            let mut rows = current.map(|(rows, _)| rows.clone()).unwrap_or_default();
            if !rows.remove(&row) {
                rows.insert(row);
            }
            let mut base = rows.clone();
            base.remove(&row);
            (rows, Drag { anchor: row, base })
        }
        How::Range => {
            let anchor = current.map_or(row, |(_, anchor)| anchor);
            (span(doc, anchor, row), Drag { anchor, base: BTreeSet::new() })
        }
    }
}

fn dragged(doc: &DiffDoc, drag: &Drag, to: usize) -> BTreeSet<usize> {
    let mut rows = drag.base.clone();
    rows.extend(span(doc, drag.anchor, to));
    rows
}

pub(crate) fn side(ui: &Ui) -> Option<bool> {
    match &ui.selection {
        Some(Selection::File { staged, .. })
            if ui.diff.stageable
                && !ui.loading_diff()
                && view::diff_shown(ui)
                && crate::image_diff::showing(ui).is_none() =>
        {
            Some(*staged)
        }
        _ => None,
    }
}

pub(crate) fn current(ui: &Ui) -> Option<&Pick> {
    let pick = ui.lines.pick.as_ref()?;
    (side(ui).is_some() && ui.selection.as_ref() == Some(&pick.selection) && !pick.rows.is_empty()).then_some(pick)
}

pub(crate) fn picked(ui: &Ui, row: usize) -> bool {
    current(ui).is_some_and(|pick| pick.rows.contains(&row))
}

pub(crate) fn target(ui: &Ui) -> Option<Target> {
    let pick = current(ui)?;
    let staged = side(ui)?;
    let Selection::File { path, .. } = &pick.selection else { return None };
    Some(Target::Lines {
        path: path.clone(),
        side: if staged { Side::Staged } else { Side::Unstaged },
        count: pick.rows.len(),
    })
}

pub(crate) fn reloaded(ui: &mut Ui) {
    let kept = ui.lines.pick.as_ref().is_some_and(|pick| {
        ui.selection.as_ref() == Some(&pick.selection)
            && ui.diff.stageable
            && pick.fingerprint == ui.diff.fingerprint()
            && pick.rows.iter().all(|&row| changed(&ui.diff, row))
    });
    if !kept {
        ui.lines = State::default();
    }
}

pub(crate) fn locate(doc: &DiffDoc, rows: &BTreeSet<usize>) -> Vec<(usize, usize)> {
    rows.iter()
        .filter(|&&row| changed(doc, row))
        .filter_map(|&row| {
            let hunk = doc.lines[row].hunk?;
            let start = doc.hunks.get(hunk)?.start;
            Some((hunk, row.checked_sub(start + 1)?))
        })
        .collect()
}

pub(crate) fn counted(verb: &str, count: usize, tail: &str) -> String {
    if count == 1 {
        format!("{verb} line{tail}")
    } else {
        format!("{verb} {count} lines{tail}")
    }
}

fn chosen(tab: &Tab, staged: bool) -> Option<(Arc<Diff>, Vec<(usize, usize)>, PathBuf)> {
    if side(&tab.ui)? != staged {
        return None;
    }
    let pick = current(&tab.ui)?;
    let Selection::File { path, .. } = &pick.selection else { return None };
    let lines = locate(&tab.ui.diff, &pick.rows);
    if lines.is_empty() {
        return None;
    }
    Some((tab.diff.clone()?, lines, path.clone()))
}

fn fingerprint(ui: &Ui, selection: &Selection) -> u64 {
    match &ui.lines.pick {
        Some(pick) if &pick.selection == selection => pick.fingerprint,
        _ => ui.diff.fingerprint(),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Press { row, how } => {
            let Some(selection) = tab.ui.selection.clone().filter(|_| side(&tab.ui).is_some()) else { return };
            if !changed(&tab.ui.diff, row) {
                return;
            }
            let current = current(&tab.ui).map(|pick| (&pick.rows, pick.anchor));
            let (rows, drag) = pressed(&tab.ui.diff, current, row, how);
            let anchor = drag.anchor;
            let fingerprint = fingerprint(&tab.ui, &selection);
            tab.ui.lines.pick = (!rows.is_empty()).then_some(Pick { selection, fingerprint, rows, anchor });
            tab.ui.lines.drag = Some(drag);
            cx.notify();
        }
        Action::DragTo(row) => {
            let Some(selection) = tab.ui.selection.clone().filter(|_| side(&tab.ui).is_some()) else { return };
            let Some(drag) = tab.ui.lines.drag.clone() else { return };
            let rows = dragged(&tab.ui.diff, &drag, row);
            if current(&tab.ui).map(|pick| &pick.rows) == Some(&rows) || (rows.is_empty() && tab.ui.lines.pick.is_none()) {
                return;
            }
            let fingerprint = fingerprint(&tab.ui, &selection);
            tab.ui.lines.pick = (!rows.is_empty()).then_some(Pick { selection, fingerprint, rows, anchor: drag.anchor });
            cx.notify();
        }
        Action::Release => tab.ui.lines.drag = None,
        Action::Clear => {
            tab.ui.lines = State::default();
            cx.notify();
        }
        Action::Stage => {
            if let Some((diff, lines, _)) = chosen(tab, false) {
                tab.mutate(cx, move |repo| repo.stage_lines(&diff, &lines));
            }
        }
        Action::Unstage => {
            if let Some((diff, lines, _)) = chosen(tab, true) {
                tab.mutate(cx, move |repo| repo.unstage_lines(&diff, &lines));
            }
        }
        Action::AskDiscard => {
            if !tab.ui.can_edit_index() || tab.ui.prompt.open() {
                return;
            }
            let Some((diff, lines, path)) = chosen(tab, false) else { return };
            prompt::ask(&mut tab.ui, prompt::Kind::Discard(DiscardAsk::Lines { path, lines, diff }), cx);
        }
    }
}

pub(crate) fn describe_discard(name: &str, count: usize) -> prompt::Spec {
    let title = if count == 1 {
        format!("Discard this line of {name}?")
    } else {
        format!("Discard {count} lines of {name}?")
    };
    let consequence = if count == 1 {
        "This line returns to its staged content. This cannot be undone."
    } else {
        "These lines return to their staged content. This cannot be undone."
    };
    prompt::Spec::new(title, counted("Discard", count, "")).line(consequence)
}

pub(crate) fn picked_text(count: usize) -> String {
    if count == 1 { "1 line picked".to_owned() } else { format!("{count} lines picked") }
}

pub(crate) fn bar<T: Desk>(ui: &Ui, cx: &Context<T>) -> Option<Div> {
    let pick = current(ui)?;
    let staged = side(ui)?;
    let editable = ui.can_edit_index();
    let buttons = if staged {
        div().child(
            kit::btn("unstage-lines", "Unstage lines")
                .small()
                .hint("U")
                .enabled(editable)
                .on(view::Action::Lines(Action::Unstage), cx),
        )
    } else {
        div()
            .flex()
            .gap_2()
            .child(
                kit::btn("discard-lines", "Discard lines…")
                    .small()
                    .danger()
                    .enabled(editable)
                    .on(view::Action::Lines(Action::AskDiscard), cx),
            )
            .child(
                kit::btn("stage-lines", "Stage lines")
                    .small()
                    .hint("S")
                    .enabled(editable)
                    .on(view::Action::Lines(Action::Stage), cx),
            )
    };
    Some(
        div()
            .flex_shrink_0()
            .px_3()
            .py(px(4.))
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(palette().selected))
            .border_b_1()
            .border_color(rgb(palette().line))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(TEXT_SMALL))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(palette().text))
                    .child(picked_text(pick.rows.len())),
            )
            .child(
                kit::btn("clear-lines", "Clear")
                    .small()
                    .ghost()
                    .tip("Clear the picked lines (Esc)")
                    .on(view::Action::Lines(Action::Clear), cx),
            )
            .child(buttons),
    )
}

pub(crate) fn gutter<T: Desk>(ui: &Ui, row: usize, cells: Div, cx: &Context<T>) -> Div {
    if side(ui).is_none() || !changed(&ui.diff, row) {
        return cells;
    }
    cells
        .when(picked(ui, row), |d| d.bg(rgb(palette().selected)))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(palette().hover)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, event: &MouseDownEvent, window, cx| {
                let how = How::of(&event.modifiers);
                app.dispatch(view::Action::Lines(Action::Press { row, how }), window, cx);
            }),
        )
        .on_mouse_move(cx.listener(move |app, event: &MouseMoveEvent, window, cx| {
            if event.pressed_button == Some(MouseButton::Left) && app.ui().lines.dragging() {
                app.dispatch(view::Action::Lines(Action::DragTo(row)), window, cx);
            }
        }))
}

fn release<T: Desk>(app: &mut T, _: &gpui::MouseUpEvent, window: &mut Window, cx: &mut Context<T>) {
    if app.ui().lines.dragging() {
        app.dispatch(view::Action::Lines(Action::Release), window, cx);
    }
}

pub(crate) fn released<T: Desk>(list: Div, cx: &Context<T>) -> Div {
    list.on_mouse_up(MouseButton::Left, cx.listener(release))
        .on_mouse_up_out(MouseButton::Left, cx.listener(release))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;

    fn sample() -> DiffDoc {
        doc(concat!(
            "diff --git a/log b/log\nindex 012..345 100644\n--- a/log\n+++ b/log\n",
            "@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n-d\n+D\n",
            "@@ -20,2 +20,3 @@\n t\n+u\n v\n",
        ))
    }

    fn set(rows: &[usize]) -> BTreeSet<usize> {
        rows.iter().copied().collect()
    }

    #[test]
    fn only_added_and_removed_rows_can_be_picked() {
        let doc = sample();
        let rows: Vec<usize> = (0..doc.lines.len()).filter(|&row| changed(&doc, row)).collect();
        assert_eq!(rows, [2, 3, 5, 6, 9]);
        assert!(!changed(&doc, 0) && !changed(&doc, 1) && !changed(&doc, 7) && !changed(&doc, 99));
    }

    #[test]
    fn a_click_picks_one_row_and_a_second_click_on_it_clears_the_pick() {
        let doc = sample();
        let (rows, drag) = pressed(&doc, None, 3, How::One);
        assert_eq!((rows.clone(), drag.anchor), (set(&[3]), 3));
        let (rows, _) = pressed(&doc, Some((&rows, 3)), 5, How::One);
        assert_eq!(rows, set(&[5]));
        let (rows, _) = pressed(&doc, Some((&rows, 5)), 5, How::One);
        assert!(rows.is_empty());
    }

    #[test]
    fn shift_picks_the_changed_rows_between_the_anchor_and_the_row_across_hunks() {
        let doc = sample();
        let (rows, drag) = pressed(&doc, Some((&set(&[3]), 3)), 9, How::Range);
        assert_eq!((rows, drag.anchor), (set(&[3, 5, 6, 9]), 3));
        let (rows, _) = pressed(&doc, Some((&set(&[6]), 6)), 2, How::Range);
        assert_eq!(rows, set(&[2, 3, 5, 6]));
        let (rows, _) = pressed(&doc, None, 5, How::Range);
        assert_eq!(rows, set(&[5]));
    }

    #[test]
    fn command_toggles_one_row_and_dragging_adds_the_rows_passed_over() {
        let doc = sample();
        let (rows, drag) = pressed(&doc, Some((&set(&[2]), 2)), 9, How::Toggle);
        assert_eq!(rows, set(&[2, 9]));
        assert_eq!(dragged(&doc, &drag, 6), set(&[2, 6, 9]));
        let (rows, _) = pressed(&doc, Some((&rows, 9)), 2, How::Toggle);
        assert_eq!(rows, set(&[9]));
        let (_, drag) = pressed(&doc, None, 2, How::One);
        assert_eq!(dragged(&doc, &drag, 5), set(&[2, 3, 5]));
        assert_eq!(dragged(&doc, &drag, 0), set(&[2]));
    }

    #[test]
    fn picked_rows_become_lines_within_their_hunks() {
        let doc = sample();
        assert_eq!(locate(&doc, &set(&[2, 6, 9, 7])), [(0, 1), (0, 5), (1, 1)]);
    }

    #[test]
    fn titles_count_lines() {
        assert_eq!(counted("Stage", 1, ""), "Stage line");
        assert_eq!(counted("Discard", 3, "…"), "Discard 3 lines…");
        assert_eq!(picked_text(1), "1 line picked");
        assert_eq!(picked_text(4), "4 lines picked");
        let spec = describe_discard("log.txt", 2);
        assert_eq!(spec.title, "Discard 2 lines of log.txt?");
        assert_eq!(spec.confirm.as_ref(), "Discard 2 lines");
        assert_eq!(describe_discard("log.txt", 1).title, "Discard this line of log.txt?");
    }
}
