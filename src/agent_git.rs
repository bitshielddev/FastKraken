//! Local Git execution for desktop-authorized agent operations.
mod local;
pub(crate) mod pinned;
mod staged;
use crate::{
    graph_view::GraphView,
    repository::{ChangedFile, DiffOptions, Repository, patch_options},
};
use local::{Local, run};
use serde_json::{Value, json};
pub(crate) use staged::{Committed, Reviewed, commit, review};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

pub(crate) use local::open;

#[derive(Debug, Clone, Copy)]
pub(crate) struct GitError {
    pub code: &'static str,
    pub message: &'static str,
}

type Result<T> = std::result::Result<T, GitError>;
fn unsupported(message: &'static str) -> GitError {
    GitError {
        code: "unsupported_configuration",
        message,
    }
}
fn changed() -> GitError {
    GitError {
        code: "state_changed",
        message: "The checkout, index or working file changed. Capture it again.",
    }
}
fn unavailable() -> GitError {
    unsupported(
        "Git could not read this repository locally. Check its objects and configuration in FastKraken.",
    )
}

#[derive(Debug, Clone)]
pub(crate) struct Patch {
    pub meta: Value,
    pub text: String,
}

pub(crate) struct StatusCapture {
    pub data: Value,
    pub diffs: Vec<Patch>,
}

pub(crate) struct HistoryCapture {
    pub rows: Vec<Value>,
    pub truncated: bool,
}

const MAX_FILES: usize = 48;
const MAX_PATCH: usize = 128 * 1024;
const MAX_TOTAL_PATCH: usize = 1024 * 1024;
const MAX_COMMITS: usize = 512;

fn label(value: &OsStr) -> String {
    if let Some(text) = value.to_str() {
        return text.to_owned();
    }
    use std::fmt::Write;
    let mut id = String::from("bytes-");
    for b in value.as_bytes() {
        let _ = write!(&mut id, "{b:02x}");
    }
    id
}

fn patch(meta: Value, bytes: Vec<u8>, truncated: bool) -> Patch {
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let state = if truncated {
        "truncated"
    } else if bytes.is_empty() {
        "empty"
    } else if text
        .lines()
        .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch")
    {
        "binary"
    } else {
        "ready"
    };
    let mut meta = meta;
    meta["state"] = json!(state);
    Patch { meta, text }
}

fn stamp(path: &Path) -> Result<Option<(u64, u64, u64, i64, i64)>> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some((m.dev(), m.ino(), m.len(), m.ctime(), m.ctime_nsec()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(unavailable()),
    }
}

fn file_patch(local: &Local, file: &ChangedFile, side: &str, limit: usize) -> Result<Patch> {
    let meta = json!({"source":"status","side":side,"file":label(file.path.as_os_str()),
        "from":file.from.as_ref().map(|p| label(p.as_os_str()))});
    let failed = || Patch {
        meta: {
            let mut m = meta.clone();
            m["state"] = json!("unreadable");
            m
        },
        text: String::new(),
    };
    if side == "conflicted" || file.submodule.is_some() || limit == 0 {
        return Ok(failed());
    }
    crate::repository::validate_path(&file.path).map_err(|_| unavailable())?;
    let path = local.repo.root.join(&file.path);
    let before = stamp(&path)?;
    let mut cmd = local.command();
    let captured;
    if side == "untracked" {
        let Some((directory, truncated)) = local::capture_untracked(local, &file.path, limit)?
        else {
            return Ok(failed());
        };
        captured = directory;
        if truncated {
            // Git would label a private prefix with the wrong blob ID and hunk
            // length. Preserve the truncation state without inventing a patch.
            return Ok(patch(meta, Vec::new(), true));
        }
        cmd.current_dir(captured.path());
        cmd.args(["diff", "--no-index"])
            .args(patch_options(false, DiffOptions::default()))
            .args(["--", "/dev/null"])
            .arg(&file.path);
    } else {
        cmd.arg("diff")
            .args(patch_options(file.from.is_some(), DiffOptions::default()))
            .args(["--ignore-submodules=all", "--submodule=short"]);
        if side == "staged" {
            cmd.arg("--cached");
        }
        cmd.arg("--");
        if let Some(from) = &file.from {
            crate::repository::validate_path(from).map_err(|_| unavailable())?;
            cmd.arg(from);
        }
        cmd.arg(&file.path);
    }
    let output = run(&mut cmd, None, limit, local.deadline)?;
    if side != "staged" && stamp(&path)? != before {
        return Err(changed());
    }
    if !output.truncated
        && !output.status.success()
        && !(side == "untracked" && output.status.code() == Some(1))
    {
        return Ok(failed());
    }
    Ok(patch(meta, output.bytes, output.truncated))
}

