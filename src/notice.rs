use crate::{
    Tab, kit,
    theme::*,
    view::{self, Desk},
};
use gpui::{Context, FontWeight, SharedString, Window, deferred, div, prelude::*, px, rgb};
use std::time::{Duration, Instant};

const SHOWN: usize = 4;
const FADE: Duration = Duration::from_secs(5);
const BRIEF: Duration = Duration::from_secs(2);
const WITH_OUTPUT: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Info,
    Progress,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Dismiss(u64),
    ToggleOutput(u64),
    Take(u64, usize),
}

pub(crate) type Offer = (SharedString, view::Action);

#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub id: u64,
    pub tone: Tone,
    pub title: String,
    pub detail: String,
    pub output: Option<String>,
    pub actions: Vec<Offer>,
    pub bar: Option<u8>,
    pub job: Option<u64>,
    pub log: Option<u64>,
    pub count: usize,
    pub expanded: bool,
    lasts: Option<Duration>,
    expires: Option<Instant>,
    changed: u64,
}

impl Notice {
    pub(crate) fn new(tone: Tone, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: 0,
            tone,
            title: title.into(),
            detail: detail.into(),
            output: None,
            actions: Vec::new(),
            bar: None,
            job: None,
            log: None,
            count: 1,
            expanded: false,
            lasts: matches!(tone, Tone::Success | Tone::Info).then_some(FADE),
            expires: None,
            changed: 0,
        }
    }

    pub(crate) fn from_text(tone: Tone, text: &str) -> Self {
        let (title, detail) = parts(text);
        let mut notice = Self::new(tone, title, detail);
        if tone == Tone::Error && text.trim().contains('\n') {
            let raw = match text.split_once(" — ") {
                Some((title, rest)) if !title.contains('\n') => rest,
                _ => text,
            };
            notice.output = Some(raw.trim().to_owned());
        }
        notice
    }

    pub(crate) fn copied(what: &str) -> Self {
        Self::new(Tone::Info, format!("Copied {what}"), "").lasting(BRIEF)
    }

    pub(crate) fn offer(mut self, label: impl Into<SharedString>, action: view::Action) -> Self {
        self.actions.push((label.into(), action));
        self
    }

    pub(crate) fn logged(mut self, entry: Option<u64>) -> Self {
        self.log = entry;
        self
    }

    pub(crate) fn lasting(mut self, time: Duration) -> Self {
        self.lasts = Some(time);
        self
    }

    pub(crate) fn sticky(mut self) -> Self {
        self.lasts = None;
        self
    }

    fn same(&self, other: &Self) -> bool {
        self.tone == other.tone
            && self.title == other.title
            && self.detail == other.detail
            && self.output == other.output
    }

    pub(crate) fn text(&self) -> String {
        if self.detail.is_empty() {
            self.title.clone()
        } else {
            format!("{} — {}", self.title, self.detail)
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub title: String,
    pub detail: String,
    pub actions: Vec<Offer>,
    pub sticky: bool,
    pub output: Option<String>,
}

impl From<()> for Outcome {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

impl Outcome {
    pub(crate) fn detail(detail: impl Into<String>) -> Self {
        Self { detail: detail.into(), ..Self::default() }
    }

    pub(crate) fn titled(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { title: title.into(), detail: detail.into(), ..Self::default() }
    }

    pub(crate) fn offer(mut self, label: impl Into<SharedString>, action: view::Action) -> Self {
        self.actions.push((label.into(), action));
        self
    }

    pub(crate) fn sticky(mut self) -> Self {
        self.sticky = true;
        self
    }

    pub(crate) fn output(mut self, output: impl Into<String>) -> Self {
        let output = output.into();
        self.output = (!output.trim().is_empty()).then(|| output.trim().to_owned());
        self
    }

    pub(crate) fn notice(self) -> Notice {
        let mut shown = Notice::new(Tone::Success, self.title, self.detail);
        if self.output.is_some() {
            shown = shown.lasting(WITH_OUTPUT);
        }
        if self.sticky {
            shown = shown.sticky();
        }
        shown.actions = self.actions;
        shown.output = self.output;
        shown
    }
}

pub(crate) fn short(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

pub(crate) fn commits(count: usize) -> String {
    match count {
        1 => "1 commit".to_owned(),
        n => format!("{n} commits"),
    }
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub text: String,
    pub actions: Vec<Offer>,
    pub output: Option<String>,
    pub plain: Option<(String, String)>,
    pub tone: Tone,
    pub expanded: bool,
}

impl From<String> for Failure {
    fn from(text: String) -> Self {
        Self { text, actions: Vec::new(), output: None, plain: None, tone: Tone::Error, expanded: false }
    }
}

impl Failure {
    pub(crate) fn offer(mut self, label: impl Into<SharedString>, action: view::Action) -> Self {
        self.actions.push((label.into(), action));
        self
    }

    pub(crate) fn worded(mut self) -> Self {
        let notice = Notice::from_text(Tone::Error, &self.text);
        self.plain = Some((notice.title, notice.detail));
        if self.output.is_none() {
            self.output = notice.output;
        }
        self
    }

    pub(crate) fn output(mut self, output: impl Into<String>) -> Self {
        let output = output.into();
        self.output = (!output.trim().is_empty()).then(|| output.trim().to_owned());
        self
    }

    pub(crate) fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }

    pub(crate) fn notice(self) -> Notice {
        let mut notice = match self.plain {
            Some((title, detail)) => Notice::new(self.tone, title, detail),
            None => Notice::from_text(self.tone, &self.text),
        };
        notice.actions = self.actions;
        if self.output.is_some() {
            notice.output = self.output;
        }
        notice.expanded = self.expanded && notice.output.is_some();
        notice
    }
}

#[derive(Default)]
pub(crate) struct State {
    items: Vec<Notice>,
    next: u64,
    changes: u64,
    timer: Option<Instant>,
}

impl State {
    pub(crate) fn items(&self) -> &[Notice] {
        &self.items
    }

    pub(crate) fn get(&self, id: u64) -> Option<&Notice> {
        self.items.iter().find(|n| n.id == id)
    }

    pub(crate) fn newest(&self) -> Option<&Notice> {
        self.items.iter().max_by_key(|n| n.changed)
    }

    pub(crate) fn dismissable(&self) -> Option<u64> {
        self.items
            .iter()
            .filter(|n| n.tone != Tone::Progress)
            .max_by_key(|n| n.changed)
            .map(|n| n.id)
    }

    pub(crate) fn text(&self) -> String {
        self.newest().map(Notice::text).unwrap_or_default()
    }

    pub(crate) fn say(&mut self, tone: Tone, text: &str) -> u64 {
        self.show(Notice::from_text(tone, text))
    }

    pub(crate) fn show(&mut self, notice: Notice) -> u64 {
        self.show_at(notice, Instant::now())
    }

    pub(crate) fn progress(&mut self, title: impl Into<String>) -> u64 {
        self.show(Notice::new(Tone::Progress, title, ""))
    }

    pub(crate) fn progressed(
        &mut self,
        id: u64,
        title: String,
        detail: String,
        bar: Option<u8>,
        job: Option<u64>,
    ) -> bool {
        let Some(notice) = self.items.iter_mut().find(|n| n.id == id && n.tone == Tone::Progress) else {
            return false;
        };
        if (&notice.title, &notice.detail, notice.bar, notice.job) == (&title, &detail, bar, job) {
            return false;
        }
        (notice.title, notice.detail, notice.bar, notice.job) = (title, detail, bar, job);
        true
    }

    pub(crate) fn finish(&mut self, id: u64, notice: Notice) -> u64 {
        self.finish_at(id, notice, Instant::now())
    }

    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|n| n.id != id);
        self.items.len() != before
    }

    fn touch(&mut self, index: usize, now: Instant) {
        self.changes += 1;
        let notice = &mut self.items[index];
        notice.changed = self.changes;
        notice.expires = notice.lasts.map(|time| now + time);
    }

    fn collapse(&mut self, notice: &mut Notice, except: Option<u64>, now: Instant) -> Option<u64> {
        if notice.tone == Tone::Progress {
            return None;
        }
        let index = self
            .items
            .iter()
            .position(|n| Some(n.id) != except && n.same(notice))?;
        let existing = &mut self.items[index];
        existing.count += 1;
        existing.actions = std::mem::take(&mut notice.actions);
        existing.log = notice.log;
        existing.lasts = notice.lasts;
        let id = existing.id;
        self.touch(index, now);
        Some(id)
    }

    fn show_at(&mut self, mut notice: Notice, now: Instant) -> u64 {
        if let Some(id) = self.collapse(&mut notice, None, now) {
            return id;
        }
        self.next += 1;
        notice.id = self.next;
        notice.count = 1;
        self.items.push(notice);
        self.touch(self.items.len() - 1, now);
        self.trim();
        self.next
    }

    fn finish_at(&mut self, id: u64, mut notice: Notice, now: Instant) -> u64 {
        let Some(index) = self.items.iter().position(|n| n.id == id) else {
            return self.show_at(notice, now);
        };
        if let Some(existing) = self.collapse(&mut notice, Some(id), now) {
            self.items.retain(|n| n.id != id);
            return existing;
        }
        notice.id = id;
        notice.count = 1;
        self.items[index] = notice;
        self.touch(index, now);
        id
    }

    fn trim(&mut self) {
        while self.items.len() > SHOWN {
            let older = &self.items[..self.items.len() - 1];
            let index = older
                .iter()
                .position(|n| !matches!(n.tone, Tone::Error | Tone::Progress))
                .or_else(|| older.iter().position(|n| n.tone != Tone::Error))
                .unwrap_or(0);
            self.items.remove(index);
        }
    }

    fn prune(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items.retain(|n| n.expires.is_none_or(|at| at > now));
        self.items.len() != before
    }

    fn next_expiry(&self) -> Option<Instant> {
        self.items.iter().filter_map(|n| n.expires).min()
    }

    fn take(&mut self, id: u64, index: usize) -> Option<view::Action> {
        let position = self.items.iter().position(|n| n.id == id)?;
        let action = self.items[position].actions.get(index)?.1.clone();
        self.items.remove(position);
        Some(action)
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Dismiss(id) => {
            tab.ui.notices.remove(id);
        }
        Action::ToggleOutput(id) => {
            if let Some(notice) = tab.ui.notices.items.iter_mut().find(|n| n.id == id) {
                notice.expanded = !notice.expanded;
                notice.lasts = None;
                notice.expires = None;
            }
        }
        Action::Take(_, _) if tab.ui.busy() || tab.ui.dialog_open() => {}
        Action::Take(id, index) => {
            let offered = tab.ui.notices.get(id).and_then(|n| n.actions.get(index)).map(|(_, action)| action);
            let refused = offered.and_then(|action| {
                if let Err(reason) = crate::operation::allows(tab.ui.operation.kind(), action) {
                    return Some(reason.to_owned());
                }
                tab.ui.jobs.refusal().filter(|_| crate::jobs::needs_network(action))
            });
            match refused {
                Some(reason) => {
                    tab.ui.notices.show(Notice::new(Tone::Info, reason, ""));
                }
                _ => {
                    if let Some(action) = tab.ui.notices.take(id, index) {
                        tab.dispatch(action, window, cx);
                    }
                }
            }
        }
    }
    cx.notify();
}

