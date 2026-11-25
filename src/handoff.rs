use crate::{
    Tab,
    commands::Side,
    notice::Notice,
    repository::{Repository, validate_path},
    view::{self, Tone},
};
use gpui::{Context, Window};
use std::path::{Path, PathBuf};

pub(crate) const COMMANDS: &[crate::commands::Command] = {
    use crate::commands::{Availability, Bar, Command, Group, Target, commit, on_repo};
    use view::{Action as Run, Ui};
    fn file(target: &Target) -> Option<&PathBuf> {
        match target {
            Target::File { path, .. } | Target::Conflict(path) | Target::Code { path: Some(path), .. } => Some(path),
            _ => None,
        }
    }
    fn missing(ui: &Ui, path: &Path) -> Option<String> {
        locate(&ui.root, path).err()
    }
    &[
        Command::new(
            "file.edit",
            Group::Open,
            |ui, target| crate::tools::file_title(ui, if let Target::Code { line, .. } = target { *line } else { None }),
            |ui, target, _| match target {
                Target::File { path, side: Side::Unstaged | Side::Staged }
                | Target::Conflict(path)
                | Target::Code { path: Some(path), line: Some(_), .. } => Availability::unless(missing(ui, path)),
                _ => Availability::Hidden,
            },
            |_, target| match target {
                Target::Code { path: Some(path), line: Some(line), .. } => {
                    Some(Run::Handoff(Action::Edit { path: path.clone(), line: Some(*line) }))
                }
                target => file(target).map(|path| Run::Handoff(Action::Edit { path: path.clone(), line: None })),
            },
        )
        .words(&["external editor", "code", "vs code", "edit"])
        .icon("external"),
        Command::new(
            "file.open",
            Group::Open,
            |_, _| "Open with default app".into(),
            |ui, target, _| match target {
                Target::File { path, side: Side::Unstaged | Side::Staged } | Target::Conflict(path) => {
                    Availability::unless(missing(ui, path))
                }
                _ => Availability::Hidden,
            },
            |_, target| file(target).map(|path| Run::Handoff(Action::Open(path.clone()))),
        )
        .words(&["editor", "launch"])
        .icon("external"),
        Command::new(
            "file.reveal",
            Group::Open,
            |_, _| "Reveal in Finder".into(),
            |ui, target, _| match file(target) {
                Some(path) => Availability::unless(missing(ui, path)),
                None => Availability::Hidden,
            },
            |_, target| file(target).map(|path| Run::Handoff(Action::Reveal(Some(path.clone())))),
        )
        .words(&["file manager", "folder", "show in finder"])
        .icon("folder"),
        Command::new(
            "repo.reveal",
            Group::Open,
            |_, _| "Reveal in Finder".into(),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(Run::Handoff(Action::Reveal(None))),
        )
        .words(&["file manager", "folder", "show in finder"])
        .icon("folder")
        .keys("alt-o")
        .bar(Bar::File),
        Command::new(
            "repo.terminal",
            Group::Open,
            |ui, _| crate::tools::terminal_title(ui),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(Run::Handoff(Action::Terminal)),
        )
        .words(&["shell", "console", "command line", "iterm", "ghostty", "warp", "kitty"])
        .icon("external")
        .keys("alt-t")
        .bar(Bar::File),
        Command::new(
            "repo.editor",
            Group::Open,
            |ui, _| crate::tools::repo_title(ui),
            |ui, target, _| {
                if on_repo(ui, target) { Availability::Enabled } else { Availability::Hidden }
            },
            |_, _| Some(Run::Handoff(Action::EditRepo)),
        )
        .words(&["external editor", "code", "vs code", "project", "folder"])
        .icon("external")
        .keys("cmd-shift-e")
        .bar(Bar::File),
        Command::new(
            "commit.copy-message",
            Group::Copy,
            |_, _| "Copy message".into(),
            |_, target, _| {
                if commit(target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |_, target| commit(target).map(|oid| Run::Handoff(Action::CopyMessage(oid.to_owned()))),
        )
        .words(&["clipboard"])
        .icon("copy"),
        Command::new(
            "file.copy-path",
            Group::Copy,
            |_, _| "Copy path".into(),
            |_, target, _| {
                if file(target).is_some() { Availability::Enabled } else { Availability::Hidden }
            },
            |_, target| {
                file(target).map(|path| Run::CopyText {
                    text: path.to_string_lossy().into_owned(),
                    what: "path",
                })
            },
        )
        .words(&["clipboard"])
        .icon("copy"),
        Command::new(
            "file.copy-full-path",
            Group::Copy,
            |_, _| "Copy full path".into(),
            |ui, target, _| {
                if file(target).is_some() && !ui.root.as_os_str().is_empty() {
                    Availability::Enabled
                } else {
                    Availability::Hidden
                }
            },
            |ui, target| {
                file(target).map(|path| Run::CopyText {
                    text: ui.root.join(path).to_string_lossy().into_owned(),
                    what: "full path",
                })
            },
        )
        .words(&["clipboard", "absolute"])
        .icon("copy"),
    ]
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    CopyMessage(String),
    Reveal(Option<PathBuf>),
    Open(PathBuf),
    Terminal,
    Edit { path: PathBuf, line: Option<u32> },
    EditRepo,
    OpenUrl(String),
}

static OPENED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn openable(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| hosted(url, rest))
}

pub(crate) fn linkable(url: &str) -> bool {
    url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).is_some_and(|rest| hosted(url, rest))
}

fn hosted(url: &str, rest: &str) -> bool {
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.is_empty() && !host.contains('@') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub(crate) fn open_url(cx: &mut gpui::App, url: &str) {
    if cfg!(test) || crate::acceptance::replaying() {
        OPENED.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_owned());
        return;
    }
    cx.open_url(url);
}

