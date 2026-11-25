use super::*;
use crate::{graph_view, notice, refs::Kind, reveal, search, theme::ROW};
use std::{io::Write, process::Stdio};

const DEPTH: usize = 5_000;
const DEEP_COMMITS: usize = 6_000;
const SEARCHED_COMMITS: usize = 400;
const RIDGE_COMMITS: usize = 300;

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

fn in_view(tab: &Tab, oid: &str) -> bool {
    let Some(index) = tab.ui.history.iter().position(|row| row.oid == oid) else {
        return false;
    };
    let row = tab.ui.stash.commit_row(index) as f32;
    let scroll = tab.ui.history_scroll.0.borrow();
    if scroll.deferred_scroll_to_item.is_some() {
        return false;
    }
    let top = -f32::from(scroll.base_handle.offset().y);
    let height = f32::from(scroll.base_handle.bounds().size.height);
    height > ROW && row * ROW >= top && (row + 1.) * ROW <= top + height
}

fn offered<'a>(tab: &'a Tab, title: &str) -> Option<&'a notice::Notice> {
    tab.ui.notices.items().iter().find(|notice| notice.title == title)
}

fn listed(root: &Path, args: &[&str]) -> Result<Vec<String>> {
    Ok(git(root, args)?.lines().map(str::to_owned).collect())
}

impl Replay<'_> {
    pub(super) async fn gap08(&mut self, dir: &Path) -> Result<()> {
        self.jump_down_to_a_tag(dir).await?;
        self.jump_to_a_hidden_commit(dir).await?;
        self.search_by_author(dir).await
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
}
