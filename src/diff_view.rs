use super::{Action, Desk, Discard, Selection, Ui};
use crate::{
    diff_doc::{self, Blank, DiffDoc, Kind, Line, PatchState, Run, runs},
    kit,
    theme::*,
};
use gpui::{
    Context, Div, ElementId, FontWeight, HighlightStyle, ListHorizontalSizingBehavior, MouseButton,
    MouseDownEvent, Pixels, SharedString, Stateful, StyledText, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use std::ops::Range;

pub(crate) const WHOLE_RENAME: &str = "Unstage the file to stage parts of it";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    #[default]
    Unified,
    Split,
}

#[derive(Debug, PartialEq, Eq)]
enum SplitRow {
    Shared(usize),
    Sides {
        old: Option<usize>,
        new: Option<usize>,
    },
}

#[derive(Default)]
pub(crate) struct Layout {
    split: Vec<SplitRow>,
    widest_unified: Option<usize>,
    widest_code: Option<usize>,
}

impl Layout {
    pub(crate) fn new(doc: &DiffDoc) -> Self {
        let widest_unified = doc
            .lines
            .iter()
            .enumerate()
            .max_by_key(|(_, row)| doc.text(row).chars().count() + if row.kind.is_code() { 13 } else { 34 })
            .map(|(index, _)| index);
        let widest_code = doc
            .lines
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind.is_code())
            .max_by_key(|(_, row)| doc.code(row).chars().count())
            .map(|(index, _)| index);
        let mut split = Vec::with_capacity(doc.lines.len());
        runs(&doc.lines, |run| match run {
            Run::Shared(row) => split.push(SplitRow::Shared(row)),
            Run::Both(row) => split.push(SplitRow::Sides { old: Some(row), new: Some(row) }),
            Run::Change { deleted, added, old_note, new_note } => {
                for offset in 0..deleted.len().max(added.len()) {
                    split.push(SplitRow::Sides {
                        old: deleted.get(offset).copied(),
                        new: added.get(offset).copied(),
                    });
                }
                if old_note.is_some() || new_note.is_some() {
                    split.push(SplitRow::Sides { old: old_note, new: new_note });
                }
            }
        });
        Self {
            split,
            widest_unified,
            widest_code,
        }
    }
}

fn mini<T: Desk>(
    id: impl Into<ElementId>,
    label: &'static str,
    danger: bool,
    enabled: bool,
    action: Action,
    cx: &Context<T>,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(18.))
        .px(px(7.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .rounded(px(4.))
        .font_family(".SystemUIFont")
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .text_color(rgb(if danger { DELETED } else { TEXT }))
        .when(!danger, |d| {
            d.bg(rgb(RAISED)).border_1().border_color(rgb(LINE_STRONG))
        })
        .child(label);
    if !enabled {
        return base.opacity(0.42);
    }
    base.cursor_pointer()
        .hover(move |s| {
            s.bg(if danger {
                soft(DELETED, 0x26)
            } else {
                rgb(HOVER)
            })
        })
        .on_click(cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx)))
}

