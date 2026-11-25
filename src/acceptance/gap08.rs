use super::*;
use crate::{
    PAGE,
    columns::{self, Column, Layout, Widths},
    commands::{self, Target},
    containing, graph_view, inspection, notice, paging,
    refs::Kind,
    reveal, search,
    theme::ROW,
};
use gpui::{ScrollStrategy, point};
use std::{io::Write, process::Stdio};

const DEPTH: usize = 5_000;
const DEEP_COMMITS: usize = 6_000;
const SEARCHED_COMMITS: usize = 400;
const RIDGE_COMMITS: usize = 300;
const KEPT_DEPTH: usize = 5_500;
const TOP_DEPTH: usize = 3_000;
const SHORT_COMMITS: usize = 1_200;
const SPUR_COMMITS: usize = 30;
const LANE_COUNT: usize = 16;
const TWELVE_LANES: f32 = 16. + 11. * 14. + 22.;
const SIXTEEN_LANES: f32 = 16. + 15. * 14. + 22.;

fn import(root: &Path, stream: &[u8]) -> Result<()> {
    let mut child = repository::bundled_command(root)?
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("git fast-import has no input")?.write_all(stream)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!("git fast-import: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    git(root, &["reset", "--hard", "-q", "main"])?;
    Ok(())
}

fn commit_record(stream: &mut Vec<u8>, mark: usize, author: &str, subject: &str) {
    let time = 1_700_000_000 + mark * 60;
    stream.extend_from_slice(
        format!(
            "commit refs/heads/main\nmark :{mark}\nauthor {author} {time} +0000\ncommitter {author} {time} +0000\ndata {}\n{subject}\n",
            subject.len() + 1
        )
        .as_bytes(),
    );
    if mark == 1 {
        stream.extend_from_slice(b"M 100644 inline route.txt\ndata 6\nRoute\n");
    }
    stream.push(b'\n');
}

fn row_in_view(tab: &Tab, row: usize) -> bool {
    let row = row as f32;
    let scroll = tab.ui.history_scroll.0.borrow();
    if scroll.deferred_scroll_to_item.is_some() {
        return false;
    }
    let top = -f32::from(scroll.base_handle.offset().y);
    let height = f32::from(scroll.base_handle.bounds().size.height);
    height > ROW && row * ROW >= top && (row + 1.) * ROW <= top + height
}

fn in_view(tab: &Tab, oid: &str) -> bool {
    match tab.ui.history.iter().position(|row| row.oid == oid) {
        Some(index) => row_in_view(tab, tab.ui.stash.commit_row(index)),
        None => false,
    }
}

fn height_in_window(tab: &Tab, oid: &str) -> Option<f32> {
    let index = tab.ui.history.iter().position(|row| row.oid == oid)?;
    let row = tab.ui.stash.commit_row(index) as f32;
    let scroll = tab.ui.history_scroll.0.borrow();
    if scroll.deferred_scroll_to_item.is_some() || f32::from(scroll.base_handle.bounds().size.height) <= ROW {
        return None;
    }
    Some(row * ROW + f32::from(scroll.base_handle.offset().y))
}

fn top_row(tab: &Tab) -> Option<String> {
    let top = -f32::from(tab.ui.history_scroll.0.borrow().base_handle.offset().y);
    let item = (top / ROW).floor() as usize;
    (0..tab.ui.history.len())
        .find(|&index| tab.ui.stash.commit_row(index) == item)
        .map(|index| tab.ui.history[index].oid.clone())
}

fn subject_at(tab: &Tab, subject: &str) -> Option<String> {
    tab.ui.history.iter().find(|row| row.subject == subject).map(|row| row.oid.clone())
}

fn offered<'a>(tab: &'a Tab, title: &str) -> Option<&'a notice::Notice> {
    tab.ui.notices.items().iter().find(|notice| notice.title == title)
}

