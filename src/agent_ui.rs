use crate::{
    Tab,
    agent::{self, Error, GrantInfo, Profile, Service, Worktree},
    agent_git::Patch,
    agent_transport,
    diff_doc::{DiffDoc, PatchState},
    graph_view::GraphView,
    kit, preferences,
    tabs::Front,
    view::{self, Centre, CommitInput, Desk, Selection, Ui},
};
use gpui::{App, AppContext, Context, Div, Entity, Global, div, prelude::*};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime},
};

pub(crate) struct CapturedView {
    pub(crate) data: Value,
    pub(crate) patch: Option<Patch>,
    pub(crate) graph: Option<GraphView>,
}

enum DesktopRequest {
    Capture {
        worktree: Worktree,
        reply: mpsc::SyncSender<agent::Result<CapturedView>>,
    },
    BeginWrite {
        worktree: Worktree,
        client: String,
        reply: mpsc::SyncSender<agent::Result<WriteLease>>,
    },
}

#[derive(Clone)]
pub(crate) struct Bridge {
    requests: async_channel::Sender<DesktopRequest>,
    // Constructed only by the isolated offscreen acceptance runner.
    fixture: Option<PathBuf>,
}

impl Bridge {
    fn request<T>(
        &self,
        make: impl FnOnce(mpsc::SyncSender<agent::Result<T>>) -> DesktopRequest,
    ) -> agent::Result<T> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.requests.try_send(make(reply)).map_err(|_| busy())?;
        answer
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| Error::new("app_unavailable", "Open FastKraken and try again."))?
    }

    pub(crate) fn capture(&self, worktree: &Worktree) -> agent::Result<CapturedView> {
        self.request(|reply| DesktopRequest::Capture {
            worktree: worktree.clone(),
            reply,
        })
    }

    pub(crate) fn begin_write(
        &self,
        worktree: &Worktree,
        label: &str,
    ) -> agent::Result<WriteLease> {
        let lease = self.request(|reply| DesktopRequest::BeginWrite {
            worktree: worktree.clone(),
            client: label.into(),
            reply,
        })?;
        if let Some(sandbox) = &self.fixture {
            crate::acceptance::pause_agent_write(sandbox, label)?;
        }
        Ok(lease)
    }
}

fn busy() -> Error {
    Error::new("busy", "Wait for the current desktop operation and retry.")
}

// The GUI reserves the tab's write slot before giving this sender to the executor.
// Dropping a failed or disconnected worker's lease also wakes the GUI to release it.
pub(crate) struct WriteLease {
    done: Option<async_channel::Sender<String>>,
}

impl WriteLease {
    pub(crate) fn committed(&mut self, oid: &str) {
        if let Some(done) = self.done.take() {
            let _ = done.try_send(oid.to_owned());
        }
    }
}

fn reserve_write(
    tab: &mut Tab,
    client: String,
    cx: &mut Context<Tab>,
) -> agent::Result<WriteLease> {
    if !tab.settled() {
        return Err(busy());
    }
    let root = tab.ui.root.clone();
    let started = Instant::now();
    let at = SystemTime::now();
    let title = format!("{client} is committing reviewed changes");
    let notice = tab.ui.notices.progress(&title);
    let id = tab
        .ui
        .jobs
        .start_write(&title, crate::jobs::Link::new(false), Some(notice));
    crate::jobs::watch(tab, cx);
    let (done, finish) = async_channel::bounded::<String>(1);
    cx.spawn(async move |tab, cx| {
        let committed = finish.recv().await.ok();
        let activity = committed
            .as_ref()
            .map(|oid| crate::activity::agent_commit(&root, &client, oid, at, started.elapsed()));
        let _ = tab.update(cx, |tab, cx| {
            if !tab.ui.jobs.write.as_ref().is_some_and(|job| job.id == id) {
                return;
            }
            tab.ui.jobs.end_write();
            if let Some(oid) = &committed {
                tab.ui.notices.finish(
                    notice,
                    crate::notice::Notice::new(
                        view::Tone::Success,
                        format!("{client} committed reviewed changes"),
                        oid.clone(),
                    )
                    .logged(activity),
                );
            } else {
                tab.ui.notices.remove(notice);
            }
            // A successful publication invalidates the background tab. The file
            // watcher independently refreshes a visible tab, as for a CLI commit.
            // Service completion must never initiate a new unrestricted Git read.
            if committed.is_some() && (!tab.front || tab.watcher.is_none()) {
                tab.stale = Some(true);
            }
            let dirty_refs = std::mem::take(&mut tab.refs_dirty);
            if dirty_refs || tab.status_dirty {
                tab.refresh(dirty_refs, cx);
            }
            tab.resume(cx);
            cx.notify();
        });
    })
    .detach();
    cx.notify();
    Ok(WriteLease { done: Some(done) })
}