pub(super) fn render<T: Desk>(app: &T, window: &Window, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let mode = ui.diff_mode;
    let layout = &ui.diff_layout;
    let doc = &ui.diff;
    let listing_history = crate::inspection::showing_history(ui);
    let side = match &ui.selection {
        Some(Selection::File { staged, .. }) => Some(*staged),
        _ => None,
    };
    let committed_file = matches!(ui.selection, Some(Selection::Commit(_)))
        .then(|| ui.inspection.path.clone())
        .flatten();
    let stashed_file = matches!(ui.selection, Some(Selection::Stash(_)))
        .then(|| ui.stash.open_path().map(std::path::Path::to_path_buf))
        .flatten();
    let open_file = committed_file.clone().or_else(|| stashed_file.clone());
    let from = ui
        .selection
        .as_ref()
        .and_then(|selection| ui.renamed_from(selection, open_file.as_deref()));
    let whole_rename = from.is_some() && side == Some(true);
    let heading = if ui.diff_title.is_empty() {
        "Changes".to_owned()
    } else {
        ui.diff_title.clone()
    };
    let editable = ui.can_edit_index();
    let header = div()
        .min_h(px(34.))
        .flex_shrink_0()
        .pl_3()
        .pr_2()
        .py_1()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x_2()
        .gap_y_1()
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .child(
            kit::fit(match &open_file {
                Some(path) => path.to_string_lossy().into_owned(),
                None => heading,
            })
            .size(12.5)
            .weight(FontWeight::SEMIBOLD)
            .cut(kit::Cut::Start),
        )
        .when_some(from.clone(), |d, from| {
            d.child(
                kit::fit(format!("from {}", from.display()))
                    .size(11.5)
                    .color(TEXT_3)
                    .cut(kit::Cut::Start),
            )
        })
        .when_some(side, |d, staged| {
            d.child(kit::chip(
                kit::Chip::Plain,
                if staged { "Staged" } else { "Unstaged" },
            ))
        })
        .child(kit::grow())
        .when_some(ui.selection.clone(), |d, selection| match selection {
            Selection::File {
                path,
                staged: false,
            } => d
                .child(crate::stash::file_button(app, cx))
                .child(
                    kit::btn("discard-file", "Discard…")
                        .small()
                        .danger()
                        .enabled(editable)
                        .on(Action::AskDiscard(Discard::File(path)), cx),
                )
                .child(
                    kit::btn("stage-file", "Stage file")
                        .small()
                        .enabled(editable)
                        .on(Action::StageFile, cx),
                ),
            Selection::File { path, staged: true } => d
                .when(whole_rename, |d| d.child(kit::muted(WHOLE_RENAME)))
                .child(
                    kit::btn("unstage-file", "Unstage file")
                        .small()
                        .enabled(editable)
                        .on(Action::UnstagePath(path), cx),
                ),
            _ => d,
        })
        .when_some(edit_button(ui, cx), |d, button| d.child(button))
        .when_some(crate::stash::restore_button(ui, cx), |d, button| d.child(button))
        .when(committed_file.is_some(), |d| {
            d.child(crate::inspection::history_button(app, cx))
        })
        .when(!listing_history && !doc.lines.is_empty(), |d| {
            d.child(mode_control(mode, cx))
        })
        .child(kit::icon_btn(
            "close-diff",
            "x",
            "Back to the graph (Esc)",
            Action::CloseDiff,
            cx,
        ));
    let list_id = match &ui.selection {
        Some(Selection::File { path, staged }) => {
            format!("patch-{staged}-{}", path.to_string_lossy())
        }
        Some(Selection::Commit(oid)) => format!("patch-{oid}-{committed_file:?}"),
        Some(Selection::Stash(oid)) => format!("stash-patch-{oid}-{stashed_file:?}"),
        _ => "patch".to_owned(),
    };
    let split = mode == Mode::Split;
    let code_width = if split {
        layout.widest_code.and_then(|row| doc.lines.get(row)).map(|row| {
            let text = doc.code(row);
            let mut run = window.text_style().to_run(text.len());
            run.font.family = MONO.into();
            window
                .text_system()
                .shape_line(text.to_owned().into(), px(TEXT_SMALL), &[run], None)
                .width
                .ceil()
                + px(8.)
        })
    } else {
        None
    };
    let count = if split {
        layout.split.len()
    } else {
        doc.lines.len()
    };
    let lines = uniform_list(
        SharedString::from(format!("{list_id}-{mode:?}")),
        count,
        cx.processor(move |app, rows: Range<usize>, _, cx| {
            let ui = app.ui();
            let editable = ui.can_edit_index();
            let layout = &ui.diff_layout;
            let doc = &ui.diff;
            rows
                .filter_map(|index| {
                    let row = if split {
                        match layout.split.get(index)? {
                            SplitRow::Shared(row) => doc.lines.get(*row)?,
                            SplitRow::Sides { old, new } => {
                                let at = new.or(*old).and_then(|row| doc.lines.get(row)).map_or(0, |row| row.at);
                                return Some(line_menu(
                                    base_row()
                                        .child(split_cell(ui, index, *old, true, code_width))
                                        .child(split_divider())
                                        .child(split_cell(ui, index, *new, false, code_width)),
                                    ui,
                                    at,
                                    cx,
                                ));
                            }
                        }
                    } else {
                        doc.lines.get(index)?
                    };
                    let text = doc.text(row);
                    Some(match row.kind {
                        Kind::Hunk => hunk_row(text, row.hunk.filter(|_| doc.stageable), side, editable, split, cx),
                        Kind::File => {
                            let path = text.rsplit_once(" b/").map_or(text, |(_, path)| path);
                            base_row()
                                .pl_3()
                                .bg(rgb(PANEL))
                                .border_b_1()
                                .border_color(rgb(LINE))
                                .child(kit::fit(path.to_owned()).size(12.5).weight(FontWeight::SEMIBOLD))
                        }
                        Kind::Note | Kind::NoNewline => base_row()
                            .pl(if split { px(12.) } else { px(96.) })
                            .child(note(text)),
                        _ => line_menu(
                            base_row()
                                .when_some(line_background(row.kind), |d, bg| d.bg(rgb(bg)))
                                .child(number(row.old, 40.))
                                .child(number(row.new, 40.))
                                .child(marker(row.kind))
                                .child(code(doc, row)),
                            ui,
                            row.at,
                            cx,
                        ),
                    })
                })
                .collect::<Vec<_>>()
        }),
    )
    .track_scroll(ui.diff_scroll.clone())
    .flex_1()
    .min_h_0();
    let mut lines = if split {
        lines.with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::FitList)
    } else {
        lines
            .with_width_from_item(layout.widest_unified)
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    };
    if split {
        lines.style().restrict_scroll_to_axis = Some(true);
    }
    let bars = (!listing_history).then(|| bars(ui)).unwrap_or_default();
    let placeholder = (!listing_history && doc.lines.is_empty() && doc.state != PatchState::Truncated)
        .then(|| placeholder(ui, from.is_some()));
    let showing = !listing_history && placeholder.is_none();
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .when(listing_history, |d| {
            d.child(crate::inspection::file_history(app, cx))
        })
        .children(bars)
        .children(placeholder)
        .when(showing, |d| {
            let sideways = if split {
                kit::scrollbar("diff-pan", &ui.split_pan).within(&ui.diff_scroll)
            } else {
                kit::scrollbar("diff-pan", &ui.diff_scroll)
            };
            d.when(split, |d| d.child(split_headings()))
                .child(lines)
                .child(kit::scrollbar("diff-scrollbar", &ui.diff_scroll))
                .child(sideways.horizontal())
        })
}