fn commit_dated(root: &Path, file: &str, message: &str, time: usize, people: &[(&str, &str)]) -> Result<String> {
    fs::write(root.join(file), format!("{message}\n"))?;
    git(root, &["add", "--", file])?;
    let date = format!("{time} +0000");
    let output = repository::bundled_command(root)?
        .envs(people.iter().copied())
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .args(["commit", "-qm", message])
        .output()?;
    if !output.status.success() {
        return Err(format!("git commit: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    oid(root, "HEAD")
}

fn chips(tab: &Tab, oid: &str) -> Vec<(String, Option<String>, String)> {
    let Some(row) = tab.ui.history.iter().find(|row| row.oid == oid) else {
        return Vec::new();
    };
    view::row_labels(&tab.ui, &row.refs)
        .into_iter()
        .map(|label| {
            let tip = view::label_tip(&tab.ui.refs.items, &label);
            (label.name, label.remote, tip)
        })
        .collect()
}

fn listed(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git(root, args)?.lines().map(str::to_owned).collect())
}

fn graph_rows(tab: &Tab) -> Vec<String> {
    tab.ui.history.iter().map(|row| row.oid.clone()).collect()
}

fn graph_labels(tab: &Tab) -> Vec<(String, Option<String>)> {
    tab.ui
        .history
        .iter()
        .flat_map(|row| view::row_labels(&tab.ui, &row.refs))
        .map(|label| (label.name, label.remote))
        .collect()
}

fn strip_reads(tab: &Tab) -> String {
    graph_view::summary(&tab.ui).join(" · ")
}

fn in_sidebar(tab: &Tab, kind: Kind, name: &str) -> bool {
    tab.ui.refs.rows.iter().any(|row| {
        matches!(row, refs::SideRow::Ref { index, .. }
            if tab.ui.refs.items.get(*index).is_some_and(|item| item.kind == kind && item.name == name))
    })
}

fn remembered_view(common: &Path) -> Result<Option<serde_json::Value>> {
    let state = storage::override_directory().ok_or("No state folder for the replay")?;
    let memory: serde_json::Value = serde_json::from_slice(&fs::read(state.join("state.json"))?)?;
    let key: Vec<serde_json::Value> = common.as_os_str().as_encoded_bytes().iter().map(|byte| (*byte).into()).collect();
    Ok(memory["repositories"]
        .as_array()
        .and_then(|views| views.iter().find(|view| view["directory"] == serde_json::Value::Array(key.clone())))
        .map(|view| view["graph"].clone())
        .filter(|graph| !graph.is_null()))
}

impl Replay<'_> {
    pub(super) async fn gap08(&mut self, dir: &Path) -> Result<()> {
        self.jump_down_to_a_tag(dir).await?;
        self.jump_to_a_hidden_commit(dir).await?;
        self.search_by_author(dir).await?;
        self.keep_the_place_deep_down(dir).await?;
        self.mark_the_end_of_history(dir).await?;
        self.hide_a_column(dir).await?;
        self.share_a_label(dir).await?;
        self.where_a_commit_lives(dir).await?;
        self.hide_a_branch(dir).await?;
        self.solo_a_branch(dir).await?;
        self.hide_a_remote(dir).await
    }

    fn drawn_columns(&mut self) -> Result<Widths> {
        Ok(self.tab.update(self.cx, |tab, window, _| {
            let viewport = f32::from(window.viewport_size().width);
            view::history_columns(&tab.ui, tab.ui.columns_drawn(viewport).1)
        })?)
    }

    fn column_menu(&mut self) -> Result<Vec<(&'static str, String, Option<bool>, bool, Option<Action>)>> {
        self.action(Action::Menu(Some(view::Menu::Object { target: Target::Columns, at: point(px(400.), px(90.)) })))?;
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let open = tab.ui.menu.clone();
            match open {
                Some(open @ view::Menu::Object { target: Target::Columns, .. }) => commands::entries_of(&tab.ui, &open, cx)
                    .into_iter()
                    .map(|entry| (entry.id, entry.title, entry.check, entry.disabled.is_some(), entry.action))
                    .collect(),
                _ => Vec::new(),
            }
        })?)
    }

    fn saved_columns(&mut self) -> Result<serde_json::Value> {
        self.cx.update(|cx| settings::flush(cx))?;
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let chosen: serde_json::Value = serde_json::from_slice(&fs::read(state.join("settings.json"))?)?;
        Ok(chosen["graph_columns"].clone())
    }

    async fn hide_a_column(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0806");
        let root = init(dir, "Column-camp")?;
        record(&root, "camp.txt", "Base camp\n", "Clear the base camp")?;
        let base = oid(&root, "HEAD")?;
        let mut tips = Vec::new();
        for lane in 1..=LANE_COUNT {
            git(&root, &["switch", "-qc", &format!("lane-{lane}"), &base])?;
            git(&root, &["commit", "--allow-empty", "-qm", &format!("Walk lane {lane}")])?;
            tips.push(oid(&root, "HEAD")?);
        }
        git(&root, &["switch", "-q", "main"])?;
        let tree = oid(&root, "HEAD^{tree}")?;
        let mut args = vec!["commit-tree", tree.as_str(), "-m", "Gather every lane"];
        for tip in &tips {
            args.extend(["-p", tip.as_str()]);
        }
        let gathered = git(&root, &args)?.trim_end().to_owned();
        git(&root, &["merge", "--ff-only", "-q", &gathered])?;
        git_is(&root, &["rev-list", "--count", "--merges", "HEAD"], "1\n")?;
        git_is(&root, &["rev-list", "--count", "HEAD^@"], &format!("{}\n", LANE_COUNT + 1))?;
        let standard = serde_json::to_value(Layout::default())?;
        self.open(&root)?;
        self.until("Column-camp open with every commit", |tab| {
            tab.ui.repo == "Column-camp" && tab.ui.history.len() == LANE_COUNT + 2
        })
        .await?;
        let (content, layout) = self.tab.read_with(self.cx, |tab, _| {
            (tab.ui.graph_content_width(), tab.ui.graph_columns.layout.clone())
        })?;
        if content != SIXTEEN_LANES || layout != Layout::default() {
            return Err(format!("The graph is {content} px of lanes with the layout {layout:?}").into());
        }
        let widths = self.drawn_columns()?;
        if (widths.graph, widths.author, widths.date, widths.sha) != (TWELVE_LANES, 140., 84., 66.) || widths.description < columns::DESCRIPTION_MIN {
            return Err(format!("The columns are drawn as {widths:?}").into());
        }

        let rows = self.column_menu()?;
        let ticks: Vec<(&str, String, Option<bool>, bool)> =
            rows.iter().map(|(id, title, check, disabled, _)| (*id, title.clone(), *check, *disabled)).collect();
        let expected: Vec<(&str, String, Option<bool>, bool)> = [
            ("columns.labels", "Branch and tag labels", Some(true), false),
            ("columns.graph", "Graph", Some(true), false),
            ("columns.description", "Description", Some(true), true),
            ("columns.author", "Author", Some(true), false),
            ("columns.date", "Date", Some(true), false),
            ("columns.sha", "SHA", Some(true), false),
            ("columns.reset", "Reset columns", None, false),
        ]
        .into_iter()
        .map(|(id, title, check, disabled)| (id, title.to_owned(), check, disabled))
        .collect();
        if ticks != expected {
            return Err(format!("The header menu lists {ticks:?}").into());
        }
        let untick = rows
            .into_iter()
            .find(|row| row.0 == "columns.author")
            .and_then(|row| row.4)
            .ok_or("The Author row has no action")?;
        self.action(untick)?;
        self.action(Action::Columns(columns::Action::Resize(Column::Date, 120.)))?;
        self.until("the Author column hidden and Date widened", |tab| {
            tab.ui.menu.is_none()
                && !tab.ui.graph_columns.layout.shows(Column::Author)
                && tab.ui.graph_columns.layout.date == 120.
        })
        .await?;
        let widths = self.drawn_columns()?;
        if (widths.author, widths.date, widths.sha) != (0., 120., 66.) {
            return Err(format!("With Author hidden the columns are drawn as {widths:?}").into());
        }
        let stored = self.saved_columns()?;
        if stored["hidden"] != serde_json::json!(["author"]) || stored["date"] != 120. {
            return Err(format!("settings.json holds the columns {stored}").into());
        }

        self.relaunch().await?;
        self.until("Column-camp reopened with Author still hidden", |tab| {
            tab.ui.repo == "Column-camp"
                && tab.ui.history.len() == LANE_COUNT + 2
                && !tab.ui.graph_columns.layout.shows(Column::Author)
                && tab.ui.graph_columns.layout.date == 120.
        })
        .await?;
        let widths = self.drawn_columns()?;
        if (widths.graph, widths.author, widths.date, widths.sha) != (TWELVE_LANES, 0., 120., 66.) {
            return Err(format!("After the relaunch the columns are drawn as {widths:?}").into());
        }
        let author = self.column_menu()?.into_iter().find(|row| row.0 == "columns.author").map(|row| row.2);
        if author != Some(Some(false)) {
            return Err(format!("After the relaunch the Author row is ticked {author:?}").into());
        }
        self.action(Action::Dismiss)?;

        let rightmost = self.tab.read_with(self.cx, |tab, _| {
            (0..tab.ui.history.len())
                .filter_map(|index| tab.ui.graph_node_x(index).map(|x| (index, x)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(index, x)| (tab.ui.history[index].oid.clone(), x))
        })?;
        let (far, x) = rightmost.ok_or("No commit sits on a lane")?;
        if x < TWELVE_LANES {
            return Err(format!("The rightmost node is only {x} px across").into());
        }
        self.action(Action::Select(Selection::Commit(far.clone())))?;
        self.until("the rightmost node scrolled into view", move |tab| {
            let offset = tab.ui.graph_columns.offset;
            tab.ui.selection == Some(Selection::Commit(far.clone())) && offset > 0. && x - offset > 0. && x - offset < TWELVE_LANES
        })
        .await?;
        self.action(Action::Columns(columns::Action::Pan(-1_000.)))?;
        self.until("the graph scrolled back to the first lane", |tab| tab.ui.graph_columns.offset == 0.).await?;
        self.action(Action::Columns(columns::Action::Pan(1_000.)))?;
        self.until("the graph scrolled to the last lane", |tab| {
            tab.ui.graph_columns.offset == SIXTEEN_LANES - TWELVE_LANES
        })
        .await?;

        self.action(Action::Columns(columns::Action::Fit(Column::Graph)))?;
        self.until("the graph column fitted to every lane", |tab| {
            tab.ui.graph_columns.layout.graph == Some(SIXTEEN_LANES) && tab.ui.graph_columns.offset == 0.
        })
        .await?;
        let widths = self.drawn_columns()?;
        if widths.graph != SIXTEEN_LANES {
            return Err(format!("The fitted graph column is drawn {} px wide", widths.graph).into());
        }

        let reset = self
            .column_menu()?
            .into_iter()
            .find(|row| row.0 == "columns.reset")
            .and_then(|row| row.4)
            .ok_or("Reset columns has no action")?;
        self.action(reset)?;
        self.until("the standard columns back", |tab| tab.ui.graph_columns.layout == Layout::default()).await?;
        let widths = self.drawn_columns()?;
        if (widths.graph, widths.author, widths.date, widths.sha) != (TWELVE_LANES, 140., 84., 66.) {
            return Err(format!("After the reset the columns are drawn as {widths:?}").into());
        }
        let stored = self.saved_columns()?;
        if stored != standard {
            return Err(format!("After the reset settings.json holds the columns {stored}").into());
        }
        self.pass();
        Ok(())
    }

    async fn keep_the_place_deep_down(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0804");
        let root = init(dir, "Deep-camp")?;
        let mut stream = Vec::new();
        let author = "Cedar Contributor <cedar@example.invalid>";
        for mark in 1..=DEEP_COMMITS {
            commit_record(&mut stream, mark, author, &format!("Pitch tent {mark}"));
        }
        import(&root, &stream)?;
        let order = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        let deep = order.get(KEPT_DEPTH).cloned().ok_or("The deep camp is too short")?;
        if git(&root, &["log", "-1", "--format=%s", &deep])?.trim_end() != format!("Pitch tent {}", DEEP_COMMITS - KEPT_DEPTH) {
            return Err(format!("Git does not list Pitch tent {} {KEPT_DEPTH} commits down", DEEP_COMMITS - KEPT_DEPTH).into());
        }
        self.open(&root)?;
        self.until("the deep camp open", |tab| tab.ui.repo == "Deep-camp" && !tab.ui.history.is_empty()).await?;
        self.action(Action::Reveal(reveal::Action::Commit(deep.clone())))?;
        self.until("the deep commit selected and in view more than 5,000 rows down", |tab| {
            tab.ui.selection == Some(Selection::Commit(deep.clone()))
                && tab.ui.reveal.finding().is_none()
                && !tab.loading_page
                && tab.ui.history.len() > paging::KEPT
                && in_view(tab, &deep)
        })
        .await?;
        let (loaded, before) = self.tab.read_with(self.cx, |tab, _| (tab.ui.history.len(), height_in_window(tab, &deep)))?;
        let before = before.ok_or("The deep commit has no place in the window")?;
        fs::write(root.join("weather.txt"), "Clear skies\n")?;
        self.during("the new file listed", |tab| !tab.ui.busy() && files(tab, &[], &["weather.txt"])).await?;
        self.action(Action::StageAll)?;
        self.during("the file staged", |tab| !tab.ui.busy() && files(tab, &["weather.txt"], &[])).await?;
        let start = oid(&root, "HEAD")?;
        self.type_commit("Note the weather")?;
        self.until("the new commit first in the graph", |tab| {
            !tab.loading_page
                && tab.ui.head.as_deref().is_some_and(|head| head != start)
                && tab.ui.history.first().is_some_and(|row| row.subject == "Note the weather")
        })
        .await?;
        git_is(&root, &["log", "-1", "--format=%s"], "Note the weather\n")?;
        git_is(&root, &["rev-parse", "HEAD~1"], &format!("{start}\n"))?;
        let (now, row, after) = self.tab.read_with(self.cx, |tab, _| {
            (
                tab.ui.history.len(),
                tab.ui.history.iter().position(|row| row.oid == deep),
                height_in_window(tab, &deep),
            )
        })?;
        if now < loaded {
            return Err(format!("The graph held {loaded} rows before the commit and {now} after it").into());
        }
        if row != Some(KEPT_DEPTH + 1) {
            return Err(format!("The deep commit is row {row:?} after the commit, not {}", KEPT_DEPTH + 1).into());
        }
        if !after.is_some_and(|after| (after - before).abs() < 0.5) {
            return Err(format!("The deep commit was {before} px down the window and is now {after:?}").into());
        }
        let top = self.tab.read_with(self.cx, |tab, _| subject_at(tab, &format!("Pitch tent {TOP_DEPTH}")))?;
        let top = top.ok_or("Pitch tent 3000 is not loaded")?;
        self.tab.update(self.cx, |tab, _, cx| {
            if let Some(index) = tab.ui.history.iter().position(|row| row.oid == top) {
                let row = tab.ui.stash.commit_row(index) as f32;
                tab.ui.history_scroll.0.borrow().base_handle.set_offset(point(px(0.), px(-(row * ROW + 9.))));
            }
            cx.notify();
        })?;
        self.until("Pitch tent 3000 the top row with the selected commit out of view", |tab| {
            top_row(tab).as_ref() == Some(&top)
                && height_in_window(tab, &top).is_some()
                && matches!(&tab.ui.selection, Some(Selection::Commit(selected)) if !in_view(tab, selected))
        })
        .await?;
        let before = self.tab.read_with(self.cx, |tab, _| height_in_window(tab, &top))?.ok_or("No height for the top row")?;
        record(&root, "wind.txt", "Gusty\n", "Record the wind")?;
        self.action(Action::Refresh)?;
        self.until("the outside commit first in the graph", |tab| {
            !tab.loading_page && tab.ui.history.first().is_some_and(|row| row.subject == "Record the wind")
        })
        .await?;
        let (shown, after) = self.tab.read_with(self.cx, |tab, _| (top_row(tab), height_in_window(tab, &top)))?;
        if shown.as_ref() != Some(&top) || !after.is_some_and(|after| (after - before).abs() < 0.5) {
            return Err(format!("Pitch tent 3000 was the top row at {before} px; now the top row is {shown:?} and it is at {after:?}").into());
        }
        self.pass();
        Ok(())
    }

    async fn mark_the_end_of_history(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0805");
        let root = init(dir, "Short-trail")?;
        let mut stream = Vec::new();
        let author = "Cedar Contributor <cedar@example.invalid>";
        for mark in 1..=SHORT_COMMITS {
            commit_record(&mut stream, mark, author, &format!("Walk mile {mark}"));
        }
        for step in 1..=SPUR_COMMITS {
            let mark = SHORT_COMMITS + step;
            let subject = format!("Explore the spur {step}");
            let time = 1_800_000_000 + step * 60;
            stream.extend_from_slice(
                format!(
                    "commit refs/heads/spur\nmark :{mark}\ncommitter {author} {time} +0000\ndata {}\n{subject}\n",
                    subject.len() + 1
                )
                .as_bytes(),
            );
            if step == 1 {
                stream.extend_from_slice(format!("from :{}\n", SHORT_COMMITS / 2).as_bytes());
            }
            stream.push(b'\n');
        }
        import(&root, &stream)?;
        let order = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        let total = SHORT_COMMITS + SPUR_COMMITS;
        if order.len() != total {
            return Err(format!("Git lists {} commits, not {total}", order.len()).into());
        }
        self.open(&root)?;
        self.until("the first page with a loading row at its end", |tab| {
            tab.ui.repo == "Short-trail"
                && tab.ui.has_more
                && tab.ui.history.len() == PAGE
                && paging::tail(&tab.ui).as_deref() == Some(paging::LOADING)
                && paging::items(&tab.ui) == tab.ui.history_items() + 1
        })
        .await?;
        for _ in 0..total / PAGE + 2 {
            let (loaded, more) = self.tab.read_with(self.cx, |tab, _| (tab.ui.history.len(), tab.ui.has_more))?;
            if !more {
                break;
            }
            self.tab.update(self.cx, |tab, _, cx| {
                let last = paging::items(&tab.ui) - 1;
                tab.ui.history_scroll.scroll_to_item_strict(last, ScrollStrategy::Bottom);
                cx.notify();
            })?;
            self.until("the next page loaded by scrolling to the bottom", |tab| {
                !tab.loading_page && (tab.ui.history.len() > loaded || !tab.ui.has_more)
            })
            .await?;
        }
        self.tab.update(self.cx, |tab, _, cx| {
            let last = paging::items(&tab.ui) - 1;
            tab.ui.history_scroll.scroll_to_item_strict(last, ScrollStrategy::Bottom);
            cx.notify();
        })?;
        let ending = format!("Start of history · {} commits", paging::grouped(total));
        if ending != "Start of history · 1,230 commits" {
            return Err(format!("The count reads {ending}").into());
        }
        self.until("every commit loaded and the last row marking the start of history", |tab| {
            !tab.ui.has_more
                && tab.ui.history.iter().map(|row| row.oid.as_str()).eq(order.iter().map(String::as_str))
                && paging::tail(&tab.ui).as_deref() == Some(ending.as_str())
                && paging::items(&tab.ui) == tab.ui.history_items() + 1
                && row_in_view(tab, paging::items(&tab.ui) - 1)
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn jump_down_to_a_tag(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0801");
        let root = init(dir, "Deep-trail")?;
        let mut stream = Vec::new();
        let author = "Cedar Contributor <cedar@example.invalid>";
        for mark in 1..=DEEP_COMMITS {
            commit_record(&mut stream, mark, author, &format!("Climb step {mark}"));
        }
        let tagged = DEEP_COMMITS - DEPTH;
        stream.extend_from_slice(
            format!(
                "tag camp-{DEPTH}\nfrom :{tagged}\ntagger {author} 1700000000 +0000\ndata 15\nDeep base camp\n\n"
            )
            .as_bytes(),
        );
        import(&root, &stream)?;
        let camp = oid(&root, &format!("camp-{DEPTH}^{{commit}}"))?;
        let order = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        if order.get(DEPTH) != Some(&camp) {
            return Err(format!("Git does not list camp-{DEPTH} {DEPTH} commits down").into());
        }
        self.open(&root)?;
        let tag = std::ffi::OsString::from(format!("camp-{DEPTH}"));
        self.until("the first page of a deep history and its tag", |tab| {
            tab.ui.repo == "Deep-trail"
                && tab.ui.has_more
                && tab.ui.history.len() < DEPTH
                && tab.ui.refs.items.iter().any(|item| item.kind == Kind::Tag && item.name == tag)
        })
        .await?;
        self.ref_action(refs::Action::Inspect(Kind::Tag, tag.clone()))?;
        let expected = format!("Climb step {tagged}");
        self.until("the tagged commit selected, loaded and in view", |tab| {
            tab.ui.selection == Some(Selection::Commit(camp.clone()))
                && tab.ui.reveal.finding().is_none()
                && in_view(tab, &camp)
                && tab.ui.inspection.subject.as_deref() == Some(expected.as_str())
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.history.iter().position(|row| row.oid == camp)
        })?;
        if shown != Some(DEPTH) {
            return Err(format!("camp-{DEPTH} is graph row {shown:?}, not {DEPTH}").into());
        }
        let notices = self.tab.read_with(self.cx, |tab, _| tab.ui.notice_text())?;
        if notices.starts_with("Finding") {
            return Err(format!("The finding notice stayed after the jump: {notices}").into());
        }
        let parent = order[DEPTH + 1].clone();
        self.action(Action::Reveal(reveal::Action::Commit(parent.clone())))?;
        self.until("its parent selected and in view", |tab| {
            tab.ui.selection == Some(Selection::Commit(parent.clone())) && in_view(tab, &parent)
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn jump_to_a_hidden_commit(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0802");
        let root = init(dir, "Hidden-trail")?;
        record(&root, "trail.txt", TRAIL, "Start the hidden trail")?;
        git(&root, &["switch", "-qc", "survey"])?;
        record(&root, "survey.txt", "Ridge\n", "Survey the ridge")?;
        let survey = oid(&root, "HEAD")?;
        git(&root, &["switch", "-qc", "abandoned"])?;
        record(&root, "abandoned.txt", "Rockfall\n", "Try the rockfall route")?;
        let abandoned = oid(&root, "HEAD")?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["branch", "-qD", "abandoned"])?;
        record(&root, "camp.txt", "Camp\n", "Mark the camp")?;
        self.open(&root)?;
        self.until("both branches in the graph", |tab| {
            tab.ui.repo == "Hidden-trail"
                && tab.ui.history.iter().any(|row| row.oid == survey)
                && tab.ui.refs.items.iter().any(|item| item.kind == Kind::Branch && item.name == "survey")
        })
        .await?;
        self.action(Action::GraphView(graph_view::Action::Hide("refs/heads/survey".into())))?;
        self.until("the survey commit gone from the graph", |tab| {
            tab.ui.graph_view.filtered()
                && tab.ui.history.len() == 2
                && !tab.ui.history.iter().any(|row| row.oid == survey)
        })
        .await?;
        let filtered = listed(&root, &["rev-list", "--exclude=refs/heads/survey", "--exclude=refs/stash", "--all", "HEAD"])?;
        if filtered.contains(&survey) {
            return Err("Git still lists the survey commit with survey hidden".into());
        }
        self.ref_action(refs::Action::Inspect(Kind::Branch, "survey".into()))?;
        let hidden = format!("{} is hidden by the current graph filter", notice::short(&survey));
        self.until("the hidden commit selected with a notice that offers Clear filter", |tab| {
            tab.ui.selection == Some(Selection::Commit(survey.clone()))
                && tab.ui.inspection.subject.as_deref() == Some("Survey the ridge")
                && offered(tab, &hidden).is_some_and(|notice| {
                    notice.actions.iter().any(|(label, _)| label.as_ref() == "Clear filter")
                })
        })
        .await?;
        let take = self.tab.read_with(self.cx, |tab, _| {
            offered(tab, &hidden).and_then(|notice| {
                let index = notice.actions.iter().position(|(label, _)| label.as_ref() == "Clear filter")?;
                Some(notice::Action::Take(notice.id, index))
            })
        })?;
        self.action(Action::Notice(take.ok_or("The notice lost its Clear filter offer")?))?;
        self.until("the filter cleared and the survey commit selected in view", |tab| {
            !tab.ui.graph_view.filtered()
                && tab.ui.selection == Some(Selection::Commit(survey.clone()))
                && in_view(tab, &survey)
                && offered(tab, &hidden).is_none()
        })
        .await?;
        self.action(Action::Reveal(reveal::Action::Commit(abandoned.clone())))?;
        let unlisted = format!("{} is not on any branch or tag", notice::short(&abandoned));
        self.until("the abandoned commit selected with a notice saying why it has no row", |tab| {
            tab.ui.selection == Some(Selection::Commit(abandoned.clone()))
                && tab.ui.inspection.subject.as_deref() == Some("Try the rockfall route")
                && offered(tab, &unlisted).is_some()
                && !tab.ui.history.iter().any(|row| row.oid == abandoned)
        })
        .await?;
        if !git(&root, &["for-each-ref", &format!("--contains={abandoned}")])?.is_empty() {
            return Err("A ref still contains the abandoned commit".into());
        }
        let camp = oid(&root, "main")?;
        let mut stream = Vec::new();
        for mark in 1..=RIDGE_COMMITS {
            let subject = format!("Climb the ridge {mark}");
            let time = 1_800_000_000 + mark * 60;
            stream.extend_from_slice(
                format!(
                    "commit refs/heads/ridge\nmark :{mark}\ncommitter Cedar Contributor <cedar@example.invalid> {time} +0000\ndata {}\n{subject}\n",
                    subject.len() + 1
                )
                .as_bytes(),
            );
            if mark == 1 {
                stream.extend_from_slice(format!("from {camp}\n").as_bytes());
            }
            stream.push(b'\n');
        }
        import(&root, &stream)?;
        let ridge = oid(&root, "ridge")?;
        self.action(Action::Refresh)?;
        self.until("a long ridge above the camp", |tab| tab.ui.history.first().is_some_and(|row| row.oid == ridge))
            .await?;
        self.action(Action::Reveal(reveal::Action::Commit(camp.clone())))?;
        self.until("the camp selected and in view below the ridge", |tab| {
            tab.ui.selection == Some(Selection::Commit(camp.clone()))
                && tab.ui.reveal.finding().is_none()
                && in_view(tab, &camp)
        })
        .await?;
        self.action(Action::GraphView(graph_view::Action::Hide("refs/heads/ridge".into())))?;
        self.until("the ridge hidden", |tab| {
            tab.ui.graph_view.filtered() && !tab.ui.history.iter().any(|row| row.oid == ridge)
        })
        .await?;
        self.action(Action::GraphView(graph_view::Action::Clear))?;
        self.until("the filter cleared and the camp still selected and in view below the ridge", |tab| {
            !tab.ui.graph_view.filtered()
                && tab.ui.history.first().is_some_and(|row| row.oid == ridge)
                && tab.ui.selection == Some(Selection::Commit(camp.clone()))
                && tab.ui.reveal.finding().is_none()
                && in_view(tab, &camp)
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn search_by_author(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0803");
        let root = init(dir, "Author-trail")?;
        let mut stream = Vec::new();
        for mark in 1..=SEARCHED_COMMITS {
            let (author, subject) = if mark % 3 == 0 {
                ("Juniper Fox <fox@example.invalid>", format!("Survey stake {mark}"))
            } else if mark % 7 == 0 {
                ("Cedar Contributor <cedar@example.invalid>", format!("Plant a juniper by stake {mark}"))
            } else {
                ("Cedar Contributor <cedar@example.invalid>", format!("Clear the path at {mark}"))
            };
            commit_record(&mut stream, mark, author, &subject);
        }
        import(&root, &stream)?;
        let log = git(&root, &["log", "--topo-order", "--exclude=refs/stash", "--all", "--format=%H%x00%an%x00%s"])?;
        let expected: Vec<String> = log
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\0');
                let (oid, author, subject) = (fields.next()?, fields.next()?, fields.next()?);
                (author.to_lowercase().contains("juniper") || subject.to_lowercase().contains("juniper"))
                    .then(|| oid.to_owned())
            })
            .collect();
        let by_author_only = log
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\0');
                let (oid, author, subject) = (fields.next()?, fields.next()?, fields.next()?);
                (author == "Juniper Fox" && !subject.to_lowercase().contains("juniper")).then(|| oid.to_owned())
            })
            .ok_or("No commit by Juniper Fox")?;
        if expected.len() <= 100 {
            return Err(format!("Only {} commits match juniper", expected.len()).into());
        }
        self.open(&root)?;
        self.until("the author trail open", |tab| tab.ui.repo == "Author-trail" && !tab.ui.history.is_empty())
            .await?;
        self.action(Action::Search(search::Action::Open))?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.search.input.update(cx, |field, cx| field.replace_text_in_range(None, "JUNIPER", window, cx))
        })?;
        let first = expected[..100].to_vec();
        self.until("the first hundred matches in history order", |tab| {
            let state = &tab.ui.search;
            search::status(state) == "Showing the first 100"
                && state.shown().iter().map(|row| row.oid.as_str()).eq(first.iter().map(String::as_str))
        })
        .await?;
        let lacking = self.tab.read_with(self.cx, |tab, _| {
            tab.ui.search.shown().iter().any(|row| row.oid == by_author_only && !row.subject.to_lowercase().contains("juniper"))
        })?;
        if !lacking {
            return Err("No result is a commit whose message lacks the query".into());
        }
        self.action(Action::Search(search::Action::More))?;
        let all = format!("{} matching commits", expected.len());
        self.until("every match after Show more", |tab| {
            let state = &tab.ui.search;
            search::status(state) == all
                && state.shown().iter().map(|row| row.oid.as_str()).eq(expected.iter().map(String::as_str))
        })
        .await?;
        self.action(Action::Search(search::Action::ShowInGraph))?;
        self.until("the first match selected in the graph with the matches marked", |tab| {
            tab.ui.search.in_graph()
                && !tab.ui.search.visible()
                && tab.ui.selection == Some(Selection::Commit(expected[0].clone()))
                && in_view(tab, &expected[0])
                && expected.iter().all(|oid| tab.ui.search.marks(oid))
                && tab.ui.history.iter().any(|row| !tab.ui.search.marks(&row.oid))
        })
        .await?;
        self.key("enter")?;
        self.until("Return moves to the next match", |tab| {
            tab.ui.selection == Some(Selection::Commit(expected[1].clone())) && in_view(tab, &expected[1])
        })
        .await?;
        self.key("shift-enter")?;
        self.until("Shift-Return moves back", |tab| tab.ui.selection == Some(Selection::Commit(expected[0].clone())))
            .await?;
        self.key("shift-enter")?;
        let last = expected.last().cloned().ok_or("No matches")?;
        self.until("Shift-Return from the first match goes round to the last, deep in history", |tab| {
            tab.ui.selection == Some(Selection::Commit(last.clone())) && in_view(tab, &last)
        })
        .await?;
        record(&root, "route.txt", "Juniper route\n", "Mark a juniper on the route")?;
        let fresh = oid(&root, "HEAD")?;
        self.action(Action::Refresh)?;
        self.until("a new matching commit marked in the graph", |tab| {
            tab.ui.search.in_graph()
                && tab.ui.history.first().is_some_and(|row| row.oid == fresh)
                && tab.ui.search.marks(&fresh)
        })
        .await?;
        self.action(Action::Search(search::Action::Close))?;
        self.until("search closed and the marks gone", |tab| {
            !tab.ui.search.open() && !tab.ui.search.marks(&expected[0])
        })
        .await?;
        self.pass();
        Ok(())
    }

    async fn share_a_label(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0807");
        let origin = dir.join("Twin-camp-origin.git");
        fs::create_dir(&origin)?;
        git(&origin, &["init", "-q", "--bare", "-b", "main"])?;
        let root = init(dir, "Twin-camp")?;
        record(&root, "camp.txt", "Camp\n", "Pitch the twin camp")?;
        git(&root, &["branch", "survey"])?;
        git(&root, &["remote", "add", "origin", &origin.display().to_string()])?;
        git(&root, &["push", "-q", "-u", "origin", "main", "survey:survey"])?;
        git(&root, &["branch", "--unset-upstream", "survey"])?;
        let pitched = oid(&root, "main")?;
        git_is(&root, &["rev-parse", "origin/main", "origin/survey"], &format!("{pitched}\n{pitched}\n"))?;
        git_is(&root, &["rev-parse", "--abbrev-ref", "main@{upstream}"], "origin/main\n")?;
        self.open(&root)?;
        self.until("Twin-camp open with main tracking origin/main", |tab| {
            tab.ui.repo == "Twin-camp"
                && tab.ui.history.len() == 1
                && tab.ui.refs.branch("main").is_some_and(|item| item.upstream.as_deref() == Some("origin/main"))
                && tab.ui.refs.lists(Kind::Remote, "origin/survey")
        })
        .await?;
        let shown = self.tab.read_with(self.cx, |tab, _| chips(tab, &pitched))?;
        let names: Vec<_> = shown.iter().map(|chip| (chip.0.as_str(), chip.1.as_deref())).collect();
        if names != [("main", Some("origin/main")), ("survey", None), ("origin/survey", None)] {
            return Err(format!("With main and origin/main on one commit the labels are {shown:?}").into());
        }
        let tip = &shown[0].2;
        if !tip.contains("refs/heads/main") || !tip.contains("refs/remotes/origin/main") || !tip.contains("matches it") {
            return Err(format!("The shared label's tooltip reads {tip:?}").into());
        }
        if !shown[2].2.contains("refs/remotes/origin/survey") {
            return Err(format!("The origin/survey label's tooltip reads {:?}", shown[2].2).into());
        }

        self.action(view::pair_menu("main".into(), "origin/main".into(), point(px(300.), px(120.))))?;
        let (rows, branch_rows, remote_rows) = self.tab.read_with(self.cx, |tab, cx| {
            let rows: Vec<_> = tab.ui.menu.as_ref().map_or_else(Vec::new, |open| {
                commands::entries_of(&tab.ui, open, cx)
                    .into_iter()
                    .map(|entry| (entry.id, entry.title, entry.children.iter().map(|child| child.id).collect::<Vec<_>>()))
                    .collect()
            });
            let ids = |target: Target| commands::menu(&tab.ui, &target, cx).into_iter().map(|entry| entry.id).collect::<Vec<_>>();
            (rows, ids(Target::Branch("main".into())), ids(Target::RemoteBranch("origin/main".into())))
        })?;
        let (last, first) = rows.split_last().ok_or("The shared label's menu is empty")?;
        let first_ids: Vec<_> = first.iter().map(|row| row.0).collect();
        if first_ids != branch_rows
            || last.0 != "pair.remote"
            || last.1 != "origin/main"
            || last.2 != remote_rows
        {
            return Err(format!("The shared label's menu lists {rows:?}; main has {branch_rows:?}, origin/main has {remote_rows:?}").into());
        }
        self.action(Action::Dismiss)?;

        record(&root, "camp.txt", "Camp\nFire\n", "Light the fire")?;
        let lit = oid(&root, "main")?;
        self.action(Action::Refresh)?;
        self.until("the local commit listed with main one ahead of origin/main", |tab| {
            tab.ui.history.first().is_some_and(|row| row.oid == lit)
                && tab.ui.refs.branch("main").is_some_and(|item| item.ahead == 1 && item.oid.as_deref() == Some(lit.as_str()))
        })
        .await?;
        git_is(&root, &["rev-parse", "origin/main"], &format!("{pitched}\n"))?;
        let (top, below) = self.tab.read_with(self.cx, |tab, _| (chips(tab, &lit), chips(tab, &pitched)))?;
        let top_names: Vec<_> = top.iter().map(|chip| (chip.0.as_str(), chip.1.as_deref())).collect();
        let mut below_names: Vec<_> = below.iter().map(|chip| (chip.0.as_str(), chip.1.as_deref())).collect();
        below_names[1..].sort();
        if top_names != [("main", None)]
            || below_names != [("survey", None), ("origin/main", None), ("origin/survey", None)]
        {
            return Err(format!("After the local commit the labels are {top:?} above {below:?}").into());
        }
        if !top[0].2.contains("1 commit ahead") || top[0].2.contains("refs/remotes/origin/main,") {
            return Err(format!("After the local commit main's tooltip reads {:?}", top[0].2).into());
        }
        self.pass();
        Ok(())
    }

    async fn where_a_commit_lives(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0808");
        let root = init(dir, "Ridge-map")?;
        let start = 1_780_000_000;
        let trail = commit_dated(&root, "trail.txt", "Lay the trail", start, &[])?;
        git(&root, &["tag", "v1.0"])?;
        let people = [
            ("GIT_AUTHOR_NAME", "Juniper Fox"),
            ("GIT_AUTHOR_EMAIL", "juniper@example.invalid"),
            ("GIT_COMMITTER_NAME", "Rowan Ash"),
            ("GIT_COMMITTER_EMAIL", "rowan@example.invalid"),
        ];
        let message = "Mark the ridge\n\nCo-authored-by: Ada Lovelace <ada@example.invalid>";
        let ridge = commit_dated(&root, "ridge.txt", message, start + 60, &people)?;
        git(&root, &["branch", "alpha"])?;
        git(&root, &["branch", "beta"])?;
        let summit = commit_dated(&root, "summit.txt", "Reach the summit", start + 120, &[])?;
        git(&root, &["tag", "v1.2"])?;
        git(&root, &["branch", "feature/x"])?;
        git(&root, &["update-ref", "refs/remotes/origin/main", &summit])?;
        commit_dated(&root, "descent.txt", "Start the descent", start + 180, &[])?;
        git(&root, &["tag", "v2.0"])?;
        let holders = |commit: &str| -> Result<Vec<String>> {
            listed(&root, &["for-each-ref", "--format=%(refname:short)", &format!("--contains={commit}"), "refs/heads", "refs/remotes"])
        };
        let mut expected = holders(&ridge)?;
        expected.sort();
        if expected != ["alpha", "beta", "feature/x", "main", "origin/main"] {
            return Err(format!("Git lists {expected:?} as holding the ridge").into());
        }
        let tags = listed(&root, &["for-each-ref", "--format=%(refname:short)", "--sort=creatordate", &format!("--contains={ridge}"), "refs/tags"])?;
        if tags != ["v1.2", "v2.0"] {
            return Err(format!("Git lists {tags:?} as the tags holding the ridge").into());
        }
        self.open(&root)?;
        self.until("Ridge-map open with its refs loaded", |tab| {
            tab.ui.repo == "Ridge-map" && tab.ui.history.len() == 4 && tab.ui.refs.lists(Kind::Branch, "feature/x")
        })
        .await?;

        self.action(Action::Select(Selection::Commit(trail.clone())))?;
        self.select_commit(&ridge).await?;
        self.until("the ridge's branches and first tag in its details", |tab| {
            tab.ui.containing.found(&ridge).is_some() && !tab.ui.containing.running()
        })
        .await?;
        let (summary, first, names, people, coauthors, stale) = self.tab.read_with(self.cx, |tab, _| {
            let found = tab.ui.containing.found(&ridge).cloned().unwrap_or_default();
            (
                containing::summary(&found),
                containing::first_in(&found),
                found.branches.iter().map(|holder| (holder.kind, holder.name.clone())).collect::<Vec<_>>(),
                inspection::people(&tab.ui.inspection)
                    .into_iter()
                    .map(|person| (person.name, person.email, person.when))
                    .collect::<Vec<_>>(),
                tab.ui.inspection.coauthors.iter().map(|author| (author.name.clone(), author.email.clone())).collect::<Vec<_>>(),
                tab.ui.containing.found(&trail).is_some(),
            )
        })?;
        if summary != "On main, alpha and 3 more" || first.as_deref() != Some("First in v1.2") {
            return Err(format!("The ridge's details read {summary:?} and {first:?}").into());
        }
        let mut listed_names: Vec<_> = names.iter().map(|name| name.1.clone()).collect();
        listed_names.sort();
        if listed_names != expected || names.first() != Some(&(Kind::Branch, "main".to_owned())) || names.last().map(|name| name.0) != Some(Kind::Remote) {
            return Err(format!("The ridge's details list {names:?}, Git lists {expected:?}").into());
        }
        if stale {
            return Err("The first selection's branches outlived the second selection".into());
        }
        let shown: Vec<_> = people.iter().map(|person| (person.0.as_str(), person.1.as_str())).collect();
        if shown != [("Juniper Fox", "juniper@example.invalid"), ("Rowan Ash", "rowan@example.invalid")]
            || !people[0].2.starts_with("authored ")
            || !people[1].2.starts_with("committed ")
            || coauthors != [("Ada Lovelace".to_owned(), "ada@example.invalid".to_owned())]
        {
            return Err(format!("The ridge's details name {people:?} with co-authors {coauthors:?}").into());
        }

        git(&root, &["branch", "delta", &summit])?;
        self.action(Action::Refresh)?;
        self.until("the new branch counted in the ridge's details", |tab| {
            tab.ui.containing.found(&ridge).is_some_and(|found| containing::summary(found) == "On main, alpha and 4 more")
                && !tab.ui.containing.running()
        })
        .await?;
        if holders(&ridge)?.len() != 6 {
            return Err("Git does not count six branches holding the ridge".into());
        }

        self.select_commit(&trail).await?;
        self.until("the trail's details", |tab| tab.ui.containing.found(&trail).is_some()).await?;
        let trail_first = self.tab.read_with(self.cx, |tab, _| tab.ui.containing.found(&trail).and_then(containing::first_in))?;
        if trail_first.as_deref() != Some("First in v1.0") {
            return Err(format!("The trail's details read {trail_first:?}").into());
        }
        let one_person = self.tab.read_with(self.cx, |tab, _| inspection::people(&tab.ui.inspection).len())?;
        if one_person != 1 {
            return Err(format!("The trail's details name {one_person} people").into());
        }

        self.action(Action::Containing(containing::Action::Show(Kind::Branch, "alpha".into())))?;
        self.until("alpha's commit selected from the details", |tab| {
            tab.ui.selection == Some(Selection::Commit(ridge.clone()))
        })
        .await?;
        self.pass();
        Ok(())
    }

    fn graph_menu(&mut self, target: Target) -> Result<Vec<(&'static str, String, Option<bool>, Option<Action>)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            commands::menu(&tab.ui, &target, cx)
                .into_iter()
                .map(|entry| (entry.id, entry.title, entry.check, entry.action))
                .collect()
        })?)
    }

    fn choose_in_graph_menu(&mut self, target: Target, id: &str, title: &str) -> Result<()> {
        let rows = self.graph_menu(target.clone())?;
        let row = rows.iter().find(|row| row.0 == id);
        let Some((_, shown, _, Some(action))) = row else {
            let listed: Vec<_> = rows.iter().map(|row| (row.0, row.1.clone())).collect();
            return Err(format!("The menu of {target:?} has no usable {id}: {listed:?}").into());
        };
        if shown != title {
            return Err(format!("The menu of {target:?} names {id} {shown:?}, not {title:?}").into());
        }
        self.action(action.clone())
    }

    fn remembered(&mut self) -> Result<Option<serde_json::Value>> {
        self.cx.update(|cx| settings::flush(cx))?;
        let common = self.tab.read_with(self.cx, |tab, _| tab.repo.as_ref().map(|repo| repo.common_dir().to_owned()))?;
        remembered_view(&common.ok_or("No repository is open")?)
    }

    async fn hide_a_branch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0809");
        let root = init(dir, "Spur-camp")?;
        record(&root, "camp.txt", "Camp\n", "Pitch the spur camp")?;
        git(&root, &["switch", "-qc", "spur"])?;
        record(&root, "spur.txt", "Scout\n", "Scout the spur")?;
        record(&root, "spur.txt", "Scout\nMap\n", "Map the spur")?;
        let spur = (oid(&root, "spur~1")?, oid(&root, "spur")?);
        git(&root, &["switch", "-q", "main"])?;
        record(&root, "camp.txt", "Camp\nStove\n", "Light the stove")?;
        fs::write(root.join("camp.txt"), "Camp\nStove\nSocks\n")?;
        git(&root, &["stash", "push", "-q", "-m", "Dry the socks"])?;
        let stash = oid(&root, "refs/stash")?;
        let everything = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        self.open(&root)?;
        self.until("Spur-camp open with its stash in the graph", |tab| {
            tab.ui.repo == "Spur-camp"
                && graph_rows(tab) == everything
                && tab.ui.stash.placed() == 1
                && tab.ui.refs.lists(Kind::Branch, "spur")
        })
        .await?;
        let spur_menu = self.graph_menu(Target::Branch("spur".into()))?;
        if !spur_menu.iter().any(|row| row.0 == "graph.solo" && row.1 == "Solo") || spur_menu.iter().any(|row| row.0 == "graph.show") {
            return Err(format!("Before hiding, spur's menu lists {:?}", spur_menu.iter().map(|row| row.0).collect::<Vec<_>>()).into());
        }
        self.choose_in_graph_menu(Target::Branch("spur".into()), "graph.hide", "Hide in the graph")?;
        let without = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/heads/spur", "--exclude=refs/stash", "--all", "HEAD"])?;
        if without.contains(&spur.0) || without.contains(&spur.1) || without.len() != 2 {
            return Err(format!("Git lists {without:?} with spur excluded").into());
        }
        self.until("the spur commits gone from the graph and the strip saying one branch is hidden", |tab| {
            graph_rows(tab) == without && strip_reads(tab) == "1 branch hidden"
        })
        .await?;
        let (labels, kept, dimmed) = self.tab.read_with(self.cx, |tab, _| {
            (graph_labels(tab), in_sidebar(tab, Kind::Branch, "spur"), graph_view::left_out(&tab.ui, "refs/heads/spur"))
        })?;
        if labels.iter().any(|label| label.0 == "spur") || !kept || !dimmed {
            return Err(format!("With spur hidden the labels are {labels:?}, the sidebar lists it {kept}, dimmed {dimmed}").into());
        }
        let shown_again = self.graph_menu(Target::Branch("spur".into()))?;
        if !shown_again.iter().any(|row| row.0 == "graph.show" && row.1 == "Show in the graph") {
            return Err("A hidden branch's menu does not offer Show in the graph".into());
        }

        self.choose_in_graph_menu(Target::Branch("main".into()), "graph.hide", "Hide in the graph")?;
        let both = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/heads/main", "--exclude=refs/heads/spur", "--exclude=refs/stash", "--all", "HEAD"])?;
        if both != without {
            return Err(format!("With main hidden too Git lists {both:?}").into());
        }
        self.until("main's commits still listed through HEAD with two branches hidden", |tab| {
            tab.ui.graph_view.hidden.len() == 2 && graph_rows(tab) == both && strip_reads(tab) == "2 branches hidden"
        })
        .await?;
        let head_label = self.tab.read_with(self.cx, |tab, _| graph_labels(tab))?;
        if !head_label.iter().any(|label| label.0 == "main") {
            return Err(format!("The checked-out branch lost its label: {head_label:?}").into());
        }

        let stash_menu = self.graph_menu(Target::Section(refs::Section::Stashes))?;
        if !stash_menu.iter().any(|row| row.0 == "graph.stashes" && row.1 == "Show stashes in the graph" && row.2 == Some(true)) {
            return Err(format!("The Stashes heading's menu lists {:?}", stash_menu.iter().map(|row| (row.0, row.2)).collect::<Vec<_>>()).into());
        }
        self.choose_in_graph_menu(Target::Section(refs::Section::Stashes), "graph.stashes", "Show stashes in the graph")?;
        self.until("the stash row gone and the strip saying so", |tab| {
            tab.ui.stash.placed() == 0
                && tab.ui.history_items() == tab.ui.history.len()
                && tab.ui.stash.entries.iter().any(|entry| entry.oid == stash)
                && strip_reads(tab) == "2 branches hidden · stashes hidden"
        })
        .await?;
        let stored = self.remembered()?.ok_or("Nothing is remembered for Spur-camp")?;
        if stored["hidden"] != serde_json::json!(["refs/heads/main", "refs/heads/spur"]) || stored["hide_stashes"] != true {
            return Err(format!("state.json remembers the view {stored}").into());
        }

        self.relaunch().await?;
        self.until("Spur-camp reopened with both branches and the stash still left out", |tab| {
            tab.ui.repo == "Spur-camp"
                && tab.ui.graph_view.hidden.len() == 2
                && tab.ui.graph_view.hide_stashes
                && graph_rows(tab) == both
                && !tab.ui.stash.entries.is_empty()
                && tab.ui.stash.placed() == 0
                && strip_reads(tab) == "2 branches hidden · stashes hidden"
        })
        .await?;

        self.choose_in_graph_menu(Target::Branch("spur".into()), "graph.show", "Show in the graph")?;
        let with_spur = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/heads/main", "--exclude=refs/stash", "--all", "HEAD"])?;
        self.until("both spur commits back", |tab| {
            graph_rows(tab) == with_spur && graph_rows(tab).contains(&spur.0) && strip_reads(tab) == "1 branch hidden · stashes hidden"
        })
        .await?;
        self.action(Action::GraphView(graph_view::Action::Clear))?;
        self.until("every commit and the stash back with no strip", |tab| {
            !tab.ui.graph_view.active() && graph_rows(tab) == everything && tab.ui.stash.placed() == 1 && graph_view::summary(&tab.ui).is_empty()
        })
        .await?;
        if let Some(stored) = self.remembered()? {
            return Err(format!("After Clear state.json still remembers {stored}").into());
        }
        self.pass();
        Ok(())
    }

    async fn solo_a_branch(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0810");
        let origin = dir.join("Solo-ridge-origin.git");
        fs::create_dir(&origin)?;
        git(&origin, &["init", "-q", "--bare", "-b", "main"])?;
        let root = init(dir, "Solo-ridge")?;
        let start = 1_790_000_000;
        commit_dated(&root, "trail.txt", "Leave the trailhead", start, &[])?;
        git(&root, &["remote", "add", "origin", &origin.display().to_string()])?;
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        let juniper = [("GIT_AUTHOR_NAME", "Juniper Fox"), ("GIT_AUTHOR_EMAIL", "juniper@example.invalid")];
        git(&root, &["switch", "-qc", "ridge"])?;
        commit_dated(&root, "ridge.txt", "Climb the ridge", start + 60, &juniper)?;
        commit_dated(&root, "ridge.txt", "Reach the ridge top", start + 120, &juniper)?;
        git(&root, &["switch", "-qc", "valley", "main"])?;
        let valley = commit_dated(&root, "valley.txt", "Wade the valley", start + 180, &[])?;
        git(&root, &["switch", "-qc", "afar", "main"])?;
        let afar = commit_dated(&root, "afar.txt", "Survey from afar", start + 240, &[])?;
        git(&root, &["update-ref", "refs/remotes/origin/main", &afar])?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["branch", "-qD", "afar"])?;
        commit_dated(&root, "trail.txt", "Brew coffee", start + 300, &[])?;
        git_is(&root, &["rev-parse", "--abbrev-ref", "main@{upstream}"], "origin/main\n")?;
        let everything = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        self.open(&root)?;
        self.until("Solo-ridge open with every branch", |tab| {
            tab.ui.repo == "Solo-ridge"
                && graph_rows(tab) == everything
                && tab.ui.refs.branch("main").is_some_and(|item| item.upstream.as_deref() == Some("origin/main"))
                && tab.ui.refs.lists(Kind::Branch, "valley")
        })
        .await?;

        self.choose_in_graph_menu(Target::Branch("ridge".into()), "graph.solo", "Solo")?;
        let ridge = listed(&root, &["rev-list", "--topo-order", "ridge", "HEAD", "--"])?;
        self.until("only ridge's and HEAD's history listed", |tab| {
            graph_rows(tab) == ridge && strip_reads(tab) == "showing only ridge"
        })
        .await?;
        let (labels, dimmed) = self.tab.read_with(self.cx, |tab, _| {
            (graph_labels(tab), graph_view::left_out(&tab.ui, "refs/heads/valley") && in_sidebar(tab, Kind::Branch, "valley"))
        })?;
        if ridge.contains(&valley) || labels.iter().any(|label| label.0 == "valley") || !dimmed {
            return Err(format!("While soloing ridge the labels are {labels:?} and valley is dimmed {dimmed}").into());
        }
        let ridge_menu = self.graph_menu(Target::Branch("ridge".into()))?;
        if !ridge_menu.iter().any(|row| row.0 == "graph.unsolo" && row.1 == "Stop soloing") {
            return Err("A soloed branch's menu does not offer Stop soloing".into());
        }

        self.choose_in_graph_menu(Target::Branch("valley".into()), "graph.solo", "Solo")?;
        let two = listed(&root, &["rev-list", "--topo-order", "ridge", "valley", "HEAD", "--"])?;
        self.until("ridge, valley and HEAD listed", |tab| {
            graph_rows(tab) == two && strip_reads(tab) == "showing only ridge and valley"
        })
        .await?;
        let title = self.graph_menu(Target::Repo)?.into_iter().find(|row| row.0 == "graph.stop-soloing").map(|row| row.1);
        if title.as_deref() != Some("Stop soloing") {
            return Err(format!("The repository commands name stop soloing {title:?}").into());
        }
        self.action(Action::Run("graph.stop-soloing"))?;
        self.until("every commit back after soloing", |tab| {
            graph_rows(tab) == everything && graph_view::summary(&tab.ui).is_empty()
        })
        .await?;

        let named = |ui: &view::Ui| commands::find("graph.only-current").map(|command| (command.title)(ui, &Target::Repo));
        let before = self.tab.read_with(self.cx, |tab, _| named(&tab.ui))?;
        if before.as_deref() != Some("Show only the current branch") {
            return Err(format!("The View command reads {before:?}").into());
        }
        self.action(Action::Run("graph.only-current"))?;
        let current = listed(&root, &["rev-list", "--topo-order", "HEAD", "origin/main", "--"])?;
        if !current.contains(&afar) || current.contains(&valley) {
            return Err(format!("Git lists {current:?} for HEAD and its upstream").into());
        }
        self.until("only HEAD and its upstream listed", |tab| {
            graph_rows(tab) == current && strip_reads(tab) == "showing only main and its upstream"
        })
        .await?;
        let after = self.tab.read_with(self.cx, |tab, _| named(&tab.ui))?;
        if after.as_deref() != Some("Show all branches") {
            return Err(format!("With only the current branch shown the View command reads {after:?}").into());
        }
        self.action(Action::Run("graph.only-current"))?;
        self.until("every commit back after the current branch view", |tab| {
            graph_rows(tab) == everything && !tab.ui.graph_view.active()
        })
        .await?;

        self.action(Action::GraphView(graph_view::Action::OpenAuthors))?;
        self.until("the author filter open", |tab| tab.ui.author_picker.open()).await?;
        self.tab.update(self.cx, |tab, window, cx| {
            tab.ui.author_picker.filter.update(cx, |field, cx| field.replace_text_in_range(None, "juni", window, cx))
        })?;
        let offered = self.tab.read_with(self.cx, |tab, cx| graph_view::matching(&tab.ui, cx))?;
        if offered.iter().map(|person| person.name.as_str()).collect::<Vec<_>>() != ["Juniper Fox"] {
            return Err(format!("Typing juni offers {offered:?}").into());
        }
        self.key("enter")?;
        let by_juniper: Vec<String> =
            listed(&root, &["log", "--format=%H", "--author=juniper@example.invalid", "--exclude=refs/stash", "--all"])?;
        if by_juniper.len() != 2 {
            return Err(format!("Git finds {by_juniper:?} by Juniper Fox").into());
        }
        self.until("other authors dimmed with every commit still listed", |tab| {
            let view = &tab.ui.graph_view;
            !tab.ui.author_picker.open()
                && view.author.as_ref().is_some_and(|person| person.name == "Juniper Fox")
                && graph_rows(tab) == everything
                && tab.ui.history.iter().all(|row| view.dims(&row.author, &row.email) != by_juniper.contains(&row.oid))
                && strip_reads(tab) == "commits by Juniper Fox"
        })
        .await?;
        self.action(Action::GraphView(graph_view::Action::Clear))?;
        self.until("nothing dimmed and no strip", |tab| {
            !tab.ui.graph_view.active() && tab.ui.history.iter().all(|row| !tab.ui.graph_view.dims(&row.author, &row.email))
        })
        .await?;
        if let Some(stored) = self.remembered()? {
            return Err(format!("After Clear state.json still remembers {stored}").into());
        }
        self.pass();
        Ok(())
    }

    async fn hide_a_remote(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP0811");
        let root = init(dir, "Two-remotes")?;
        record(&root, "camp.txt", "Camp\n", "Pitch the two camps")?;
        for remote in ["origin", "upstream"] {
            let bare = dir.join(format!("Two-remotes-{remote}.git"));
            fs::create_dir(&bare)?;
            git(&bare, &["init", "-q", "--bare", "-b", "main"])?;
            git(&root, &["remote", "add", remote, &bare.display().to_string()])?;
        }
        git(&root, &["push", "-q", "-u", "origin", "main"])?;
        git(&root, &["push", "-q", "upstream", "main"])?;
        git(&root, &["switch", "-qc", "trail"])?;
        record(&root, "trail.txt", "Trail\n", "Mark the trail")?;
        git(&root, &["push", "-q", "origin", "trail"])?;
        git(&root, &["switch", "-qc", "scout", "main"])?;
        record(&root, "scout.txt", "Scout\n", "Scout ahead")?;
        let scout = oid(&root, "HEAD")?;
        git(&root, &["push", "-q", "origin", "scout"])?;
        git(&root, &["switch", "-q", "main"])?;
        git(&root, &["branch", "-qD", "scout"])?;
        let everything = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/stash", "--all"])?;
        self.open(&root)?;
        self.until("Two-remotes open with both remotes", |tab| {
            tab.ui.repo == "Two-remotes"
                && graph_rows(tab) == everything
                && tab.ui.refs.remotes.len() == 2
                && tab.ui.refs.lists(Kind::Remote, "origin/scout")
                && tab.ui.refs.branch("main").is_some_and(|item| item.upstream.as_deref() == Some("origin/main"))
        })
        .await?;
        let mut labels = self.tab.read_with(self.cx, |tab, _| graph_labels(tab))?;
        labels.sort();
        let pair = |name: &str, remote: Option<&str>| (name.to_owned(), remote.map(str::to_owned));
        let all_labels = vec![
            pair("main", Some("origin/main")),
            pair("origin/scout", None),
            pair("origin/trail", None),
            pair("trail", None),
            pair("upstream/main", None),
        ];
        if labels != all_labels {
            return Err(format!("Before hiding, the labels are {labels:?}").into());
        }

        self.choose_in_graph_menu(Target::Remote("origin".into()), "graph.hide", "Hide in the graph")?;
        let without = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/remotes/origin/*", "--exclude=refs/stash", "--all", "HEAD"])?;
        if without.contains(&scout) {
            return Err("Git still lists the scout commit with origin excluded".into());
        }
        self.until("origin's own commit gone and the strip naming the remote", |tab| {
            graph_rows(tab) == without && strip_reads(tab) == "remote origin hidden"
        })
        .await?;
        let (mut labels, listed_branches, dimmed) = self.tab.read_with(self.cx, |tab, _| {
            (
                graph_labels(tab),
                ["origin/main", "origin/trail", "origin/scout"].iter().all(|name| in_sidebar(tab, Kind::Remote, name)),
                graph_view::left_out(&tab.ui, "refs/remotes/origin/") && graph_view::left_out(&tab.ui, "refs/remotes/origin/trail"),
            )
        })?;
        labels.sort();
        if labels != [pair("main", None), pair("trail", None), pair("upstream/main", None)] || !listed_branches || !dimmed {
            return Err(format!("With origin hidden the labels are {labels:?}, its branches listed {listed_branches}, dimmed {dimmed}").into());
        }

        self.choose_in_graph_menu(Target::RemoteBranch("origin/trail".into()), "graph.show", "Show in the graph")?;
        self.until("origin shown again", |tab| graph_rows(tab) == everything && !tab.ui.graph_view.active()).await?;
        let mut labels = self.tab.read_with(self.cx, |tab, _| graph_labels(tab))?;
        labels.sort();
        if labels != all_labels {
            return Err(format!("With origin shown again the labels are {labels:?}").into());
        }

        let local = listed(&root, &["rev-list", "--topo-order", "--exclude=refs/remotes/*", "--exclude=refs/stash", "--all", "HEAD"])?;
        self.choose_in_graph_menu(Target::Section(refs::Section::Remotes), "graph.hide-section", "Hide all remotes")?;
        self.until("no remote labels with every remote hidden", |tab| {
            strip_reads(tab) == "all remotes hidden" && graph_rows(tab) == local
        })
        .await?;
        let mut labels = self.tab.read_with(self.cx, |tab, _| graph_labels(tab))?;
        labels.sort();
        if labels != [pair("main", None), pair("trail", None)] {
            return Err(format!("With every remote hidden the labels are {labels:?}").into());
        }
        self.choose_in_graph_menu(Target::Section(refs::Section::Remotes), "graph.show-section", "Show all remotes")?;
        self.until("every remote back", |tab| graph_rows(tab) == everything && !tab.ui.graph_view.active()).await?;
        let mut labels = self.tab.read_with(self.cx, |tab, _| graph_labels(tab))?;
        labels.sort();
        if labels != all_labels {
            return Err(format!("With every remote shown again the labels are {labels:?}").into());
        }
        if let Some(stored) = self.remembered()? {
            return Err(format!("After showing every remote state.json still remembers {stored}").into());
        }
        self.pass();
        Ok(())
    }
}
