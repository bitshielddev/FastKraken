mod acceptance;
mod activity;
mod address;
mod askpass;
mod assets;
mod auth;
mod autofetch;
mod autostash;
mod branch;
mod cli;
mod clone;
mod commands;
mod commit_options;
mod conflict;
mod connect;
mod credentials;
mod diff_doc;
mod discard;
mod environment;
mod explain;
mod github;
mod guard;
mod handoff;
mod help;
mod hooks;
mod identity;
mod ignore;
mod init;
mod instance;
mod inspection;
mod jobs;
mod kit;
mod merge;
mod navigate;
mod notice;
mod operation;
mod palette;
mod perf;
mod pick;
mod picked;
mod preferences;
mod rebase;
mod prompt;
mod publish;
mod refs;
mod remotes;
mod repository;
mod reverse;
mod scene;
mod scrollbar;
mod search;
mod settings;
mod start;
mod stash;
mod storage;
mod sync;
mod tabs;
mod tag;
mod tracking;
mod theme;
mod tools;
mod view;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Application, Bounds, Context, Focusable, IntoElement, PathPromptOptions, Render, Window,
    WindowBounds, WindowOptions, px, size,
};
use repository::{Diff, Repository};
use view::{
    Action, Desk, DiffLayout, Discard, DiscardAsk, FileRow, Selection, Tone, Ui,
};

const PAGE: usize = 250;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Follow {
    #[default]
    Nothing,
    Head,
    ChangesOrHead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    Head,
    Changes,
}

impl Follow {
    fn head_if(select_head: bool) -> Self {
        if select_head { Self::Head } else { Self::Nothing }
    }

    fn landing(self, carried: bool) -> Option<Landing> {
        match self {
            Self::Nothing => None,
            Self::Head => Some(Landing::Head),
            Self::ChangesOrHead if carried => Some(Landing::Changes),
            Self::ChangesOrHead => Some(Landing::Head),
        }
    }
}

struct Tab {
    ui: Ui,
    repo: Option<Repository>,
    diff: Option<Arc<Diff>>,
    epoch: u64,
    opening_ticket: u64,
    history_ticket: u64,
    status_ticket: u64,
    diff_ticket: u64,
    loading_page: bool,
    loading_status: bool,
    status_dirty: bool,
    refs_dirty: bool,
    follow: Follow,
    waiting: Option<Box<dyn FnOnce(&mut Tab, &mut Context<Tab>)>>,
    queued: Option<Box<dyn FnOnce(&mut Tab, &mut Context<Tab>)>>,
    trail: activity::Trail,
    acting: Option<Action>,
    watcher: Option<notify::RecommendedWatcher>,
    bench: Option<perf::Run>,
    front: bool,
    stale: Option<bool>,
    transient: bool,
}

impl Tab {
    fn new(cx: &mut Context<Self>, bench: Option<perf::Run>) -> Self {
        Self {
            ui: Ui::new(cx),
            repo: None,
            diff: None,
            epoch: 0,
            opening_ticket: 0,
            history_ticket: 0,
            status_ticket: 0,
            diff_ticket: 0,
            loading_page: false,
            loading_status: false,
            status_dirty: false,
            refs_dirty: false,
            follow: Follow::Nothing,
            waiting: None,
            queued: None,
            trail: activity::Trail::default(),
            acting: None,
            watcher: None,
            bench,
            front: true,
            stale: None,
            transient: false,
        }
    }

