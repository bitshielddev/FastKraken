//! The desktop owns authorization, worktree resolution and immutable captures.
//! The stdio process can supply only this request envelope.
mod contract;
mod grants;
mod requests;
mod snapshots;

use crate::{agent_git, agent_ui::Bridge, graph_view::GraphView, repository::Repository};
use contract::{CommitArgs, DiffArgs, Empty, HistoryArgs, Operation, arguments, handle};
use grants::{Authorized, Store};
pub(crate) use grants::{GrantInfo, Profile, Worktree};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use snapshots::{Capture, Content, Snapshots};
use std::{
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum Request {
    List,
    Call { name: String, arguments: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Error {
    pub code: String,
    pub message: String,
}

impl Error {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    fn denied() -> Self {
        Self::new(
            "access_denied",
            "Check Agent access in the desktop for this client and worktree.",
        )
    }
    fn expired() -> Self {
        Self::new(
            "snapshot_expired",
            "Request a fresh view, status, history page or staged review.",
        )
    }
    fn invalid(message: &str) -> Self {
        Self::new("invalid_params", message)
    }
}
impl From<agent_git::GitError> for Error {
    fn from(e: agent_git::GitError) -> Self {
        Self::new(e.code, e.message)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Error {}

pub(crate) struct Service {
    store: Store,
    bridge: Bridge,
    captures: Mutex<Snapshots>,
    requests: requests::Requests,
    // A revocation that wins this lock prevents a queued publication. A write
    // that wins first completes its ref transaction before revocation returns.
    publication: RwLock<()>,
}

#[derive(Serialize)]
struct CommitResult {
    worktree_id: String,
    repository_data: bool,
    #[serde(flatten)]
    receipt: agent_git::Committed,
}

#[derive(Serialize)]
struct HistoryPage<'a> {
    captured_at_ms: u64,
    commits: &'a [Value],
    complete: bool,
    truncated: bool,
    next_cursor: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Service {
    pub fn new(state: &Path, bridge: Bridge) -> Arc<Self> {
        Arc::new(Self {
            store: Store::new(state),
            bridge,
            captures: Mutex::default(),
            requests: Default::default(),
            publication: RwLock::default(),
        })
    }
    pub fn issue(&self, repo: &Repository, label: &str, profile: Profile) -> Result<String> {
        self.store.issue(repo, label, profile)
    }
    pub fn grants(&self, repo: &Repository) -> Result<Vec<GrantInfo>> {
        self.store.grants(repo)
    }
    pub fn revoke(&self, repo: &Repository, id: &str) -> Result<()> {
        let _guard = self.publication.write().unwrap();
        self.store.revoke(repo, id)?;
        self.captures.lock().unwrap().revoke(id);
        Ok(())
    }

    fn save(&self, grant: &Authorized, time: u64, content: Content) -> Result<String> {
        self.captures
            .lock()
            .unwrap()
            .put(&grant.info.id, time, content)
    }
    fn get(&self, grant: &Authorized, id: &str) -> Result<Arc<Capture>> {
        self.captures
            .lock()
            .unwrap()
            .get(&grant.info.id, handle(id)?)
    }

    fn reauthorize(&self, token: &str, original: &Authorized) -> Result<()> {
        let live = self.store.authenticate(token)?;
        if live.info.id != original.info.id
            || live.info.profile != original.info.profile
            || live.worktree != original.worktree
        {
            return Err(Error::denied());
        }
        Ok(())
    }

    pub fn handle(&self, token: &str, request: Request) -> Result<Value> {
        let grant = self.store.authenticate(token)?;
        let allowed = Operation::allowed(grant.info.profile);
        let (operation, args) = match request {
            Request::List => {
                grant.worktree.reopen()?;
                self.reauthorize(token, &grant)?;
                return Ok(Value::Array(allowed.iter().map(|op| op.schema()).collect()));
            }
            Request::Call { name, arguments } => {
                let op = allowed
                    .iter()
                    .find(|op| op.name() == name)
                    .ok_or_else(Error::denied)?;
                (*op, arguments)
            }
        };
        let repo = grant.worktree.reopen()?;
        let time = now();
        let mut result = match operation {
            Operation::Review => {
                arguments::<Empty>(args)?;
                let review = agent_git::review(&repo)?;
                let mut data = review.data;
                data["diff_handle"] =
                    json!(self.save(&grant, time, Content::Diff(review.patch))?);
                data["review_id"] =
                    json!(self.save(&grant, time, Content::Review(review.review))?);
                data["captured_at_ms"] = json!(time);
                data
            }
            Operation::Commit => return self.commit(&repo, &grant, token, arguments(args)?),
            Operation::View => {
                arguments::<Empty>(args)?;
                let view = self.bridge.capture(&grant.worktree)?;
                let mut data = view.data;
                data["view_handle"] = json!(
                    view.graph
                        .map(|g| self.save(&grant, time, Content::Graph(g)))
                        .transpose()?
                );
                data["diff_handle"] = json!(
                    view.patch
                        .map(|p| self.save(&grant, time, Content::Diff(p)))
                        .transpose()?
                );
                data["captured_at_ms"] = json!(time);
                data
            }
            Operation::Status => {
                arguments::<Empty>(args)?;
                let mut capture = agent_git::status(&repo)?;
                let handles = capture
                    .diffs
                    .into_iter()
                    .map(|p| self.save(&grant, time, Content::Diff(p)))
                    .collect::<Result<Vec<_>>>()?;
                for group in ["staged", "unstaged", "conflicted", "untracked"] {
                    if let Some(entries) = capture.data[group].as_array_mut() {
                        for entry in entries {
                            if let Some(index) = entry
                                .as_object_mut()
                                .and_then(|e| e.remove("diff_index"))
                                .and_then(|v| v.as_u64())
                            {
                                entry["diff_handle"] = json!(handles.get(index as usize));
                            }
                        }
                    }
                }
                capture.data["captured_at_ms"] = json!(time);
                capture.data["complete"] = json!(capture.data["truncated"] == false);
                capture.data
            }
            Operation::History => self.history(&repo, &grant, arguments(args)?, time)?,
            Operation::Diff => self.diff(&grant, arguments(args)?)?,
        };
        // A replaced worktree or revoked grant cannot supply the result of a slow read.
        grant.worktree.reopen()?;
        self.reauthorize(token, &grant)?;
        result["worktree_id"] = json!(grant.worktree.id());
        result["repository_data"] = json!(true);
        Ok(result)
    }

    fn commit(
        &self,
        repo: &Repository,
        grant: &Authorized,
        token: &str,
        args: CommitArgs,
    ) -> Result<Value> {
        args.validate()?;
        self.requests.run(
            &self.store.dir,
            &grant.info.id,
            &args.request_id,
            &args.review_id,
            &args.message,
            || {
                let snapshot = self.get(grant, &args.review_id).map_err(|_| Error::new("snapshot_expired",
                    "Check the branch history for any prior commit, then review the index again."))?;
                let Content::Review(review) = &snapshot.content else {
                    return Err(Error::expired());
                };
                let mut lease =
                    self.bridge
                        .begin_write(&grant.worktree, &grant.info.label)?;
                let publish = || {
                    let denied = || agent_git::GitError {
                        code: "access_denied",
                        message: "The grant or worktree changed. Check Agent access.",
                    };
                    let guard = self.publication.read().unwrap();
                    self.reauthorize(token,grant).map_err(|_| denied())?;
                    grant.worktree.reopen().map_err(|_| denied())?;
                    self.get(grant, &args.review_id)
                        .map_err(|_| agent_git::GitError {
                            code: "snapshot_expired",
                            message: "Review the staged changes again.",
                        })?;
                    Ok(guard)
                };
                // The GUI may have queued us while another writer was finishing.
                // Authenticate before preparing any Git objects, then again under
                // Git's final locks. Only that last guard spans publication.
                drop(publish()?);
                let outcome = agent_git::commit(repo, review, &args.message, publish);
                if outcome.is_ok()
                    || outcome
                        .as_ref()
                        .is_err_and(|e| matches!(e.code, "state_changed" | "outcome_unknown"))
                {
                    self.captures
                        .lock()
                        .unwrap()
                        .remove(&grant.info.id, &args.review_id);
                }
                let receipt = outcome?;
                lease.committed(&receipt.commit);
                Ok(json!(CommitResult {
                    worktree_id: grant.worktree.id(),
                    repository_data: true,
                    receipt,
                }))
            },
        )
    }

    fn cursor(&self, grant: &Authorized, id: &str) -> Result<(String, usize, usize)> {
        let capture = self.get(grant, id)?;
        match &capture.content {
            Content::Cursor {
                original,
                offset,
                limit,
            } => Ok((original.clone(), *offset, *limit)),
            _ => Err(Error::expired()),
        }
    }

    fn next_cursor(
        &self,
        grant: &Authorized,
        time: u64,
        id: &str,
        end: usize,
        total: usize,
        limit: usize,
    ) -> Result<Option<String>> {
        if end >= total {
            return Ok(None);
        }
        self.save(
            grant,
            time,
            Content::Cursor {
                original: id.to_owned(),
                offset: end,
                limit,
            },
        )
        .map(Some)
    }

    fn history(
        &self,
        repo: &Repository,
        grant: &Authorized,
        args: HistoryArgs,
        time: u64,
    ) -> Result<Value> {
        let (id, start, limit) = if let Some(cursor) = args.cursor {
            if args.view_handle.is_some() || args.limit.is_some() {
                return Err(Error::invalid(
                    "Continue the original history query with cursor alone.",
                ));
            }
            self.cursor(grant, &cursor)?
        } else {
            let limit = args.limit.unwrap_or(50);
            if !(1..=100).contains(&limit) {
                return Err(Error::invalid("Choose a history page size from 1 to 100."));
            }
            let graph = if let Some(view) = args.view_handle {
                let capture = self.get(grant, &view)?;
                let Content::Graph(g) = &capture.content else {
                    return Err(Error::expired());
                };
                g.clone()
            } else {
                GraphView::default()
            };
            let history = agent_git::history(repo, &graph)?;
            (self.save(grant, time, Content::History(history))?, 0, limit)
        };
        let capture = self.get(grant, &id)?;
        let Content::History(history) = &capture.content else {
            return Err(Error::expired());
        };
        let end = (start + limit).min(history.rows.len());
        let next = self.next_cursor(grant, capture.time, &id, end, history.rows.len(), limit)?;
        Ok(json!(HistoryPage {
            captured_at_ms: capture.time,
            commits: &history.rows[start..end],
            complete: next.is_none() && !history.truncated,
            truncated: history.truncated,
            next_cursor: next,
        }))
    }

    fn diff(&self, grant: &Authorized, args: DiffArgs) -> Result<Value> {
        let id = handle(&args.diff_handle)?;
        let start = if let Some(cursor) = &args.cursor {
            let (original, offset, _) = self.cursor(grant, cursor)?;
            if original != id {
                return Err(Error::expired());
            }
            offset
        } else {
            0
        };
        let capture = self.get(grant, id)?;
        let Content::Diff(patch) = &capture.content else {
            return Err(Error::expired());
        };
        const PAGE: usize = 16 * 1024;
        let mut end = (start + PAGE).min(patch.text.len());
        while !patch.text.is_char_boundary(end) {
            end -= 1;
        }
        let mut data = patch.meta.clone();
        let next = self.next_cursor(grant, capture.time, id, end, patch.text.len(), PAGE)?;
        data["captured_at_ms"] = json!(capture.time);
        data["text"] = json!(&patch.text[start..end]);
        data["next_cursor"] = json!(next);
        data["complete"] = json!(
            next.is_none()
                && !matches!(
                    data["state"].as_str(),
                    Some("truncated" | "loading" | "unreadable")
                )
        );
        Ok(data)
    }
}
