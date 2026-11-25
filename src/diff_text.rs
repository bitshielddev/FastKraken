use crate::{
    Tab, kit,
    commands::{Availability, Command, Group, on_repo},
    notice::Notice,
    preferences,
    settings::{self, Settings},
    theme::*,
    view::{self, CommitInput, Desk, DiffMode, Tone, Ui},
};
use gpui::{
    App, Context, Div, Entity, Focusable, Pixels, SharedString, StyledText, TextSystem, Window, div, prelude::*, px,
    rgb,
};
use std::{borrow::Cow, ops::Range, sync::Arc};

pub(crate) const SMALLEST: u32 = 9;
pub(crate) const LARGEST: u32 = 24;
pub(crate) const SIZE: u32 = 12;
pub(crate) const NARROWEST_TAB: u32 = 1;
pub(crate) const WIDEST_TAB: u32 = 8;
pub(crate) const TAB: u32 = 4;
const SHORTEST_LINE: f32 = 18.;
const READING: &str = "Looking for the fonts on this Mac…";

const KNOWN_MONOSPACED: &[&str] = &[
    "Menlo",
    "Monaco",
    "SF Mono",
    "Courier New",
    "Andale Mono",
    "PT Mono",
    "JetBrains Mono",
    "Fira Code",
    "Fira Mono",
    "Source Code Pro",
    "IBM Plex Mono",
    "Cascadia Code",
    "Cascadia Mono",
    "Hack",
    "Inconsolata",
    "Roboto Mono",
    "Ubuntu Mono",
    "DejaVu Sans Mono",
    "Iosevka",
    "Berkeley Mono",
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Face {
    pub(crate) family: SharedString,
    pub(crate) size: u32,
    pub(crate) tab: usize,
    pub(crate) numbers: bool,
}

impl Default for Face {
    fn default() -> Self {
        Self::of(&Settings::default())
    }
}

impl Face {
    pub(crate) fn of(settings: &Settings) -> Self {
        let family = settings
            .diff_font
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .map_or(SharedString::new_static(MONO), |family| SharedString::from(family.to_owned()));
        Self {
            family,
            size: settings.diff_font_size.unwrap_or(SIZE).clamp(SMALLEST, LARGEST),
            tab: settings.tab_width.unwrap_or(TAB).clamp(NARROWEST_TAB, WIDEST_TAB) as usize,
            numbers: settings.line_numbers,
        }
    }

    pub(crate) fn text(&self) -> Pixels {
        px(self.size as f32)
    }

    pub(crate) fn small(&self) -> Pixels {
        px(self.size as f32 - 1.)
    }

    pub(crate) fn line(&self) -> f32 {
        (self.size as f32 * 5. / 3.).round().max(SHORTEST_LINE)
    }

    fn scaled(&self, width: f32) -> f32 {
        (width * self.size as f32 / SIZE as f32).round()
    }

    pub(crate) fn number(&self, split: bool) -> f32 {
        match (self.numbers, split) {
            (false, _) => 0.,
            (true, false) => self.scaled(40.),
            (true, true) => self.scaled(34.),
        }
    }

    pub(crate) fn marker(&self) -> f32 {
        self.scaled(16.)
    }

    pub(crate) fn unified_gutter(&self) -> f32 {
        self.number(false) * 2. + self.marker()
    }

    pub(crate) fn split_gutter(&self) -> f32 {
        self.number(true) + self.marker()
    }
}

pub(crate) fn expanded(text: &str, tab: usize) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace('\t', &" ".repeat(tab.max(1))))
}

pub(crate) fn shown_at(text: &str, at: usize, tab: usize) -> usize {
    let at = at.min(text.len());
    at + text.as_bytes()[..at].iter().filter(|byte| **byte == b'\t').count() * (tab.max(1) - 1)
}

pub(crate) fn stored_at(text: &str, at: usize, tab: usize, nearest: bool) -> usize {
    let tab = tab.max(1);
    let mut shown = 0;
    for (index, c) in text.char_indices() {
        let width = if c == '\t' { tab } else { c.len_utf8() };
        if at < shown + width {
            let inside = at - shown;
            let after = c == '\t' && inside > 0 && nearest && inside * 2 >= tab;
            return if after { index + 1 } else { index };
        }
        shown += width;
    }
    text.len()
}

pub(crate) fn shown_runs<S>(text: &str, runs: Vec<(Range<usize>, S)>, tab: usize) -> Vec<(Range<usize>, S)> {
    if tab == 1 || !text.contains('\t') {
        return runs;
    }
    runs.into_iter()
        .map(|(range, style)| (shown_at(text, range.start, tab)..shown_at(text, range.end, tab), style))
        .collect()
}