struct Desktop {
    service: Arc<Service>,
    _listener: agent_transport::Listener,
}
impl Global for Desktop {}

pub(crate) fn start(state: &Path, front: Front, cx: &mut App) -> Result<(), String> {
    listen(state, front, cx, None)
}

pub(crate) fn start_fixture(
    state: &Path,
    front: Front,
    cx: &mut App,
    sandbox: &Path,
) -> Result<(), String> {
    listen(state, front, cx, Some(sandbox.to_owned()))
}

fn listen(
    state: &Path,
    front: Front,
    cx: &mut App,
    fixture: Option<PathBuf>,
) -> Result<(), String> {
    let (send, recv) = async_channel::bounded::<DesktopRequest>(8);
    let service = Service::new(
        state,
        Bridge {
            requests: send,
            fixture,
        },
    );
    let server = service.clone();
    let listener =
        agent_transport::listen(state, move |token, request| server.handle(token, request))?;
    cx.set_global(Desktop {
        service,
        _listener: listener,
    });
    cx.on_app_quit(|cx| {
        cx.remove_global::<Desktop>();
        async {}
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(request) = recv.recv().await {
            match request {
                DesktopRequest::Capture { worktree, reply } => {
                    let result = front
                        .read_with(cx, |tab, _| {
                            let Some(repo) = &tab.repo else {
                                return Err(no_view());
                            };
                            if !worktree.matches_opened(repo) {
                                return Err(Error::new(
                                    "access_denied",
                                    "Switch to the granted worktree to read its view.",
                                ));
                            }
                            Ok(capture_tab(tab))
                        })
                        .unwrap_or_else(|_| Err(no_view()));
                    let _ = reply.send(result);
                }
                DesktopRequest::BeginWrite {
                    worktree,
                    client,
                    reply,
                } => {
                    let result = front
                        .worktree_tab(cx, &worktree)
                        .ok()
                        .flatten()
                        .ok_or_else(no_view)
                        .and_then(|tab| {
                            tab.update(cx, |tab, cx| reserve_write(tab, client, cx))
                                .unwrap_or_else(|_| Err(no_view()))
                        });
                    let _ = reply.send(result);
                }
            }
        }
    })
    .detach();
    Ok(())
}

fn no_view() -> Error {
    Error::new("no_active_view", "Open the granted worktree in FastKraken.")
}

fn name(raw: &OsStr) -> Value {
    match raw.to_str() {
        Some(s) => json!(s),
        None => {
            use std::fmt::Write;
            let mut id = String::from("git-bytes:");
            for b in raw.as_bytes() {
                let _ = write!(id, "{b:02x}");
            }
            json!({"id":id})
        }
    }
}

fn preview(text: &str) -> String {
    let end = text.floor_char_boundary(2048);
    if end == text.len() {
        text.into()
    } else {
        format!("{}…", &text[..end])
    }
}

fn selection(ui: &Ui) -> Value {
    match &ui.selection {
        None => Value::Null,
        Some(Selection::Changes) => {
            let (unstaged, staged) = ui.file_ranges();
            json!({"kind":"working_changes", "staged_count":staged.len(), "unstaged_count":unstaged.len()})
        }
        Some(Selection::File { path, staged }) => json!({
            "kind":"file", "file":name(path.as_os_str()), "side":if *staged { "staged" } else { "unstaged" }
        }),
        Some(Selection::Commit(oid)) => json!({
            "kind":"commit", "oid":oid, "subject":ui.inspection.subject.as_deref().map(preview), "changed_files":ui.inspection.files.len(),
            "file":ui.inspection.path.as_deref().map(|p| name(p.as_os_str()))
        }),
        Some(Selection::Stash(oid)) => json!({
            "kind":"stash", "oid":oid, "subject":ui.stash.title(oid).map(preview), "changed_files":ui.stash.files.len(),
            "file":ui.stash.open_path().map(|p| name(p.as_os_str()))
        }),
        Some(Selection::Commits) => {
            json!({"kind":"commits","oids": crate::compare::picked_commits(ui)})
        }
        Some(Selection::Compare {
            from,
            to,
            since_fork,
        }) => {
            let rev = |r: &crate::compare::Rev| match r.oid() {
                Some(oid) => json!({"kind":"commit","oid":oid}),
                None => json!({"kind":"working_tree"}),
            };
            json!({"kind":"comparison", "from":rev(from), "to":rev(to), "since_fork":since_fork,
                "merge_base":ui.compare.fork(), "changed_files":ui.compare.files.len(), "file":ui.compare.path.as_deref().map(|p| name(p.as_os_str()))})
        }
    }
}

