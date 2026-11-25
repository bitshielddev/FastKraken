use crate::{
    Tab,
    commands::{Availability, Command, Group, on_repo},
    repository::DiffOptions,
    scrollbar::Mark,
    settings::{self, Settings},
    theme::*,
    view::{self, DiffColumns, DiffMode, DiffWrapped, Selection, Ui},
};
use gpui::{Context, Pixels, ScrollStrategy, Window};

const RUN_LEAD: usize = 3;
const MARK_GAP: f32 = 0.002;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    WholeFile,
    IgnoreWhitespace,
    Wrap,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Set(Choice, bool),
    Next,
    Previous,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) whole_file: bool,
    pub(crate) ignore_whitespace: bool,
    pub(crate) wrap: bool,
    pub(crate) columns: DiffColumns,
    pub(crate) measured: Pixels,
    pub(crate) wrapped: Option<DiffWrapped>,
    pub(crate) jumped: Option<usize>,
}

impl State {
    pub(crate) fn new(settings: &Settings) -> Self {
        Self {
            whole_file: settings.whole_file,
            ignore_whitespace: settings.ignore_whitespace,
            wrap: settings.wrap_lines,
            ..Self::default()
        }
    }

    pub(crate) fn request(&self) -> DiffOptions {
        DiffOptions { whole_file: self.whole_file, ignore_whitespace: self.ignore_whitespace }
    }

    pub(crate) fn on(&self, choice: Choice) -> bool {
        match choice {
            Choice::WholeFile => self.whole_file,
            Choice::IgnoreWhitespace => self.ignore_whitespace,
            Choice::Wrap => self.wrap,
        }
    }

    fn set(&mut self, choice: Choice, on: bool) {
        match choice {
            Choice::WholeFile => self.whole_file = on,
            Choice::IgnoreWhitespace => self.ignore_whitespace = on,
            Choice::Wrap => self.wrap = on,
        }
    }
}

fn store(settings: &mut Settings, choice: Choice, on: bool) {
    match choice {
        Choice::WholeFile => settings.whole_file = on,
        Choice::IgnoreWhitespace => settings.ignore_whitespace = on,
        Choice::Wrap => settings.wrap_lines = on,
    }
}

pub(crate) fn showing(ui: &Ui) -> bool {
    view::centre(ui) == view::Centre::Diff && !ui.diff.lines.is_empty() && !crate::inspection::showing_history(ui)
}

pub(crate) fn resized(ui: &mut Ui, width: Pixels, columns: DiffColumns) -> bool {
    ui.diff_options.measured = width;
    if ui.diff_options.columns == columns {
        return false;
    }
    ui.diff_options.columns = columns;
    if !ui.diff_options.wrap {
        return false;
    }
    ui.rewrap();
    true
}

pub(crate) fn targets(ui: &Ui) -> Vec<usize> {
    let doc = &ui.diff;
    let rows: Vec<usize> = if doc.options.whole_file {
        view::diff_changes(ui).iter().map(|change| change.start).collect()
    } else {
        doc.hunks.iter().map(|hunk| hunk.start).collect()
    };
    let mut visual: Vec<usize> = rows.into_iter().filter_map(|row| view::diff_visual_of(ui, row)).collect();
    visual.dedup();
    visual
}

pub(crate) fn marks(ui: &Ui) -> Vec<Mark> {
    let total = view::diff_rows(ui);
    if total == 0 {
        return Vec::new();
    }
    let split = ui.diff_mode == DiffMode::Split;
    let mut marks: Vec<Mark> = Vec::new();
    for change in view::diff_changes(ui) {
        let sides = [(change.deleted, DELETED), (change.added, ADDED)];
        for (row, color) in sides.into_iter().filter_map(|(row, color)| Some((row?, color))) {
            let Some(visual) = view::diff_visual_of(ui, row) else { continue };
            let at = visual as f32 / total as f32;
            match marks.last_mut() {
                Some(last) if (at - last.at).abs() < MARK_GAP || (split && last.at == at) => {
                    if last.color != color {
                        last.color = MODIFIED;
                    }
                }
                _ => marks.push(Mark { at, color }),
            }
        }
    }
    marks
}

pub(crate) fn viewport(ui: &Ui) -> Option<(usize, usize)> {
    let state = ui.diff_scroll.0.borrow();
    let base = &state.base_handle;
    let height = f32::from(base.bounds().size.height);
    let top = (-f32::from(base.offset().y) / DIFF_LINE).max(0.).round() as usize;
    let shown = (height / DIFF_LINE).floor().max(1.) as usize;
    (height > 0.).then_some((top, shown))
}

fn pending(ui: &Ui) -> bool {
    ui.diff_scroll.0.borrow().deferred_scroll_to_item.is_some()
}

pub(crate) fn next(targets: &[usize], anchor: usize, forward: bool) -> Option<usize> {
    if forward {
        targets.iter().copied().find(|&target| target > anchor)
    } else {
        targets.iter().rev().copied().find(|&target| target < anchor)
    }
}

pub(crate) fn anchor(jumped: Option<usize>, view: Option<(usize, usize)>, pending: bool) -> usize {
    match (jumped, view) {
        (Some(jumped), _) if pending => jumped,
        (Some(jumped), Some((top, shown))) if jumped >= top && jumped < top + shown => jumped,
        (_, Some((top, _))) => top,
        (jumped, None) => jumped.unwrap_or(0),
    }
}