pub(crate) fn checked(
    typed: &str,
    installed: Option<&[String]>,
    monospaced: impl FnOnce(&str) -> bool,
) -> Result<Option<String>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    let Some(installed) = installed else { return Err(READING.to_owned()) };
    let Some(name) = installed.iter().find(|name| name.eq_ignore_ascii_case(typed)) else {
        return Err(format!("No font named “{typed}” is installed on this Mac."));
    };
    if !monospaced(name) {
        return Err(format!("{name} is not monospaced. Diffs need a font whose characters are all one width."));
    }
    Ok(Some(name.clone()))
}

pub(crate) fn suggested(installed: &[String]) -> Vec<&'static str> {
    KNOWN_MONOSPACED
        .iter()
        .copied()
        .filter(|known| installed.iter().any(|name| name.eq_ignore_ascii_case(known)))
        .collect()
}

pub(crate) fn monospaced(system: &TextSystem, family: &str) -> bool {
    let font = system.resolve_font(&gpui::font(family.to_owned()));
    let size = px(SIZE as f32);
    let widths: Vec<Option<f32>> = ['i', 'W', 'm', '.', '0', ' ']
        .into_iter()
        .map(|c| system.advance(font, size, c).ok().map(|advance| f32::from(advance.width)))
        .collect();
    let Some(Some(first)) = widths.first().copied() else { return false };
    first > 0. && widths.iter().all(|width| width.is_some_and(|width| (width - first).abs() < 0.01))
}

pub(crate) fn size_text(size: u32) -> String {
    format!("{size} pt")
}

pub(crate) fn tab_text(tab: u32) -> String {
    match tab {
        1 => "1 space".to_owned(),
        n => format!("{n} spaces"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Font(String),
    Size(u32),
    Tab(u32),
    ToggleNumbers,
}

pub(crate) struct State {
    pub(crate) face: Face,
    pub(crate) font: Entity<CommitInput>,
    installed: Option<Arc<Vec<String>>>,
    reading: bool,
    problem: Option<String>,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let chosen = settings::settings(cx);
        let saved = chosen.diff_font.clone().unwrap_or_default();
        let font = cx.new(|cx| {
            let mut field = CommitInput::with_placeholder(cx, MONO);
            field.set_text(&saved, cx);
            field
        });
        cx.observe(&font, |tab, _, cx| typed(tab, cx)).detach();
        Self { face: Face::of(&chosen), font, installed: None, reading: false, problem: None }
    }

    pub(crate) fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub(crate) fn installed(&self) -> Option<&[String]> {
        self.installed.as_deref().map(Vec::as_slice)
    }
}

pub(crate) fn follow(tab: &mut Tab, chosen: &Settings) {
    let face = Face::of(chosen);
    if face != tab.ui.diff_text.face {
        tab.ui.restyle(face);
    }
}

pub(crate) fn opened(tab: &mut Tab, cx: &mut Context<Tab>) {
    let saved = settings::settings(cx).diff_font.unwrap_or_default();
    tab.ui.diff_text.problem = None;
    tab.ui.diff_text.font.update(cx, |field, cx| field.set_text(&saved, cx));
}

pub(crate) fn look(tab: &mut Tab, cx: &mut Context<Tab>) {
    check(tab, cx);
    if tab.ui.diff_text.reading {
        return;
    }
    tab.ui.diff_text.reading = true;
    let system = cx.text_system().clone();
    Tab::work(cx, move || system.all_font_names(), |tab, names, cx| {
        tab.ui.diff_text.reading = false;
        tab.ui.diff_text.installed = Some(Arc::new(names));
        check(tab, cx);
        cx.notify();
    });
}

fn typed(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.preferences.shows_diff() {
        check(tab, cx);
    }
    cx.notify();
}

fn check(tab: &mut Tab, cx: &mut Context<Tab>) {
    let typed = tab.ui.diff_text.font.read(cx).text().to_owned();
    let system = cx.text_system().clone();
    let verdict = checked(&typed, tab.ui.diff_text.installed(), |name| monospaced(&system, name));
    match verdict {
        Ok(family) => {
            tab.ui.diff_text.problem = None;
            if settings::settings(cx).diff_font != family {
                Settings::update(cx, |s| s.diff_font = family);
                follow(tab, &settings::settings(cx));
            }
        }
        Err(problem) => tab.ui.diff_text.problem = Some(problem),
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Font(name) => {
            if tab.ui.diff_text.installed.is_none() {
                tab.ui.diff_text.installed = Some(Arc::new(cx.text_system().all_font_names()));
            }
            tab.ui.diff_text.font.update(cx, |field, cx| field.set_text(&name, cx));
            check(tab, cx);
        }
        Action::Size(size) => {
            let size = size.clamp(SMALLEST, LARGEST);
            Settings::update(cx, |s| s.diff_font_size = Some(size));
        }
        Action::Tab(width) => {
            let width = width.clamp(NARROWEST_TAB, WIDEST_TAB);
            Settings::update(cx, |s| s.tab_width = Some(width));
        }
        Action::ToggleNumbers => {
            let on = !tab.ui.diff_text.face.numbers;
            Settings::update(cx, |s| s.line_numbers = on);
            if !tab.ui.preferences.open() {
                let title = if on { "Diffs show line numbers" } else { "Diffs hide line numbers" };
                tab.ui.notices.show(Notice::new(Tone::Info, title, ""));
            }
        }
    }
    follow(tab, &settings::settings(cx));
    cx.notify();
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    !ui.preferences.shows_diff() && ui.diff_text.font.focus_handle(cx).is_focused(window)
}

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "view.line-numbers",
    Group::View,
    |ui, _| if ui.diff_text.face.numbers { "Hide line numbers in diffs" } else { "Show line numbers in diffs" }.into(),
    |ui, target, _| if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden },
    |_, _| Some(view::Action::DiffText(Action::ToggleNumbers)),
)
.words(&["gutter", "numbers", "line numbers"])];