fn reveal(cx: &mut gpui::App, path: &Path) {
    if crate::tools::standing_in() {
        crate::tools::record(vec!["reveal".into(), path.to_string_lossy().into_owned()]);
        return;
    }
    cx.reveal_path(path);
}

fn open_with_system(cx: &mut gpui::App, path: &Path) {
    if crate::tools::standing_in() {
        crate::tools::record(vec!["open".into(), path.to_string_lossy().into_owned()]);
        return;
    }
    cx.open_with_system(path);
}

pub(crate) fn opened() -> Vec<String> {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn locate(root: &Path, path: &Path) -> Result<PathBuf, String> {
    validate_path(path)?;
    let full = root.join(path);
    if root.as_os_str().is_empty() || full.symlink_metadata().is_err() {
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
        return Err(format!("{name} is not in the working tree"));
    }
    Ok(full)
}

fn message(repo: &Repository, oid: &str) -> Result<String, String> {
    Ok(repo.commit_message(oid)?.trim_end_matches('\n').to_owned())
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    if let Action::OpenUrl(url) = &action {
        if openable(url) {
            open_url(cx, url);
        } else {
            tab.ui.say(Tone::Error, "FastKraken opens only https:// addresses");
            cx.notify();
        }
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    match action {
        Action::OpenUrl(_) => {}
        Action::CopyMessage(oid) => Tab::work(
            cx,
            move || message(&repo, &oid),
            |tab, result, cx| {
                match result {
                    Ok(text) => {
                        view::copy(text, cx);
                        tab.ui.notices.show(Notice::copied("message"));
                    }
                    Err(error) => tab.ui.say(Tone::Error, error),
                }
                cx.notify();
            },
        ),
        Action::Reveal(None) => reveal(cx, &repo.root),
        Action::Reveal(Some(path)) => match locate(&repo.root, &path) {
            Ok(full) => reveal(cx, &full),
            Err(error) => {
                tab.ui.say(Tone::Error, error);
                cx.notify();
            }
        },
        Action::Open(path) => match locate(&repo.root, &path) {
            Ok(full) => open_with_system(cx, &full),
            Err(error) => {
                tab.ui.say(Tone::Error, error);
                cx.notify();
            }
        },
        Action::Terminal => crate::tools::open_terminal(tab, window, cx),
        Action::EditRepo => crate::tools::open_in_editor(tab, None, None, window, cx),
        Action::Edit { path, line } => match locate(&repo.root, &path) {
            Ok(full) => crate::tools::open_in_editor(tab, Some(full), line, window, cx),
            Err(error) => {
                tab.ui.say(Tone::Error, error);
                cx.notify();
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    #[test]
    fn a_working_tree_file_is_located_inside_the_repository() {
        let f = Fixture::new();
        fs::create_dir(f.dir.join("docs")).unwrap();
        fs::write(f.dir.join("docs/trail.txt"), "FastKraken: git for the impatient\n").unwrap();
        let full = locate(&f.repo.root, Path::new("docs/trail.txt")).unwrap();
        assert_eq!(full, f.repo.root.join("docs/trail.txt"));
        assert!(full.is_absolute());
        for outside in ["../trail.txt", "/etc/hosts", "docs/../../x", ""] {
            assert!(locate(&f.repo.root, Path::new(outside)).is_err(), "{outside} was accepted");
        }
        assert!(locate(Path::new(""), Path::new("docs/trail.txt")).is_err());
    }

    #[test]
    fn a_file_deleted_after_the_menu_opened_is_refused() {
        let f = Fixture::new();
        let file = f.dir.join("trail.txt");
        fs::write(&file, "FastKraken: git for the impatient\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", "Start the trail"]);
        assert!(locate(&f.repo.root, Path::new("trail.txt")).is_ok());
        fs::remove_file(&file).unwrap();
        assert_eq!(
            locate(&f.repo.root, Path::new("trail.txt")),
            Err("trail.txt is not in the working tree".to_owned())
        );
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b" D trail.txt\n");
    }

    #[test]
    fn copy_message_takes_the_whole_message_and_changes_nothing() {
        let f = Fixture::new();
        fs::write(f.dir.join("trail.txt"), "FastKraken: git for the impatient\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", "Mark the ford", "-m", "PROJ-12: the river rose.\n\nSee https://example.invalid/ford"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap();
        let head = head.trim_end();
        let index = f.git(&["ls-files", "--stage"]);
        let refs = f.git(&["for-each-ref"]);
        assert_eq!(
            message(&f.repo, head).unwrap(),
            "Mark the ford\n\nPROJ-12: the river rose.\n\nSee https://example.invalid/ford"
        );
        assert_eq!(f.git(&["ls-files", "--stage"]), index);
        assert_eq!(f.git(&["for-each-ref"]), refs);
        assert_eq!(f.git(&["status", "--porcelain=v1"]), b"");
        assert!(message(&f.repo, "not-an-object").is_err());
    }

    #[test]
    fn only_https_addresses_with_a_host_are_opened() {
        for url in ["https://github.com/cedar/ledger/pull/new/topic", "https://example.invalid/a?b=c#d"] {
            assert!(openable(url), "{url}");
        }
        for url in ["http://example.invalid/", "file:///etc/hosts", "https://", "https://user:pw@example.invalid/", "https://example.invalid/a b", "javascript:alert(1)"] {
            assert!(!openable(url), "{url}");
        }
        assert!(linkable("http://example.invalid/ford") && linkable("https://example.invalid/"));
        for url in ["http://", "ftp://example.invalid/", "http://user@example.invalid/", "http://example.invalid/a\tb"] {
            assert!(!linkable(url), "{url}");
        }
    }
}
