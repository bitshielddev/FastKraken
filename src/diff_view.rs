use super::{Action, Desk, Discard, Selection, Ui};
use crate::{kit, theme::*};
use gpui::{
    Context, Div, ElementId, FontWeight, HighlightStyle, ListHorizontalSizingBehavior, MouseButton,
    MouseDownEvent, Pixels, SharedString, Stateful, StyledText, Window, div, prelude::*, px, rgb,
    uniform_list,
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub text: String,
    pub hunk: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    #[default]
    Unified,
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Context,
    Added,
    Deleted,
    Hunk,
    File,
    Note,
    NoNewline,
}

impl Kind {
    fn is_code(self) -> bool {
        matches!(self, Self::Context | Self::Added | Self::Deleted)
    }
}

#[derive(Debug)]
struct Shown {
    line: usize,
    kind: Kind,
    old: u32,
    new: u32,
    at: u32,
    highlights: Vec<Range<usize>>,
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
    unified: Vec<Shown>,
    split: Vec<SplitRow>,
    widest_unified: Option<usize>,
    widest_code: Option<usize>,
}

impl Layout {
    pub(crate) fn new(lines: &[DiffLine]) -> Self {
        let files = lines
            .iter()
            .filter(|line| line.text.starts_with("diff --git "))
            .count();
        let mut unified = Vec::with_capacity(lines.len());
        let mut old = 0..0;
        let mut new = 0..0;
        let mut previous_was_code = false;
        for (line, row) in lines.iter().enumerate() {
            let text = row.text.as_str();
            let (kind, old_number, new_number) = if text.starts_with("diff --git ") {
                old = 0..0;
                new = 0..0;
                previous_was_code = false;
                if files < 2 {
                    continue;
                }
                (Kind::File, 0, 0)
            } else if text.starts_with("@@ ") {
                let mut fields = text.split_ascii_whitespace().skip(1);
                old = hunk_range(fields.next(), '-');
                new = hunk_range(fields.next(), '+');
                (Kind::Hunk, 0, 0)
            } else if previous_was_code && text.starts_with("\\ ") {
                (Kind::NoNewline, 0, 0)
            } else if text.starts_with('+') && !new.is_empty() {
                (Kind::Added, 0, new.next().unwrap_or(0))
            } else if text.starts_with('-') && !old.is_empty() {
                (Kind::Deleted, old.next().unwrap_or(0), 0)
            } else if (text.starts_with(' ') || text.is_empty())
                && !old.is_empty()
                && !new.is_empty()
            {
                (Kind::Context, old.next().unwrap_or(0), new.next().unwrap_or(0))
            } else if text.is_empty()
                || [
                    "index ",
                    "--- ",
                    "+++ ",
                    "new file mode",
                    "deleted file mode",
                    "old mode",
                    "new mode",
                ]
                .iter()
                .any(|noise| text.starts_with(noise))
            {
                continue;
            } else {
                (Kind::Note, 0, 0)
            };
            previous_was_code = kind.is_code();
            let at = match kind {
                Kind::Deleted => new.start.max(1),
                _ => new_number,
            };
            unified.push(Shown {
                line,
                kind,
                old: old_number,
                new: new_number,
                at,
                highlights: Vec::new(),
            });
        }

        let widest_unified = unified
            .iter()
            .enumerate()
            .max_by_key(|(_, row)| {
                lines[row.line].text.chars().count() + if row.kind.is_code() { 13 } else { 34 }
            })
            .map(|(index, _)| index);
        let widest_code = unified
            .iter()
            .filter(|row| row.kind.is_code())
            .map(|row| row.line)
            .max_by_key(|&line| lines[line].text.chars().count());
        let split = split_rows(&mut unified, lines);
        Self {
            unified,
            split,
            widest_unified,
            widest_code,
        }
    }
}

fn hunk_range(field: Option<&str>, prefix: char) -> Range<u32> {
    let field = field
        .and_then(|value| value.strip_prefix(prefix))
        .unwrap_or_default();
    let (start, count) = field.split_once(',').unwrap_or((field, "1"));
    let start = start.parse::<u32>().unwrap_or(1);
    start..start.saturating_add(count.parse().unwrap_or(1))
}