fn act(action: Action) -> view::Action {
    view::Action::DiffText(action)
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let chosen = settings::settings(cx);
    let state = &ui.diff_text;
    let face = &state.face;
    let size = face.size;
    let tab = face.tab as u32;
    let help: SharedString = match state.problem() {
        Some(problem) => problem.to_owned().into(),
        None => format!("Any monospaced font on this Mac. Leave it empty for {MONO}.").into(),
    };
    let mut picks = div().pt_1().pb_2().flex().flex_wrap().items_center().gap_x_3().gap_y_1();
    if let Some(installed) = state.installed() {
        let known = suggested(installed);
        if !known.is_empty() {
            picks = picks.child(kit::muted("Installed:"));
            for (index, name) in known.into_iter().enumerate() {
                picks = picks.child(kit::link(("preferences-diff-font-pick", index), name, act(Action::Font(name.to_owned())), cx));
            }
        }
    }
    let layout = |label: &'static str, index: usize, mode: DiffMode| {
        kit::segment(("preferences-diff-layout", index), label, None, chosen.diff_mode == mode, view::Action::DiffMode(mode), cx)
    };
    let context = |label: &'static str, index: usize, whole: bool| {
        use crate::diff_options::{Action as Show, Choice};
        kit::segment(
            ("preferences-diff-context", index),
            label,
            None,
            chosen.whole_file == whole,
            view::Action::DiffOptions(Show::Set(Choice::WholeFile, whole)),
            cx,
        )
    };
    div()
        .flex()
        .flex_col()
        .child(preferences::group("TEXT"))
        .child(preferences::setting(
            "Font",
            Some(help),
            div().w(px(200.)).child(state.font.clone()),
        ))
        .child(picks)
        .child(preferences::setting(
            "Font size",
            Some("Rows grow with the text.".into()),
            kit::stepper(
                "preferences-diff-size",
                size_text(size),
                (size > SMALLEST).then(|| act(Action::Size(size - 1))),
                (size < LARGEST).then(|| act(Action::Size(size + 1))),
                cx,
            ),
        ))
        .child(preferences::setting(
            "Tab width",
            Some("A tab is drawn as this many spaces. Copying still gives back the tab.".into()),
            kit::stepper(
                "preferences-diff-tab",
                tab_text(tab),
                (tab > NARROWEST_TAB).then(|| act(Action::Tab(tab - 1))),
                (tab < WIDEST_TAB).then(|| act(Action::Tab(tab + 1))),
                cx,
            ),
        ))
        .child(preferences::tick(
            "preferences-line-numbers",
            face.numbers,
            "Show line numbers",
            "Old and new line numbers beside each row of a diff, and the line number in the file view.",
            act(Action::ToggleNumbers),
            cx,
        ))
        .child(preferences::tick(
            "preferences-colour-code",
            chosen.colour_code,
            "Colour code by language",
            "Keywords, strings, comments and other code take colours from the file’s language. Files over 512 KB stay plain.",
            view::Action::Syntax(crate::syntax::Action::Toggle),
            cx,
        ))
        .child(div().pt_3().child(preview(face)))
        .child(preferences::group("WHEN A DIFF OPENS"))
        .child(preferences::setting(
            "Layout",
            Some("The Unified and Split buttons above a diff change it too.".into()),
            kit::segmented().child(layout("Unified", 0, DiffMode::Unified)).child(layout("Split", 1, DiffMode::Split)),
        ))
        .child(preferences::setting(
            "Context",
            Some("Show 3 lines around each change, or the whole file. The diff header changes it too.".into()),
            kit::segmented().child(context("3 lines", 0, false)).child(context("Whole file", 1, true)),
        ))
        .child(preferences::tick(
            "preferences-wrap-lines",
            chosen.wrap_lines,
            "Wrap long lines",
            "Long lines continue on the next row instead of scrolling sideways. The Wrap button above a diff changes it too.",
            view::Action::DiffOptions(crate::diff_options::Action::Set(crate::diff_options::Choice::Wrap, !chosen.wrap_lines)),
            cx,
        ))
}