pub(crate) fn status(repo: &Repository) -> Result<StatusCapture> {
    let local = Local::new(repo, true)?;
    let read = || {
        local.read(
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--no-ahead-behind",
                "--untracked-files=all",
                "--ignore-submodules=all",
                "--find-renames",
                "-z",
            ],
            4 * 1024 * 1024,
        )
    };
    let output = read()?;
    let (status, _) = Repository::parse_status(&output).map_err(|_| unavailable())?;
    let mut data = json!({"staged":[],"unstaged":[],"untracked":[],"conflicted":[],
        "branch":status.head_ref.as_deref().map(|name| label(name).trim_start_matches("refs/heads/").to_owned()),
        "head":status.head,"upstream":status.upstream,"upstream_source":"locally_observed",
        "submodules":"not_inspected", "truncated":false});
    let mut conflicts = HashSet::new();
    let mut diffs = Vec::new();
    let mut remaining = MAX_TOTAL_PATCH;
    for (files, side) in [(&status.staged, "staged"), (&status.unstaged, "unstaged")] {
        for file in files {
            let side = match file.status {
                "Untracked" => "untracked",
                "Conflicted" if !conflicts.insert(file.path.clone()) => continue,
                "Conflicted" => "conflicted",
                _ => side,
            };
            if diffs.len() == MAX_FILES {
                data["truncated"] = json!(true);
                break;
            }
            let captured = file_patch(&local, file, side, remaining.min(MAX_PATCH))?;
            remaining = remaining.saturating_sub(captured.text.len());
            data[side].as_array_mut().unwrap().push(json!({"path":label(file.path.as_os_str()),
                "from":file.from.as_ref().map(|p| label(p.as_os_str())), "status":file.status,"diff_index":diffs.len()}));
            diffs.push(captured);
        }
    }
    if output != read()? || !local.same_index_and_checkout()? {
        return Err(changed());
    }
    Ok(StatusCapture { data, diffs })
}