pub(crate) fn placeholder_text(ui: &Ui, renamed: bool) -> Option<String> {
    let whole = match &ui.selection {
        Some(Selection::Commit(_)) if ui.inspection.path.is_none() => Some("commit"),
        Some(Selection::Stash(_)) if ui.stash.open_path().is_none() => Some("stash"),
        _ => None,
    };
    Some(match &ui.diff.state {
        PatchState::Loading => "Loading diff…".to_owned(),
        PatchState::Text | PatchState::Truncated => return None,
        PatchState::Binary { old, new } => diff_doc::binary_text(*old, *new),
        PatchState::ModeOnly { old, new } => diff_doc::mode_only_text(*old, *new),
        PatchState::Unreadable(reason) => format!("This diff could not be shown. {reason}"),
        PatchState::Empty(Blank::EmptyFile) => diff_doc::EMPTY_FILE.to_owned(),
        PatchState::Empty(Blank::EmptyDeleted) => diff_doc::EMPTY_DELETED.to_owned(),
        PatchState::Empty(Blank::Renamed) => RENAMED_ONLY.to_owned(),
        PatchState::Empty(Blank::Nothing) => match whole {
            Some(what) => format!("This {what} changes no files."),
            None if renamed => RENAMED_ONLY.to_owned(),
            None => "No text changes to show for this selection.".to_owned(),
        },
    })
}

