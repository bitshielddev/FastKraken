use super::{Error, Result, grants::nonce};
use crate::{
    agent_git::{HistoryCapture, Patch},
    graph_view::GraphView,
};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

const LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ITEMS: usize = 2048;

pub(super) enum Content {
    Diff(Patch),
    Review(crate::agent_git::Reviewed),
    History(HistoryCapture),
    Graph(GraphView),
    Cursor {
        original: String,
        offset: usize,
        limit: usize,
    },
}

impl Content {
    fn size(&self) -> usize {
        (match self {
            Self::Review(review) => review.size(),
            Self::Diff(p) => p.text.len() + p.meta.to_string().len(),
            Self::History(h) => h.rows.iter().map(|r| r.to_string().len()).sum(),
            Self::Graph(g) => {
                serde_json::to_vec(g).map_or(0, |v| v.len())
                    + g.stale.iter().map(String::len).sum::<usize>()
            }
            Self::Cursor { original, .. } => original.len(),
        }) + 256
    }
}

pub(super) struct Capture {
    pub content: Content,
    pub time: u64,
}

struct Entry {
    id: String,
    grant: String,
    until: Instant,
    bytes: usize,
    capture: Arc<Capture>,
}

#[derive(Default)]
pub(super) struct Snapshots {
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl Snapshots {
    pub fn put(&mut self, grant: &str, time: u64, content: Content) -> Result<String> {
        let bytes = content.size();
        if bytes > MAX_BYTES {
            return Err(Error::new(
                "unsupported_configuration",
                "This capture exceeds the local snapshot limit.",
            ));
        }
        while self.entries.front().is_some_and(|e| {
            e.until <= Instant::now()
                || self.bytes + bytes > MAX_BYTES
                || self.entries.len() >= MAX_ITEMS
        }) {
            self.bytes -= self.entries.pop_front().unwrap().bytes;
        }
        let id = nonce()?;
        self.entries.push_back(Entry {
            id: id.clone(),
            grant: grant.into(),
            until: Instant::now() + LIFETIME,
            bytes,
            capture: Arc::new(Capture { content, time }),
        });
        self.bytes += bytes;
        Ok(id)
    }

    pub fn get(&self, grant: &str, id: &str) -> Result<Arc<Capture>> {
        self.entries
            .iter()
            .find(|e| e.id == id && e.grant == grant && e.until > Instant::now())
            .map(|e| e.capture.clone())
            .ok_or_else(Error::expired)
    }

    pub fn remove(&mut self, grant: &str, id: &str) {
        if let Some(at) = self
            .entries
            .iter()
            .position(|e| e.grant == grant && e.id == id)
        {
            self.bytes -= self.entries.remove(at).unwrap().bytes;
        }
    }

    pub fn revoke(&mut self, grant: &str) {
        self.entries.retain(|e| e.grant != grant);
        self.bytes = self.entries.iter().map(|e| e.bytes).sum();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handles_are_grant_bound_expire_and_are_invalidated_on_revoke() {
        let mut s = Snapshots::default();
        let id = s
            .put("one", 42, Content::Graph(GraphView::default()))
            .unwrap();
        assert!(s.get("two", &id).is_err());
        assert_eq!(s.get("one", &id).unwrap().time, 42);
        s.entries[0].until = Instant::now();
        assert!(s.get("one", &id).is_err());
        let id = s
            .put("two", 43, Content::Graph(GraphView::default()))
            .unwrap();
        s.revoke("two");
        assert!(s.get("two", &id).is_err());
        assert_eq!(s.bytes, 0);
    }
}