pub(crate) fn history(repo: &Repository, view: &GraphView) -> Result<HistoryCapture> {
    let local = Local::new(repo, false)?;
    let mut cmd = local.command();
    cmd.args([
        "log",
        "--topo-order",
        "--decorate=short",
        crate::journal::UNDECORATED,
        "--no-color",
        "--no-show-signature",
        "--no-patch",
        "--no-notes",
        "-z",
        "--format=%H%x00%P%x00%s%x00%D%x00%an%x00%ae",
    ])
    .arg(format!("--max-count={}", MAX_COMMITS + 1))
    .args(view.history_args())
    .arg("--");
    let output = run(&mut cmd, None, 1024 * 1024, local.deadline)?;
    if !output.truncated && !output.status.success() {
        return Err(unavailable());
    }
    let mut rows = Vec::new();
    let mut fields = output.bytes.split_inclusive(|&b| b == 0);
    while rows.len() < MAX_COMMITS {
        let Some(oid) = fields.next() else { break };
        let Some([oid, parents, subject, refs, author, email]) = (|| {
            Some([
                oid.strip_suffix(&[0])?,
                fields.next()?.strip_suffix(&[0])?,
                fields.next()?.strip_suffix(&[0])?,
                fields.next()?.strip_suffix(&[0])?,
                fields.next()?.strip_suffix(&[0])?,
                fields.next()?.strip_suffix(&[0])?,
            ])
        })() else {
            if output.truncated {
                break;
            }
            return Err(unavailable());
        };
        let refs: Vec<_> = label(OsStr::from_bytes(refs))
            .split(", ")
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        rows.push(json!({"oid":String::from_utf8_lossy(oid),"parents":String::from_utf8_lossy(parents).split_whitespace().collect::<Vec<_>>(),
            "subject":String::from_utf8_lossy(subject),"refs":refs,
            "dimmed":view.dims(&String::from_utf8_lossy(author),&String::from_utf8_lossy(email))}));
    }
    Ok(HistoryCapture {
        rows,
        truncated: output.truncated || fields.next().is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;

    #[test]
    fn status_captures_both_sides_and_leaves_index_refs_and_working_file_intact() {
        let f = Fixture::new();
        let path = f.dir.join("dual");
        fs::write(&path, "base\n").unwrap();
        f.git(&["add", "dual"]);
        f.git(&["commit", "-qm", "base"]);
        fs::write(&path, "staged\n").unwrap();
        f.git(&["add", "dual"]);
        fs::write(&path, "working\n").unwrap();
        let index = fs::read(f.repo.git_dir().join("index")).unwrap();
        let refs = f.git(&["show-ref"]);
        let capture = status(&f.repo).unwrap();
        let staged = capture.data["staged"][0]["diff_index"].as_u64().unwrap() as usize;
        let unstaged = capture.data["unstaged"][0]["diff_index"].as_u64().unwrap() as usize;
        assert_ne!(staged, unstaged);
        assert!(capture.diffs[staged].text.contains("+staged\n"));
        assert!(!capture.diffs[staged].text.contains("working"));
        assert!(capture.diffs[unstaged].text.contains("-staged\n+working\n"));
        assert_eq!(index, fs::read(f.repo.git_dir().join("index")).unwrap());
        assert_eq!(refs, f.git(&["show-ref"]));
        assert_eq!(fs::read(path).unwrap(), b"working\n");
        let history = history(&f.repo, &GraphView::default()).unwrap();
        assert_eq!(history.rows.len(), 1);
        assert_eq!(history.rows[0]["subject"], "base");
        assert_eq!(history.rows[0]["parents"], json!([]));
        assert!(!history.truncated);
    }
    #[test]
    fn filtered_graph_includes_its_pinned_head_and_empty_history_works() {
        let f = Fixture::new();
        assert!(
            history(&f.repo, &GraphView::default())
                .unwrap()
                .rows
                .is_empty()
        );
        f.git(&["commit", "--allow-empty", "-qm", "root"]);
        f.git(&["switch", "-qc", "other"]);
        f.git(&["commit", "--allow-empty", "-qm", "hidden"]);
        f.git(&["switch", "-q", "main"]);
        let view = GraphView {
            hidden: ["refs/heads/other".into()].into(),
            ..GraphView::default()
        };
        assert_eq!(history(&f.repo, &view).unwrap().rows.len(), 1);
        assert_eq!(
            history(&f.repo, &GraphView::default()).unwrap().rows.len(),
            2
        );
    }
    #[test]
    fn refuses_partial_clones_and_configured_filters_without_spawning_them() {
        let f = Fixture::new();
        let marker = f.dir.join("called");
        let script = format!("touch {}", marker.display());
        f.git(&["config", "core.fsmonitor", &script]);
        f.git(&["config", "filter.sample.clean", &script]);
        fs::write(f.dir.join(".gitattributes"), "dual filter=sample\n").unwrap();
        fs::write(f.dir.join("dual"), "content").unwrap();
        assert_eq!(
            status(&f.repo).err().unwrap().code,
            "unsupported_configuration"
        );
        f.git(&["config", "--unset", "filter.sample.clean"]);
        let capture = status(&f.repo).unwrap();
        assert_eq!(capture.data["untracked"].as_array().unwrap().len(), 2);
        f.git(&["config", "remote.origin.promisor", "true"]);
        assert_eq!(
            history(&f.repo, &GraphView::default()).err().unwrap().code,
            "unsupported_configuration"
        );
        assert!(!marker.exists());
    }
    #[test]
    fn binary_and_truncated_worktree_patches_remain_distinguishable() {
        let f = Fixture::new();
        fs::write(f.dir.join("binary"), b"\0binary").unwrap();
        fs::write(f.dir.join("long"), vec![b'x'; MAX_PATCH * 2]).unwrap();
        let capture = status(&f.repo).unwrap();
        let state = |name| {
            capture
                .diffs
                .iter()
                .find(|p| p.meta["file"] == name)
                .unwrap()
                .meta["state"]
                .clone()
        };
        assert_eq!(state("binary"), "binary");
        assert_eq!(state("long"), "truncated");
    }
}