fn split_rows(shown: &mut [Shown], lines: &[DiffLine]) -> Vec<SplitRow> {
    let mut split = Vec::with_capacity(shown.len());
    let mut index = 0;
    while index < shown.len() {
        let kind = shown[index].kind;
        if kind == Kind::Context {
            split.push(SplitRow::Sides {
                old: Some(index),
                new: Some(index),
            });
            index += 1;
        } else if !matches!(kind, Kind::Added | Kind::Deleted) {
            split.push(SplitRow::Shared(index));
            index += 1;
        } else {
            let mut deleted = Vec::new();
            let mut added = Vec::new();
            let (mut old_note, mut new_note) = (None, None);
            while index < shown.len() {
                match shown[index].kind {
                    Kind::Deleted => deleted.push(index),
                    Kind::Added => added.push(index),
                    Kind::NoNewline if shown[index - 1].kind == Kind::Deleted => {
                        old_note = Some(index);
                    }
                    Kind::NoNewline => new_note = Some(index),
                    _ => break,
                }
                index += 1;
            }
            for offset in 0..deleted.len().max(added.len()) {
                let old = deleted.get(offset).copied();
                let new = added.get(offset).copied();
                if let (Some(old), Some(new)) = (old, new) {
                    let (before, after) = word_changes(
                        code_text(&lines[shown[old].line].text),
                        code_text(&lines[shown[new].line].text),
                    );
                    shown[old].highlights = before;
                    shown[new].highlights = after;
                }
                split.push(SplitRow::Sides { old, new });
            }
            if old_note.is_some() || new_note.is_some() {
                split.push(SplitRow::Sides {
                    old: old_note,
                    new: new_note,
                });
            }
        }
    }
    split
}

fn code_text(line: &str) -> &str {
    line.get(1..).unwrap_or_default()
}

fn word_changes(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_words: Vec<_> = old.split_word_bound_indices().collect();
    let new_words: Vec<_> = new.split_word_bound_indices().collect();
    let prefix = old_words
        .iter()
        .zip(&new_words)
        .take_while(|(a, b)| a.1 == b.1)
        .count();
    let suffix = old_words[prefix..]
        .iter()
        .rev()
        .zip(new_words[prefix..].iter().rev())
        .take_while(|(a, b)| a.1 == b.1)
        .count();
    let old_middle = &old_words[prefix..old_words.len() - suffix];
    let new_middle = &new_words[prefix..new_words.len() - suffix];
    let (mut before, mut after) = (Vec::new(), Vec::new());

    const MAX_CELLS: usize = 4096;
    let cols = new_middle.len() + 1;
    let cells = (old_middle.len() + 1) * cols;
    if cells > MAX_CELLS || old_middle.is_empty() || new_middle.is_empty() {
        before.extend(token_range(old_middle));
        after.extend(token_range(new_middle));
        return (before, after);
    }

    let mut common = vec![0u16; cells];
    for (i, &(_, old_word)) in old_middle.iter().enumerate().rev() {
        for (j, &(_, new_word)) in new_middle.iter().enumerate().rev() {
            let cell = i * cols + j;
            common[cell] = if old_word == new_word {
                common[cell + cols + 1] + 1
            } else {
                common[cell + 1].max(common[cell + cols])
            };
        }
    }
    let (mut i, mut j, mut old_start, mut new_start) = (0, 0, 0, 0);
    while i < old_middle.len() && j < new_middle.len() {
        if old_middle[i].1 == new_middle[j].1 {
            mark(&mut before, old, &old_middle[old_start..i]);
            mark(&mut after, new, &new_middle[new_start..j]);
            (i, j, old_start, new_start) = (i + 1, j + 1, i + 1, j + 1);
        } else if common[i * cols + j + 1] > common[(i + 1) * cols + j] {
            j += 1;
        } else {
            i += 1;
        }
    }
    mark(&mut before, old, &old_middle[old_start..]);
    mark(&mut after, new, &new_middle[new_start..]);
    (before, after)
}

fn token_range(tokens: &[(usize, &str)]) -> Option<Range<usize>> {
    let start = tokens.first()?.0;
    let (end, last) = *tokens.last()?;
    Some(start..end + last.len())
}

