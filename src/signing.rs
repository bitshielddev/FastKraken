use crate::{
    Tab, kit,
    repository::{Repository, Run, checked, validate_oid},
    theme::*,
    view::{self, Desk, Selection, Ui},
};
use gpui::{Context, Div, SharedString, Stateful, Window, div, prelude::*, px, rgb};

pub(crate) const TAGS_CHECKED: usize = 3;
pub(crate) const PINENTRY_PAGE: &str = "https://formulae.brew.sh/formula/pinentry-mac";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Format {
    #[default]
    Gpg,
    Ssh,
    X509,
}

impl Format {
    fn configured(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "ssh" => Self::Ssh,
            "x509" => Self::X509,
            _ => Self::Gpg,
        }
    }

    pub(crate) fn of_signature(block: &str) -> Self {
        if block.contains("BEGIN SSH SIGNATURE") {
            Self::Ssh
        } else if block.contains("BEGIN SIGNED MESSAGE") {
            Self::X509
        } else {
            Self::Gpg
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Gpg => "GPG",
            Self::Ssh => "SSH",
            Self::X509 => "X.509",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Config {
    pub commits: bool,
    pub tags: bool,
    pub key: Option<String>,
    pub format: Format,
    pub key_command: bool,
}

impl Config {
    pub(crate) fn read(&mut self, key: &str, value: Option<&str>) {
        match key {
            "commit.gpgsign" => self.commits = truth(value),
            "tag.gpgsign" => self.tags = truth(value),
            "user.signingkey" => self.key = value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
            "gpg.format" => self.format = Format::configured(value.unwrap_or_default()),
            "gpg.ssh.defaultkeycommand" => self.key_command = value.is_some_and(|value| !value.trim().is_empty()),
            _ => {}
        }
    }

    pub(crate) fn key_shown(&self) -> Option<String> {
        let key = self.key.as_deref()?;
        let literal = key.strip_prefix("key::").unwrap_or(key);
        if ["ssh-", "ecdsa-", "sk-"].iter().any(|kind| literal.starts_with(kind)) {
            let mut parts = literal.split_whitespace();
            let kind = parts.next().unwrap_or_default();
            let body = parts.next().unwrap_or_default();
            let tail: String = body.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
            let comment: Vec<&str> = parts.collect();
            return Some(if comment.is_empty() { format!("{kind} …{tail}") } else { format!("{kind} …{tail} {}", comment.join(" ")) });
        }
        Some(key.to_owned())
    }

    pub(crate) fn described(&self) -> String {
        let format = self.format.name();
        match (self.key_shown(), self.format) {
            (Some(key), _) => format!("Signed with your {format} key {key}."),
            (None, Format::Ssh) if self.key_command => "Signed with the SSH key gpg.ssh.defaultKeyCommand gives.".to_owned(),
            (None, Format::Ssh) => "Git has no SSH key to sign with: user.signingKey is not set.".to_owned(),
            (None, _) => format!("Signed with the {format} key for your commit email."),
        }
    }
}

pub(crate) fn truth(value: Option<&str>) -> bool {
    let Some(value) = value else { return true };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => true,
        "false" | "no" | "off" | "" => false,
        number => number.parse::<i64>().is_ok_and(|number| number != 0),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grade {
    Verified,
    Untrusted,
    Bad,
    Expired,
    ExpiredKey,
    Revoked,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub grade: Grade,
    pub format: Format,
    pub signer: String,
    pub key: String,
    pub fingerprint: String,
    pub reason: Option<String>,
}

impl Signature {
    fn new(grade: Grade, format: Format) -> Self {
        Self { grade, format, signer: String::new(), key: String::new(), fingerprint: String::new(), reason: None }
    }

    fn because(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into()).filter(|reason: &String| !reason.is_empty());
        self
    }

    pub(crate) fn word(&self) -> &'static str {
        match self.grade {
            Grade::Verified => "Verified",
            Grade::Bad => "Bad signature",
            _ => "Signed",
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self.grade {
            Grade::Verified => self.signer.clone(),
            Grade::Untrusted => "the key's trust is unknown".to_owned(),
            Grade::Bad => String::new(),
            Grade::Expired => "the signature has expired".to_owned(),
            Grade::ExpiredKey => "with an expired key".to_owned(),
            Grade::Revoked => "with a revoked key".to_owned(),
            Grade::Unknown => "cannot be checked here".to_owned(),
        }
    }

    pub(crate) fn sentence(&self) -> String {
        match self.grade {
            Grade::Verified if self.signer.is_empty() => "Verified".to_owned(),
            Grade::Verified => format!("Verified, signed by {}", self.signer),
            Grade::Bad => "Bad signature: it does not match this content".to_owned(),
            Grade::ExpiredKey | Grade::Revoked => format!("Signed {}", self.detail()),
            _ => format!("Signed; {}", self.detail()),
        }
    }

    pub(crate) fn tip(&self) -> String {
        let mut lines = vec![self.sentence(), format!("{} signature", self.format.name())];
        if !self.signer.is_empty() {
            lines.push(format!("Signer: {}", self.signer));
        }
        if !self.key.is_empty() && self.key != self.fingerprint {
            lines.push(format!("Key: {}", self.key));
        }
        if !self.fingerprint.is_empty() {
            lines.push(format!("Fingerprint: {}", self.fingerprint));
        }
        lines.extend(self.reason.clone());
        lines.push("Choose to check it again.".to_owned());
        lines.join("\n")
    }

    fn shade(&self) -> u32 {
        match self.grade {
            Grade::Verified => ADDED,
            Grade::Bad | Grade::Revoked => DELETED,
            Grade::Expired | Grade::ExpiredKey => MODIFIED,
            Grade::Untrusted | Grade::Unknown => TEXT_2,
        }
    }
}

fn not_allowed() -> String {
    "The key is not one of the allowed signers in gpg.ssh.allowedSignersFile.".to_owned()
}

fn unchecked(format: Format, said: &str) -> String {
    let lower = said.to_lowercase();
    if lower.contains("allowedsignersfile") {
        return "Git has no allowed signers file to check SSH signatures against. Set gpg.ssh.allowedSignersFile.".to_owned();
    }
    if let Some(program) = said.split("cannot run ").nth(1).and_then(|rest| rest.split(':').next()).filter(|name| !name.is_empty()) {
        return format!("Git could not run {program} to check it.");
    }
    if format == Format::Gpg {
        return "Its key is not in this Mac's GPG keyring.".to_owned();
    }
    said.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().trim_start_matches("error: ").to_owned()
}

pub(crate) fn graded(format: Format, fields: &str, said: &str) -> Signature {
    let mut parts = fields.trim_end_matches('\n').split('\0');
    let mut next = || parts.next().unwrap_or_default().trim().to_owned();
    let letter = next();
    let (signer, key, fingerprint) = (next(), next(), next());
    let mut signature = match (letter.as_str(), format) {
        ("G", _) => Signature::new(Grade::Verified, format),
        ("U", Format::Ssh) => Signature::new(Grade::Unknown, format).because(not_allowed()),
        ("U", _) => Signature::new(Grade::Untrusted, format),
        ("B", _) => Signature::new(Grade::Bad, format),
        ("X", _) => Signature::new(Grade::Expired, format),
        ("Y", _) => Signature::new(Grade::ExpiredKey, format),
        ("R", _) => Signature::new(Grade::Revoked, format),
        _ => Signature::new(Grade::Unknown, format).because(unchecked(format, said)),
    };
    signature.signer = signer;
    signature.key = key;
    signature.fingerprint = fingerprint;
    signature
}

pub(crate) fn raw_graded(format: Format, said: &str) -> Signature {
    if format == Format::Ssh {
        return ssh_graded(said);
    }
    let mut found: Option<Signature> = None;
    let mut trusted = false;
    let mut fingerprint = String::new();
    for line in said.lines() {
        let Some(status) = line.trim().strip_prefix("[GNUPG:] ") else { continue };
        let (word, rest) = status.split_once(' ').unwrap_or((status, ""));
        let (key, signer) = rest.split_once(' ').unwrap_or((rest, ""));
        let grade = match word {
            "GOODSIG" => Grade::Untrusted,
            "BADSIG" => Grade::Bad,
            "EXPSIG" => Grade::Expired,
            "EXPKEYSIG" => Grade::ExpiredKey,
            "REVKEYSIG" => Grade::Revoked,
            "ERRSIG" | "NO_PUBKEY" => Grade::Unknown,
            "VALIDSIG" => {
                fingerprint = key.to_owned();
                continue;
            }
            "TRUST_MARGINAL" | "TRUST_FULLY" | "TRUST_ULTIMATE" => {
                trusted = true;
                continue;
            }
            _ => continue,
        };
        if found.as_ref().is_some_and(|known| known.grade != Grade::Unknown) {
            continue;
        }
        let mut signature = Signature::new(grade, format);
        signature.key = key.to_owned();
        if grade == Grade::Unknown {
            signature = signature.because(unchecked(format, ""));
        } else {
            signature.signer = signer.to_owned();
        }
        found = Some(signature);
    }
    let mut signature = found.unwrap_or_else(|| Signature::new(Grade::Unknown, format).because(unchecked(format, said)));
    if signature.grade == Grade::Untrusted && trusted {
        signature.grade = Grade::Verified;
    }
    signature.fingerprint = fingerprint;
    signature
}

fn ssh_graded(said: &str) -> Signature {
    for line in said.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Good \"git\" signature for ") {
            let (signer, key) = rest.rsplit_once(" with ").unwrap_or((rest, ""));
            let mut signature = Signature::new(Grade::Verified, Format::Ssh);
            signature.signer = signer.to_owned();
            signature.fingerprint = key.rsplit(' ').next().unwrap_or_default().to_owned();
            return signature;
        }
        if let Some(rest) = line.strip_prefix("Good \"git\" signature with ") {
            let mut signature = Signature::new(Grade::Unknown, Format::Ssh).because(not_allowed());
            signature.fingerprint = rest.rsplit(' ').next().unwrap_or_default().to_owned();
            return signature;
        }
    }
    let lower = said.to_lowercase();
    if lower.contains("allowedsignersfile") || lower.contains("cannot run ") {
        return Signature::new(Grade::Unknown, Format::Ssh).because(unchecked(Format::Ssh, said));
    }
    Signature::new(Grade::Bad, Format::Ssh)
}