fn capture_tab(tab: &Tab) -> CapturedView {
    let ui = &tab.ui;
    let branch = ui
        .head_ref
        .as_ref()
        .map(|r| name(r))
        .or_else(|| (!ui.detached && !ui.branch.is_empty()).then(|| json!(ui.branch)));
    let mut data = json!({"selection":null,"loading":false});
    let centre = view::centre(ui);
    let pane = if ui.modal_open() || ui.dialog_open() {
        "modal"
    } else if ui.preferences.open() {
        "preferences"
    } else if ui.transient_open() {
        "menu"
    } else {
        match centre {
            Centre::Activity => "activity",
            Centre::Blame => "blame",
            Centre::Browse => "file_browser",
            Centre::FileHistory => "file_history",
            Centre::RebaseEditor => "rebase_editor",
            Centre::Resolver => "conflict_resolver",
            Centre::Chooser => "conflict_chooser",
            Centre::Search => "search",
            Centre::History => "history",
            Centre::Diff => "diff",
        }
    };
    data["active_pane"] = json!(pane);
    if pane != "modal" && pane != "preferences" {
        data["branch"] = json!(branch);
        data["head"] = json!(ui.head);
    }
    if pane != "history" && pane != "diff" {
        data["details_available"] = json!(false);
        return CapturedView {
            data,
            graph: None,
            patch: None,
        };
    }

    let shown = ui.shown();
    data["panels"] = json!({"sidebar":shown.sidebar,"details":shown.panel});
    let graph = (centre == Centre::History).then(|| crate::graph_view::query(ui));
    data["graph_filters"] = json!(graph);
    let selected = selection(ui);
    data["selection"] = selected.clone();
    if shown.panel
        && let Some(panel) = crate::file_list::Panel::of(ui)
    {
        data["file_filter"] = json!(preview(ui.file_list.filter(panel)));
    }
    data["history_complete"] = json!(ui.history_complete);
    data["loading"] = json!(match centre {
        Centre::History if matches!(ui.selection, Some(Selection::Compare { .. })) =>
            ui.compare.loading(),
        Centre::History if matches!(ui.selection, Some(Selection::Changes)) => tab.loading_status,
        Centre::History => tab.loading_page && ui.history.is_empty(),
        _ => false,
    });
    // Commit and stash summaries have a loaded whole-object patch. A file selection
    // underneath a different centre never supplies the foreground diff.
    let has_patch = centre == Centre::Diff
        || (centre == Centre::History
            && matches!(
                ui.selection,
                Some(Selection::Commit(_) | Selection::Stash(_))
            ));
    let patch = if has_patch {
        data["diff_options"] = json!({"ignore_whitespace":ui.diff.options.ignore_whitespace,
            "whole_file_context":ui.diff.options.whole_file, "wrap":ui.diff_options.wrap,
            "layout":ui.shown_mode(), "display":if ui.diff.file.is_some() { "file" } else { "patch" }});
        let captured = displayed_patch(ui, selected);
        if captured.meta["state"] == "loading" {
            data["loading"] = json!(true);
            data["recovery"] = json!("Request a new view snapshot when the diff finishes loading.");
        }
        data["patch_state"] = captured.meta["state"].clone();
        Some(captured)
    } else {
        None
    };
    CapturedView { data, graph, patch }
}

const MAX_DISPLAY_BYTES: usize = 1024 * 1024;