fn mark(out: &mut Vec<Range<usize>>, text: &str, tokens: &[(usize, &str)]) {
    if let Some(range) = token_range(tokens) {
        if let Some(previous) = out.last_mut()
            && text[previous.end..range.start].trim().is_empty()
        {
            previous.end = range.end;
        } else {
            out.push(range);
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
    let listing_history = crate::inspection::showing_history(ui);
    let side = match &ui.selection {
        Some(Selection::File { staged, .. }) => Some(*staged),
        _ => None,
    };
    let committed_file = matches!(ui.selection, Some(Selection::Commit(_)))
        .then(|| ui.inspection.path.clone())
        .flatten();
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
            kit::fit(match &committed_file {
                Some(path) => path.to_string_lossy().into_owned(),
                None => heading,
            })
            .size(12.5)
            .weight(FontWeight::SEMIBOLD)
            .cut(kit::Cut::Start),
        )
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
            Selection::File { path, staged: true } => d.child(
                kit::btn("unstage-file", "Unstage file")
                    .small()
                    .enabled(editable)
                    .on(Action::UnstagePath(path), cx),
            ),
            _ => d,
        })
        .when_some(edit_button(ui, cx), |d, button| d.child(button))
        .when(committed_file.is_some(), |d| {
            d.child(crate::inspection::history_button(app, cx))
        })
        .when(!listing_history && !layout.unified.is_empty(), |d| {
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
        Some(Selection::Stash(oid)) => format!("stash-patch-{oid}"),
        _ => "patch".to_owned(),
    };
    let split = mode == Mode::Split;
    let code_width = if split {
        layout.widest_code.map(|line| {
            let text = code_text(&ui.diff[line].text);
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
        layout.unified.len()
    };
    let lines = uniform_list(
        SharedString::from(format!("{list_id}-{mode:?}")),
        count,
        cx.processor(move |app, rows: Range<usize>, _, cx| {
            let ui = app.ui();
            let editable = ui.can_edit_index();
            let layout = &ui.diff_layout;
            rows
                .filter_map(|index| {
                    let row = if split {
                        match layout.split.get(index)? {
                            SplitRow::Shared(row) => layout.unified.get(*row)?,
                            SplitRow::Sides { old, new } => {
                                let at = new.or(*old).and_then(|row| layout.unified.get(row)).map_or(0, |row| row.at);
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
                        layout.unified.get(index)?
                    };
                    let line = ui.diff.get(row.line)?;
                    Some(match row.kind {
                        Kind::Hunk => hunk_row(line, side, editable, split, cx),
                        Kind::File => {
                            let path = line.text.rsplit_once(" b/")
                                .map_or(line.text.as_str(), |(_, path)| path);
                            base_row()
                                .pl_3()
                                .bg(rgb(PANEL))
                                .border_b_1()
                                .border_color(rgb(LINE))
                                .child(kit::fit(path.to_owned()).size(12.5).weight(FontWeight::SEMIBOLD))
                        }
                        Kind::Note | Kind::NoNewline => base_row()
                            .pl(if split { px(12.) } else { px(96.) })
                            .child(note(&line.text)),
                        _ => line_menu(
                            base_row()
                                .when_some(line_background(row.kind), |d, bg| d.bg(rgb(bg)))
                                .child(number(row.old, 40.))
                                .child(number(row.new, 40.))
                                .child(marker(row.kind))
                                .child(code(row, line)),
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
    let empty = layout.unified.is_empty();
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
        .when(!listing_history && empty && ui.loading_diff, |d| {
            d.child(
                div()
                    .p_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kit::spinner("diff-loading"))
                    .child(kit::muted("Loading diff…")),
            )
        })
        .when(!listing_history && empty && !ui.loading_diff, |d| {
            d.child(
                div()
                    .p_4()
                    .child(kit::muted("No text changes to show for this selection.")),
            )
        })
        .when(!listing_history && !empty, |d| {
            d.when(split, |d| d.child(split_headings())).child(lines)
        })
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
    let Some(row) = side.and_then(|i| ui.diff_layout.unified.get(i)) else {
        return cell.bg(rgb(PANEL));
    };
    let line = &ui.diff[row.line];
    if row.kind == Kind::NoNewline {
        return cell.pl_3().child(note(&line.text));
    }
    let mut text = div()
        .id((if old { "split-old" } else { "split-new" }, index))
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .track_scroll(&ui.split_pan)
        .child(code(row, line).when_some(width, |d, width| d.min_w(width)));
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

fn number(number: u32, width: f32) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .pr_2()
        .flex()
        .justify_end()
        .font_family(MONO)
        .text_size(px(TEXT_TINY))
        .text_color(rgb(TEXT_OFF))
        .when(number > 0, |d| d.child(number.to_string()))
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

fn code(row: &Shown, line: &DiffLine) -> Div {
    let text = code_text(&line.text).to_owned();
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
            row.highlights.iter().cloned().map(|span| (span, highlight)),
        ))
}

fn note(text: &str) -> impl IntoElement {
    kit::fit(text.trim_start_matches("\\ ").to_owned())
        .size(TEXT_SMALL)
        .color(TEXT_3)
}

fn hunk_row<T: Desk>(
    line: &DiffLine,
    side: Option<bool>,
    editable: bool,
    split: bool,
    cx: &Context<T>,
) -> Div {
    let text = line.text.as_str();
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
    row.when_some(line.hunk.zip(side), |d, (hunk, staged)| {
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

    fn patch(text: &str) -> Vec<DiffLine> {
        text.lines()
            .map(|text| DiffLine { text: text.to_owned(), hunk: None })
            .collect()
    }

    fn marked(text: &str, spans: &[Range<usize>]) -> Vec<String> {
        spans.iter().map(|span| text[span.clone()].to_owned()).collect()
    }

    #[test]
    fn each_line_of_code_knows_where_it_is_in_the_new_file() {
        let layout = Layout::new(&patch(
            "diff --git a/trail.txt b/trail.txt\n--- a/trail.txt\n+++ b/trail.txt\n@@ -1,4 +1,4 @@\n first\n-ford\n+stepping stones\n summit\n@@ -9,2 +9,1 @@\n ninth\n-tenth",
        ));
        let at: Vec<(Kind, u32)> = layout.unified.iter().map(|row| (row.kind, row.at)).collect();
        assert_eq!(
            at,
            [
                (Kind::Hunk, 0),
                (Kind::Context, 1),
                (Kind::Deleted, 2),
                (Kind::Added, 2),
                (Kind::Context, 3),
                (Kind::Hunk, 0),
                (Kind::Context, 9),
                (Kind::Deleted, 10),
            ]
        );
    }

    #[test]
    fn a_staged_line_is_offered_only_while_the_working_tree_has_the_same_lines() {
        use crate::commands::Target;
        let row = |staged| super::super::FileRow { path: "trail.txt".into(), staged, status: "M" };
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
    fn highlights_disjoint_changes_at_unicode_word_boundaries() {
        let old = "Length: café via 石 gate";
        let new = "Length: cafés via 木 gate";
        let (before, after) = word_changes(old, new);
        assert_eq!(marked(old, &before), ["café", "石"]);
        assert_eq!(marked(new, &after), ["cafés", "木"]);

        let old = "Waypoint: Willow";
        let new = "Waypoint: Willow (seasonal)";
        let (before, after) = word_changes(old, new);
        assert!(before.is_empty());
        assert_eq!(marked(new, &after), [" (seasonal)"]);
    }

    #[test]
    fn split_aligns_context_and_pairs_changed_rows_within_each_hunk() {
        let mut lines = patch(concat!(
            "diff --git a/trail b/trail\nindex 012..345 100644\n--- a/trail\n+++ b/trail\n",
            "@@ -4,3 +4,4 @@ section\n here\n-Old hut\n+New hut\n+New trail\n end\n",
            "@@ -20 +21,0 @@\n-deleted only\n",
        ));
        lines[4].hunk = Some(1);
        lines[10].hunk = Some(2);
        let layout = Layout::new(&lines);
        assert_eq!(layout.split, [
            SplitRow::Shared(0),
            SplitRow::Sides { old: Some(1), new: Some(1) },
            SplitRow::Sides { old: Some(2), new: Some(3) },
            SplitRow::Sides { old: None, new: Some(4) },
            SplitRow::Sides { old: Some(5), new: Some(5) },
            SplitRow::Shared(6),
            SplitRow::Sides { old: Some(7), new: None },
        ]);
        assert_eq!(lines[layout.unified[0].line].hunk, Some(1));
        assert_eq!(lines[layout.unified[6].line].hunk, Some(2));
        assert_eq!((layout.unified[5].old, layout.unified[5].new), (6, 7));
        assert_eq!(marked("Old hut", &layout.unified[2].highlights), ["Old"]);
        assert_eq!(marked("New hut", &layout.unified[3].highlights), ["New"]);
        assert!(layout.unified[4].highlights.is_empty());
    }

    #[test]
    fn split_keeps_newline_annotations_and_file_boundaries() {
        let lines = patch(concat!(
            "diff --git a/trail b/trail\n@@ -1 +1 @@\n-Old hut\n",
            "\\ No newline at end of file\n+New hut\n\\ No newline at end of file\n",
            "diff --git a/map b/map\n@@ -0,0 +1 @@\n+map\n\n[Preview limited to 1 MiB]\n",
        ));
        let layout = Layout::new(&lines);
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
        assert_eq!(layout.unified[9].kind, Kind::Note);
        assert_eq!(layout.unified[8].new, 1);
    }
}
