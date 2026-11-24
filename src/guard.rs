use crate::repository::Run;
use crate::{
    repository::{Repository, checked, validate_oid},
    view::Ui,
};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

type Result<T> = std::result::Result<T, String>;

const CHECKOUT_CHANGED: &str = "The checked-out branch or its tip changed. Reopen the action.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Seen {
    pub head_ref: Option<OsString>,
    pub head: Option<String>,
}

impl Seen {
    pub(crate) fn of(ui: &Ui) -> Self {
        Self {
            head_ref: ui.head_ref.clone(),
            head: ui.head.clone(),
        }
    }

    pub(crate) fn detached_tip(&self) -> Option<&str> {
        self.head_ref.is_none().then_some(self.head.as_deref()).flatten()
    }
}

pub(crate) fn display(full_name: &OsStr) -> String {
    let bytes = full_name.as_bytes();
    let short = ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|prefix| bytes.strip_prefix(prefix.as_bytes()))
        .unwrap_or(bytes);
    String::from_utf8_lossy(short).into_owned()
}

fn line(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

impl Repository {
    pub(crate) fn expect_checkout(&self, seen: &Seen) -> Result<()> {
        if let Some(head) = &seen.head {
            validate_oid(head)?;
        }
        let reference = self
            .read_command()
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let reference = match reference.status.code() {
            Some(0) => Some(line(&reference.stdout).to_vec()),
            Some(1) => None,
            _ => return Err(String::from_utf8_lossy(&reference.stderr).trim().to_owned()),
        };
        let tip = self
            .read_command()
            .args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])
            .run()
            .map_err(|e| format!("Run bundled Git: {e}"))?;
        let tip = tip.status.success().then(|| line(&tip.stdout).to_vec());
        if reference.as_deref() != seen.head_ref.as_deref().map(OsStr::as_bytes)
            || tip.as_deref() != seen.head.as_deref().map(str::as_bytes)
        {
            return Err(CHECKOUT_CHANGED.into());
        }
        Ok(())
    }

    pub(crate) fn expect_ref(&self, full_name: &OsStr, oid: &str) -> Result<()> {
        validate_oid(oid)?;
        match self.ref_target(full_name)? {
            Some(actual) if actual == oid => Ok(()),
            Some(_) => Err(format!(
                "{} changed since you chose it. Reopen the action.",
                display(full_name)
            )),
            None => Err(format!(
                "{} no longer exists. Reopen the action.",
                display(full_name)
            )),
        }
    }

    pub(crate) fn expect_names_commit(&self, full_name: &OsStr, commit: &str) -> Result<()> {
        validate_oid(commit)?;
        if self.ref_target(full_name)?.is_none() {
            return Err(format!("{} no longer exists. Reopen the action.", display(full_name)));
        }
        let mut peeled = full_name.to_owned();
        peeled.push("^{commit}");
        let named = checked(
            self.read_command()
                .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
                .arg(peeled),
        )
        .map_err(|_| format!("{} no longer names a commit. Reopen the action.", display(full_name)))?;
        if line(&named) != commit.as_bytes() {
            return Err(format!("{} changed since you chose it. Reopen the action.", display(full_name)));
        }
        Ok(())
    }

    fn ref_target(&self, full_name: &OsStr) -> Result<Option<String>> {
        if !full_name.as_bytes().starts_with(b"refs/") {
            return Err("Select a branch or tag by its full ref name.".into());
        }
        let listed = checked(
            self.read_command()
                .args(["for-each-ref", "--format=%(refname)%00%(objectname)", "--"])
                .arg(full_name),
        )?;
        Ok(listed.split(|&b| b == b'\n').find_map(|entry| {
            let (name, oid) = entry.split_at(entry.iter().position(|&b| b == 0)?);
            (name == full_name.as_bytes()).then(|| String::from_utf8_lossy(&oid[1..]).into_owned())
        }))
    }

    pub(crate) fn expect_no_operation(&self) -> Result<()> {
        match self.operation()? {
            Some(kind) if kind.holds() => Err(format!("{}.", kind.refusal())),
            _ => Ok(()),
        }
    }

    pub(crate) fn resolve(&self, name: &OsStr) -> Result<String> {
        let mut object = name.to_owned();
        object.push("^{object}");
        let oid = checked(
            self.read_command()
                .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
                .arg(object),
        )
        .map_err(|_| format!("{} does not exist.", display(name)))?;
        let oid = String::from_utf8_lossy(line(&oid)).into_owned();
        validate_oid(&oid)?;
        Ok(oid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn oid(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name]))
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn notebook() -> (Fixture, Seen) {
        let f = Fixture::new();
        fs::write(f.dir.join("notes.txt"), "First page\n").unwrap();
        f.git(&["add", "--", "notes.txt"]);
        f.git(&["commit", "-qm", "Start notebook"]);
        let seen = Seen {
            head_ref: Some("refs/heads/main".into()),
            head: Some(oid(&f, "HEAD")),
        };
        (f, seen)
    }

    #[test]
    fn checkout_check_refuses_another_branch_at_the_same_tip_and_a_moved_tip() {
        let (f, seen) = notebook();
        f.repo.expect_checkout(&seen).unwrap();

        f.git(&["switch", "-qc", "other"]);
        let err = f.repo.expect_checkout(&seen).unwrap_err();
        assert!(err.contains("tip changed"), "{err}");

        f.git(&["switch", "-q", "main"]);
        f.git(&["commit", "--allow-empty", "-qm", "Later work"]);
        let err = f.repo.expect_checkout(&seen).unwrap_err();
        assert!(err.contains("tip changed"), "{err}");

        f.git(&["switch", "-q", "--detach", seen.head.as_deref().unwrap()]);
        assert!(f.repo.expect_checkout(&seen).is_err());
        let detached = Seen { head_ref: None, ..seen.clone() };
        f.repo.expect_checkout(&detached).unwrap();
        f.git(&["switch", "-q", "main"]);
        assert!(f.repo.expect_checkout(&detached).is_err());

        assert!(f.git(&["status", "--porcelain"]).is_empty());
        assert_eq!(f.git(&["symbolic-ref", "HEAD"]), b"refs/heads/main\n");
    }

    #[test]
    fn checkout_check_compares_raw_ref_bytes_and_accepts_an_unborn_branch() {
        let f = Fixture::new();
        let unborn = Seen {
            head_ref: Some("refs/heads/main".into()),
            head: None,
        };
        f.repo.expect_checkout(&unborn).unwrap();
        f.git(&["commit", "--allow-empty", "-qm", "First"]);
        assert!(f.repo.expect_checkout(&unborn).is_err());

        use std::os::unix::ffi::OsStringExt;
        let tip = oid(&f, "HEAD");
        let invalid = OsString::from_vec(b"refs/heads/caf\xe9".to_vec());
        f.git(&["update-ref", "refs/heads/caf\u{fffd}", &tip]);
        f.git(&["symbolic-ref", "HEAD", "refs/heads/caf\u{fffd}"]);
        let seen = Seen {
            head_ref: Some(invalid),
            head: Some(tip),
        };
        assert!(f.repo.expect_checkout(&seen).is_err());
        assert!(f.repo.expect_checkout(&Seen::default()).is_err());
        assert!(
            f.repo
                .expect_checkout(&Seen {
                    head: Some("not an id".into()),
                    ..seen
                })
                .is_err()
        );
    }

    #[test]
    fn ref_check_accepts_the_shown_object_and_refuses_a_moved_or_deleted_ref() {
        let (f, seen) = notebook();
        let first = seen.head.unwrap();
        f.git(&["branch", "topic"]);
        f.git(&["branch", "topic-two"]);
        f.git(&["tag", "-a", "-m", "Release", "v1"]);
        let tag = oid(&f, "refs/tags/v1");
        assert_ne!(tag, first);
        let topic = OsStr::new("refs/heads/topic");
        f.repo.expect_ref(topic, &first).unwrap();
        f.repo.expect_ref(OsStr::new("refs/tags/v1"), &tag).unwrap();
        assert!(f.repo.expect_ref(OsStr::new("refs/tags/v1"), &first).is_err());

        f.git(&["commit", "--allow-empty", "-qm", "Second"]);
        f.git(&["branch", "-f", "topic", "main"]);
        let err = f.repo.expect_ref(topic, &first).unwrap_err();
        assert_eq!(err, "topic changed since you chose it. Reopen the action.");

        f.git(&["branch", "-D", "topic"]);
        let err = f.repo.expect_ref(topic, &first).unwrap_err();
        assert_eq!(err, "topic no longer exists. Reopen the action.");
        f.git(&["branch", "topic/inner", &first]);
        let err = f.repo.expect_ref(topic, &first).unwrap_err();
        assert_eq!(err, "topic no longer exists. Reopen the action.");
        f.repo
            .expect_ref(OsStr::new("refs/heads/topic-two"), &first)
            .unwrap();
        assert!(f.repo.expect_ref(OsStr::new("topic-two"), &first).is_err());
        assert!(f.repo.expect_ref(topic, "abc").is_err());
    }

    #[test]
    fn commit_check_looks_through_a_tag_and_refuses_one_that_moved_or_went() {
        let (f, seen) = notebook();
        let first = seen.head.unwrap();
        f.git(&["tag", "-a", "-m", "Release", "v1"]);
        f.git(&["tag", "light"]);
        let v1 = OsStr::new("refs/tags/v1");
        f.repo.expect_names_commit(v1, &first).unwrap();
        f.repo.expect_names_commit(OsStr::new("refs/tags/light"), &first).unwrap();
        assert!(f.repo.expect_names_commit(v1, &oid(&f, "refs/tags/v1")).is_err());
        f.git(&["tag", "-f", "-a", "-m", "Release again", "v1"]);
        f.repo.expect_names_commit(v1, &first).unwrap();

        f.git(&["commit", "--allow-empty", "-qm", "Second"]);
        f.git(&["tag", "-f", "-a", "-m", "Moved", "v1"]);
        let err = f.repo.expect_names_commit(v1, &first).unwrap_err();
        assert_eq!(err, "v1 changed since you chose it. Reopen the action.");
        f.git(&["tag", "-d", "v1"]);
        let err = f.repo.expect_names_commit(v1, &first).unwrap_err();
        assert_eq!(err, "v1 no longer exists. Reopen the action.");
        let tree = oid(&f, "HEAD^{tree}");
        f.git(&["tag", "tree-tag", &tree]);
        let err = f.repo.expect_names_commit(OsStr::new("refs/tags/tree-tag"), &first).unwrap_err();
        assert_eq!(err, "tree-tag no longer names a commit. Reopen the action.");
        assert!(f.repo.expect_names_commit(OsStr::new("light"), &first).is_err());
        assert!(f.repo.expect_names_commit(OsStr::new("refs/tags/light"), "abc").is_err());
    }

    #[test]
    fn operation_check_names_the_operation_in_progress() {
        let (f, _) = notebook();
        f.repo.expect_no_operation().unwrap();
        for (marker, expected) in [
            ("MERGE_HEAD", "Finish or abort the merge first."),
            ("CHERRY_PICK_HEAD", "Finish or abort the cherry-pick first."),
            ("REVERT_HEAD", "Finish or abort the revert first."),
        ] {
            let path = f.repo.git_dir().join(marker);
            fs::write(&path, "0000000000000000000000000000000000000000\n").unwrap();
            assert_eq!(f.repo.expect_no_operation().unwrap_err(), expected);
            fs::remove_file(path).unwrap();
        }
        for (dir, expected) in [
            ("rebase-merge", "Finish or abort the rebase first."),
            ("rebase-apply", "Finish or abort the rebase first."),
            ("sequencer", "Finish or abort the cherry-pick first."),
        ] {
            let path = f.repo.git_dir().join(dir);
            fs::create_dir(&path).unwrap();
            assert_eq!(f.repo.expect_no_operation().unwrap_err(), expected);
            fs::remove_dir(path).unwrap();
        }
        f.repo.expect_no_operation().unwrap();
    }

    #[test]
    fn operation_check_sees_a_real_conflicted_merge() {
        let (f, _) = notebook();
        f.git(&["switch", "-qc", "side"]);
        fs::write(f.dir.join("notes.txt"), "Side page\n").unwrap();
        f.git(&["commit", "-qam", "Side"]);
        f.git(&["switch", "-q", "main"]);
        fs::write(f.dir.join("notes.txt"), "Main page\n").unwrap();
        f.git(&["commit", "-qam", "Main"]);
        assert!(f.repo.command().args(["merge", "side"]).output().unwrap().status.code() == Some(1));
        assert_eq!(
            f.repo.expect_no_operation().unwrap_err(),
            "Finish or abort the merge first."
        );
    }

    #[test]
    fn resolve_names_the_object_and_refuses_what_does_not_exist() {
        let (f, seen) = notebook();
        let head = seen.head.unwrap();
        assert_eq!(f.repo.resolve(OsStr::new("refs/heads/main")).unwrap(), head);
        assert_eq!(f.repo.resolve(OsStr::new("HEAD")).unwrap(), head);
        assert_eq!(f.repo.resolve(OsStr::new(&head)).unwrap(), head);
        assert_eq!(
            f.repo.resolve(OsStr::new("refs/heads/gone")).unwrap_err(),
            "gone does not exist."
        );
        let missing = "1234567890123456789012345678901234567890";
        assert_ne!(missing, head);
        assert!(f.repo.resolve(OsStr::new(missing)).is_err());
        f.git(&["tag", "-a", "-m", "Release", "v1"]);
        let tag = oid(&f, "refs/tags/v1");
        assert_ne!(tag, head);
        assert_eq!(f.repo.resolve(OsStr::new("refs/tags/v1")).unwrap(), tag);
        assert!(f.repo.resolve(OsStr::new("--all")).is_err());
    }
}