const RENAMED_ONLY: &str = "Renamed with no changes to its content.";

fn placeholder(ui: &Ui, renamed: bool) -> Div {
    let text = placeholder_text(ui, renamed).unwrap_or_default();
    div()
        .flex_1()
        .min_h_0()
        .p_4()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .when(ui.diff.state == PatchState::Loading, |d| d.child(kit::spinner("diff-loading")))
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(TEXT_2))
                .child(text),
        )
}

fn bar_texts(ui: &Ui) -> Vec<&'static str> {
    let stageable_view = matches!(ui.selection, Some(Selection::File { .. }));
    let doc = &ui.diff;
    let mut texts = Vec::new();
    if doc.state == PatchState::Truncated {
        texts.push(diff_doc::truncated_text(stageable_view));
    }
    if doc.shortened {
        texts.push(diff_doc::shortened_text(stageable_view));
    }
    texts
}

fn bars(ui: &Ui) -> Vec<Div> {
    bar_texts(ui)
        .into_iter()
        .map(|text| {
            div()
                .flex_shrink_0()
                .px_3()
                .py(px(6.))
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(RAISED))
                .border_b_1()
                .border_color(rgb(LINE))
                .child(kit::icon("alert").text_color(rgb(MODIFIED)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_SMALL))
                        .text_color(rgb(TEXT_2))
                        .child(text),
                )
        })
        .collect()
}

fn edit_button<T: Desk>(ui: &Ui, cx: &Context<T>) -> Option<impl IntoElement + use<T>> {
    let Some(Selection::File { path, staged }) = &ui.selection else { return None };
    let target = crate::commands::Target::File {
        path: path.clone(),
        side: if *staged { crate::commands::Side::Staged } else { crate::commands::Side::Unstaged },
    };
    let entry = crate::commands::entry(crate::commands::find("file.edit")?, ui, &target, cx)?;
    let action = entry.action?;
    Some(kit::btn("edit-file", entry.title).small().ghost().icon("external").on(action, cx))
}