pub(crate) fn schedule(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.notices;
    let Some(due) = state.next_expiry() else {
        return;
    };
    if state.timer.is_some_and(|at| at <= due) {
        return;
    }
    state.timer = Some(due);
    cx.spawn(async move |tab, cx| {
        cx.background_executor()
            .timer(due.saturating_duration_since(Instant::now()))
            .await;
        let _ = tab.update(cx, |tab, cx| {
            let state = &mut tab.ui.notices;
            if state.timer == Some(due) {
                state.timer = None;
            }
            if state.prune(Instant::now()) {
                cx.notify();
            }
            schedule(tab, cx);
        });
    })
    .detach();
}

pub(crate) fn parts(text: &str) -> (String, String) {
    let clean = |line: &str| {
        line.trim()
            .trim_start_matches("fatal: ")
            .trim_start_matches("error: ")
            .to_owned()
    };
    let useful = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| !line.trim_start().starts_with("hint:"))
            .map(clean)
            .filter(|line| !line.is_empty())
            .collect()
    };
    if let Some((title, rest)) = text.split_once(" — ")
        && !title.contains('\n')
    {
        return (title.to_owned(), useful(rest).join(" "));
    }
    let mut lines = useful(text).into_iter();
    let title = lines.next().unwrap_or_default();
    (title, lines.collect::<Vec<_>>().join(" "))
}