fn excerpt(doc: &DiffDoc, limit: usize) -> (&'static str, String) {
    let mut state = match doc.state {
        PatchState::Loading => "loading",
        PatchState::Text => "ready",
        PatchState::Empty(_) => "empty",
        PatchState::Binary { .. } => "binary",
        PatchState::Lfs { .. } => "lfs",
        PatchState::ModeOnly { .. } => "mode_only",
        PatchState::Truncated => "truncated",
        PatchState::Unreadable(_) => "unreadable",
    };
    let mut text = String::new();
    for row in &doc.lines {
        let line = doc.text(row);
        if line.len() + 1 > limit - text.len() {
            state = "truncated";
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    if doc.shortened || doc.file.as_ref().is_some_and(|file| file.cut) {
        state = "truncated";
    }
    (state, text)
}

fn displayed_patch(ui: &Ui, mut meta: Value) -> Patch {
    let doc = &ui.diff;
    let (mut state, mut text) = excerpt(doc, MAX_DISPLAY_BYTES);
    // Placeholder messages can include backend diagnostics. Do not export them.
    if doc.lines.is_empty() && !matches!(doc.state, PatchState::Unreadable(_)) {
        text = view::diff_placeholder(ui, false).unwrap_or_default();
    }
    if let Some(image) = crate::image_diff::showing(ui) {
        state = if !image.settled() {
            "loading"
        } else if image
            .sides
            .iter()
            .flatten()
            .flatten()
            .any(|p| matches!(p.look, crate::image_diff::Look::Unreadable))
        {
            "unreadable"
        } else {
            "binary"
        };
        text.clear();
        meta["display"] = json!("image");
    } else if let Some(file) = &doc.file {
        meta["display"] = json!("file");
        meta["version"] = json!(match file.version {
            crate::file_view::Version::Before => "before",
            crate::file_view::Version::Changed => "changed",
            crate::file_view::Version::Working => "working",
        });
    } else {
        meta["display"] = json!("patch");
    }
    meta["source"] = json!("displayed");
    meta["state"] = json!(state);
    meta["truncated"] = json!(state == "truncated");
    Patch { meta, text }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Create(Profile),
    Copy,
    Revoke(String),
}

pub(crate) struct Settings {
    pub(crate) label: Entity<CommitInput>,
    grants: Vec<GrantInfo>,
    pub(crate) setup: Option<String>,
    message: Option<String>,
    ticket: u64,
    pending: bool,
}

impl Settings {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let label = cx.new(|cx| CommitInput::with_placeholder(cx, "Client name"));
        cx.observe(&label, |_, _, cx| cx.notify()).detach();
        Self {
            label,
            grants: Vec::new(),
            setup: None,
            message: None,
            ticket: 0,
            pending: false,
        }
    }

    pub(crate) fn close(&mut self) {
        self.setup = None;
        self.message = None;
        self.ticket += 1;
        self.pending = false;
    }
}

fn service(cx: &App) -> Option<Arc<Service>> {
    cx.try_global::<Desktop>()
        .map(|desktop| desktop.service.clone())
}

pub(crate) fn show(tab: &mut Tab, cx: &mut Context<Tab>) {
    let (Some(repo), Some(service)) = (tab.repo.clone(), service(cx)) else {
        return;
    };
    let state = &mut tab.ui.preferences.agent;
    state.ticket += 1;
    let ticket = state.ticket;
    state.grants.clear();
    Tab::work(
        cx,
        move || service.grants(&repo),
        move |tab, grants, cx| {
            let state = &mut tab.ui.preferences.agent;
            if state.ticket == ticket {
                match grants {
                    Ok(grants) => state.grants = grants,
                    Err(error) => state.message = Some(error.message),
                }
                cx.notify();
            }
        },
    );
}

fn host_setup(token: String) -> Result<String, String> {
    let executable = crate::cli::executable()?;
    let mut env = json!({"FASTKRAKEN_MCP_TOKEN":token});
    if let Some(dir) = crate::storage::override_directory() {
        env["FASTKRAKEN_STATE_DIR"] = json!(
            dir.to_str()
                .ok_or("The settings directory requires a UTF-8 path for JSON setup.")?
        );
    }
    serde_json::to_string_pretty(&json!({"mcpServers":{"fastkraken":{
        "command":executable.to_str().ok_or("The application requires a UTF-8 path for JSON setup.")?,
        "args":["--mcp"],"env":env
    }}})).map_err(|_| "Could not encode the MCP host configuration.".into())
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    if !tab.ui.preferences.open()
        || tab.ui.preferences.section() != preferences::Section::Repository
    {
        return;
    }
    let (Some(repo), Some(service)) = (tab.repo.clone(), service(cx)) else {
        return;
    };
    let state = &mut tab.ui.preferences.agent;
    if matches!(action, Action::Copy) {
        if let Some(setup) = &state.setup {
            view::copy(setup.clone(), cx);
            state.message = Some("Host setup copied. It contains the client credential.".into());
        }
        return;
    }
    if state.pending {
        return;
    }
    let label = state.label.read(cx).text().trim().to_owned();
    state.close();
    state.pending = true;
    let epoch = tab.epoch;
    let ticket = state.ticket;
    Tab::work(
        cx,
        move || match action {
            Action::Create(profile) => {
                let token = service
                    .issue(&repo, &label, profile)
                    .map_err(|e| e.message)?;
                Ok(Some(host_setup(token)?))
            }
            Action::Revoke(id) => service
                .revoke(&repo, &id)
                .map(|()| None)
                .map_err(|e| e.message),
            Action::Copy => unreachable!(),
        },
        move |tab, result, cx| {
            let state = &mut tab.ui.preferences.agent;
            if tab.epoch != epoch || state.ticket != ticket {
                return;
            }
            state.pending = false;
            match result {
                Ok(setup) => {
                    state.message = Some(
                        if setup.is_some() {
                            "Grant created. Copy the setup before leaving Preferences."
                        } else {
                            "Access revoked."
                        }
                        .into(),
                    );
                    state.setup = setup;
                }
                Err(message) => state.message = Some(message),
            }
            show(tab, cx);
            cx.notify();
        },
    );
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &Context<T>) -> Div {
    let state = &ui.preferences.agent;
    let act = |action| view::Action::Preferences(preferences::Action::Agent(action));
    let mut page = div()
        .flex()
        .flex_col()
        .child(preferences::group("Agent access"));
    if service(cx).is_none() {
        return page.child(kit::muted("Agent access is unavailable in this session."));
    }
    page = page
        .child(kit::muted("Grant a named MCP client access to this worktree. Its host controls any separate shell, filesystem, network and credential access."))
        .child(preferences::setting("Client name",None,div().w(crate::theme::px(200.)).child(state.label.clone())));
    for (profile, help, id) in [
        (
            Profile::ReadOnly,
            "Inspect this view, status, history and diffs.",
            "agent-read",
        ),
        (
            Profile::LocalCommit,
            "Also review the entire index and commit it on the current branch.",
            "agent-commit",
        ),
    ] {
        page = page.child(preferences::setting(
            profile_name(profile),
            Some(help.into()),
            kit::btn(id, "Create grant")
                .small()
                .enabled(!state.pending && !state.label.read(cx).text().trim().is_empty())
                .on(act(Action::Create(profile)), cx),
        ));
    }
    if let Some(message) = &state.message {
        page = page.child(kit::muted(message.clone()));
    }
    if state.setup.is_some() {
        page = page.child(
            kit::btn("agent-copy", "Copy MCP host setup")
                .small()
                .on(act(Action::Copy), cx),
        );
    }
    if state.grants.is_empty() {
        page = page.child(kit::muted("No clients have access."));
    }
    for (i, grant) in state.grants.iter().enumerate() {
        let profile = profile_name(grant.profile);
        page = page.child(preferences::setting(
            grant.label.clone(),
            Some(profile.into()),
            kit::btn(("agent-revoke", i), "Revoke")
                .small()
                .enabled(!state.pending)
                .on(act(Action::Revoke(grant.id.clone())), cx),
        ));
    }
    page
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::ReadOnly => "Read only",
        Profile::LocalCommit => "Read and commit locally",
    }
}