    fn work<R: Send + 'static>(
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        done: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
    ) {
        let task = cx.background_executor().spawn(async move { activity::traced(work) });
        cx.spawn(async move |tab, cx| {
            let (result, trail) = task.await;
            let _ = tab.update(cx, |tab, cx| {
                let outer = std::mem::replace(&mut tab.trail, trail);
                done(tab, result, cx);
                tab.trail = outer;
            });
        })
        .detach();
    }

    fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.open_then(path, |_, _| {}, cx);
    }

    fn open_then(
        &mut self,
        path: PathBuf,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = tabs::holding(&path, cx.entity_id(), cx) {
            tabs::ask(cx, tabs::Request::Show(index));
            return;
        }
        if !self.can_open() {
            if self.repo.is_some() && tabs::present(cx) {
                tabs::ask(cx, tabs::Request::Open { path, then: Box::new(then) });
                return;
            }
            self.refuse(self.open_refusal(), cx);
            return;
        }
        self.opening_ticket += 1;
        let ticket = self.opening_ticket;
        self.ui.opening = Some(path.display().to_string());
        cx.notify();
        let asked = path.clone();
        Self::work(
            cx,
            move || {
                let repo = Repository::open(&path).map_err(|error| init::Unopenable::of(&path, error))?;
                let watcher = watch::subscribe(&repo.root, repo.watch_paths());
                let configured = sync::configured(&repo);
                let fetched = autofetch::fetched_at(&repo);
                Ok::<_, init::Unopenable>((repo, watcher, configured, fetched))
            },
            move |tab, result, cx| {
                if tab.opening_ticket != ticket {
                    return;
                }
                tab.ui.opening = None;
                let (repo, subscription, configured, fetched) = match result {
                    Ok(opened) => opened,
                    Err(why) => {
                        tab.transient = false;
                        init::refused(tab, asked, why, cx);
                        return;
                    }
                };
                let me = cx.entity_id();
                if let Some(index) = tabs::holding(&repo.root, me, cx) {
                    if std::mem::take(&mut tab.transient) && tab.repo.is_none() {
                        tabs::ask(cx, tabs::Request::Close(me));
                    }
                    tabs::ask(cx, tabs::Request::Show(index));
                    cx.notify();
                    return;
                }
                if !tab.can_open() {
                    tab.refuse(tab.open_refusal(), cx);
                    return;
                }
                autofetch::stop(tab);
                tab.transient = false;
                tab.stale = None;
                tab.epoch += 1;
                let epoch = tab.epoch;
                tab.watcher = None;
                tab.diff = None;
                tab.loading_page = false;
                tab.loading_status = false;
                tab.status_dirty = false;
                tab.refs_dirty = false;
                tab.follow = Follow::Head;
                tab.ui = Ui::new(cx);
                tab.ui.root = repo.root.clone();
                tab.ui.repo = repo
                    .root
                    .file_name()
                    .unwrap_or(repo.root.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                if let Some(notice) = settings::take_startup_notice(cx) {
                    tab.ui.say(Tone::Info, notice);
                }
                tab.ui
                    .refs
                    .restore(settings::memory(cx).and_then(|m| m.repo(repo.common_dir())));
                start::remember(&repo.root, cx);
                tab.repo = Some(repo);
                tab.ui.autofetch.fetched = fetched;
                autofetch::arm(tab, cx);
                sync::restore(tab, configured, cx);
                merge::restore(tab, cx);
                commit_options::restore(tab, cx);
                commit_options::load_template(tab, cx);
                refs::refresh(tab, cx);
                stash::refresh(tab, cx);
                operation::refresh(tab, cx);
                tab.load_history(true, cx);
                tab.load_status(cx);
                match subscription {
                    Ok((watcher, changes)) => {
                        tab.watcher = Some(watcher);
                        cx.spawn(async move |tab, cx| {
                            while changes.ready.recv().await.is_ok() {
                                cx.background_executor()
                                    .timer(Duration::from_millis(90))
                                    .await;
                                let refs_changed = changes.take();
                                let alive = tab.update(cx, |tab, cx| {
                                    if tab.epoch != epoch {
                                        return false;
                                    }
                                    if tab.ui.busy() {
                                        tab.refs_dirty |= refs_changed;
                                    } else {
                                        tab.refresh(refs_changed, cx);
                                    }
                                    true
                                });
                                if !matches!(alive, Ok(true)) {
                                    break;
                                }
                            }
                        })
                        .detach();
                    }
                    Err(err) => {
                        tab.error(format!("Automatic refresh unavailable: {err}"), cx)
                    }
                }
                then(tab, cx);
                cx.notify();
            },
        );
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.repo.is_none() {
            return;
        }
        if !self.can_open() {
            let reason = match self.ui.jobs.refusal() {
                Some(running) => format!("{running}. Cancel it or let it finish before closing {}.", self.ui.repo),
                None => "Wait for the current operation to finish".to_owned(),
            };
            self.refuse(reason, cx);
            return;
        }
        autofetch::stop(self);
        self.opening_ticket += 1;
        self.epoch += 1;
        self.watcher = None;
        self.diff = None;
        self.repo = None;
        self.loading_page = false;
        self.loading_status = false;
        self.status_dirty = false;
        self.refs_dirty = false;
        self.follow = Follow::Nothing;
        self.stale = None;
        self.ui = Ui::new(cx);
        cx.notify();
    }

    fn error(&mut self, message: String, cx: &mut Context<Self>) {
        let notice = explain::notice(message).logged(self.trail.entry(true));
        self.ui.notices.show(notice);
        cx.notify();
    }

    fn load_history(&mut self, reset: bool, cx: &mut Context<Self>) {
        if !reset && (self.loading_page || !self.ui.has_more) {
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let offset = if reset { 0 } else { self.ui.history.len() };
        self.history_ticket += 1;
        let ticket = self.history_ticket;
        let epoch = self.epoch;
        self.loading_page = true;
        Self::work(
            cx,
            move || repo.history(offset, PAGE + 1),
            move |tab, result, cx| {
                if tab.epoch != epoch || ticket != tab.history_ticket {
                    return;
                }
                tab.loading_page = false;
                match result {
                    Ok(mut page) => {
                        tab.ui.has_more = page.len() > PAGE;
                        page.truncate(PAGE);
                        if reset {
                            tab.ui.replace_history(page);
                        } else {
                            tab.ui.append_history(page);
                        }
                        if let Some(Selection::Commit(oid)) = &tab.ui.selection {
                            if let Some(row) = tab.ui.history.iter().find(|row| &row.oid == oid) {
                                tab.ui.diff_title = row.subject.clone();
                            }
                        }
                        if tab.ui.selection.is_none() {
                            if let Some(first) = tab.ui.history.first() {
                                tab.select(Selection::Commit(first.oid.clone()), cx);
                            }
                        }
                        cx.notify();
                    }
                    Err(err) => {
                        tab.ui.has_more = false;
                        tab.error(err, cx);
                    }
                }
            },
        );
    }

    fn load_status(&mut self, cx: &mut Context<Self>) {
        if self.loading_status {
            self.status_dirty = true;
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.status_ticket += 1;
        let ticket = self.status_ticket;
        let epoch = self.epoch;
        self.loading_status = true;
        Self::work(
            cx,
            move || (repo.status(), identity::of(&repo)),
            move |tab, (result, identity), cx| {
                if tab.epoch != epoch || ticket != tab.status_ticket {
                    return;
                }
                tab.loading_status = false;
                if tab.ui.busy() {
                    tab.status_dirty = true;
                    return;
                }
                if std::mem::take(&mut tab.status_dirty) {
                    tab.load_status(cx);
                    return;
                }
                identity::landed(tab, identity, cx);
                match result {
                    Ok(status) => {
                        tab.ui.branch = status.branch;
                        tab.ui.detached = status.detached;
                        if tab.ui.operation.mark(status.operation) {
                            operation::refresh(tab, cx);
                        }
                        tab.ui.head = status.head.clone();
                        tab.ui.head_ref = status.head_ref;
                        tab.ui.upstream = status.upstream;
                        let carried = !status.unstaged.is_empty() || !status.staged.is_empty();
                        match std::mem::take(&mut tab.follow).landing(carried) {
                            Some(Landing::Head) => {
                                if let Some(oid) = status.head {
                                    if tab.ui.selection != Some(Selection::Commit(oid.clone())) {
                                        tab.select(Selection::Commit(oid), cx);
                                    }
                                }
                            }
                            Some(Landing::Changes) => {
                                if !tab.ui.working() {
                                    tab.select(Selection::Changes, cx);
                                }
                            }
                            None => {}
                        }
                        if tab.ui.head.is_none() && tab.ui.selection.is_none() {
                            tab.select(Selection::Changes, cx);
                        }
                        tab.ui.changed_paths = status.changed_paths;
                        tab.ui.files = [(false, status.unstaged), (true, status.staged)]
                            .into_iter()
                            .flat_map(|(staged, paths)| {
                                paths.into_iter().map(move |file| FileRow {
                                    path: file.path,
                                    staged,
                                    status: file.status,
                                    from: file.from,
                                })
                            })
                            .collect();
                        picked::refresh(&mut tab.ui);
                        if let Some(Selection::File { path, staged }) = tab.ui.selection.clone() {
                            if tab
                                .ui
                                .files
                                .iter()
                                .any(|f| f.path == path && f.staged == staged)
                            {
                                tab.select(Selection::File { path, staged }, cx);
                            } else if tab.ui.files.iter().any(|f| f.path == path) {
                                tab.select(
                                    Selection::File {
                                        path,
                                        staged: !staged,
                                    },
                                    cx,
                                );
                            } else if !tab.ui.files.is_empty() {
                                tab.ui.diff_open = false;
                                tab.select(Selection::Changes, cx);
                            } else if let Some(first) = tab.ui.history.first() {
                                tab.ui.diff_open = false;
                                tab.select(Selection::Commit(first.oid.clone()), cx);
                            } else if tab.ui.head.is_none() {
                                tab.ui.diff_open = false;
                                tab.select(Selection::Changes, cx);
                            } else {
                                tab.clear_diff();
                                tab.ui.selection = None;
                                inspection::selected(tab, None, cx);
                            }
                        }
                        cx.notify();
                    }
                    Err(err) => tab.error(err, cx),
                }
            },
        );
    }

    fn refresh(&mut self, refs_changed: bool, cx: &mut Context<Self>) {
        if !self.front {
            self.stale = Some(self.stale.unwrap_or(false) || refs_changed);
            cx.notify();
            return;
        }
        self.load_status(cx);
        if refs_changed || self.ui.operation.kind().is_some() {
            operation::refresh(self, cx);
        }
        if refs_changed {
            self.load_history(true, cx);
            refs::refresh(self, cx);
            stash::refresh(self, cx);
            search::refresh(self, true, cx);
        }
    }

    fn clear_diff(&mut self) {
        self.diff_ticket += 1;
        self.diff = None;
        self.ui.replace_diff(diff_doc::DiffDoc::default(), DiffLayout::default());
        self.ui.diff_title.clear();
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.repo.is_none() {
            return;
        }
        if !matches!(selection, Selection::File { .. })
            && self.ui.selection.as_ref() != Some(&selection)
        {
            self.ui.diff_open = false;
        }
        if !picked::kept_by(&self.ui, &selection) {
            self.ui.picked = None;
        }
        self.ui.selection = Some(selection.clone());
        stash::selected(
            self,
            match &selection {
                Selection::Stash(oid) => Some(oid),
                _ => None,
            },
            cx,
        );
        inspection::selected(
            self,
            match &selection {
                Selection::Commit(oid) => Some(oid.as_str()),
                Selection::Changes | Selection::File { .. } | Selection::Stash(_) => None,
            },
            cx,
        );
        if selection == Selection::Changes {
            self.clear_diff();
            cx.notify();
            return;
        }
        self.load_selected_patch(selection, self.ui.inspection.path.clone(), cx);
    }

    fn load_selected_patch(
        &mut self,
        selection: Selection,
        commit_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        self.clear_diff();
        self.ui.diff_title = match &selection {
            Selection::Commit(oid) => self
                .ui
                .history
                .iter()
                .find(|c| c.oid == *oid)
                .map(|c| c.subject.clone())
                .or_else(|| self.ui.inspection.subject.clone())
                .unwrap_or_else(|| oid.clone()),
            Selection::File { path, .. } => path.to_string_lossy().into_owned(),
            Selection::Stash(oid) => self.ui.stash.title(oid).unwrap_or(oid).to_owned(),
            Selection::Changes => return,
        };
        self.ui.diff.state = diff_doc::PatchState::Loading;
        cx.notify();
        let ticket = self.diff_ticket;
        let epoch = self.epoch;
        let from = self.ui.renamed_from(&selection, commit_path.as_deref());
        Self::work(
            cx,
            move || {
                let diff = match &selection {
                    Selection::Commit(oid) => match commit_path {
                        Some(path) => repo.commit_path_diff(oid, &path, from.as_deref()),
                        None => repo.commit_diff(oid),
                    },
                    Selection::File { path, staged } => match (&from, staged) {
                        (Some(from), true) => repo.staged_rename_diff(from, path),
                        _ => repo.file_diff(path, *staged),
                    },
                    Selection::Stash(oid) => match commit_path {
                        Some(path) => repo.stash_file_diff(oid, &path, from.as_deref()),
                        None => repo.stash_diff(oid),
                    },
                    Selection::Changes => return Err("Select a file to see its changes.".into()),
                }?;
                let blobs = diff_doc::binary_blobs(&diff.text);
                let (sizes, working_size) = if blobs.is_empty() {
                    Default::default()
                } else {
                    let working_size = match &selection {
                        Selection::File { path, staged: false } => std::fs::symlink_metadata(repo.root.join(path))
                            .ok()
                            .filter(|metadata| !metadata.is_dir())
                            .map(|metadata| metadata.len()),
                        _ => None,
                    };
                    (repo.object_sizes(&blobs).unwrap_or_default(), working_size)
                };
                let doc = diff_doc::DiffDoc::build(&diff_doc::Source {
                    text: &diff.text,
                    stageable: diff.stageable() && matches!(selection, Selection::File { .. }),
                    truncated: diff.truncated,
                    sizes: &sizes,
                    working_size,
                });
                let layout = DiffLayout::new(&doc);
                Ok::<_, String>((Arc::new(diff), doc, layout))
            },
            move |tab, result, cx| {
                if tab.epoch != epoch || tab.diff_ticket != ticket {
                    return;
                }
                match result {
                    Ok((diff, doc, layout)) => {
                        tab.diff = Some(diff);
                        tab.ui.replace_diff(doc, layout);
                    }
                    Err(err) => {
                        tab.ui.diff.state = diff_doc::PatchState::Unreadable(err.clone());
                        if !tab.ui.diff_open {
                            tab.error(err, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(Repository) -> Result<(), String> + Send + 'static,
    ) {
        self.apply_operation(cx, "Updating the staging area…", None, false, job, |_, _| {});
    }

    fn ask_discard(&mut self, discard: Discard, cx: &mut Context<Self>) {
        if !self.ui.can_edit_index() || self.ui.prompt.open() {
            return;
        }
        let ask = match discard {
            Discard::File(path) => {
                let Some(row) = self.ui.files.iter().find(|f| !f.staged && f.path == path) else {
                    return;
                };
                DiscardAsk::File {
                    untracked: row.status == "Untracked",
                    path,
                }
            }
            Discard::Hunk(index) => {
                let (Some(diff), Some(Selection::File { path, staged: false })) =
                    (self.diff.clone(), &self.ui.selection)
                else {
                    return;
                };
                if index >= diff.hunks.len() {
                    return;
                }
                DiscardAsk::Hunk {
                    path: path.clone(),
                    index,
                    diff,
                }
            }
        };
        prompt::ask(&mut self.ui, prompt::Kind::Discard(ask), cx);
    }

    fn discard(&mut self, ask: DiscardAsk, cx: &mut Context<Self>) {
        match ask {
            DiscardAsk::File { path, untracked } => self.apply_operation(
                cx,
                "Discarding changes…",
                Some("Changes discarded"),
                false,
                move |repo| repo.discard_file(&path, untracked),
                |_, _| {},
            ),
            DiscardAsk::Files { paths, untracked } => self.apply_operation(
                cx,
                "Discarding changes…",
                Some("Changes discarded"),
                false,
                move |repo| discard::discard_picked(&repo, paths, &untracked),
                |_, _| {},
            ),
            DiscardAsk::All(seen) => discard::confirm(self, seen, cx),
            DiscardAsk::Hunk { index, diff, .. } => self.apply_operation(
                cx,
                "Discarding the hunk…",
                Some("Hunk discarded"),
                false,
                move |repo| repo.discard_hunk(&diff, index),
                |_, _| {},
            ),
        }
    }

    fn toggle_amend(&mut self, cx: &mut Context<Self>) {
        if !self.ui.can_edit_index() || self.ui.prompt.open() || self.ui.refs.draft.is_some() {
            return;
        }
        if let Some(amend) = self.ui.amend.take() {
            self.ui.set_draft(&amend.previous, cx);
            cx.notify();
            return;
        }
        let (Some(repo), Some(head)) = (self.repo.clone(), self.ui.head.clone()) else {
            return;
        };
        let epoch = self.epoch;
        let reference = self.ui.head_ref.clone();
        self.ui.amend = Some(view::AmendDraft {
            head: head.clone(),
            reference: reference.clone(),
            previous: self.ui.draft(cx),
            original: None,
        });
        cx.notify();
        let oid = head.clone();
        Self::work(cx, move || repo.commit_message(&oid), move |tab, result, cx| {
            if tab.epoch != epoch || !tab.ui.amend.as_ref().is_some_and(|amend| {
                amend.original.is_none() && amend.head == head && amend.reference == reference
            }) {
                return;
            }
            match result {
                Ok(message) => {
                    let original = view::CommitDraft::from_message(&message);
                    tab.ui.set_draft(&original, cx);
                    tab.ui.amend.as_mut().unwrap().original = Some(original);
                }
                Err(error) => {
                    tab.ui.amend = None;
                    tab.ui.say(Tone::Error, error);
                }
            }
            cx.notify();
        });
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.ui.can_commit(cx) {
            return;
        }
        let draft = self.ui.draft(cx);
        let message = draft.message();
        let amend = self.ui.amend.as_ref().map(|amend| {
            let unchanged = amend.original.as_ref() == Some(&draft);
            (amend.head.clone(), amend.reference.clone(), unchanged)
        });
        let (progress, notice) = if amend.is_some() {
            view::focus(&self.ui, window);
            ("Amending the last commit…", "Last commit amended")
        } else {
            ("Committing…", "Changes committed")
        };
        let summary = draft.summary.trim().to_owned();
        let kept = autostash::Kept::default();
        let left = kept.clone();
        let shown = self.ui.identity.effective().cloned();
        let hooks = self.ui.commit_options.hooks();
        let push = commit_options::push_after(&self.ui, cx);
        let landed = Arc::new(std::sync::Mutex::new(None));
        let head = landed.clone();
        self.run_operation_or(
            cx,
            progress,
            notice,
            true,
            move |repo| {
                if let Some(shown) = &shown {
                    identity::still(&repo, shown)?;
                }
                let pending = autostash::pending(&repo);
                let committed = match amend {
                    Some((head, reference, unchanged)) => {
                        repo.amend_with(&head, reference.as_deref(), (!unchanged).then_some(message.as_str()), hooks)?
                    }
                    None => repo.commit_with(&message, hooks)?,
                };
                *landed.lock().unwrap_or_else(|e| e.into_inner()) = committed.head.clone();
                let outcome = hooks::told(notice::Outcome::titled(notice, summary), &committed);
                autostash::settled(&repo, pending, &kept, outcome)
            },
            move |tab, cx| {
                if let Some(amend) = tab.ui.amend.take() {
                    tab.ui.set_draft(&amend.previous, cx);
                } else if tab.ui.draft(cx) == draft {
                    tab.ui.input.update(cx, |input, cx| input.clear(cx));
                    tab.ui.description.update(cx, |input, cx| input.clear(cx));
                }
                let head = head.lock().unwrap_or_else(|e| e.into_inner()).take();
                commit_options::committed(tab, head, push, cx);
            },
            move |tab, _| left.remember(&mut tab.ui),
        );
    }

    fn commit_all(&mut self, cx: &mut Context<Self>) {
        if !self.ui.can_commit_all(cx) {
            return;
        }
        let draft = self.ui.draft(cx);
        let message = draft.message();
        let head = self.ui.head.clone();
        let reference = self.ui.head_ref.clone();
        let summary = draft.summary.trim().to_owned();
        let kept = autostash::Kept::default();
        let left = kept.clone();
        let shown = self.ui.identity.effective().cloned();
        let hooks = self.ui.commit_options.hooks();
        let push = commit_options::push_after(&self.ui, cx);
        let landed = Arc::new(std::sync::Mutex::new(None));
        let tip = landed.clone();
        self.run_operation_or(
            cx,
            "Staging everything and committing…",
            "Changes committed",
            true,
            move |repo| {
                if let Some(shown) = &shown {
                    identity::still(&repo, shown)?;
                }
                let pending = autostash::pending(&repo);
                let committed = repo.commit_all_with(head.as_deref(), reference.as_deref(), &message, hooks)?;
                *landed.lock().unwrap_or_else(|e| e.into_inner()) = committed.head.clone();
                let outcome = hooks::told(notice::Outcome::titled("Changes committed", summary), &committed);
                autostash::settled(&repo, pending, &kept, outcome)
            },
            move |tab, cx| {
                if tab.ui.draft(cx) == draft {
                    tab.ui.input.update(cx, |input, cx| input.clear(cx));
                    tab.ui.description.update(cx, |input, cx| input.clear(cx));
                }
                let head = tip.lock().unwrap_or_else(|e| e.into_inner()).take();
                commit_options::committed(tab, head, push, cx);
            },
            move |tab, _| left.remember(&mut tab.ui),
        );
    }

    pub(crate) fn settled(&self) -> bool {
        self.ui.jobs.idle() && self.waiting.is_none() && self.queued.is_none() && self.ui.opening.is_none()
    }

    pub(crate) fn can_open(&self) -> bool {
        self.ui.jobs.idle_but_background() && self.waiting.is_none() && self.queued.is_none()
    }

    fn taking_over(&self) -> bool {
        self.ui.jobs.background().is_some() && self.ui.jobs.refusal().is_none()
    }

    fn take_over(
        &mut self,
        kind: jobs::Kind,
        start: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(job) = self.ui.jobs.background() {
            job.link.cancel();
        }
        self.ui.jobs.queued = Some(kind);
        let acting = self.acting.clone();
        self.queued = Some(Box::new(move |tab, cx| {
            let outer = std::mem::replace(&mut tab.acting, acting);
            start(tab, cx);
            tab.acting = outer;
        }));
        cx.notify();
    }

    fn open_refusal(&self) -> String {
        match self.ui.jobs.refusal() {
            Some(running) => format!("{running}. Cancel it or let it finish before opening another repository."),
            None => "Wait for the current operation to finish".to_owned(),
        }
    }

    fn refuse(&mut self, reason: String, cx: &mut Context<Self>) {
        self.ui.notices.show(notice::Notice::new(Tone::Info, reason, ""));
        cx.notify();
    }

    fn run_network<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        spec: jobs::Spec,
        progress: &str,
        notice: &'static str,
        select_head: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        self.run_network_or(cx, spec, progress, notice, select_head, job, after_success, |_, _| {});
    }

    fn run_network_or<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        spec: jobs::Spec,
        progress: &str,
        notice: &'static str,
        select_head: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        after_failure: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        if self.taking_over() {
            let (kind, progress) = (spec.kind, progress.to_owned());
            let start = move |tab: &mut Self, cx: &mut Context<Self>| {
                tab.run_network_or(cx, spec, &progress, notice, select_head, job, after_success, after_failure)
            };
            return self.take_over(kind, start, cx);
        }
        let about = explain::About { push: spec.kind == jobs::Kind::Push, ..self.about(spec.remote.clone()) };
        let stop = (spec.kind == jobs::Kind::Push).then_some(&hooks::PUSHING);
        let Some((repo, link, id, spinner)) = self.start_network(spec, progress, cx) else { return };
        let work = move || {
            link.enter(|| {
                job(repo.clone()).map_err(|error| about.explain(&repo, hooks::named(error.into(), &link.ran(), stop)))
            })
        };
        Self::work(cx, work, move |tab, result, cx| {
            let Some(job) = tab.ui.jobs.end_network(id) else { return };
            let result = result.map(Into::into);
            askpass::poll(tab, cx);
            if result.is_err() && job.link.cancelled() {
                tab.ui.notices.finish(spinner, jobs::cancelled(&job.spec).logged(tab.trail.entry(true)));
                after_failure(tab, cx);
            } else if result.is_err() && job.link.declined() {
                tab.ui.notices.finish(spinner, declined());
                after_failure(tab, cx);
            } else if tab.conclude(Some(spinner), Some(notice), result) {
                tab.follow = Follow::head_if(select_head);
                after_success(tab, cx);
            } else {
                after_failure(tab, cx);
            }
            tab.refresh(true, cx);
            tab.resume(cx);
            cx.notify();
        });
    }

    fn start_network(
        &mut self,
        spec: jobs::Spec,
        progress: &str,
        cx: &mut Context<Self>,
    ) -> Option<(Repository, jobs::Link, u64, u64)> {
        if let Some(reason) = self.ui.jobs.refusal() {
            self.refuse(reason, cx);
            return None;
        }
        if spec.writes && self.ui.busy() {
            self.refuse("Wait for the current operation to finish".to_owned(), cx);
            return None;
        }
        let repo = self.repo.clone()?;
        let link = if spec.interactive { jobs::Link::interactive() } else { jobs::Link::new(true) };
        let spinner = self.ui.notices.progress(progress);
        let id = self.ui.jobs.start_network(spec, progress, link.clone(), Some(spinner));
        jobs::tick(&mut self.ui, Instant::now());
        jobs::watch(self, cx);
        cx.notify();
        Some((repo, link, id, spinner))
    }

    fn run_pull<F, R, E>(
        &mut self,
        cx: &mut Context<Self>,
        spec: jobs::Spec,
        progress: &'static str,
        notice: &'static str,
        select_head: bool,
        fetch: impl FnOnce(Repository) -> Result<F, notice::Failure> + Send + 'static,
        integrate: impl FnOnce(Repository, F, bool) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        after_failure: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        F: Send + 'static,
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        if self.taking_over() {
            let kind = spec.kind;
            let start = move |tab: &mut Self, cx: &mut Context<Self>| {
                tab.run_pull(cx, spec, progress, notice, select_head, fetch, integrate, after_success, after_failure)
            };
            return self.take_over(kind, start, cx);
        }
        let before = self.ui.jobs.writes;
        let about = self.about(spec.remote.clone());
        let Some((repo, link, id, spinner)) = self.start_network(spec, progress, cx) else { return };
        let explaining = about.clone();
        let work = move || link.enter(|| fetch(repo.clone()).map_err(|failure| explaining.explain(&repo, failure)));
        Self::work(cx, work, move |tab, result, cx| {
            let Some(job) = tab.ui.jobs.network.as_ref().filter(|job| job.id == id) else { return };
            match result {
                Ok(fetched) if !job.link.cancelled() => {
                    job.link.hold();
                    let busy = tab.ui.busy();
                    if let Some(job) = tab.ui.jobs.network.as_mut() {
                        job.waiting = busy;
                    }
                    jobs::tick(&mut tab.ui, Instant::now());
                    let start = move |tab: &mut Self, cx: &mut Context<Self>| {
                        let shown = (spinner, notice, select_head);
                        tab.integrate(id, before, shown, about, fetched, integrate, after_success, after_failure, cx)
                    };
                    if busy {
                        tab.waiting = Some(Box::new(start));
                    } else {
                        start(tab, cx);
                    }
                }
                result => {
                    let job = tab.ui.jobs.end_network(id);
                    askpass::poll(tab, cx);
                    match result {
                        Err(_) if job.as_ref().is_some_and(|job| job.link.declined() && !job.link.cancelled()) => {
                            tab.ui.notices.finish(spinner, declined());
                        }
                        Err(failure) if !job.as_ref().is_some_and(|job| job.link.cancelled()) => {
                            tab.conclude::<notice::Outcome>(Some(spinner), Some(notice), Err(failure));
                        }
                        _ => {
                            let spec = job.map_or_else(|| jobs::Spec::pull(None), |job| job.spec);
                            tab.ui.notices.finish(spinner, jobs::cancelled(&spec).logged(tab.trail.entry(true)));
                        }
                    }
                    after_failure(tab, cx);
                    tab.refresh(true, cx);
                    tab.resume(cx);
                }
            }
            cx.notify();
        });
    }

    fn integrate<F, R, E>(
        &mut self,
        id: u64,
        before: u64,
        (spinner, notice, select_head): (u64, &'static str, bool),
        about: explain::About,
        fetched: F,
        integrate: impl FnOnce(Repository, F, bool) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        after_failure: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) where
        F: Send + 'static,
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        let moved = self.ui.jobs.writes != before;
        let Some(repo) = self.repo.clone() else {
            self.ui.jobs.end_network(id);
            return;
        };
        let Some(job) = self.ui.jobs.network.as_mut().filter(|job| job.id == id) else { return };
        job.waiting = false;
        let (link, title) = (job.link.clone(), job.title.clone());
        self.ui.jobs.start_write(&title, link.clone(), None);
        jobs::tick(&mut self.ui, Instant::now());
        cx.notify();
        let work = move || link.enter(|| integrate(repo.clone(), fetched, moved).map_err(|error| about.explain(&repo, error.into())));
        Self::work(cx, work, move |tab, result, cx| {
            tab.ui.jobs.end_write();
            tab.ui.jobs.end_network(id);
            if tab.conclude(Some(spinner), Some(notice), result.map(Into::into)) {
                tab.follow = Follow::head_if(select_head);
                after_success(tab, cx);
            } else {
                after_failure(tab, cx);
            }
            tab.refs_dirty = false;
            tab.refresh(true, cx);
            tab.resume(cx);
            cx.notify();
        });
    }

    fn resume(&mut self, cx: &mut Context<Self>) {
        if !self.ui.busy() {
            if let Some(next) = self.waiting.take() {
                next(self, cx);
            }
        }
        autofetch::resume(self, cx);
    }

    fn about(&self, remote: Option<String>) -> explain::About {
        explain::About::new(remote, self.acting.clone())
    }

    fn conclude<O: Into<notice::Outcome>>(
        &mut self,
        spinner: Option<u64>,
        notice: Option<&str>,
        result: Result<O, notice::Failure>,
    ) -> bool {
        match result {
            Ok(outcome) => {
                let mut outcome: notice::Outcome = outcome.into();
                if outcome.title.is_empty() {
                    outcome.title = notice.unwrap_or_default().to_owned();
                }
                if outcome.title.is_empty() {
                    if let Some(id) = spinner {
                        self.ui.notices.remove(id);
                    }
                } else {
                    let shown = outcome.notice().logged(self.trail.entry(false));
                    match spinner {
                        Some(id) => self.ui.notices.finish(id, shown),
                        None => self.ui.notices.show(shown),
                    };
                }
                true
            }
            Err(failure) => {
                let shown = failure.notice().logged(self.trail.entry(true));
                match spinner {
                    Some(id) => self.ui.notices.finish(id, shown),
                    None => self.ui.notices.show(shown),
                };
                false
            }
        }
    }

    fn run_operation_then<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        progress: &'static str,
        notice: &'static str,
        select_head: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        self.apply_operation(cx, progress, Some(notice), true, job, move |tab, cx| {
            tab.follow = Follow::head_if(select_head);
            after_success(tab, cx);
        });
    }

    fn run_operation_or<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        progress: &'static str,
        notice: &'static str,
        select_head: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        after_failure: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        let after_success = move |tab: &mut Self, cx: &mut Context<Self>| {
            tab.follow = Follow::head_if(select_head);
            after_success(tab, cx);
        };
        self.apply_operation_or(cx, progress, Some(notice), true, job, after_success, after_failure);
    }

    fn apply_operation<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        progress: &'static str,
        notice: Option<&'static str>,
        refresh_refs: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        self.apply_operation_or(cx, progress, notice, refresh_refs, job, after_success, |_, _| {});
    }

    fn apply_operation_or<R, E>(
        &mut self,
        cx: &mut Context<Self>,
        progress: &'static str,
        notice: Option<&'static str>,
        refresh_refs: bool,
        job: impl FnOnce(Repository) -> Result<R, E> + Send + 'static,
        after_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        after_failure: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) where
        R: Into<notice::Outcome> + Send + 'static,
        E: Into<notice::Failure> + Send + 'static,
    {
        if self.ui.busy() {
            return;
        }
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let link = jobs::Link::new(false);
        let spinner = notice.is_some().then(|| self.ui.notices.progress(progress));
        let id = self.ui.jobs.start_write(progress, link.clone(), spinner);
        if notice.is_none() {
            cx.spawn(async move |tab, cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = tab.update(cx, |tab, cx| {
                    if let Some(job) = tab.ui.jobs.write.as_mut().filter(|job| job.id == id && job.notice.is_none()) {
                        job.notice = Some(tab.ui.notices.progress(progress));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        jobs::watch(self, cx);
        cx.notify();
        let about = explain::About { local: true, ..self.about(None) };
        Self::work(
            cx,
            move || {
                link.enter(|| job(repo.clone()).map_err(|error| about.explain(&repo, hooks::named(error.into(), &link.ran(), None))))
            },
            move |tab, result, cx| {
                let spinner = tab.ui.jobs.end_write().and_then(|job| job.notice);
                if tab.conclude(spinner, notice, result.map(Into::into)) {
                    after_success(tab, cx);
                } else {
                    after_failure(tab, cx);
                }
                let refs_changed = std::mem::take(&mut tab.refs_dirty) || refresh_refs;
                tab.refresh(refs_changed, cx);
                tab.resume(cx);
                cx.notify();
            },
        );
    }
}

impl Desk for Tab {
    fn ui(&self) -> &Ui {
        &self.ui
    }
    fn ui_mut(&mut self) -> &mut Ui {
        &mut self.ui
    }
    fn dispatch(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let covering = |ui: &Ui| {
            [ui.diff_open, ui.search.open(), ui.merge.editing().is_some(), ui.conflict.showing().is_some()]
        };
        let before = covering(&self.ui);
        let outer = self.acting.replace(action.clone());
        self.route(action, window, cx);
        self.acting = outer;
        let after = covering(&self.ui);
        if self.ui.activity.open() && before.iter().zip(after).any(|(was, is)| !was && is) {
            self.ui.activity.close();
            cx.notify();
        }
    }
}

impl Tab {
    fn route(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(action, Action::Run(_) | Action::Activate(_)) {
            self.ui.closed_by_press = None;
        } else if let Some(closed) = self.ui.closed_by_press.take() {
            if closed.reopened_by(&action) {
                cx.notify();
                return;
            }
            self.ui.closed_by_press = Some(closed);
        }
        if let Action::Navigate(action) = action {
            navigate::dispatch(self, action, window, cx);
            return;
        }
        if let Action::Run(id) = action {
            let closing = id == "tab.close";
            if self.ui.palette.open() {
                let toggled = self.ui.palette.toggled_by(id);
                palette::dispatch(self, palette::Action::Close, window, cx);
                if toggled || closing {
                    return;
                }
            }
            if self.ui.dialog_open() {
                self.ui.notices.show(notice::Notice::new(Tone::Info, "Close the open dialog first", ""));
                cx.notify();
                return;
            }
            if self.ui.preferences.open() && !matches!(id, "app.palette" | "app.shortcuts" | "app.about") {
                preferences::dispatch(self, preferences::Action::Close, window, cx);
                if id == "app.preferences" || closing {
                    return;
                }
            }
            if let Some(sheet) = self.ui.help.sheet() {
                help::dispatch(self, help::Action::Close, window, cx);
                let same = matches!(
                    (sheet, id),
                    (help::Sheet::About, "app.about") | (help::Sheet::Shortcuts, "app.shortcuts")
                );
                if same || closing {
                    return;
                }
            }
            if self.ui.transient_open() {
                self.dispatch(Action::Dismiss, window, cx);
                if closing {
                    return;
                }
            }
            commands::invoke(self, id, window, cx);
            return;
        }
        if let Action::Activate(target) = action {
            self.ui.menu = None;
            if !self.ui.prompt.open() && !self.ui.publish.open() {
                commands::activate(self, target, window, cx);
            }
            return;
        }
        match action {
            Action::MenuKey(key) => return commands::key(self, key, window, cx),
            Action::MenuHover(cursor) => {
                if self.ui.menu.is_some() && self.ui.menu_cursor != cursor {
                    self.ui.menu_cursor = cursor;
                    cx.notify();
                }
                return;
            }
            _ => {}
        }
        let chrome = matches!(
            action,
            Action::Dismiss
                | Action::Notice(_)
                | Action::CopyText { .. }
                | Action::Menu(_)
                | Action::Handoff(_)
                | Action::Jobs(_)
                | Action::Activity(_)
                | Action::Help(_)
                | Action::Palette(_)
                | Action::ToggleSidebar
                | Action::TogglePanel
                | Action::Tabs(_)
                | Action::Cli(_)
        );
        if self.ui.prompt.open() {
            if !chrome && !matches!(action, Action::Prompt(_) | Action::Open | Action::Refresh) {
                return;
            }
        } else if self.ui.publish.open()
            && !chrome
            && !matches!(action, Action::Publish(_) | Action::Open | Action::Refresh)
        {
            return;
        }
        if !self.ui.prompt.open() && !self.ui.publish.open() && !chrome {
            if let Err(reason) = operation::allows(self.ui.operation.kind(), &action) {
                self.ui.menu = None;
                self.ui.notices.show(notice::Notice::new(Tone::Info, reason, ""));
                cx.notify();
                return;
            }
        }
        if !self.ui.prompt.open()
            && self.ui.refs.draft.is_some()
            && !chrome
            && !matches!(action, Action::Refs(_) | Action::Open | Action::Refresh)
        {
            return;
        }
        if !self.ui.prompt.open() && self.ui.cloning.open() && !chrome && !matches!(action, Action::Clone(_)) {
            return;
        }
        if !self.ui.prompt.open() && self.ui.init.open() && !chrome && !matches!(action, Action::Init(_)) {
            return;
        }
        if !matches!(action, Action::Menu(Some(_))) && self.ui.menu.take().is_some() {
            cx.notify();
        }
        if matches!(
            action,
            Action::ViewChanges
                | Action::Select(Selection::Changes | Selection::File { .. } | Selection::Stash(_))
                | Action::Refs(refs::Action::Inspect(..) | refs::Action::Switch(_))
        ) {
            self.ui.search.hide(cx);
        }
        if self.ui.preferences.open()
            && matches!(action, Action::Search(search::Action::Open) | Action::Refs(refs::Action::ToggleList))
        {
            preferences::dispatch(self, preferences::Action::Close, window, cx);
        }
        match action {
            Action::Open if !self.can_open() =>self.refuse(self.open_refusal(), cx),
            Action::Open => {
                let picker = cx.prompt_for_paths(PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("Open repository".into()),
                });
                window
                    .spawn(cx, {
                        let tab = cx.weak_entity();
                        async move |cx| match picker.await {
                            Ok(Ok(Some(paths))) => {
                                if let Some(path) = paths.into_iter().next() {
                                    let _ = cx.update(|window, cx| {
                                        tab.update(cx, |tab, cx| {
                                            tab.open(path, cx);
                                            view::focus(&tab.ui, window);
                                        })
                                    });
                                }
                            }
                            Ok(Err(err)) => {
                                let _ = tab.update(cx, |tab, cx| tab.error(err.to_string(), cx));
                            }
                            _ => {}
                        }
                    })
                    .detach();
            }
            Action::Refresh => self.refresh(true, cx),
            Action::Inspect(action) => inspection::dispatch(self, action, window, cx),
            Action::Refs(action) => refs::dispatch(self, action, window, cx),
            Action::Branch(action) => branch::dispatch(self, action, window, cx),
            Action::Tag(action) => tag::dispatch(self, action, window, cx),
            Action::Tracking(action) => tracking::dispatch(self, action, window, cx),
            Action::Merge(action) => merge::dispatch(self, action, window, cx),
            Action::Operation(action) => operation::dispatch(self, action, window, cx),
            Action::Conflict(action) => conflict::dispatch(self, action, window, cx),
            Action::Reverse(action) => reverse::dispatch(self, action, window, cx),
            Action::Pick(action) => pick::dispatch(self, action, window, cx),
            Action::Picked(action) => picked::dispatch(self, action, window, cx),
            Action::Discard(action) => discard::dispatch(self, action, window, cx),
            Action::Ignore(action) => ignore::dispatch(self, action, window, cx),
            Action::Rebase(action) => rebase::dispatch(self, action, window, cx),
            Action::Autostash(action) => autostash::dispatch(self, action, window, cx),
            Action::Explain(action) => explain::dispatch(self, action, window, cx),
            Action::Stash(action) => stash::dispatch(self, action, window, cx),
            Action::Sync(action) => sync::dispatch(self, action, window, cx),
            Action::Publish(action) => publish::dispatch(self, action, window, cx),
            Action::Search(action) => search::dispatch(self, action, window, cx),
            Action::ViewChanges => {
                if let Some(first) = self.ui.files.first() {
                    self.follow = Follow::Nothing;
                    self.ui.diff_open = true;
                    self.select(
                        Selection::File {
                            path: first.path.clone(),
                            staged: first.staged,
                        },
                        cx,
                    );
                    view::focus(&self.ui, window);
                }
            }
            Action::StageAll => self.mutate(cx, |repo| repo.stage_all()),
            Action::UnstageAll => self.mutate(cx, |repo| repo.unstage_all()),
            Action::More => self.load_history(false, cx),
            Action::Select(selection) => {
                self.ui.picked = None;
                self.ui.navigate.chose(&selection);
                self.follow = Follow::Nothing;
                self.ui.diff_open = matches!(selection, Selection::File { .. });
                self.select(selection, cx);
            }
            Action::OpenDiff => {
                self.ui.diff_open = true;
                cx.notify();
            }
            Action::DiffMode(mode) => {
                self.ui.diff_mode = mode;
                settings::Settings::update(cx, |s| s.diff_mode = mode);
                cx.notify();
            }
            Action::CloseDiff => {
                self.ui.diff_open = false;
                if matches!(self.ui.selection, Some(Selection::File { .. })) {
                    self.select(Selection::Changes, cx);
                }
                view::focus(&self.ui, window);
                cx.notify();
            }
            Action::Menu(menu) => {
                self.ui.refs.list_open = false;
                if menu.is_some() {
                    view::focus(&self.ui, window);
                }
                self.ui.menu = menu;
                self.ui.menu_cursor = Default::default();
                cx.notify();
            }
            Action::Dismiss if self.ui.prompt.open() => {
                prompt::dispatch(self, prompt::Action::Cancel, window, cx);
            }
            Action::Dismiss => {
                self.ui.menu = None;
                self.ui.refs.list_open = false;
                if self.ui.sync.options_open() {
                    sync::dispatch(self, sync::Action::Dismiss, window, cx);
                }
                if self.ui.publish.branch_open() && !self.ui.busy() {
                    publish::dispatch(self, publish::Action::Escape, window, cx);
                }
                if self.ui.stash.options_open() {
                    stash::dispatch(self, stash::Action::Dismiss, window, cx);
                }
                if self.ui.commit_options.picking() {
                    commit_options::dispatch(self, commit_options::Action::CloseCoauthors, window, cx);
                }
                view::focus(&self.ui, window);
                cx.notify();
            }
            Action::Notice(action) => notice::dispatch(self, action, window, cx),
            Action::CopyText { text, what } => {
                view::copy(text, cx);
                self.ui.notices.show(notice::Notice::copied(what));
                cx.notify();
            }
            Action::Handoff(action) => handoff::dispatch(self, action, window, cx),
            Action::Jobs(action) => jobs::dispatch(self, action, window, cx),
            Action::CommitOptions(action) => commit_options::dispatch(self, action, window, cx),
            Action::Credentials(action) => credentials::dispatch(self, action, cx),
            Action::Clone(action) => clone::dispatch(self, action, window, cx),
            Action::Init(action) => init::dispatch(self, action, window, cx),
            Action::Start(action) => start::dispatch(self, action, window, cx),
            Action::Connect(action) => connect::dispatch(self, action, cx),
            Action::Address(action) => address::dispatch(self, action, window, cx),
            Action::Activity(action) => activity::dispatch(self, action, window, cx),
            Action::Help(action) => help::dispatch(self, action, window, cx),
            Action::Preferences(action) => preferences::dispatch(self, action, window, cx),
            Action::Identity(action) => identity::dispatch(self, action, window, cx),
            Action::AutoFetch(action) => autofetch::dispatch(self, action, window, cx),
            Action::Tools(action) => tools::dispatch(self, action, window, cx),
            Action::Tabs(action) => tabs::dispatch(self, action, window, cx),
            Action::Cli(action) => cli::dispatch(self, action, window, cx),
            Action::Palette(action) => palette::dispatch(self, action, window, cx),
            Action::ToggleSidebar => {
                let hidden = !self.ui.sidebar_hidden;
                self.ui.sidebar_hidden = hidden;
                settings::Settings::update(cx, |s| s.sidebar_hidden = hidden);
                cx.notify();
            }
            Action::TogglePanel => {
                let hidden = !self.ui.panel_hidden;
                self.ui.panel_hidden = hidden;
                settings::Settings::update(cx, |s| s.panel_hidden = hidden);
                cx.notify();
            }
            Action::FocusSummary => {
                if self.ui.panel_hidden {
                    self.dispatch(Action::TogglePanel, window, cx);
                }
                if !self.ui.working() {
                    self.dispatch(Action::Select(Selection::Changes), window, cx);
                }
                window.focus(&self.ui.input.focus_handle(cx));
            }
            Action::Run(_)
            | Action::Activate(_)
            | Action::MenuKey(_)
            | Action::MenuHover(_)
            | Action::Navigate(_) => {}
            Action::AskDiscard(discard) => self.ask_discard(discard, cx),
            Action::Prompt(action) => prompt::dispatch(self, action, window, cx),
            Action::UnstageHunk(index) => {
                if let (Some(diff), Some(Selection::File { staged: true, .. })) =
                    (self.diff.clone(), &self.ui.selection)
                {
                    self.mutate(cx, move |repo| repo.unstage_hunk(&diff, index));
                }
            }
            Action::StagePath(path) => self.mutate(cx, move |repo| repo.stage_file(&path)),
            Action::UnstagePath(path) => {
                self.mutate(cx, move |repo| repo.unstage_file(&path))
            }
            Action::StageHunk(index) => {
                if let (Some(diff), Some(Selection::File { staged: false, .. })) =
                    (self.diff.clone(), &self.ui.selection)
                {
                    self.mutate(cx, move |repo| repo.stage_hunk(&diff, index));
                }
            }
            Action::StageFile => {
                if let Some(Selection::File {
                    path,
                    staged: false,
                }) = self.ui.selection.clone()
                {
                    self.mutate(cx, move |repo| repo.stage_file(&path));
                }
            }
            Action::ToggleAmend => self.toggle_amend(cx),
            Action::Commit => self.commit(window, cx),
            Action::CommitAll => self.commit_all(cx),
        }
    }
}

impl Render for Tab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        perf::schedule(self, window, cx);
        notice::schedule(self, cx);
        view::render(self, window, cx)
    }
}

fn hand_over(argv0: &std::ffi::OsStr, path: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let cwd = std::env::current_dir().ok();
    let search = std::env::var_os("PATH");
    let command = cwd.as_deref().is_some_and(|cwd| cli::through_link(argv0, search.as_deref(), cwd));
    let path = match path {
        Some(path) => path,
        None if command => cwd.clone()?.into_os_string(),
        None => return None,
    };
    let given = std::path::Path::new(&path);
    let joined = match &cwd {
        Some(cwd) if given.is_relative() => cwd.join(given),
        _ => given.to_owned(),
    };
    let absolute = std::fs::canonicalize(&joined).unwrap_or(joined);
    let Ok(state) = storage::directory() else { return Some(absolute.into_os_string()) };
    match instance::hand_over(&instance::socket(&state), &absolute) {
        Ok(instance::Handed::Taken) => std::process::exit(0),
        Ok(instance::Handed::Alone) if acceptance::replaying() => {
            eprintln!("No FastKraken is running for this sandbox.");
            std::process::exit(3);
        }
        Ok(instance::Handed::Alone) => {
            if command {
                if let Ok(executable) = cli::executable() {
                    if cli::bundle(&executable).is_some() && cli::detach(&executable, &absolute).is_ok() {
                        std::process::exit(0);
                    }
                }
            }
            Some(absolute.into_os_string())
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

fn declined() -> notice::Notice {
    notice::Notice::new(Tone::Info, "Cancelled", "")
}

fn main() {
    let launched = Instant::now();
    let argv0 = std::env::args_os().next().unwrap_or_default();
    let all: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if std::env::var_os(askpass::SOCKET).is_some()
        && all.len() == 1
        && !all[0].as_encoded_bytes().starts_with(b"--git-credential")
    {
        std::process::exit(askpass::helper(&all[0]));
    }
    let mut args = all.into_iter();
    let first = args.next();
    let flag = first.as_deref().and_then(|s| s.to_str());
    if flag == Some(credentials::HELPER) {
        if let Err(message) = credentials::helper(args) {
            eprintln!("FastKraken saved credentials: {message}");
        }
        return;
    }
    if flag == Some("--git-credential") {
        if let Err(message) = auth::git_credential(args) {
            eprintln!("FastKraken credentials: {message}");
            println!("quit=true\n");
        }
        return;
    }
    if std::env::var_os(storage::STATE_DIR).is_some() && storage::override_directory().is_none() {
        eprintln!("{} must be an absolute path.", storage::STATE_DIR);
        std::process::exit(2);
    }
    if let Some(wave) = flag.and_then(acceptance::Wave::from_flag) {
        if args.next().is_some() {
            eprintln!("The acceptance replay creates its own repositories.");
            std::process::exit(2);
        }
        acceptance::run(wave);
        return;
    }
    let edits = matches!(flag, Some("--measure-edits" | "--measure-edits-offscreen"));
    let offscreen = matches!(
        flag,
        Some("--measure-offscreen" | "--measure-edits-offscreen")
    );
    let benchmark = edits || matches!(flag, Some("--measure" | "--measure-offscreen"));
    let fixture = if edits {
        Some(perf::fixture().unwrap_or_else(|err| {
            eprintln!("Create fixture: {err}");
            std::process::exit(1)
        }))
    } else {
        None
    };
    let path = if let Some(dir) = &fixture {
        Some(dir.path().as_os_str().to_owned())
    } else if benchmark {
        args.next()
    } else {
        first
    };
    if path.as_deref() == Some("--help".as_ref()) {
        println!(
            "fastkraken [repository]           Open the repository, in a tab of the FastKraken already running\nfastkraken                        Through the installed command: open the folder you are in\nfastkraken --acceptance-wave-1    Replay the first 16 core Git scenarios in temporary repositories\nfastkraken --acceptance-wave-2    Replay 13 stash, merge, reset and revert scenarios\nfastkraken --acceptance-wave-3    Replay five sync scenarios against local bare remotes\nfastkraken --acceptance-gap-NN    Run additional acceptance suite NN (01 to 06)\nfastkraken --measure repository   Measure a read-only walkthrough\nfastkraken --measure-edits        Measure staging and committing in a new temporary repo\nAppend -offscreen to a measure option to time native scene builds without display presentation."
        );
        return;
    }
    let extra = args.next();
    let hold = benchmark && !edits && extra.as_deref() == Some("--hold".as_ref());
    if (extra.is_some() && !hold) || args.next().is_some() || (benchmark && path.is_none()) {
        eprintln!("Usage: fastkraken [--measure] [repository]");
        std::process::exit(2);
    }
    let started = if edits { Instant::now() } else { launched };
    let scene = std::env::var_os("FASTKRAKEN_SCENE").and_then(|value| value.into_string().ok());
    let single = !benchmark && scene.is_none();
    let path = if single { hand_over(&argv0, path) } else { path };
    let state_dir = if benchmark || (scene.is_some() && storage::override_directory().is_none()) {
        None
    } else {
        storage::directory().ok()
    };
    let (handed, folders) = async_channel::unbounded::<Vec<String>>();
    let application = Application::new().with_assets(assets::Assets);
    application.on_open_urls(move |urls| {
        let _ = handed.try_send(urls);
    });
    let listen = state_dir.clone().filter(|_| single);
    application.run(move |cx: &mut App| {
        view::bindings(cx);
        settings::load(state_dir, cx);
        environment::use_login_shell(settings::settings(cx).login_path, cx);
        cx.on_app_quit(|cx| {
            settings::flush(cx);
            repository::stop_running();
            askpass::stop();
            instance::stop();
            async {}
        })
        .detach();
        cx.on_window_closed(|cx| if cx.windows().is_empty() { cx.quit(); }).detach();
        let (display_id, window_bounds) = match settings::placement(cx) {
            Some((display, bounds)) => (Some(display), bounds),
            None => (None, WindowBounds::Windowed(Bounds::centered(None, size(px(1180.), px(760.)), cx))),
        };
        let options = WindowOptions { window_bounds: Some(window_bounds), display_id,
            show: !offscreen, focus: !offscreen,
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("FastKraken".into()),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(14.), px(15.))),
            }),
            window_min_size: Some(size(px(settings::MIN_WINDOW.0), px(settings::MIN_WINDOW.1))),
            ..Default::default() };
        let handle=cx.open_window(options, move |window, cx| cx.new(|cx| {
            let bench = benchmark.then(|| perf::Run::new(started, fixture, offscreen, hold));
            let mut desk = tabs::Workspace::new(window, cx, bench);
            desk.launch(path.map(PathBuf::from), window, cx);
            cx.observe_window_bounds(window, |_, window, cx| settings::remember_window(window, cx)).detach();
            desk
        })).expect("Could not open the native application window");
        let handle = tabs::Front(handle);
        if benchmark {
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                if handle.read_with(cx, |tab,_| tab.bench.is_some()).unwrap_or(false) {
                    eprintln!("Native measurement timed out. Visible measurements require an awake, unlocked display.");
                    std::process::exit(1);
                }
            }).detach();
        }
        cx.spawn(async move |cx| {
            while let Ok(urls) = folders.recv().await {
                let Some(path) = urls.iter().find_map(|url| url::Url::parse(url).ok()?.to_file_path().ok()) else {
                    continue;
                };
                let _ = cx.update(|cx| tabs::ask(cx, tabs::Request::Handed(path)));
            }
        })
        .detach();
        if let Some(state) = listen {
            if let Err(error) = instance::serve_app(&state, cx) {
                eprintln!("FastKraken cannot take folders from a second launch: {error}");
            }
        }
        if let Some(scene) = scene {
            scene::drive(handle, scene, cx);
        }
        if offscreen { perf::drive_offscreen(handle, cx); } else { cx.activate(true); }
    });
}

#[cfg(test)]
mod tests {
    use super::{Follow, Landing};

    #[test]
    fn a_switch_lands_on_the_changes_it_carries_and_on_head_when_clean() {
        assert_eq!(Follow::ChangesOrHead.landing(true), Some(Landing::Changes));
        assert_eq!(Follow::ChangesOrHead.landing(false), Some(Landing::Head));
    }

    #[test]
    fn other_operations_keep_landing_on_head_or_leave_the_selection() {
        assert_eq!(Follow::Head.landing(true), Some(Landing::Head));
        assert_eq!(Follow::Head.landing(false), Some(Landing::Head));
        assert_eq!(Follow::Nothing.landing(true), None);
        assert_eq!(Follow::head_if(true), Follow::Head);
        assert_eq!(Follow::head_if(false), Follow::Nothing);
        assert_eq!(Follow::default(), Follow::Nothing);
    }
}