pub(crate) fn header_signature(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let headers = text.split("\n\n").next().unwrap_or_default();
    let mut block: Option<String> = None;
    for line in headers.lines() {
        match &mut block {
            Some(found) if line.starts_with(' ') => {
                found.push('\n');
                found.push_str(&line[1..]);
            }
            Some(_) => break,
            None => {
                if let Some(first) = line.strip_prefix("gpgsig ").or_else(|| line.strip_prefix("gpgsig-sha256 ")) {
                    block = Some(first.to_owned());
                }
            }
        }
    }
    block.filter(|block| !block.trim().is_empty())
}

impl Repository {
    pub(crate) fn commit_signature(&self, oid: &str) -> Result<Option<Signature>, String> {
        validate_oid(oid)?;
        let raw = checked(self.read_command().args(["cat-file", "commit", oid]))?;
        let Some(block) = header_signature(&raw) else { return Ok(None) };
        let format = Format::of_signature(&block);
        let output = self
            .read_command()
            .args(["log", "-1", "--no-notes", "--format=%G?%x00%GS%x00%GK%x00%GF", oid, "--"])
            .run()
            .map_err(|error| format!("Run bundled Git: {error}"))?;
        if !output.status.success() {
            return Err(crate::repository::git_error(&output.stderr, output.status));
        }
        Ok(Some(graded(format, &String::from_utf8_lossy(&output.stdout), &String::from_utf8_lossy(&output.stderr))))
    }