fn jump(tab: &mut Tab, forward: bool) -> bool {
    let ui = &tab.ui;
    if !showing(ui) {
        return false;
    }
    let targets = targets(ui);
    let from = anchor(ui.diff_options.jumped, viewport(ui), pending(ui));
    let Some(target) = next(&targets, from, forward) else { return false };
    let lead = if ui.diff.options.whole_file { RUN_LEAD } else { 0 };
    let lead = lead.min(target);
    tab.ui.diff_scroll.scroll_to_item_strict_with_offset(target, ScrollStrategy::Top, lead);
    tab.ui.diff_options.jumped = Some(target);
    true
}

fn reload(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(selection) = tab.ui.selection.clone().filter(|selection| *selection != Selection::Changes) else { return };
    let path = match &selection {
        Selection::Commit(_) => tab.ui.inspection.path.clone(),
        Selection::Stash(_) => tab.ui.stash.open_path().map(std::path::Path::to_path_buf),
        _ => None,
    };
    tab.load_selected_patch(selection, path, cx);
}

pub(crate) fn follow(tab: &mut Tab, chosen: &Settings, cx: &mut Context<Tab>) {
    let before = tab.ui.diff_options.request();
    let wrapped = tab.ui.diff_options.wrap;
    tab.ui.diff_options.whole_file = chosen.whole_file;
    tab.ui.diff_options.ignore_whitespace = chosen.ignore_whitespace;
    tab.ui.diff_options.wrap = chosen.wrap_lines;
    if tab.ui.diff_options.request() != before {
        reload(tab, cx);
    } else if wrapped != chosen.wrap_lines {
        tab.ui.rewrap();
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Set(choice, on) => {
            if tab.ui.diff_options.on(choice) == on {
                return;
            }
            tab.ui.diff_options.set(choice, on);
            settings::Settings::update(cx, |s| store(s, choice, on));
            match choice {
                Choice::Wrap => tab.ui.rewrap(),
                Choice::WholeFile | Choice::IgnoreWhitespace => reload(tab, cx),
            }
        }
        Action::Next | Action::Previous => {
            if !jump(tab, matches!(action, Action::Next)) {
                return;
            }
        }
    }
    cx.notify();
}

fn diff_open(ui: &Ui, target: &crate::commands::Target) -> Availability {
    if !on_repo(ui, target) {
        Availability::Hidden
    } else if showing(ui) {
        Availability::Enabled
    } else {
        Availability::Disabled("Open a diff first".to_owned())
    }
}

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "view.whole-file",
        Group::View,
        |ui, _| if ui.diff_options.whole_file { "Show only the changed hunks" } else { "Show the whole file in diffs" }.into(),
        |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
        |ui, _| Some(view::Action::DiffOptions(Action::Set(Choice::WholeFile, !ui.diff_options.whole_file))),
    )
    .words(&["context", "inline", "full file", "expand", "hunks"]),
    Command::new(
        "view.ignore-whitespace",
        Group::View,
        |ui, _| if ui.diff_options.ignore_whitespace { "Show whitespace changes" } else { "Ignore whitespace in diffs" }.into(),
        |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
        |ui, _| Some(view::Action::DiffOptions(Action::Set(Choice::IgnoreWhitespace, !ui.diff_options.ignore_whitespace))),
    )
    .words(&["whitespace", "spaces", "indentation", "trailing"]),
    Command::new(
        "view.wrap-lines",
        Group::View,
        |ui, _| if ui.diff_options.wrap { "Stop wrapping long lines" } else { "Wrap long lines in diffs" }.into(),
        |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
        |ui, _| Some(view::Action::DiffOptions(Action::Set(Choice::Wrap, !ui.diff_options.wrap))),
    )
    .words(&["word wrap", "soft wrap", "long lines"]),
    Command::new(
        "view.next-change",
        Group::View,
        |_, _| "Next change".into(),
        |ui, target, _| diff_open(ui, target),
        |_, _| Some(view::Action::DiffOptions(Action::Next)),
    )
    .words(&["hunk", "jump", "down"])
    .icon("chevron-down")
    .keys("alt-down"),
    Command::new(
        "view.previous-change",
        Group::View,
        |_, _| "Previous change".into(),
        |ui, target, _| diff_open(ui, target),
        |_, _| Some(view::Action::DiffOptions(Action::Previous)),
    )
    .words(&["hunk", "jump", "up"])
    .icon("chevron-up")
    .keys("alt-up"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_previous_move_from_the_last_jump_while_it_is_on_screen_and_from_the_top_otherwise() {
        let targets = [0, 12, 40, 41, 90];
        assert_eq!(next(&targets, anchor(None, Some((0, 30)), false), true), Some(12));
        assert_eq!(next(&targets, anchor(Some(12), Some((12, 30)), false), true), Some(40));
        assert_eq!(next(&targets, anchor(Some(90), Some((70, 30)), false), true), None);
        assert_eq!(next(&targets, anchor(Some(90), Some((70, 30)), false), false), Some(41));
        assert_eq!(next(&targets, anchor(Some(12), Some((60, 30)), false), true), Some(90));
        assert_eq!(next(&targets, anchor(Some(12), Some((60, 30)), false), false), Some(41));
        assert_eq!(next(&targets, anchor(Some(40), Some((0, 30)), true), true), Some(41));
        assert_eq!(next(&targets, anchor(None, None, false), false), None);
        assert_eq!(next(&[], 0, true), None);
    }

    #[test]
    fn each_choice_is_stored_in_its_own_setting() {
        let mut settings = Settings::default();
        for choice in [Choice::WholeFile, Choice::IgnoreWhitespace, Choice::Wrap] {
            store(&mut settings, choice, true);
            let state = State::new(&settings);
            assert!(state.on(choice));
        }
        let state = State::new(&settings);
        assert_eq!(state.request(), DiffOptions { whole_file: true, ignore_whitespace: true });
        assert!(!State::new(&Settings::default()).wrap);
    }
}
