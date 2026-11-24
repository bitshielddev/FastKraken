use gpui::Keystroke;
use std::{
    collections::VecDeque,
    ops::Range,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const UNDO_LIMIT: usize = 200;
pub(crate) const TYPING_PAUSE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    ParagraphStart,
    ParagraphEnd,
    Start,
    End,
}

impl Motion {
    fn backward(self) -> bool {
        matches!(self, Self::Left | Self::WordLeft | Self::LineStart | Self::ParagraphStart | Self::Start)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Move(Motion),
    Extend(Motion),
    Delete(Motion),
    DeleteToParagraphEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    Typing,
    Deleting,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unit {
    Character,
    Word,
    Paragraph,
}

impl Unit {
    pub(crate) fn of_clicks(count: usize) -> Self {
        match count {
            0 | 1 => Self::Character,
            2 => Self::Word,
            _ => Self::Paragraph,
        }
    }
}

pub(crate) fn command(keystroke: &Keystroke) -> Option<Command> {
    let held = &keystroke.modifiers;
    let key = keystroke.key.as_str();
    let motion = match (held.platform, held.alt, held.control) {
        (false, false, false) => match key {
            "left" => Motion::Left,
            "right" => Motion::Right,
            "home" => Motion::ParagraphStart,
            "end" => Motion::ParagraphEnd,
            "backspace" => return Some(Command::Delete(Motion::Left)),
            "delete" => return Some(Command::Delete(Motion::Right)),
            _ => return None,
        },
        (true, false, false) => match key {
            "left" => Motion::LineStart,
            "right" => Motion::LineEnd,
            "up" => Motion::Start,
            "down" => Motion::End,
            "backspace" => return Some(Command::Delete(Motion::LineStart)),
            "delete" => return Some(Command::Delete(Motion::LineEnd)),
            _ => return None,
        },
        (false, true, false) => match key {
            "left" => Motion::WordLeft,
            "right" => Motion::WordRight,
            "backspace" => return Some(Command::Delete(Motion::WordLeft)),
            "delete" => return Some(Command::Delete(Motion::WordRight)),
            _ => return None,
        },
        (false, false, true) => match key {
            "a" => Motion::ParagraphStart,
            "e" => Motion::ParagraphEnd,
            "k" if !held.shift => return Some(Command::DeleteToParagraphEnd),
            _ => return None,
        },
        _ => return None,
    };
    Some(if held.shift { Command::Extend(motion) } else { Command::Move(motion) })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    value: String,
    anchor: usize,
    caret: usize,
}

#[derive(Default)]
struct History {
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    open: Option<(Edit, Instant)>,
}

impl History {
    fn record(&mut self, before: Snapshot, edit: Edit, inserted: &str, now: Instant, composing: bool) {
        let joins = composing
            || matches!(self.open, Some((open, last))
                if open == edit && edit != Edit::Other && now.saturating_duration_since(last) < TYPING_PAUSE);
        if !joins {
            if self.undo.len() == UNDO_LIMIT {
                self.undo.pop_front();
            }
            self.undo.push_back(before);
        }
        self.redo.clear();
        let ends_word = inserted.chars().next_back().is_some_and(char::is_whitespace);
        self.open = (edit != Edit::Other && !ends_word).then_some((edit, now));
    }
}

pub(crate) struct Editor {
    pub(crate) value: String,
    pub(crate) anchor: usize,
    pub(crate) caret: usize,
    pub(crate) multiline: bool,
    pub(crate) masked: bool,
    history: History,
}

impl Editor {
    pub(crate) fn new(multiline: bool, masked: bool) -> Self {
        Self { value: String::new(), anchor: 0, caret: 0, multiline, masked, history: History::default() }
    }

    pub(crate) fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    pub(crate) fn normalized(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    pub(crate) fn reset(&mut self, text: &str) {
        self.value = self.normalized(text);
        self.caret = self.value.len();
        self.anchor = self.caret;
        self.history = History::default();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { value: self.value.clone(), anchor: self.anchor, caret: self.caret }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.value = snapshot.value;
        self.anchor = snapshot.anchor.min(self.value.len());
        self.caret = snapshot.caret.min(self.value.len());
    }

    pub(crate) fn replace(&mut self, range: Range<usize>, text: &str, edit: Edit, now: Instant, composing: bool) -> usize {
        let text = self.normalized(text);
        if !(range.is_empty() && text.is_empty()) {
            let before = self.snapshot();
            self.history.record(before, edit, &text, now, composing);
            self.value.replace_range(range.clone(), &text);
        }
        self.caret = range.start + text.len();
        self.anchor = self.caret;
        text.len()
    }

    pub(crate) fn select(&mut self, anchor: usize, caret: usize) {
        self.anchor = anchor.min(self.value.len());
        self.caret = caret.min(self.value.len());
        self.history.open = None;
    }

    pub(crate) fn select_all(&mut self) {
        self.select(0, self.value.len());
    }

    #[cfg(test)]
    pub(crate) fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) -> bool {
        let Some(earlier) = self.history.undo.pop_back() else { return false };
        self.history.redo.push(self.snapshot());
        self.history.open = None;
        self.restore(earlier);
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        let Some(later) = self.history.redo.pop() else { return false };
        self.history.undo.push_back(self.snapshot());
        self.history.open = None;
        self.restore(later);
        true
    }

    pub(crate) fn delete_selection(&mut self, now: Instant) -> bool {
        let selected = self.selection();
        if selected.is_empty() {
            return false;
        }
        self.replace(selected, "", Edit::Other, now, false);
        true
    }

    pub(crate) fn apply(&mut self, command: Command, rows: Option<&[Range<usize>]>, now: Instant) {
        let selected = self.selection();
        match command {
            Command::Move(motion) => {
                let to = match motion {
                    Motion::Left if !selected.is_empty() => selected.start,
                    Motion::Right if !selected.is_empty() => selected.end,
                    _ if selected.is_empty() => self.target(motion, self.caret, rows),
                    _ if motion.backward() => self.target(motion, selected.start, rows),
                    _ => self.target(motion, selected.end, rows),
                };
                self.select(to, to);
            }
            Command::Extend(motion) => {
                let to = self.target(motion, self.caret, rows);
                self.select(self.anchor, to);
            }
            Command::Delete(motion) => {
                if !self.delete_selection(now) {
                    let to = self.target(motion, self.caret, rows);
                    let edit = if matches!(motion, Motion::Left | Motion::Right) { Edit::Deleting } else { Edit::Other };
                    self.replace(to.min(self.caret)..to.max(self.caret), "", edit, now, false);
                }
            }
            Command::DeleteToParagraphEnd => {
                if !self.delete_selection(now) {
                    let end = paragraph_end(&self.value, self.caret);
                    let end = if end == self.caret { next_grapheme(&self.value, end) } else { end };
                    self.replace(self.caret..end, "", Edit::Other, now, false);
                }
            }
        }
    }

    pub(crate) fn target(&self, motion: Motion, from: usize, rows: Option<&[Range<usize>]>) -> usize {
        let text = self.value.as_str();
        let rows = rows.filter(|rows| !rows.is_empty());
        match motion {
            Motion::Left => previous_grapheme(text, from),
            Motion::Right => next_grapheme(text, from),
            Motion::WordLeft if self.masked => 0,
            Motion::WordRight if self.masked => text.len(),
            Motion::WordLeft => word_left(text, from),
            Motion::WordRight => word_right(text, from),
            Motion::LineStart => rows.map_or_else(|| paragraph_start(text, from), |rows| rows[row_of(rows, from)].start),
            Motion::LineEnd => rows.map_or_else(|| paragraph_end(text, from), |rows| row_end(text, rows, from)),
            Motion::ParagraphStart => paragraph_start(text, from),
            Motion::ParagraphEnd => paragraph_end(text, from),
            Motion::Start => 0,
            Motion::End => text.len(),
        }
    }

    pub(crate) fn unit_at(&self, index: usize, unit: Unit) -> Range<usize> {
        let text = self.value.as_str();
        let index = index.min(text.len());
        match unit {
            Unit::Character => index..index,
            Unit::Word if self.masked => 0..text.len(),
            Unit::Word => {
                let mut last = index..index;
                for (start, word) in text.split_word_bound_indices() {
                    last = start..start + word.len();
                    if index < last.end {
                        break;
                    }
                }
                last
            }
            Unit::Paragraph => paragraph_start(text, index)..paragraph_end(text, index),
        }
    }

    pub(crate) fn dragged(&self, origin: &Range<usize>, unit: Unit, index: usize) -> (usize, usize) {
        let reached = self.unit_at(index, unit);
        if reached.start < origin.start {
            (origin.end, reached.start)
        } else {
            (origin.start, reached.end.max(origin.end))
        }
    }
}

pub(crate) fn previous_grapheme(text: &str, from: usize) -> usize {
    text[..from].grapheme_indices(true).next_back().map_or(0, |(at, _)| at)
}

pub(crate) fn next_grapheme(text: &str, from: usize) -> usize {
    from + text[from..].graphemes(true).next().map_or(0, str::len)
}

fn words(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    text.split_word_bound_indices()
        .filter(|(_, word)| word.chars().any(char::is_alphanumeric))
        .map(|(start, word)| start..start + word.len())
}

fn word_left(text: &str, from: usize) -> usize {
    words(text).take_while(|word| word.start < from).last().map_or(0, |word| word.start)
}

fn word_right(text: &str, from: usize) -> usize {
    words(text).find(|word| word.end > from).map_or(text.len(), |word| word.end)
}

fn paragraph_start(text: &str, from: usize) -> usize {
    text[..from].rfind('\n').map_or(0, |at| at + 1)
}

fn paragraph_end(text: &str, from: usize) -> usize {
    text[from..].find('\n').map_or(text.len(), |at| from + at)
}

fn row_of(rows: &[Range<usize>], index: usize) -> usize {
    rows.iter().rposition(|row| row.start <= index).unwrap_or(0)
}

fn row_end(text: &str, rows: &[Range<usize>], from: usize) -> usize {
    let at = row_of(rows, from);
    let row = &rows[at];
    let wrapped = rows.get(at + 1).is_some_and(|next| next.start == row.end);
    match text[row.clone()].chars().next_back() {
        Some(last) if wrapped && last.is_whitespace() => row.end - last.len_utf8(),
        _ => row.end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "Fix the café's menu, again";

    fn editor(text: &str, caret: usize) -> Editor {
        let mut editor = Editor::new(false, false);
        editor.reset(text);
        editor.select(caret, caret);
        editor
    }

    fn press(editor: &mut Editor, keys: &str, now: Instant) {
        for key in keys.split_whitespace() {
            let stroke = Keystroke::parse(key).unwrap();
            let command = command(&stroke).unwrap_or_else(|| panic!("{key} is not a text command"));
            editor.apply(command, None, now);
        }
    }

    fn typed(editor: &mut Editor, text: &str, now: Instant) {
        for ch in text.chars() {
            let selected = editor.selection();
            editor.replace(selected, &ch.to_string(), Edit::Typing, now, false);
        }
    }

    fn shown(editor: &Editor) -> String {
        let mut shown = editor.value.clone();
        let selected = editor.selection();
        if selected.is_empty() {
            shown.insert(editor.caret, '|');
        } else {
            shown.insert(selected.end, ']');
            shown.insert(selected.start, '[');
        }
        shown
    }

    #[test]
    fn option_arrows_move_by_word_over_spaces_punctuation_and_accents() {
        let now = Instant::now();
        let mut field = editor(TEXT, TEXT.len());
        let mut seen = Vec::new();
        for _ in 0..6 {
            press(&mut field, "alt-left", now);
            seen.push(shown(&field));
        }
        assert_eq!(
            seen,
            [
                "Fix the café's menu, |again",
                "Fix the café's |menu, again",
                "Fix the |café's menu, again",
                "Fix |the café's menu, again",
                "|Fix the café's menu, again",
                "|Fix the café's menu, again",
            ]
        );
        seen.clear();
        for _ in 0..6 {
            press(&mut field, "alt-right", now);
            seen.push(shown(&field));
        }
        assert_eq!(
            seen,
            [
                "Fix| the café's menu, again",
                "Fix the| café's menu, again",
                "Fix the café's| menu, again",
                "Fix the café's menu|, again",
                "Fix the café's menu, again|",
                "Fix the café's menu, again|",
            ]
        );
        let mut field = editor(TEXT, "Fix the ca".len());
        press(&mut field, "alt-right", now);
        assert_eq!(shown(&field), "Fix the café's| menu, again");
        press(&mut field, "alt-left", now);
        assert_eq!(shown(&field), "Fix the |café's menu, again");
    }

    #[test]
    fn shift_extends_every_motion_and_a_plain_motion_leaves_the_selection_from_its_edge() {
        let now = Instant::now();
        let mut field = editor(TEXT, "Fix the ".len());
        press(&mut field, "alt-shift-right alt-shift-right", now);
        assert_eq!(shown(&field), "Fix the [café's menu], again");
        press(&mut field, "alt-left", now);
        assert_eq!(shown(&field), "Fix |the café's menu, again");
        let mut field = editor(TEXT, "Fix the ".len());
        press(&mut field, "alt-shift-right", now);
        press(&mut field, "alt-right", now);
        assert_eq!(shown(&field), "Fix the café's menu|, again");
        let mut field = editor(TEXT, "Fix".len());
        press(&mut field, "cmd-shift-right", now);
        assert_eq!(shown(&field), "Fix[ the café's menu, again]");
        press(&mut field, "cmd-shift-left", now);
        assert_eq!(shown(&field), "[Fix] the café's menu, again");
        let mut field = editor(TEXT, 4);
        press(&mut field, "shift-right shift-right", now);
        assert_eq!(shown(&field), "Fix [th]e café's menu, again");
        press(&mut field, "left", now);
        assert_eq!(shown(&field), "Fix |the café's menu, again");
        press(&mut field, "ctrl-shift-e", now);
        assert_eq!(shown(&field), "Fix [the café's menu, again]");
        press(&mut field, "cmd-shift-up", now);
        assert_eq!(shown(&field), "[Fix ]the café's menu, again");
    }

    #[test]
    fn command_and_control_keys_reach_the_ends_of_lines_and_of_the_text() {
        let now = Instant::now();
        let text = "Summary line\nsecond café line\nthird";
        let mut field = Editor::new(true, false);
        field.reset(text);
        field.select(18, 18);
        press(&mut field, "cmd-left", now);
        assert_eq!(field.caret, 13);
        press(&mut field, "cmd-right", now);
        assert_eq!(field.caret, 30);
        press(&mut field, "ctrl-a", now);
        assert_eq!(field.caret, 13);
        press(&mut field, "ctrl-e", now);
        assert_eq!(field.caret, 30);
        press(&mut field, "cmd-up", now);
        assert_eq!(field.caret, 0);
        press(&mut field, "cmd-down", now);
        assert_eq!(field.caret, text.len());
        press(&mut field, "home", now);
        assert_eq!(field.caret, 31);
    }

    #[test]
    fn command_arrows_follow_wrapped_rows_in_a_description() {
        let text = "one two three\nfour";
        let mut field = Editor::new(true, false);
        field.reset(text);
        let rows = [0..8, 8..13, 14..18];
        field.select(2, 2);
        field.apply(Command::Move(Motion::LineEnd), Some(&rows), Instant::now());
        assert_eq!(field.caret, 7);
        field.apply(Command::Move(Motion::LineEnd), Some(&rows), Instant::now());
        assert_eq!(field.caret, 7);
        field.select(10, 10);
        field.apply(Command::Move(Motion::LineStart), Some(&rows), Instant::now());
        assert_eq!(field.caret, 8);
        field.apply(Command::Move(Motion::LineEnd), Some(&rows), Instant::now());
        assert_eq!(field.caret, 13);
        field.apply(Command::Delete(Motion::LineStart), Some(&rows), Instant::now());
        assert_eq!(field.value, "one two \nfour");
    }

    #[test]
    fn option_and_command_deletes_remove_words_and_lines() {
        let now = Instant::now();
        let mut field = editor(TEXT, "Fix the café's menu".len());
        press(&mut field, "alt-backspace", now);
        assert_eq!(shown(&field), "Fix the café's |, again");
        press(&mut field, "alt-backspace", now);
        assert_eq!(shown(&field), "Fix the |, again");
        press(&mut field, "alt-delete", now);
        assert_eq!(shown(&field), "Fix the |");
        press(&mut field, "alt-left alt-delete", now);
        assert_eq!(shown(&field), "Fix | ");
        press(&mut field, "cmd-backspace", now);
        assert_eq!(shown(&field), "| ");
        let mut field = editor(TEXT, "Fix the ".len());
        press(&mut field, "cmd-delete", now);
        assert_eq!(shown(&field), "Fix the |");
        let mut field = editor("café crème", 3);
        press(&mut field, "delete", now);
        assert_eq!(shown(&field), "caf| crème");
        press(&mut field, "backspace", now);
        assert_eq!(shown(&field), "ca| crème");
        let mut field = editor(TEXT, 4);
        press(&mut field, "alt-shift-right", now);
        press(&mut field, "alt-backspace", now);
        assert_eq!(shown(&field), "Fix | café's menu, again");
        let mut field = editor("", 0);
        press(&mut field, "alt-backspace backspace cmd-backspace ctrl-k", now);
        assert_eq!(shown(&field), "|");
        assert!(!field.can_undo());
    }

    #[test]
    fn control_k_deletes_to_the_end_of_the_paragraph_then_joins_the_next() {
        let now = Instant::now();
        let mut field = Editor::new(true, false);
        field.reset("first line\nsecond");
        field.select(5, 5);
        press(&mut field, "ctrl-k", now);
        assert_eq!(shown(&field), "first|\nsecond");
        press(&mut field, "ctrl-k", now);
        assert_eq!(shown(&field), "first|second");
        press(&mut field, "ctrl-e ctrl-k", now);
        assert_eq!(shown(&field), "firstsecond|");
    }

    #[test]
    fn keys_without_a_text_meaning_are_left_for_the_window() {
        for key in ["cmd-enter", "cmd-shift-m", "alt-o", "alt-t", "ctrl-tab", "cmd-z", "a", "alt-e", "up", "ctrl-shift-k", "alt-cmd-left"] {
            assert_eq!(command(&Keystroke::parse(key).unwrap()), None, "{key}");
        }
        assert_eq!(command(&Keystroke::parse("shift-backspace").unwrap()), Some(Command::Delete(Motion::Left)));
    }

    #[test]
    fn a_secret_field_moves_and_deletes_words_as_one() {
        let now = Instant::now();
        let mut field = Editor::new(false, true);
        field.reset("pass word.é");
        press(&mut field, "alt-left", now);
        assert_eq!(field.caret, 0);
        press(&mut field, "alt-right", now);
        assert_eq!(field.caret, field.value.len());
        press(&mut field, "alt-backspace", now);
        assert_eq!(field.value, "");
        field.reset("pass word");
        assert_eq!(field.unit_at(2, Unit::Word), 0..9);
    }

    #[test]
    fn typing_joins_into_one_undo_until_a_space_a_pause_or_a_move() {
        let start = Instant::now();
        let mut field = editor("", 0);
        typed(&mut field, "Fix the", start);
        assert_eq!(field.value, "Fix the");
        assert!(field.undo());
        assert_eq!(shown(&field), "Fix |");
        assert!(field.undo());
        assert_eq!(shown(&field), "|");
        assert!(!field.undo());
        assert!(field.redo() && field.redo());
        assert_eq!(shown(&field), "Fix the|");
        assert!(!field.redo());

        let mut field = editor("", 0);
        typed(&mut field, "caf", start);
        typed(&mut field, "é", start + TYPING_PAUSE);
        typed(&mut field, "s", start + TYPING_PAUSE + Duration::from_millis(900));
        assert!(field.undo());
        assert_eq!(field.value, "caf");
        assert!(field.undo());
        assert_eq!(field.value, "");

        let mut field = editor("", 0);
        typed(&mut field, "ab", start);
        field.apply(Command::Move(Motion::Left), None, start);
        typed(&mut field, "x", start);
        assert_eq!(field.value, "axb");
        assert!(field.undo());
        assert_eq!(shown(&field), "a|b");
        assert!(field.undo());
        assert_eq!(shown(&field), "|");
    }

    #[test]
    fn deleting_characters_joins_but_word_deletes_and_new_edits_stand_alone() {
        let now = Instant::now();
        let mut field = editor("Fix the menu", 12);
        press(&mut field, "backspace backspace", now);
        typed(&mut field, "ow", now);
        press(&mut field, "alt-backspace", now);
        assert_eq!(field.value, "Fix the ");
        assert!(field.undo());
        assert_eq!(field.value, "Fix the meow");
        assert!(field.undo());
        assert_eq!(field.value, "Fix the me");
        assert!(field.undo());
        assert_eq!(shown(&field), "Fix the menu|");
        assert!(field.redo());
        typed(&mut field, "!", now);
        assert!(!field.can_redo());
        assert_eq!(field.value, "Fix the me!");
    }

    #[test]
    fn a_paste_is_one_step_of_its_own() {
        let now = Instant::now();
        let mut field = editor("", 0);
        typed(&mut field, "abc", now);
        let selected = field.selection();
        field.replace(selected, " def\nghi", Edit::Other, now, false);
        typed(&mut field, "j", now);
        assert_eq!(field.value, "abc def ghij");
        assert!(field.undo());
        assert_eq!(shown(&field), "abc def ghi|");
        assert!(field.undo());
        assert_eq!(shown(&field), "abc|");
        assert!(field.redo());
        assert_eq!(shown(&field), "abc def ghi|");
        field.select(0, 3);
        field.replace(field.selection(), "", Edit::Other, now, false);
        assert!(field.undo());
        assert_eq!(shown(&field), "[abc] def ghi");
    }

    #[test]
    fn composing_text_joins_one_step_and_reset_forgets_every_step() {
        let start = Instant::now();
        let mut field = editor("", 0);
        field.replace(0..0, "k", Edit::Typing, start, false);
        field.replace(0..1, "か", Edit::Typing, start + Duration::from_secs(3), true);
        field.replace(0.."か".len(), "漢字", Edit::Typing, start + Duration::from_secs(6), true);
        assert!(field.undo());
        assert_eq!(field.value, "");
        assert!(!field.undo());
        typed(&mut field, "draft", start);
        field.reset("amend message");
        assert!(!field.can_undo() && !field.can_redo());
        assert_eq!(shown(&field), "amend message|");
    }

    #[test]
    fn the_history_keeps_the_latest_two_hundred_steps() {
        let start = Instant::now();
        let mut field = editor("", 0);
        for step in 0..250 {
            let selected = field.selection();
            field.replace(selected, "x", Edit::Other, start + Duration::from_secs(step), false);
        }
        let mut undone = 0;
        while field.undo() {
            undone += 1;
        }
        assert_eq!(undone, UNDO_LIMIT);
        assert_eq!(field.value, "x".repeat(50));
    }

    #[test]
    fn clicks_pick_words_and_paragraphs_and_dragging_extends_by_the_same_unit() {
        let mut field = Editor::new(true, false);
        field.reset("Fix the café's menu, again\nsecond line");
        let at = |text: &str| text.len();
        assert_eq!(field.unit_at(at("Fix the ca"), Unit::Word), at("Fix the ")..at("Fix the café's"));
        assert_eq!(field.unit_at(at("Fix"), Unit::Word), at("Fix")..at("Fix "));
        assert_eq!(field.unit_at(at("Fix the café's menu"), Unit::Word), at("Fix the café's menu")..at("Fix the café's menu,"));
        assert_eq!(field.unit_at(field.value.len(), Unit::Word), at("Fix the café's menu, again\nsecond ")..field.value.len());
        assert_eq!(field.unit_at(4, Unit::Paragraph), 0..at("Fix the café's menu, again"));
        assert_eq!(field.unit_at(30, Unit::Paragraph), at("Fix the café's menu, again\n")..field.value.len());
        assert_eq!(field.unit_at(5, Unit::Character), 5..5);

        let origin = field.unit_at(at("Fix the ca"), Unit::Word);
        assert_eq!(field.dragged(&origin, Unit::Word, at("Fix the café's me")), (at("Fix the "), at("Fix the café's menu")));
        assert_eq!(field.dragged(&origin, Unit::Word, 1), (at("Fix the café's"), 0));
        assert_eq!(field.dragged(&origin, Unit::Word, at("Fix the caf")), (at("Fix the "), at("Fix the café's")));
        let origin = field.unit_at(2, Unit::Paragraph);
        assert_eq!(field.dragged(&origin, Unit::Paragraph, 30), (0, field.value.len()));
        assert_eq!(field.dragged(&(6..6), Unit::Character, 2), (6, 2));
        assert_eq!(Unit::of_clicks(1), Unit::Character);
        assert_eq!(Unit::of_clicks(2), Unit::Word);
        assert_eq!(Unit::of_clicks(3), Unit::Paragraph);
        assert_eq!(Unit::of_clicks(4), Unit::Paragraph);
    }
}