pub(crate) fn output_label(notice: &Notice) -> &'static str {
    match (notice.tone, notice.expanded) {
        (Tone::Success, false) => "Show output",
        (Tone::Success, true) => "Hide output",
        (_, false) => "Show Git output",
        (_, true) => "Hide Git output",
    }
}

pub(crate) fn stack<T: Desk>(app: &T, cx: &mut Context<T>) -> Option<impl IntoElement + use<T>> {
    let busy = app.ui().busy() || app.ui().dialog_open();
    let items = app.ui().notices.items();
    if items.is_empty() {
        return None;
    }
    Some(
        deferred(
            div()
                .absolute()
                .bottom(px(14.))
                .right(px(14.))
                .w(px(360.))
                .flex()
                .flex_col()
                .gap_2()
                .children(items.iter().map(|notice| card(notice, busy, cx))),
        )
        .with_priority(4),
    )
}

fn card<T: Desk>(notice: &Notice, busy: bool, cx: &mut Context<T>) -> impl IntoElement + use<T> {
    let id = notice.id;
    let mark = match notice.tone {
        Tone::Progress => kit::any(kit::spinner(("notice-spinner", id))),
        Tone::Success => kit::any(kit::icon("ok").size(px(16.)).text_color(rgb(ADDED))),
        Tone::Error => kit::any(kit::icon("alert").size(px(16.)).text_color(rgb(DELETED))),
        Tone::Info => kit::any(kit::icon("commit").size(px(16.))),
    };
    let output = notice.output.clone();
    let buttons = !notice.actions.is_empty() || output.is_some() || notice.log.is_some();
    kit::float()
        .id(("notice", id))
        .w_full()
        .rounded(px(8.))
        .p(px(10.))
        .pl_3()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(div().pt(px(1.)).flex_shrink_0().child(mark))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(notice.title.clone()),
                        )
                        .when(notice.count > 1, |d| {
                            d.child(
                                div()
                                    .flex_shrink_0()
                                    .px(px(5.))
                                    .rounded(px(RADIUS_CHIP))
                                    .bg(rgb(RAISED))
                                    .text_size(px(TEXT_TINY))
                                    .text_color(rgb(TEXT_2))
                                    .child(format!("×{}", notice.count)),
                            )
                        }),
                )
                .when(!notice.detail.is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(rgb(TEXT_2))
                            .line_clamp(4)
                            .child(notice.detail.clone()),
                    )
                })
                .when_some(notice.bar.filter(|_| notice.tone == Tone::Progress), |d, percent| {
                    d.child(crate::jobs::bar(percent))
                })
                .when_some(notice.job.filter(|_| notice.tone == Tone::Progress), |d, job| {
                    d.child(div().pt_1().flex().child(crate::jobs::cancel_button(job, cx)))
                })
                .when_some(output.clone().filter(|_| notice.expanded), |d, raw| {
                    d.child(
                        div()
                            .id("notice-output")
                            .mt_1()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .p_2()
                            .rounded(px(RADIUS))
                            .border_1()
                            .border_color(rgb(LINE))
                            .bg(rgb(CANVAS))
                            .font_family(MONO)
                            .text_size(px(TEXT_TINY))
                            .text_color(rgb(TEXT_2))
                            .children(raw.lines().map(|line| div().child(line.to_owned()))),
                    )
                })
                .when(buttons, |d| {
                    let mut row = div().pt_1().flex().flex_wrap().gap_1();
                    for (index, (label, _)) in notice.actions.iter().enumerate() {
                        row = row.child(
                            kit::btn(("notice-action", index), label.clone())
                                .small()
                                .enabled(!busy)
                                .on(view::Action::Notice(Action::Take(id, index)), cx),
                        );
                    }
                    if let Some(entry) = notice.log {
                        row = row.child(
                            kit::btn("notice-log", "Show in activity log")
                                .small()
                                .ghost()
                                .on(view::Action::Activity(crate::activity::Action::Show(entry)), cx),
                        );
                    }
                    if let Some(raw) = output {
                        row = row.child(
                            kit::btn("notice-output-toggle", output_label(notice))
                            .small()
                            .ghost()
                            .on(view::Action::Notice(Action::ToggleOutput(id)), cx),
                        );
                        if notice.expanded {
                            row = row.child(
                                kit::btn("notice-copy", "Copy")
                                    .small()
                                    .ghost()
                                    .icon("copy")
                                    .on(view::Action::CopyText { text: raw, what: "Git output" }, cx),
                            );
                        }
                    }
                    d.child(row)
                }),
        )
        .when(notice.tone != Tone::Progress, |d| {
            d.child(kit::icon_btn(
                "notice-close",
                "x",
                "Dismiss",
                view::Action::Notice(Action::Dismiss(id)),
                cx,
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(state: &State) -> Vec<(Tone, &str, usize)> {
        state.items().iter().map(|n| (n.tone, n.title.as_str(), n.count)).collect()
    }

    #[test]
    fn a_notice_about_a_git_command_points_at_its_log_entry_and_a_repeat_at_the_newer() {
        let start = Instant::now();
        let mut state = State::default();
        let first = state.show_at(Notice::new(Tone::Error, "Push Failed", "rejected").logged(Some(7)), start);
        assert_eq!(state.get(first).unwrap().log, Some(7));
        let again = state.show_at(Notice::new(Tone::Error, "Push Failed", "rejected").logged(Some(9)), start);
        assert_eq!((again, state.get(first).unwrap().count, state.get(first).unwrap().log), (first, 2, Some(9)));
        assert_eq!(Notice::new(Tone::Info, "Copied SHA", "").log, None);
        let shown = Outcome::titled("Pushed main", "").output("remote: hello\n").notice();
        assert_eq!((shown.tone, shown.output.as_deref()), (Tone::Success, Some("remote: hello")));
    }

    #[test]
    fn a_hook_failure_opens_its_output_and_a_cancel_is_calm() {
        let failure = Failure::from("pre-commit stopped the commit".to_owned()).output("line 1\nline 200").expanded().notice();
        assert!(failure.expanded && failure.tone == Tone::Error);
        assert_eq!(output_label(&failure), "Hide Git output");
        assert!(!Failure::from("Nothing to show".to_owned()).expanded().notice().expanded);
        let calm = Failure { tone: Tone::Info, plain: Some(("Commit cancelled".into(), "No commit was made.".into())), ..Failure::from(String::new()) };
        let calm = calm.notice();
        assert_eq!((calm.tone, calm.lasts), (Tone::Info, Some(FADE)));
        let told = Outcome::titled("Changes committed", "Pitch the tent").output("lint: fine").notice();
        assert_eq!((output_label(&told), told.lasts), ("Show output", Some(WITH_OUTPUT)));
        assert_eq!(Outcome::titled("Changes committed", "").notice().lasts, Some(FADE));
    }

    #[test]
    fn outcome_helpers_name_commits_as_a_sentence_does() {
        assert_eq!(short("0123456789abcdef0123456789abcdef01234567"), "0123456");
        assert_eq!(short("abc"), "abc");
        assert_eq!(commits(0), "0 commits");
        assert_eq!(commits(1), "1 commit");
        assert_eq!(commits(12), "12 commits");
        let outcome = Outcome::titled("Deleted branch topic", "was 0123456");
        assert_eq!((outcome.title.as_str(), outcome.detail.as_str()), ("Deleted branch topic", "was 0123456"));
    }

    #[test]
    fn notices_lead_with_what_happened_and_drop_git_hints() {
        let raw = "Pull Failed — hint: Diverging branches can't be fast-forwarded\nhint:\nhint: git rebase\nfatal: Not possible to fast-forward, aborting.";
        assert_eq!(
            parts(raw),
            (
                "Pull Failed".to_owned(),
                "Not possible to fast-forward, aborting.".to_owned()
            )
        );
        assert_eq!(parts("Changes committed"), ("Changes committed".to_owned(), String::new()));
    }

    #[test]
    fn a_long_error_keeps_what_git_printed_hints_and_all() {
        let raw = "Pull Failed — hint: Diverging branches can't be fast-forwarded\nhint: git rebase\nfatal: Not possible to fast-forward, aborting.";
        let notice = Notice::from_text(Tone::Error, raw);
        assert_eq!(notice.title, "Pull Failed");
        assert_eq!(notice.detail, "Not possible to fast-forward, aborting.");
        let output = notice.output.as_deref().unwrap();
        assert!(output.starts_with("hint: Diverging branches"), "{output}");
        assert!(output.contains("hint: git rebase"));
        assert!(output.ends_with("fatal: Not possible to fast-forward, aborting."));
        assert!(Notice::from_text(Tone::Error, "Pull Failed — No upstream.").output.is_none());
        assert!(Notice::from_text(Tone::Success, "Done\nmore").output.is_none());
    }

    #[test]
    fn an_error_stays_when_a_success_arrives_after_it() {
        let start = Instant::now();
        let mut state = State::default();
        let error = state.show_at(Notice::from_text(Tone::Error, "Pull Failed — No upstream."), start);
        state.show_at(Notice::new(Tone::Success, "Changes stashed", ""), start);
        assert_eq!(
            titles(&state),
            [(Tone::Error, "Pull Failed", 1), (Tone::Success, "Changes stashed", 1)]
        );
        assert_eq!(state.text(), "Changes stashed");
        assert!(!state.prune(start + Duration::from_secs(4)));
        assert!(state.prune(start + FADE + Duration::from_millis(1)));
        assert_eq!(titles(&state), [(Tone::Error, "Pull Failed", 1)]);
        assert_eq!(state.text(), "Pull Failed — No upstream.");
        assert!(!state.prune(start + Duration::from_secs(3600)));
        assert!(state.remove(error));
        assert!(state.items().is_empty());
        assert_eq!(state.text(), "");
    }

    #[test]
    fn progress_is_replaced_in_place_by_its_outcome() {
        let start = Instant::now();
        let mut state = State::default();
        state.show_at(Notice::new(Tone::Info, "Copied SHA", ""), start);
        let progress = state.show_at(Notice::new(Tone::Progress, "Pushing to upstream…", ""), start);
        state.show_at(Notice::from_text(Tone::Error, "Merge conflict."), start);
        assert!(state.next_expiry().is_some());
        let outcome = state.finish_at(progress, Notice::new(Tone::Success, "Pushed Successfully", ""), start);
        assert_eq!(outcome, progress);
        assert_eq!(
            titles(&state),
            [
                (Tone::Info, "Copied SHA", 1),
                (Tone::Success, "Pushed Successfully", 1),
                (Tone::Error, "Merge conflict.", 1),
            ]
        );
        assert_eq!(state.text(), "Pushed Successfully");
        let gone = state.finish_at(99, Notice::new(Tone::Success, "Fetched Successfully", ""), start);
        assert_ne!(gone, 99);
        assert_eq!(state.items().last().unwrap().title, "Fetched Successfully");
    }

    #[test]
    fn identical_notices_collapse_with_a_count() {
        let start = Instant::now();
        let mut state = State::default();
        let first = state.show_at(Notice::copied("SHA"), start);
        let later = start + Duration::from_millis(1500);
        assert_eq!(state.show_at(Notice::copied("SHA"), later), first);
        assert_eq!(titles(&state), [(Tone::Info, "Copied SHA", 2)]);
        assert!(!state.prune(start + BRIEF + Duration::from_millis(1)));
        assert!(state.prune(later + BRIEF + Duration::from_millis(1)));
        let error = state.say(Tone::Error, "Pull Failed — No upstream.");
        let retry = state.progress("Pulling from upstream…");
        assert_eq!(state.finish(retry, Notice::from_text(Tone::Error, "Pull Failed — No upstream.")), error);
        assert_eq!(titles(&state), [(Tone::Error, "Pull Failed", 2)]);
        state.say(Tone::Error, "Pull Failed — Other.");
        assert_eq!(state.items().len(), 2);
    }

    #[test]
    fn a_fifth_notice_pushes_out_the_oldest_that_is_not_an_error() {
        let start = Instant::now();
        let mut state = State::default();
        state.show_at(Notice::from_text(Tone::Error, "First failure"), start);
        state.show_at(Notice::new(Tone::Progress, "Fetching all remotes…", ""), start);
        state.show_at(Notice::new(Tone::Success, "Tag created", ""), start);
        state.show_at(Notice::from_text(Tone::Error, "Second failure"), start);
        state.show_at(Notice::new(Tone::Info, "Copied name", ""), start);
        assert_eq!(
            titles(&state),
            [
                (Tone::Error, "First failure", 1),
                (Tone::Progress, "Fetching all remotes…", 1),
                (Tone::Error, "Second failure", 1),
                (Tone::Info, "Copied name", 1),
            ]
        );
        state.show_at(Notice::new(Tone::Info, "Copied SHA", ""), start);
        assert_eq!(state.items().len(), SHOWN);
        assert_eq!(state.items()[1].title, "Fetching all remotes…");
        assert_eq!(state.items()[3].title, "Copied SHA");
        state.show_at(Notice::from_text(Tone::Error, "Third failure"), start);
        state.show_at(Notice::from_text(Tone::Error, "Fourth failure"), start);
        state.show_at(Notice::from_text(Tone::Error, "Fifth failure"), start);
        let left: Vec<_> = state.items().iter().map(|n| n.title.as_str()).collect();
        assert_eq!(left, ["Second failure", "Third failure", "Fourth failure", "Fifth failure"]);
    }

    #[test]
    fn an_offered_step_closes_its_notice() {
        let mut state = State::default();
        let id = state.show(
            Notice::from_text(Tone::Error, "Push Failed — rejected")
                .offer("Pull", view::Action::Sync(crate::sync::Action::Pull)),
        );
        assert!(state.take(id, 1).is_none());
        assert!(matches!(
            state.take(id, 0),
            Some(view::Action::Sync(crate::sync::Action::Pull))
        ));
        assert!(state.get(id).is_none());
    }

    #[test]
    fn the_shim_keeps_the_strings_the_replays_compare() {
        for (tone, raw) in [
            (Tone::Success, "Pushed Successfully"),
            (Tone::Success, "Fetched Successfully"),
            (Tone::Success, "Pulled Successfully"),
            (Tone::Error, "Pull Failed — Not possible to fast-forward, aborting."),
            (Tone::Error, "Merge conflict. Open a conflicted file or abort the merge."),
            (Tone::Error, "Staged changes prevent applying this stash. Commit or unstage them first."),
            (Tone::Success, "main now tracks origin/main."),
            (Tone::Info, "The last repository, /tmp/Session, is no longer there. Choose another to open."),
        ] {
            let mut state = State::default();
            state.say(Tone::Progress, "Working…");
            state.say(tone, raw);
            assert_eq!(state.text(), raw);
        }
        let mut state = State::default();
        state.say(
            Tone::Error,
            "Pull Failed — hint: You have divergent branches\nfatal: Not possible to fast-forward, aborting.",
        );
        assert!(state.text().starts_with("Pull Failed"));
        assert_eq!(state.text(), "Pull Failed — Not possible to fast-forward, aborting.");
    }
}
