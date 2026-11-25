use crate::{
    Tab, kit,
    jobs::Link,
    refs::Kind,
    repository::{Repository, checked, validate_oid},
    theme::*,
    view::{self, Desk},
};
use gpui::{Context, Div, div, prelude::*, px, rgb};
use std::{ffi::OsString, time::Duration};

const SETTLE: Duration = Duration::from_millis(120);
const NAMED: usize = 3;
const LISTED_MORE: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Holder {
    pub kind: Kind,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Containing {
    pub branches: Vec<Holder>,
    pub first_tag: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Action {
    Show(Kind, String),
}

#[derive(Default)]
pub(crate) struct State {
    oid: Option<String>,
    ticket: u64,
    link: Option<Link>,
    found: Option<Containing>,
}

impl State {
    pub(crate) fn found(&self, oid: &str) -> Option<&Containing> {
        self.found.as_ref().filter(|_| self.oid.as_deref() == Some(oid))
    }

    pub(crate) fn running(&self) -> bool {
        self.link.is_some()
    }

    fn stop(&mut self) {
        if let Some(link) = self.link.take() {
            link.cancel();
        }
        self.ticket += 1;
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Repository {
    pub(crate) fn containing(&self, oid: &str, current: Option<&str>) -> Result<Containing, String> {
        validate_oid(oid)?;
        let branches = checked(
            self.read_command()
                .args(["for-each-ref", "--format=%(refname)"])
                .arg(format!("--contains={oid}"))
                .args(["refs/heads", "refs/remotes"]),
        )?;
        let mut holders: Vec<Holder> = String::from_utf8_lossy(&branches)
            .lines()
            .filter_map(|full| {
                if let Some(name) = full.strip_prefix("refs/heads/") {
                    Some(Holder { kind: Kind::Branch, name: name.to_owned() })
                } else {
                    full.strip_prefix("refs/remotes/")
                        .filter(|name| !name.ends_with("/HEAD"))
                        .map(|name| Holder { kind: Kind::Remote, name: name.to_owned() })
                }
            })
            .collect();
        holders.sort_by_key(|holder| match holder.kind {
            Kind::Branch if Some(holder.name.as_str()) == current => 0,
            Kind::Branch => 1,
            _ => 2,
        });
        let tags = checked(
            self.read_command()
                .args(["for-each-ref", "--format=%(refname)", "--sort=creatordate", "--count=1"])
                .arg(format!("--contains={oid}"))
                .arg("refs/tags"),
        )?;
        let first_tag = String::from_utf8_lossy(&tags)
            .lines()
            .next()
            .and_then(|full| full.strip_prefix("refs/tags/"))
            .map(str::to_owned);
        Ok(Containing { branches: holders, first_tag })
    }
}

pub(crate) fn named(found: &Containing) -> (&[Holder], usize) {
    let total = found.branches.len();
    let shown = if total <= NAMED { total } else { NAMED - 1 };
    (&found.branches[..shown], total - shown)
}

pub(crate) fn summary(found: &Containing) -> String {
    let (shown, more) = named(found);
    let names: Vec<&str> = shown.iter().map(|holder| holder.name.as_str()).collect();
    match (names.as_slice(), more) {
        ([], _) => "Not on any branch".to_owned(),
        ([only], 0) => format!("On {only}"),
        ([first @ .., last], 0) => format!("On {} and {last}", first.join(", ")),
        (all, more) => format!("On {} and {more} more", all.join(", ")),
    }
}

pub(crate) fn first_in(found: &Containing) -> Option<String> {
    found.first_tag.as_ref().map(|tag| format!("First in {tag}"))
}

fn rest(found: &Containing) -> String {
    let (shown, _) = named(found);
    let rest = &found.branches[shown.len()..];
    let mut lines: Vec<&str> = rest.iter().take(LISTED_MORE).map(|holder| holder.name.as_str()).collect();
    let unlisted = rest.len().saturating_sub(LISTED_MORE);
    let tail = format!("and {unlisted} more");
    if unlisted > 0 {
        lines.push(&tail);
    }
    lines.join("\n")
}

pub(crate) fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.containing;
    if oid.is_some() && state.oid.as_deref() == oid && (state.running() || state.found.is_some()) {
        return;
    }
    state.stop();
    state.found = None;
    state.oid = oid.map(str::to_owned);
    if oid.is_some() {
        later(tab, cx);
    }
}

pub(crate) fn refresh(tab: &mut Tab, cx: &mut Context<Tab>) {
    if tab.ui.containing.oid.is_some() {
        tab.ui.containing.stop();
        later(tab, cx);
    }
}

fn later(tab: &mut Tab, cx: &mut Context<Tab>) {
    let ticket = tab.ui.containing.ticket;
    cx.spawn(async move |tab, cx| {
        cx.background_executor().timer(SETTLE).await;
        let _ = tab.update(cx, |tab, cx| {
            if tab.ui.containing.ticket == ticket {
                start(tab, cx);
            }
        });
    })
    .detach();
}

fn start(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(oid), Some(repo)) = (tab.ui.containing.oid.clone(), tab.repo.clone()) else {
        return;
    };
    let current = (!tab.ui.detached && !tab.ui.branch.is_empty()).then(|| tab.ui.branch.clone());
    let state = &mut tab.ui.containing;
    state.stop();
    let link = Link::new(true);
    state.link = Some(link.clone());
    let (ticket, epoch) = (state.ticket, tab.epoch);
    Tab::work(
        cx,
        move || link.enter(|| repo.containing(&oid, current.as_deref())),
        move |tab, result, cx| {
            let state = &mut tab.ui.containing;
            if tab.epoch != epoch || state.ticket != ticket {
                return;
            }
            state.link = None;
            state.found = result.ok();
            cx.notify();
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut gpui::Window, cx: &mut Context<Tab>) {
    match action {
        Action::Show(kind, name) => {
            crate::refs::dispatch(tab, crate::refs::Action::Inspect(kind, OsString::from(name)), window, cx)
        }
    }
}

fn holder_link<T: Desk>(index: usize, holder: &Holder, cx: &Context<T>) -> impl IntoElement + use<T> {
    let action = view::Action::Containing(Action::Show(holder.kind, holder.name.clone()));
    let tip = match holder.kind {
        Kind::Branch => format!("Show {} in the graph", holder.name),
        _ => format!("Show the remote branch {} in the graph", holder.name),
    };
    div()
        .id(("containing-branch", index))
        .flex_shrink_0()
        .cursor_pointer()
        .text_color(rgb(if holder.kind == Kind::Branch { ACCENT } else { REMOTE }))
        .hover(|style| style.underline())
        .child(holder.name.clone())
        .tooltip(kit::tip(tip))
        .on_click(cx.listener(move |app, _, w, cx| app.dispatch(action.clone(), w, cx)))
}

pub(crate) fn lines<T: Desk>(ui: &view::Ui, oid: &str, cx: &Context<T>) -> Option<Div> {
    let found = ui.containing.found(oid)?;
    let (shown, more) = named(found);
    let mut on = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(4.))
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(TEXT_3))
        .child(kit::icon("branch").size(px(11.)).text_color(rgb(TEXT_3)))
        .child(if shown.is_empty() { "Not on any branch" } else { "On" });
    for (index, holder) in shown.iter().enumerate() {
        let after = shown.len() - index - 1;
        let comma = after > 1 || (after == 1 && more > 0);
        let and = (after == 1 && more == 0) || (after == 0 && more > 0);
        on = on.child(div().flex().child(holder_link(index, holder, cx)).when(comma, |d| d.child(",")));
        if and {
            on = on.child("and");
        }
    }
    if more > 0 {
        on = on.child(
            div()
                .id("containing-more")
                .text_color(rgb(TEXT_2))
                .child(format!("{more} more"))
                .tooltip(kit::tip(rest(found))),
        );
    }
    Some(div().flex().flex_col().gap(px(4.)).child(on).when_some(found.first_tag.clone(), |d, tag| {
        d.child(
            div()
                .flex()
                .items_center()
                .gap_x(px(4.))
                .text_size(px(TEXT_SMALL))
                .text_color(rgb(TEXT_3))
                .child(kit::icon("tag").size(px(11.)).text_color(rgb(TEXT_3)))
                .child("First in")
                .child(
                    div()
                        .id("containing-tag")
                        .cursor_pointer()
                        .text_color(rgb(TAG))
                        .hover(|style| style.underline())
                        .child(tag.clone())
                        .tooltip(kit::tip(format!("Show {tag} in the graph")))
                        .on_click(cx.listener(move |app, _, w, cx| {
                            app.dispatch(view::Action::Containing(Action::Show(Kind::Tag, tag.clone())), w, cx)
                        })),
                ),
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn commit(f: &Fixture, file: &str, subject: &str) -> String {
        fs::write(f.dir.join(file), subject).unwrap();
        f.git(&["add", "--", file]);
        f.git(&["commit", "-qm", subject]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn snapshot(f: &Fixture) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (f.git(&["for-each-ref"]), f.git(&["ls-files", "-s"]), f.git(&["status", "--porcelain=v1", "-uall"]))
    }

    fn holder(kind: Kind, name: &str) -> Holder {
        Holder { kind, name: name.to_owned() }
    }

    #[test]
    fn a_commit_lists_the_branches_that_contain_it_and_the_first_tag_and_changes_nothing() {
        let f = Fixture::new();
        let base = commit(&f, "base.txt", "Base");
        f.git(&["tag", "-a", "-m", "First", "v1.0"]);
        let middle = commit(&f, "middle.txt", "Middle");
        f.git(&["branch", "feature/x"]);
        f.git(&["branch", "alpha"]);
        f.git(&["update-ref", "refs/remotes/origin/main", &middle]);
        f.git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        let tip = commit(&f, "tip.txt", "Tip");
        f.git(&["tag", "v2.0"]);
        f.git(&["switch", "-q", "feature/x"]);
        let side = commit(&f, "side.txt", "Side");
        f.git(&["switch", "-q", "main"]);
        let before = snapshot(&f);

        let found = f.repo.containing(&middle, Some("main")).unwrap();
        assert_eq!(
            found.branches,
            [holder(Kind::Branch, "main"), holder(Kind::Branch, "alpha"), holder(Kind::Branch, "feature/x"), holder(Kind::Remote, "origin/main")]
        );
        assert_eq!(found.first_tag.as_deref(), Some("v2.0"));
        assert_eq!(summary(&found), "On main, alpha and 2 more");
        assert_eq!(first_in(&found).as_deref(), Some("First in v2.0"));
        assert_eq!(rest(&found), "feature/x\norigin/main");

        let found = f.repo.containing(&base, Some("feature/x")).unwrap();
        assert_eq!(found.branches[0], holder(Kind::Branch, "feature/x"));
        assert_eq!(found.first_tag.as_deref(), Some("v1.0"));

        let found = f.repo.containing(&tip, Some("main")).unwrap();
        assert_eq!(found.branches, [holder(Kind::Branch, "main")]);
        assert_eq!(summary(&found), "On main");
        let found = f.repo.containing(&side, None).unwrap();
        assert_eq!((summary(&found), found.first_tag), ("On feature/x".to_owned(), None));
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn a_commit_no_ref_leads_to_is_on_no_branch_and_a_bad_id_is_refused() {
        let f = Fixture::new();
        commit(&f, "base.txt", "Base");
        let lost = commit(&f, "lost.txt", "Lost");
        f.git(&["reset", "-q", "--hard", "HEAD~1"]);
        let before = snapshot(&f);
        let found = f.repo.containing(&lost, Some("main")).unwrap();
        assert_eq!(found, Containing::default());
        assert_eq!(summary(&found), "Not on any branch");
        assert!(f.repo.containing("--all", None).is_err());
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn a_cancelled_lookup_runs_no_git_and_changes_nothing() {
        let f = Fixture::new();
        let base = commit(&f, "base.txt", "Base");
        let before = snapshot(&f);
        let link = Link::new(true);
        link.cancel();
        assert!(link.enter(|| f.repo.containing(&base, Some("main"))).is_err());
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn the_summary_names_up_to_three_branches_then_counts_the_rest() {
        let found = |names: &[&str]| Containing {
            branches: names.iter().map(|name| holder(Kind::Branch, name)).collect(),
            first_tag: None,
        };
        assert_eq!(summary(&found(&["main", "feature/x"])), "On main and feature/x");
        assert_eq!(summary(&found(&["main", "feature/x", "dev"])), "On main, feature/x and dev");
        assert_eq!(summary(&found(&["a", "b", "c", "d", "e"])), "On a, b and 3 more");
        let many: Vec<String> = (0..20).map(|n| format!("b{n}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let listed = rest(&found(&many));
        assert_eq!(listed.lines().count(), LISTED_MORE + 1);
        assert_eq!(listed.lines().last(), Some("and 6 more"));
    }
}
