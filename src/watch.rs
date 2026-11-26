use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use async_channel::Receiver;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

const WORK: u8 = 1;
const REFS: u8 = 2;

pub struct Changes {
    pub ready: Receiver<()>,
    flags: Arc<AtomicU8>,
}

impl Changes {
    pub fn take(&self) -> bool {
        while self.ready.try_recv().is_ok() {}
        self.flags.swap(0, Ordering::AcqRel) & REFS != 0
    }
}

fn operation_marker(relative: &Path) -> bool {
    [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "AUTO_MERGE",
        "BISECT_LOG",
    ]
    .iter()
    .any(|name| relative == Path::new(name))
        || ["rebase-merge", "rebase-apply", "sequencer"].iter().any(|dir| relative.starts_with(dir))
}

fn other_worktree(relative: &Path) -> bool {
    let Ok(inside) = relative.strip_prefix("worktrees") else { return false };
    let parts: Vec<_> = inside.components().collect();
    parts.len() <= 1 || (parts.len() == 2 && ["HEAD", "locked", "gitdir"].iter().any(|name| parts[1].as_os_str() == *name))
}

fn inside_module(relative: &Path) -> bool {
    relative.starts_with("modules")
        && (relative.file_name().is_some_and(|name| name == "HEAD" || name == "packed-refs")
            || relative.components().any(|part| part.as_os_str() == "refs"))
}

fn changed_path(path: &Path, metadata: &[PathBuf]) -> u8 {
    for dir in metadata {
        if let Ok(relative) = path.strip_prefix(dir) {
            if relative == Path::new("index") {
                return WORK;
            }
            if relative.as_os_str().is_empty()
                || relative == Path::new("HEAD")
                || relative == Path::new("packed-refs")
                || relative == Path::new("logs/refs/stash")
                || relative.starts_with("refs")
                || operation_marker(relative)
                || other_worktree(relative)
            {
                return WORK | REFS;
            }
            if inside_module(relative) {
                return WORK;
            }
            return 0;
        }
    }
    WORK
}

pub fn subscribe(
    root: &Path,
    paths: Vec<PathBuf>,
) -> Result<(RecommendedWatcher, Changes), String> {
    let (send, ready) = async_channel::bounded(1);
    let flags = Arc::new(AtomicU8::new(0));
    let event_flags = Arc::clone(&flags);
    let mut metadata: Vec<_> = paths.iter().filter(|p| *p != root).cloned().collect();
    metadata.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let bits = match result {
            Ok(event) if event.need_rescan() => WORK | REFS,
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => 0,
            Ok(event) => event
                .paths
                .iter()
                .fold(0, |bits, path| bits | changed_path(path, &metadata)),
            Err(_) => WORK | REFS,
        };
        if bits != 0 {
            event_flags.fetch_or(bits, Ordering::Release);
            let _ = send.try_send(());
        }
    })
    .map_err(|error| error.to_string())?;
    for path in &paths {
        if paths
            .iter()
            .any(|outer| outer != path && path.starts_with(outer))
        {
            continue;
        }
        watcher
            .watch(path, RecursiveMode::Recursive)
            .map_err(|error| error.to_string())?;
    }
    Ok((watcher, Changes { ready, flags }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_index_and_refs_in_a_linked_worktree() {
        let common = PathBuf::from("/fixture/.git");
        let linked = common.join("worktrees/review");
        let roots = vec![linked.clone(), common.clone()];
        assert_eq!(changed_path(&linked.join("index"), &roots), WORK);
        for marker in [
            "MERGE_HEAD",
            "REVERT_HEAD",
            "CHERRY_PICK_HEAD",
            "REBASE_HEAD",
            "AUTO_MERGE",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-merge/msgnum",
            "rebase-apply/next",
            "sequencer/todo",
        ] {
            assert_eq!(changed_path(&linked.join(marker), &roots), WORK | REFS, "{marker}");
            assert_eq!(changed_path(&common.join(marker), &roots), WORK | REFS, "{marker}");
        }
        assert_eq!(changed_path(&linked.join("rebase-merged"), &roots), 0);
        assert_eq!(changed_path(&linked.join("MERGE_MSG"), &roots), 0);
        assert_eq!(changed_path(&linked.join("HEAD"), &roots), WORK | REFS);
        assert_eq!(
            changed_path(&common.join("refs/heads/main"), &roots),
            WORK | REFS
        );
        assert_eq!(
            changed_path(&common.join("logs/refs/stash"), &roots),
            WORK | REFS
        );
        assert_eq!(changed_path(&common.join("index.lock"), &roots), 0);
        assert_eq!(
            changed_path(&common.join("objects/pack/pack-file"), &roots),
            0
        );
        assert_eq!(changed_path(Path::new("/working/notes.txt"), &roots), WORK);
    }

    #[test]
    fn a_submodule_moving_or_committing_reloads_the_status_but_its_objects_do_not() {
        let common = PathBuf::from("/fixture/.git");
        let roots = vec![common.clone()];
        for moved in ["modules/maps/HEAD", "modules/maps/refs/heads/main", "modules/maps/packed-refs", "modules/maps/modules/inner/HEAD"] {
            assert_eq!(changed_path(&common.join(moved), &roots), WORK, "{moved}");
        }
        for quiet in ["modules/maps/objects/ab/cdef", "modules/maps/index.lock", "modules/maps/FETCH_HEAD", "modules/maps/config"] {
            assert_eq!(changed_path(&common.join(quiet), &roots), 0, "{quiet}");
        }
    }

    #[test]
    fn another_worktree_appearing_switching_or_going_refreshes_the_list() {
        let common = PathBuf::from("/fixture/.git");
        let roots = vec![common.clone()];
        for changed in ["worktrees", "worktrees/ridge", "worktrees/ridge/HEAD", "worktrees/ridge/locked", "worktrees/ridge/gitdir"] {
            assert_eq!(changed_path(&common.join(changed), &roots), WORK | REFS, "{changed}");
        }
        for quiet in ["worktrees/ridge/index", "worktrees/ridge/logs/HEAD", "worktrees/ridge/ORIG_HEAD", "worktreesx/HEAD"] {
            assert_eq!(changed_path(&common.join(quiet), &roots), 0, "{quiet}");
        }
    }
}