fn mode_control<T: Desk>(mode: Mode, cx: &Context<T>) -> Div {
    let segment = |id, label, choice| {
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(mode == choice)
            .on(Action::DiffMode(choice), cx)
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .p(px(1.))
        .rounded(px(RADIUS))
        .bg(rgb(CANVAS))
        .border_1()
        .border_color(rgb(LINE))
        .child(segment("diff-unified", "Unified", Mode::Unified))
        .child(segment("diff-split", "Split", Mode::Split))
}

fn line_menu<T: Desk>(row: Div, ui: &Ui, at: u32, cx: &Context<T>) -> Div {
    let Some(target) = line_target(&ui.selection, &ui.files, at) else { return row };
    row.on_mouse_down(
        MouseButton::Right,
        cx.listener(move |app, event: &MouseDownEvent, window, cx| {
            let menu = super::Menu::Object { target: target.clone(), at: event.position };
            app.dispatch(Action::Menu(Some(menu)), window, cx);
        }),
    )
}

fn line_target(selection: &Option<Selection>, files: &[super::FileRow], at: u32) -> Option<crate::commands::Target> {
    let Some(Selection::File { path, staged }) = selection else { return None };
    if at == 0 || (*staged && files.iter().any(|file| !file.staged && file.path == *path)) {
        return None;
    }
    Some(crate::commands::Target::Line { path: path.clone(), line: at })
}

fn base_row() -> Div {
    div().h(px(DIFF_LINE)).w_full().flex().items_center()
}

fn split_divider() -> Div {
    div().w(px(1.)).h_full().flex_shrink_0().bg(rgb(LINE))
}

fn split_headings() -> Div {
    base_row()
        .flex_shrink_0()
        .bg(rgb(PANEL))
        .border_b_1()
        .border_color(rgb(LINE))
        .text_size(px(TEXT_TINY))
        .text_color(rgb(TEXT_2))
        .child(div().pl_3().flex_1().child("Before"))
        .child(split_divider())
        .child(div().pl_3().flex_1().child("After"))
}

fn split_cell(ui: &Ui, index: usize, side: Option<usize>, old: bool, width: Option<Pixels>) -> Div {
    let cell = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .overflow_hidden();
    let doc = &ui.diff;
    let Some(row) = side.and_then(|i| doc.lines.get(i)) else {
        return cell.bg(rgb(PANEL));
    };
    if row.kind == Kind::NoNewline {
        return cell.pl_3().child(note(doc.text(row)));
    }
    let mut text = div()
        .id((if old { "split-old" } else { "split-new" }, index))
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .track_scroll(&ui.split_pan)
        .child(code(doc, row).when_some(width, |d, width| d.min_w(width)));
    text.style().restrict_scroll_to_axis = Some(true);
    cell.when_some(line_background(row.kind), |d, bg| d.bg(rgb(bg)))
        .child(number(if old { row.old } else { row.new }, 34.))
        .child(marker(row.kind))
        .child(text)
}

fn line_background(kind: Kind) -> Option<u32> {
    match kind {
        Kind::Added => Some(ADDED_BG),
        Kind::Deleted => Some(DELETED_BG),
        _ => None,
    }
}

fn number(number: Option<u32>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .pr_2()
        .flex()
        .justify_end()
        .font_family(MONO)
        .text_size(px(TEXT_TINY))
        .text_color(rgb(TEXT_OFF))
        .when_some(number, |d, number| d.child(number.to_string()))
}

fn marker(kind: Kind) -> Div {
    let (sign, ink) = match kind {
        Kind::Added => ("+", ADDED),
        Kind::Deleted => ("−", DELETED),
        _ => ("", TEXT_3),
    };
    div()
        .w(px(16.))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .font_family(MONO)
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(ink))
        .child(sign)
}

fn code(doc: &DiffDoc, row: &Line) -> Div {
    let text = doc.code(row).to_owned();
    let color = if row.kind == Kind::Added { ADDED_HI } else { DELETED_HI };
    let highlight = HighlightStyle {
        background_color: Some(rgb(color).into()),
        ..Default::default()
    };
    div()
        .flex_shrink_0()
        .font_family(MONO)
        .whitespace_nowrap()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(TEXT))
        .child(StyledText::new(text).with_highlights(
            row.words.iter().cloned().map(|span| (span, highlight)),
        ))
}

fn note(text: &str) -> impl IntoElement {
    kit::fit(text.trim_start_matches("\\ ").to_owned())
        .size(TEXT_SMALL)
        .color(TEXT_3)
}