    pub(crate) fn tag_signatures(&self, oid: &str) -> Result<Vec<(String, Signature)>, String> {
        validate_oid(oid)?;
        let listed = checked(self.read_command().args([
            "for-each-ref",
            "--points-at",
            oid,
            "--format=%(refname)%00%(objecttype)%00%(contents:signature)%00",
            "refs/tags",
        ]))?;
        let text = String::from_utf8_lossy(&listed);
        let fields: Vec<&str> = text.split('\0').collect();
        let mut signed = Vec::new();
        for record in fields.chunks(3) {
            let [name, kind, block] = record else { continue };
            let name = name.trim_start_matches('\n');
            if *kind != "tag" || block.trim().is_empty() {
                continue;
            }
            let Some(short) = name.strip_prefix("refs/tags/") else { continue };
            signed.push((short.to_owned(), Format::of_signature(block)));
        }
        let mut found = Vec::new();
        for (name, format) in signed.into_iter().take(TAGS_CHECKED) {
            let output = self
                .read_command()
                .args(["verify-tag", "--raw"])
                .arg(format!("refs/tags/{name}"))
                .run()
                .map_err(|error| format!("Run bundled Git: {error}"))?;
            let said = String::from_utf8_lossy(&output.stderr).into_owned() + &String::from_utf8_lossy(&output.stdout);
            found.push((name, raw_graded(format, &said)));
        }
        Ok(found)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Recheck,
}

#[derive(Default)]
pub(crate) struct State {
    oid: Option<String>,
    ticket: u64,
    checking: bool,
    pub(crate) commit: Option<Signature>,
    pub(crate) tags: Vec<(String, Signature)>,
    pub(crate) error: Option<String>,
}

impl State {
    pub(crate) fn of(&self, oid: &str) -> Option<&Signature> {
        self.commit.as_ref().filter(|_| self.oid.as_deref() == Some(oid) && !self.checking)
    }