const SAMPLE: [(Option<u32>, Option<u32>, char, &str); 3] = [
    (Some(7), Some(7), ' ', "fn trail(map: &Map) -> usize {"),
    (Some(8), None, '-', "\tmap.steps().len()"),
    (None, Some(8), '+', "\tmap.steps().count()"),
];

fn preview(face: &Face) -> Div {
    let number = |value: Option<u32>| {
        div()
            .w(px(face.number(false)))
            .flex_shrink_0()
            .pr_2()
            .flex()
            .justify_end()
            .text_size(face.small())
            .text_color(rgb(TEXT_OFF))
            .when_some(value, |d, value| d.child(value.to_string()))
    };
    let mut rows = div()
        .w_full()
        .py_1()
        .flex()
        .flex_col()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(CANVAS))
        .overflow_hidden()
        .font_family(face.family.clone());
    for (old, new, mark, code) in SAMPLE {
        let (background, ink) = match mark {
            '-' => (Some(DELETED_BG), DELETED),
            '+' => (Some(ADDED_BG), ADDED),
            _ => (None, TEXT_3),
        };
        rows = rows.child(
            div()
                .h(px(face.line()))
                .flex()
                .items_center()
                .when_some(background, |d, background| d.bg(rgb(background)))
                .when(face.numbers, |d| d.child(number(old)).child(number(new)))
                .child(
                    div()
                        .w(px(face.marker()))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .text_size(face.text())
                        .text_color(rgb(ink))
                        .child(match mark {
                            '-' => "−",
                            '+' => "+",
                            _ => "",
                        }),
                )
                .child(
                    div()
                        .whitespace_nowrap()
                        .text_size(face.text())
                        .text_color(rgb(TEXT))
                        .child(StyledText::new(expanded(code, face.tab).into_owned())),
                ),
        );
    }
    div().child(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(size: Option<u32>, tab: Option<u32>, numbers: bool) -> Face {
        Face::of(&Settings { diff_font_size: size, tab_width: tab, line_numbers: numbers, ..Settings::default() })
    }

    #[test]
    fn the_default_face_matches_the_fixed_diff_text_it_replaces() {
        let face = Face::default();
        assert_eq!(face.family.as_ref(), MONO);
        assert_eq!((face.size, face.tab, face.numbers), (12, 4, true));
        assert_eq!(face.line(), DIFF_LINE);
        assert_eq!(face.text(), px(TEXT_SMALL));
        assert_eq!(face.small(), px(TEXT_TINY));
        assert_eq!((face.unified_gutter(), face.split_gutter()), (96., 50.));
        assert_eq!((face.number(false), face.number(true), face.marker()), (40., 34., 16.));
    }

    #[test]
    fn sizes_and_tab_widths_are_kept_in_range_and_rows_grow_with_the_text() {
        assert_eq!(face(Some(3), Some(0), true).size, SMALLEST);
        assert_eq!(face(Some(3), Some(0), true).tab, 1);
        assert_eq!(face(Some(99), Some(40), true).size, LARGEST);
        assert_eq!(face(Some(99), Some(40), true).tab, 8);
        assert_eq!(face(Some(9), None, true).line(), SHORTEST_LINE);
        assert_eq!(face(Some(15), None, true).line(), 25.);
        assert_eq!(face(Some(24), None, true).line(), 40.);
        let large = face(Some(18), None, true);
        assert_eq!((large.number(false), large.number(true), large.marker()), (60., 51., 24.));
        assert_eq!(large.unified_gutter(), 144.);
        let named = Face::of(&Settings { diff_font: Some("  Monaco ".into()), ..Settings::default() });
        assert_eq!(named.family.as_ref(), "Monaco");
        let blank = Face::of(&Settings { diff_font: Some("   ".into()), ..Settings::default() });
        assert_eq!(blank.family.as_ref(), MONO);
    }

    #[test]
    fn hiding_line_numbers_leaves_only_the_change_mark_in_the_gutter() {
        let hidden = face(None, None, false);
        assert_eq!((hidden.number(false), hidden.number(true)), (0., 0.));
        assert_eq!((hidden.unified_gutter(), hidden.split_gutter()), (16., 16.));
    }

    #[test]
    fn tabs_are_drawn_as_spaces_and_offsets_convert_both_ways() {
        let code = "\tif a\t{ é }";
        assert_eq!(expanded(code, 4), "    if a    { é }");
        assert_eq!(expanded(code, 1), " if a { é }");
        assert_eq!(expanded(code, 0), " if a { é }");
        assert!(matches!(expanded("no tabs", 4), Cow::Borrowed(_)));
        assert_eq!(shown_at(code, 0, 4), 0);
        assert_eq!(shown_at(code, 1, 4), 4);
        assert_eq!(shown_at(code, 5, 4), 8);
        assert_eq!(shown_at(code, 6, 4), 12);
        assert_eq!(shown_at(code, code.len(), 4), expanded(code, 4).len());
        assert_eq!(shown_at(code, 99, 4), expanded(code, 4).len());
        for at in 0..=code.len() {
            if code.is_char_boundary(at) {
                assert_eq!(stored_at(code, shown_at(code, at, 4), 4, true), at);
                assert_eq!(stored_at(code, shown_at(code, at, 4), 4, false), at);
            }
        }
        assert_eq!(stored_at(code, 1, 4, true), 0);
        assert_eq!(stored_at(code, 2, 4, true), 1);
        assert_eq!(stored_at(code, 3, 4, true), 1);
        assert_eq!(stored_at(code, 3, 4, false), 0);
        assert_eq!(stored_at(code, 999, 4, true), code.len());
        assert_eq!(stored_at(code, 3, 1, true), 3);
    }

    #[test]
    fn highlight_runs_move_with_the_spaces_a_tab_becomes() {
        let code = "\tx\ty";
        let runs = shown_runs(code, vec![(0..1, 'a'), (1..2, 'b'), (2..4, 'c')], 3);
        assert_eq!(runs, [(0..3, 'a'), (3..4, 'b'), (4..8, 'c')]);
        let shown = expanded(code, 3);
        assert_eq!(&shown[4..8], "   y");
        assert_eq!(shown_runs("plain", vec![(1..3, 'a')], 8), [(1..3, 'a')]);
        assert_eq!(shown_runs(code, vec![(1..2, 'b')], 1), [(1..2, 'b')]);
        assert_eq!(&expanded(code, 1)[1..2], "x");
    }

    #[test]
    fn a_typed_font_must_be_installed_and_monospaced() {
        let installed = vec!["Helvetica".to_owned(), "Menlo".to_owned(), "SF Mono".to_owned()];
        let mono = |name: &str| name != "Helvetica";
        assert_eq!(checked("", Some(&installed), mono), Ok(None));
        assert_eq!(checked("  ", None, mono), Ok(None));
        assert_eq!(checked("sf mono ", Some(&installed), mono), Ok(Some("SF Mono".to_owned())));
        assert_eq!(checked("Menlo", None, mono), Err(READING.to_owned()));
        assert_eq!(
            checked("Comic", Some(&installed), mono),
            Err("No font named “Comic” is installed on this Mac.".to_owned())
        );
        assert_eq!(
            checked("helvetica", Some(&installed), mono),
            Err("Helvetica is not monospaced. Diffs need a font whose characters are all one width.".to_owned())
        );
    }

    #[test]
    fn suggestions_name_only_installed_monospaced_fonts() {
        let installed = vec!["Arial".to_owned(), "menlo".to_owned(), "Monaco".to_owned(), "Fira Code".to_owned()];
        assert_eq!(suggested(&installed), ["Menlo", "Monaco", "Fira Code"]);
        assert!(suggested(&[]).is_empty());
    }

    #[test]
    fn sizes_and_widths_read_as_words() {
        assert_eq!(size_text(13), "13 pt");
        assert_eq!(tab_text(1), "1 space");
        assert_eq!(tab_text(4), "4 spaces");
    }
}