fn hunk_row<T: Desk>(
    text: &str,
    hunk: Option<usize>,
    side: Option<bool>,
    editable: bool,
    split: bool,
    cx: &Context<T>,
) -> Div {
    let (range, context) = text
        .get(2..)
        .and_then(|rest| rest.split_once("@@"))
        .map_or((text, ""), |(range, context)| (range.trim(), context.trim()));
    let row = base_row()
        .pl_3()
        .pr_2()
        .gap_2()
        .bg(rgb(HUNK))
        .border_b_1()
        .border_color(rgb(LINE))
        .font_family(MONO)
        .text_size(px(11.5))
        .whitespace_nowrap()
        .child(
            div()
                .flex_shrink_0()
                .text_color(rgb(TEXT_3))
                .child(format!("@@ {range} @@")),
        )
        .when(split, |d| {
            d.child(kit::fit(context.to_owned()).mono().size(11.5).color(TEXT_2).grow())
        })
        .when(!split, |d| {
            d.child(div().text_color(rgb(TEXT_2)).child(context.to_owned()))
        });
    row.when_some(hunk.zip(side), |d, (hunk, staged)| {
        if staged {
            d.child(mini(
                ("unstage-hunk", hunk),
                "Unstage hunk",
                false,
                editable,
                Action::UnstageHunk(hunk),
                cx,
            ))
        } else {
            d.child(mini(
                ("discard-hunk", hunk),
                "Discard hunk",
                true,
                editable,
                Action::AskDiscard(Discard::Hunk(hunk)),
                cx,
            ))
            .child(mini(
                ("stage-hunk", hunk),
                "Stage hunk",
                false,
                editable,
                Action::StageHunk(hunk),
                cx,
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;

    #[test]
    fn a_staged_line_is_offered_only_while_the_working_tree_has_the_same_lines() {
        use crate::commands::Target;
        let row = |staged| super::super::FileRow { path: "trail.txt".into(), staged, status: "M", from: None };
        let selected = |staged| Some(Selection::File { path: "trail.txt".into(), staged });
        let line = Some(Target::Line { path: "trail.txt".into(), line: 2 });
        assert_eq!(line_target(&selected(false), &[row(false)], 2), line);
        assert_eq!(line_target(&selected(true), &[row(true)], 2), line);
        assert_eq!(line_target(&selected(true), &[row(true), row(false)], 2), None);
        assert_eq!(line_target(&selected(false), &[row(true), row(false)], 2), line);
        assert_eq!(line_target(&selected(false), &[row(false)], 0), None);
        assert_eq!(line_target(&Some(Selection::Commit("abc".into())), &[], 2), None);
    }

    #[test]
    fn split_aligns_context_and_pairs_changed_rows_within_each_hunk() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -4,3 +4,4 @@ section\n here\n-Old hut\n+New hut\n+New trail\n end\n",
            "@@ -20 +21,0 @@\n-deleted only\n",
        ));
        let layout = Layout::new(&doc);
        assert_eq!(layout.split, [
            SplitRow::Shared(0),
            SplitRow::Sides { old: Some(1), new: Some(1) },
            SplitRow::Sides { old: Some(2), new: Some(3) },
            SplitRow::Sides { old: None, new: Some(4) },
            SplitRow::Sides { old: Some(5), new: Some(5) },
            SplitRow::Shared(6),
            SplitRow::Sides { old: Some(7), new: None },
        ]);
        assert_eq!(doc.lines[0].hunk, Some(0));
        assert_eq!(doc.lines[6].hunk, Some(1));
        assert_eq!((doc.lines[5].old, doc.lines[5].new), (Some(6), Some(7)));
        assert_eq!(layout.widest_code, Some(7));
    }

    #[test]
    fn split_keeps_newline_annotations_and_file_boundaries() {
        let doc = doc(concat!(
            "diff --git a/trail b/trail\n@@ -1 +1 @@\n-Old hut\n",
            "\\ No newline at end of file\n+New hut\n\\ No newline at end of file\n",
            "diff --git a/map b/map\n@@ -0,0 +1 @@\n+map\n\n[an unexpected line]\n",
        ));
        let layout = Layout::new(&doc);
        assert_eq!(layout.split, [
            SplitRow::Shared(0),
            SplitRow::Shared(1),
            SplitRow::Sides { old: Some(2), new: Some(4) },
            SplitRow::Sides { old: Some(3), new: Some(5) },
            SplitRow::Shared(6),
            SplitRow::Shared(7),
            SplitRow::Sides { old: None, new: Some(8) },
            SplitRow::Shared(9),
        ]);
    }
}