// Used by the offscreen fixture to inspect the same projection before Git responds.
pub(crate) fn capture_for_acceptance(tab: &Tab) -> Value {
    capture_tab(tab).data
}

impl Settings {
    pub(crate) fn id_for(&self, label: &str) -> Option<String> {
        self.grants
            .iter()
            .find(|g| g.label == label)
            .map(|g| g.id.clone())
    }

    pub(crate) fn revoked(&self, label: &str) -> bool {
        self.message.as_deref() == Some("Access revoked.") && self.id_for(label).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::{Blank, Blob, Source};

    #[test]
    fn captured_rows_keep_visible_bytes_and_lifecycle_after_the_document_changes() {
        let patch = "@@ -1 +1 @@\n-staged\n+working\n";
        let mut doc = DiffDoc::build(&Source {
            text: patch,
            stageable: true,
            truncated: false,
            sizes: &Default::default(),
            working_size: None,
            options: Default::default(),
        });
        let snapshot = excerpt(&doc, 1024);
        assert_eq!(snapshot.0, "ready");
        assert_eq!(snapshot.1, patch);
        let limited = excerpt(&doc, 24);
        assert_eq!(limited.0, "truncated");
        assert!(limited.1.len() <= 24);
        doc.state = PatchState::Truncated;
        assert_eq!(excerpt(&doc, 1024).0, "truncated");
        for (state, expected) in [
            (PatchState::Loading, "loading"),
            (PatchState::Empty(Blank::Nothing), "empty"),
            (
                PatchState::Binary {
                    old: Blob::Absent,
                    new: Blob::Bytes(4),
                },
                "binary",
            ),
            (
                PatchState::Unreadable("private diagnostic".into()),
                "unreadable",
            ),
        ] {
            doc.replace_with(state);
            assert_eq!(excerpt(&doc, 1024), (expected, String::new()));
        }
        assert_eq!(snapshot.1, patch);
    }
}