    pub(crate) fn checked(&self, oid: &str) -> bool {
        self.oid.as_deref() == Some(oid) && !self.checking
    }
}

pub(crate) fn config(ui: &Ui) -> Config {
    ui.identity.known().map(|known| known.signing.clone()).unwrap_or_default()
}

pub(crate) fn selected(tab: &mut Tab, oid: Option<&str>, cx: &mut Context<Tab>) {
    let state = &tab.ui.signing;
    if oid.is_some() && state.oid.as_deref() == oid {
        return;
    }
    check(tab, oid.map(str::to_owned), cx);
}

fn check(tab: &mut Tab, oid: Option<String>, cx: &mut Context<Tab>) {
    let state = &mut tab.ui.signing;
    state.ticket += 1;
    state.oid = oid.clone();
    state.commit = None;
    state.tags.clear();
    state.error = None;
    state.checking = false;
    let (Some(oid), Some(repo)) = (oid, tab.repo.clone()) else { return };
    state.checking = true;
    let (ticket, epoch) = (state.ticket, tab.epoch);
    Tab::work(
        cx,
        move || repo.commit_signature(&oid).and_then(|commit| Ok((commit, repo.tag_signatures(&oid)?))),
        move |tab, result, cx| {
            let state = &mut tab.ui.signing;
            if tab.epoch != epoch || state.ticket != ticket {
                return;
            }
            state.checking = false;
            match result {
                Ok((commit, tags)) => {
                    state.commit = commit;
                    state.tags = tags;
                }
                Err(error) => state.error = Some(error),
            }
            cx.notify();
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Recheck => {
            let Some(Selection::Commit(oid)) = tab.ui.selection.clone() else { return };
            check(tab, Some(oid), cx);
        }
    }
    cx.notify();
}

fn pill<T: Desk>(id: impl Into<gpui::ElementId>, signature: &Signature, cx: &Context<T>) -> Stateful<Div> {
    let shade = signature.shade();
    let icon = match signature.grade {
        Grade::Verified => "check",
        Grade::Bad | Grade::Revoked => "alert",
        _ => "lock",
    };
    let row = div()
        .id(id)
        .flex()
        .min_w_0()
        .items_center()
        .gap(px(6.))
        .rounded(px(RADIUS_CHIP))
        .border_1()
        .border_color(kit::clear())
        .cursor_pointer()
        .focusable()
        .focus(|s| s.border_color(rgb(ACCENT)))
        .child(
            div()
                .h(px(18.))
                .px(px(6.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(4.))
                .rounded(px(RADIUS_CHIP))
                .bg(soft(shade, 0x26))
                .child(kit::icon(icon).size(px(10.)).text_color(rgb(shade)))
                .child(div().text_size(px(TEXT_TINY)).text_color(rgb(shade)).whitespace_nowrap().child(signature.word())),
        )
        .when(!signature.detail().is_empty(), |d| d.child(kit::fit(signature.detail()).size(TEXT_SMALL).color(TEXT_3).grow()))
        .tooltip(kit::tip(signature.tip()));
    kit::pressable(row, view::Action::Signing(Action::Recheck), cx)
}

pub(crate) fn lines<T: Desk>(ui: &Ui, oid: &str, cx: &Context<T>) -> Option<Div> {
    let state = &ui.signing;
    if !state.checked(oid) || state.commit.is_none() && state.tags.is_empty() {
        return None;
    }
    let mut column = div().flex().flex_col().gap(px(4.));
    if let Some(signature) = &state.commit {
        column = column.child(pill("commit-signature", signature, cx));
    }
    for (index, (name, signature)) in state.tags.iter().enumerate() {
        column = column.child(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap(px(6.))
                .child(kit::icon("tag").size(px(11.)).text_color(rgb(TAG)))
                .child(div().flex_shrink_0().text_size(px(TEXT_SMALL)).text_color(rgb(TAG)).child(name.clone()))
                .child(pill(("tag-signature", index), signature, cx).flex_1()),
        );
    }
    Some(column)
}

pub(crate) fn commit_chip(ui: &Ui) -> Option<Stateful<Div>> {
    if !crate::commit_options::signs(ui) {
        return None;
    }
    let config = config(ui);
    let mut tip: SharedString = config.described().into();
    if ui.commit_options.sign_chosen == Some(true) && !config.commits {
        tip = format!("{tip} Only this commit; Sign this commit is cleared afterwards.").into();
    }
    Some(
        div()
            .id("commit-signed")
            .h(px(CONTROL))
            .px(px(8.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(4.))
            .rounded(px(RADIUS))
            .bg(soft(ADDED, 0x1c))
            .child(kit::icon("lock").size(px(11.)).text_color(rgb(ADDED)))
            .child(div().text_size(px(TEXT_SMALL)).text_color(rgb(ADDED)).child("Signed"))
            .tooltip(kit::tip(tip)),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    pub(crate) const KEYGEN: &str = "/usr/bin/ssh-keygen";

    pub(crate) fn keygen_present() -> bool {
        Path::new(KEYGEN).is_file()
    }

    pub(crate) fn ssh_key(dir: &Path, name: &str) -> PathBuf {
        let key = dir.join(name);
        let output = Command::new(KEYGEN)
            .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
            .arg(&key)
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        key.with_extension("pub")
    }

    fn public(key: &Path) -> String {
        fs::read_to_string(key).unwrap().trim().to_owned()
    }

    pub(crate) fn allow(file: &Path, principal: &str, key: &Path) {
        let mut text = fs::read_to_string(file).unwrap_or_default();
        text.push_str(&format!("{principal} {}\n", public(key)));
        fs::write(file, text).unwrap();
    }

    pub(crate) fn ssh_signing(f: &Fixture, key: &Path) -> PathBuf {
        let keys = f.repo.git_dir().join("signing");
        fs::create_dir_all(&keys).unwrap();
        let allowed = keys.join("allowed_signers");
        for (name, value) in [
            ("gpg.format", "ssh"),
            ("gpg.ssh.program", KEYGEN),
            ("user.signingKey", key.to_str().unwrap()),
            ("gpg.ssh.allowedSignersFile", allowed.to_str().unwrap()),
        ] {
            f.git(&["config", "--local", name, value]);
        }
        allowed
    }

    fn head(f: &Fixture) -> String {
        String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned()
    }

    fn refs(f: &Fixture) -> Vec<u8> {
        let mut seen = f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
        seen.extend(f.git(&["status", "--porcelain"]));
        seen
    }

    #[test]
    fn git_truth_values_follow_git_rules() {
        for (value, expected) in [
            (None, true),
            (Some("true"), true),
            (Some("Yes"), true),
            (Some("on"), true),
            (Some("1"), true),
            (Some("7"), true),
            (Some("false"), false),
            (Some("off"), false),
            (Some("0"), false),
            (Some(""), false),
            (Some("maybe"), false),
        ] {
            assert_eq!(truth(value), expected, "{value:?}");
        }
    }

    #[test]
    fn signing_settings_are_read_from_the_config_listing_last_one_winning() {
        let mut config = Config::default();
        config.read("commit.gpgsign", Some("true"));
        config.read("tag.gpgsign", None);
        config.read("gpg.format", Some("SSH"));
        config.read("user.signingkey", Some("~/.ssh/id_trail.pub"));
        assert!(config.commits && config.tags && config.format == Format::Ssh);
        assert_eq!(config.described(), "Signed with your SSH key ~/.ssh/id_trail.pub.");
        config.read("commit.gpgsign", Some("false"));
        config.read("user.signingkey", Some("key::ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabcdefgh cedar@trail"));
        assert!(!config.commits);
        assert_eq!(config.key_shown().as_deref(), Some("ssh-ed25519 …abcdefgh cedar@trail"));
        config.read("user.signingkey", Some(""));
        assert_eq!(config.described(), "Git has no SSH key to sign with: user.signingKey is not set.");
        config.read("gpg.ssh.defaultkeycommand", Some("ssh-add -L"));
        assert_eq!(config.described(), "Signed with the SSH key gpg.ssh.defaultKeyCommand gives.");
        config.read("gpg.format", Some("openpgp"));
        assert_eq!(config.described(), "Signed with the GPG key for your commit email.");
    }

    #[test]
    fn each_verification_letter_gives_its_badge() {
        let fields = "G\0cedar@example.invalid\0SHA256:abc\0SHA256:abc\n";
        let verified = graded(Format::Ssh, fields, "");
        assert_eq!((verified.grade, verified.word(), verified.detail().as_str()), (Grade::Verified, "Verified", "cedar@example.invalid"));
        assert_eq!(verified.sentence(), "Verified, signed by cedar@example.invalid");
        assert!(!verified.tip().contains("Key: "), "{}", verified.tip());
        assert!(verified.tip().contains("SSH signature\nSigner: cedar@example.invalid\nFingerprint: SHA256:abc"), "{}", verified.tip());
        let untrusted = graded(Format::Gpg, "U\0Fern <fern@example.invalid>\0ABCD1234ABCD1234\0F00D\n", "");
        assert_eq!(untrusted.sentence(), "Signed; the key's trust is unknown");
        assert!(untrusted.tip().contains("Key: ABCD1234ABCD1234\nFingerprint: F00D"), "{}", untrusted.tip());
        let stranger = graded(Format::Ssh, "U\0\0SHA256:abc\0SHA256:abc\n", "");
        assert_eq!((stranger.grade, stranger.sentence().as_str()), (Grade::Unknown, "Signed; cannot be checked here"));
        assert_eq!(stranger.reason.as_deref(), Some("The key is not one of the allowed signers in gpg.ssh.allowedSignersFile."));
        assert_eq!(graded(Format::Gpg, "B\0\0\0\n", "").sentence(), "Bad signature: it does not match this content");
        assert_eq!(graded(Format::Gpg, "X\0\0\0\n", "").sentence(), "Signed; the signature has expired");
        assert_eq!(graded(Format::Gpg, "Y\0\0\0\n", "").sentence(), "Signed with an expired key");
        assert_eq!(graded(Format::Gpg, "R\0\0\0\n", "").sentence(), "Signed with a revoked key");
        let missing = graded(Format::Gpg, "E\0\0ABCD\0\n", "");
        assert_eq!((missing.grade, missing.reason.as_deref()), (Grade::Unknown, Some("Its key is not in this Mac's GPG keyring.")));
        let no_file = graded(Format::Ssh, "N\0\0\0\n", "error: gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification\n");
        assert_eq!(no_file.grade, Grade::Unknown);
        assert!(no_file.reason.unwrap().starts_with("Git has no allowed signers file"));
        let no_gpg = graded(Format::Gpg, "N\0\0\0\n", "error: cannot run gpg: No such file or directory\n");
        assert_eq!(no_gpg.reason.as_deref(), Some("Git could not run gpg to check it."));
    }

    #[test]
    fn raw_tag_verification_output_gives_the_same_badges() {
        let good = "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG 1234ABCD1234ABCD Cedar <cedar@example.invalid>\n[GNUPG:] VALIDSIG F00DF00D 2026-11-26\n[GNUPG:] TRUST_ULTIMATE 0 pgp\n";
        let verified = raw_graded(Format::Gpg, good);
        assert_eq!(
            (verified.grade, verified.signer.as_str(), verified.key.as_str(), verified.fingerprint.as_str()),
            (Grade::Verified, "Cedar <cedar@example.invalid>", "1234ABCD1234ABCD", "F00DF00D")
        );
        let untrusted = raw_graded(Format::Gpg, &good.replace("TRUST_ULTIMATE", "TRUST_UNDEFINED"));
        assert_eq!(untrusted.grade, Grade::Untrusted);
        let unknown = raw_graded(Format::Gpg, "[GNUPG:] ERRSIG 1234ABCD1234ABCD 22 10 00 1764115200 9 -\n[GNUPG:] NO_PUBKEY 1234ABCD1234ABCD\n");
        assert_eq!((unknown.grade, unknown.key.as_str()), (Grade::Unknown, "1234ABCD1234ABCD"));
        assert_eq!(raw_graded(Format::Gpg, "[GNUPG:] REVKEYSIG 12 Cedar\n").grade, Grade::Revoked);
        assert_eq!(raw_graded(Format::Gpg, "[GNUPG:] BADSIG 12 Cedar\n").grade, Grade::Bad);
        let ssh = raw_graded(Format::Ssh, "Good \"git\" signature for cedar@example.invalid with ED25519 key SHA256:abc\n");
        assert_eq!((ssh.grade, ssh.signer.as_str(), ssh.fingerprint.as_str()), (Grade::Verified, "cedar@example.invalid", "SHA256:abc"));
        let stranger = raw_graded(Format::Ssh, "Good \"git\" signature with ED25519 key SHA256:abc\nNo principal matched.\n");
        assert_eq!((stranger.grade, stranger.fingerprint.as_str()), (Grade::Unknown, "SHA256:abc"));
        let no_file = raw_graded(Format::Ssh, "error: gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification\n");
        assert_eq!(no_file.grade, Grade::Unknown);
        assert_eq!(raw_graded(Format::Ssh, "Signature verification failed: incorrect signature\nCould not verify signature.\n").grade, Grade::Bad);
    }

    #[test]
    fn a_signature_header_is_found_only_in_the_commit_headers() {
        let signed = b"tree abc\nparent def\nauthor A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\ngpgsig -----BEGIN SSH SIGNATURE-----\n U1NIU0lH\n -----END SSH SIGNATURE-----\n\nMessage\n";
        let block = header_signature(signed).unwrap();
        assert_eq!(block, "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----");
        assert_eq!(Format::of_signature(&block), Format::Ssh);
        let plain = b"tree abc\nauthor A <a@b> 1 +0000\n\ngpgsig -----BEGIN PGP SIGNATURE-----\n";
        assert_eq!(header_signature(plain), None);
        assert_eq!(Format::of_signature("-----BEGIN PGP SIGNATURE-----"), Format::Gpg);
    }

    #[test]
    fn an_ssh_signed_commit_is_verified_for_an_allowed_signer_and_unknown_otherwise() {
        if !keygen_present() {
            return;
        }
        let f = Fixture::new();
        let keys = f.repo.git_dir().join("keys");
        fs::create_dir_all(&keys).unwrap();
        let cedar = ssh_key(&keys, "cedar");
        let allowed = ssh_signing(&f, &cedar);
        allow(&allowed, "cedar@example.invalid", &cedar);
        fs::write(f.dir.join("trail.txt"), "Trail\n").unwrap();
        f.git(&["add", "trail.txt"]);
        f.repo.commit_with("Mark the trail", crate::repository::Choices { sign: Some(true), ..Default::default() }).unwrap();
        let signed = head(&f);
        let before = refs(&f);
        let verified = f.repo.commit_signature(&signed).unwrap().unwrap();
        assert_eq!((verified.grade, verified.format, verified.signer.as_str()), (Grade::Verified, Format::Ssh, "cedar@example.invalid"));
        assert!(verified.fingerprint.starts_with("SHA256:"), "{verified:?}");
        assert_eq!(refs(&f), before, "checking a signature changes nothing");

        let stranger = ssh_key(&keys, "stranger");
        f.git(&["config", "--local", "user.signingKey", stranger.to_str().unwrap()]);
        f.git(&["commit", "--allow-empty", "-S", "-qm", "A stranger passes"]);
        let unknown = f.repo.commit_signature(&head(&f)).unwrap().unwrap();
        assert_eq!((unknown.grade, unknown.sentence().as_str()), (Grade::Unknown, "Signed; cannot be checked here"));

        f.git(&["config", "--local", "--unset", "gpg.ssh.allowedSignersFile"]);
        let unconfigured = f.repo.commit_signature(&signed).unwrap().unwrap();
        assert_eq!(unconfigured.grade, Grade::Unknown);
        assert!(unconfigured.reason.unwrap().contains("gpg.ssh.allowedSignersFile"));

        f.git(&["commit", "--allow-empty", "--no-gpg-sign", "-qm", "Unsigned"]);
        assert_eq!(f.repo.commit_signature(&head(&f)).unwrap(), None);
        assert!(f.repo.commit_signature("not-a-commit").is_err());
    }

    #[test]
    fn signed_tags_at_a_commit_are_checked_and_unsigned_ones_skipped() {
        if !keygen_present() {
            return;
        }
        let f = Fixture::new();
        let keys = f.repo.git_dir().join("keys");
        fs::create_dir_all(&keys).unwrap();
        let cedar = ssh_key(&keys, "cedar");
        let allowed = ssh_signing(&f, &cedar);
        allow(&allowed, "cedar@example.invalid", &cedar);
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        let base = head(&f);
        f.git(&["tag", "-s", "-m", "Signed release", "v1"]);
        f.git(&["tag", "--no-sign", "-a", "-m", "Plain release", "v1-plain"]);
        f.git(&["tag", "light"]);
        let stranger = ssh_key(&keys, "stranger");
        f.git(&["config", "--local", "user.signingKey", stranger.to_str().unwrap()]);
        f.git(&["tag", "-s", "-m", "Stranger's release", "v1-stranger"]);
        f.git(&["commit", "--allow-empty", "-qm", "Later"]);
        f.git(&["tag", "-s", "-m", "Later release", "v2"]);
        let before = refs(&f);
        let found = f.repo.tag_signatures(&base).unwrap();
        let grades: Vec<(&str, Grade)> = found.iter().map(|(name, signature)| (name.as_str(), signature.grade)).collect();
        assert_eq!(grades, vec![("v1", Grade::Verified), ("v1-stranger", Grade::Unknown)]);
        assert_eq!(found[0].1.signer, "cedar@example.invalid");
        assert_eq!(refs(&f), before);
        assert!(f.repo.tag_signatures("../main").is_err());
    }

    fn signed(f: &Fixture, revision: &str) -> bool {
        header_signature(&f.git(&["cat-file", "commit", revision])).is_some()
    }

    #[test]
    fn the_sign_choice_signs_or_leaves_unsigned_only_the_commit_it_is_given_to() {
        use crate::repository::{Choices, Hooks};
        if !keygen_present() {
            return;
        }
        let f = Fixture::new();
        let keys = f.repo.git_dir().join("keys");
        fs::create_dir_all(&keys).unwrap();
        let cedar = ssh_key(&keys, "cedar");
        ssh_signing(&f, &cedar);
        fs::write(f.dir.join("a.txt"), "one\n").unwrap();
        f.git(&["add", "a.txt"]);
        f.repo.commit_with("Base", Hooks::Run).unwrap();
        assert!(!signed(&f, "HEAD"), "commit.gpgSign is off, so nothing is signed unasked");
        fs::write(f.dir.join("a.txt"), "two\n").unwrap();
        f.git(&["add", "a.txt"]);
        let sign = Choices { hooks: Hooks::Run, sign: Some(true) };
        let committed = f.repo.commit_with("Signed on request", sign).unwrap();
        assert_eq!(committed.head.as_deref(), Some(head(&f).as_str()));
        assert!(signed(&f, "HEAD"));
        assert_eq!(String::from_utf8(f.git(&["show", "HEAD:a.txt"])).unwrap(), "two\n");
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), "");

        f.git(&["config", "--local", "commit.gpgSign", "true"]);
        let unsigned = Choices { hooks: Hooks::Run, sign: Some(false) };
        let tip = head(&f);
        let reference = std::ffi::OsString::from("refs/heads/main");
        f.repo.amend_with(&tip, Some(&reference), Some("Amended unsigned"), unsigned).unwrap();
        assert!(!signed(&f, "HEAD"));
        assert_eq!(String::from_utf8(f.git(&["rev-parse", "HEAD^"])).unwrap(), String::from_utf8(f.git(&["rev-parse", &format!("{tip}^")])).unwrap());
        fs::write(f.dir.join("b.txt"), "new\n").unwrap();
        let amended = head(&f);
        f.repo.commit_all_with(Some(&amended), Some(&reference), "Configured signing", Hooks::Run).unwrap();
        assert!(signed(&f, "HEAD"), "with no choice the configuration decides");
        assert_eq!(String::from_utf8(f.git(&["show", "HEAD:b.txt"])).unwrap(), "new\n");

        let moved = head(&f);
        fs::write(f.dir.join("c.txt"), "late\n").unwrap();
        let stale = f.repo.commit_all_with(Some(&amended), Some(&reference), "Stale", sign).unwrap_err();
        assert!(stale.text.contains("changed"), "{}", stale.text);
        assert_eq!(head(&f), moved);
        assert_eq!(String::from_utf8(f.git(&["status", "--porcelain"])).unwrap(), "?? c.txt\n");
        let gone = f.repo.amend_with(&amended, Some(&reference), Some("Again"), sign).unwrap_err();
        assert!(gone.text.contains("changed"), "{}", gone.text);
        assert_eq!(head(&f), moved);
    }

    #[test]
    fn a_locked_signing_key_is_unlocked_through_the_app_only_in_a_job_that_can_ask() {
        use crate::repository::{Choices, Hooks};
        use std::os::unix::fs::PermissionsExt;
        if !keygen_present() {
            return;
        }
        let f = Fixture::new();
        let keys = f.repo.git_dir().join("keys");
        fs::create_dir_all(&keys).unwrap();
        let script = |name: &str, body: &str| {
            let path = keys.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let maker = script("make-pass.sh", "echo 'ridge lantern'\n");
        let key = keys.join("locked");
        let made = Command::new(KEYGEN)
            .args(["-q", "-t", "ed25519", "-C", "locked", "-f"])
            .arg(&key)
            .env_remove("SSH_AUTH_SOCK")
            .env("SSH_ASKPASS", &maker)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let allowed = ssh_signing(&f, &key.with_extension("pub"));
        allow(&allowed, "cedar@example.invalid", &key.with_extension("pub"));
        let asked = keys.join("asked.log");
        let program = script(
            "askpass.sh",
            &format!(
                "echo \"$1\" >> '{}'\n[ -n \"$FASTKRAKEN_ASKPASS_TOKEN\" ] && [ \"$SSH_ASKPASS_REQUIRE\" = force ] || exit 1\necho 'ridge lantern'\n",
                asked.display()
            ),
        );
        let _program = crate::askpass::tests::Program::set(program);
        let sign = Choices { hooks: Hooks::Run, sign: Some(true) };
        f.git(&["commit", "--allow-empty", "--no-gpg-sign", "-qm", "Base"]);
        let base = head(&f);
        fs::write(f.dir.join("lantern.txt"), "Lit\n").unwrap();
        f.git(&["add", "lantern.txt"]);

        let named = |command: &Command, key: &str| command.get_envs().any(|(name, value)| name == key && value.is_some_and(|value| !value.is_empty()));
        let silent = crate::jobs::Link::new(false).enter(|| f.repo.command());
        assert!(!named(&silent, "SSH_ASKPASS") && !named(&silent, crate::askpass::TOKEN), "a job that cannot ask hands out no token");
        assert!(!asked.exists());
        let answering = crate::jobs::Link::answering();
        assert!(answering.session().is_some() && !answering.cancellable());
        let asking = answering.enter(|| f.repo.command());
        assert!(named(&asking, "SSH_ASKPASS") && named(&asking, crate::askpass::TOKEN) && named(&asking, crate::askpass::SOCKET));
        answering.enter(|| f.repo.commit_with("Unlocked in the app", sign)).unwrap();
        assert_ne!(head(&f), base);
        assert!(signed(&f, "HEAD"));
        let asked = fs::read_to_string(&asked).unwrap();
        assert!(asked.starts_with("Enter passphrase for "), "{asked}");
        assert_eq!(
            crate::askpass::Question::parse(asked.trim_end()),
            crate::askpass::Question::Passphrase { key: key.display().to_string() }
        );
        let verified = f.repo.commit_signature(&head(&f)).unwrap().unwrap();
        assert_eq!(verified.grade, Grade::Verified);
    }

    #[test]
    fn a_signed_tag_needs_a_message_and_carries_a_signature() {
        if !keygen_present() {
            return;
        }
        let f = Fixture::new();
        let keys = f.repo.git_dir().join("keys");
        fs::create_dir_all(&keys).unwrap();
        let cedar = ssh_key(&keys, "cedar");
        ssh_signing(&f, &cedar);
        f.git(&["commit", "--allow-empty", "-qm", "Base"]);
        let base = head(&f);
        f.git(&["config", "--local", "tag.gpgSign", "true"]);
        let listed = |f: &Fixture| f.git(&["for-each-ref", "--format=%(refname) %(objecttype) %(*objectname)", "refs/tags"]);
        let tagged = crate::refs::tests::create_signed(&f.repo, "v1", &base, "", true).unwrap_err();
        assert_eq!(tagged, "A signed tag needs a message.");
        assert!(listed(&f).is_empty());
        crate::refs::tests::create_signed(&f.repo, "v1", &base, "Release one\n", true).unwrap();
        assert_eq!(String::from_utf8(listed(&f)).unwrap(), format!("refs/tags/v1 tag {base}\n"));
        let object = f.git(&["cat-file", "tag", "refs/tags/v1"]);
        assert!(String::from_utf8_lossy(&object).contains("-----BEGIN SSH SIGNATURE-----"));
        crate::refs::tests::create_signed(&f.repo, "v1-plain", &base, "Plain release", false).unwrap();
        let plain = f.git(&["cat-file", "tag", "refs/tags/v1-plain"]);
        assert!(!String::from_utf8_lossy(&plain).contains("SIGNATURE"), "tag.gpgSign is overridden when unticked");
        crate::refs::tests::create_signed(&f.repo, "light", &base, "", false).unwrap();
        assert_eq!(String::from_utf8(f.git(&["cat-file", "-t", "refs/tags/light"])).unwrap(), "commit\n");
        assert_eq!(head(&f), base);
        assert!(crate::refs::tests::create_signed(&f.repo, "v1", &base, "Again", true).is_err());
    }

    #[test]
    fn the_config_listing_gives_the_signing_settings_alongside_the_identity() {
        let listed = b"global\0commit.gpgsign\0global\0user.name\nCedar\0local\0gpg.format\nssh\0local\0user.signingkey\n/k/id.pub\0local\0tag.gpgsign\nfalse\0";
        let identity = crate::identity::parse(listed);
        assert_eq!(identity.effective.name.as_deref(), Some("Cedar"));
        assert_eq!(
            identity.signing,
            Config { commits: true, tags: false, key: Some("/k/id.pub".into()), format: Format::Ssh, key_command: false }
        );
    }

    pub(crate) fn gpg() -> Option<PathBuf> {
        ["/opt/homebrew/bin/gpg", "/usr/local/bin/gpg"].into_iter().map(PathBuf::from).find(|path| path.is_file())
    }

    pub(crate) struct Keyring {
        pub(crate) home: PathBuf,
        gpg: PathBuf,
    }

    impl Keyring {
        pub(crate) fn new(gpg: &Path, home: PathBuf) -> Self {
            use std::os::unix::fs::PermissionsExt;
            fs::create_dir_all(&home).unwrap();
            fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
            Self { home, gpg: gpg.to_owned() }
        }

        pub(crate) fn run(&self, args: &[&str], input: &str) -> std::process::Output {
            use std::io::Write;
            let mut child = Command::new(&self.gpg)
                .args(["--homedir"])
                .arg(&self.home)
                .args(["--batch", "--no-tty"])
                .args(args)
                .env("GNUPGHOME", &self.home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            child.wait_with_output().unwrap()
        }

        pub(crate) fn program(&self, at: &Path) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            fs::write(at, format!("#!/bin/sh\nGNUPGHOME='{}' exec '{}' \"$@\"\n", self.home.display(), self.gpg.display())).unwrap();
            fs::set_permissions(at, fs::Permissions::from_mode(0o755)).unwrap();
            at.to_owned()
        }
    }

    impl Drop for Keyring {
        fn drop(&mut self) {
            let gpgconf = self.gpg.with_file_name("gpgconf");
            let _ = Command::new(gpgconf).args(["--kill", "all"]).env("GNUPGHOME", &self.home).output();
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn a_gpg_signed_commit_is_verified_with_its_key_and_cannot_be_checked_without_it() {
        let Some(gpg) = gpg() else { return };
        let f = Fixture::new();
        let serial = f.dir.file_name().unwrap().to_string_lossy().rsplit('-').next().unwrap().to_owned();
        let keyring = Keyring::new(&gpg, PathBuf::from(format!("/tmp/fk-gpg-{}-{serial}", std::process::id())));
        let made = keyring.run(
            &["--gen-key"],
            "%no-protection\nKey-Type: eddsa\nKey-Curve: ed25519\nName-Real: Cedar\nName-Email: cedar@example.invalid\nExpire-Date: 0\n%commit\n",
        );
        if !made.status.success() {
            return;
        }
        let program = keyring.program(&f.repo.git_dir().join("gpg.sh"));
        f.git(&["config", "--local", "gpg.program", program.to_str().unwrap()]);
        f.git(&["config", "--local", "commit.gpgSign", "true"]);
        f.git(&["commit", "--allow-empty", "-qm", "Signed with GPG"]);
        let signed = head(&f);
        let verified = f.repo.commit_signature(&signed).unwrap().unwrap();
        assert_eq!((verified.grade, verified.format, verified.signer.as_str()), (Grade::Verified, Format::Gpg, "Cedar <cedar@example.invalid>"));
        assert!(!verified.fingerprint.is_empty() && !verified.key.is_empty(), "{verified:?}");
        f.git(&["tag", "-s", "-m", "GPG release", "v1"]);
        let tags = f.repo.tag_signatures(&signed).unwrap();
        assert_eq!(tags.iter().map(|(name, signature)| (name.as_str(), signature.grade)).collect::<Vec<_>>(), vec![("v1", Grade::Verified)]);

        let empty = Keyring::new(&gpg, PathBuf::from(format!("/tmp/fk-gpg-{}-{serial}-empty", std::process::id())));
        let elsewhere = empty.program(&f.repo.git_dir().join("gpg-empty.sh"));
        f.git(&["config", "--local", "gpg.program", elsewhere.to_str().unwrap()]);
        let unknown = f.repo.commit_signature(&signed).unwrap().unwrap();
        assert_eq!((unknown.grade, unknown.sentence().as_str()), (Grade::Unknown, "Signed; cannot be checked here"));
        assert_eq!(f.repo.tag_signatures(&signed).unwrap()[0].1.grade, Grade::Unknown);
    }
}
