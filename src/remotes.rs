use crate::repository::Run;
use crate::repository::{Repository, checked, validate_oid};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Remote {
    pub name: String,
    pub push_urls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    Tracking(Said),
    PublishedWithoutTracking(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Upstream {
    pub remote: OsString,
    pub remote_ref: OsString,
    pub tracking_ref: OsString,
}

pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.starts_with('-')
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
        || name
            .split('/')
            .any(|c| c.is_empty() || c.starts_with('.') || c.ends_with(".lock"))
    {
        return Err("Enter a valid Git remote name, such as origin.".into());
    }
    Ok(())
}

pub(crate) fn validate_url(value: &str) -> Result<(), String> {
    const INVALID: &str = "Enter an HTTPS, SSH, Git or local repository address.";
    if value.is_empty()
        || value.trim() != value
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err(INVALID.into());
    }
    if value.starts_with('/') || value.starts_with("./") || value.starts_with("../") {
        return Ok(());
    }
    if let Some((scheme, rest)) = value.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git" | "file") || value.contains('\\') {
            return Err(INVALID.into());
        }
        let url = Url::parse(value).map_err(|_| INVALID)?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let user = authority.rsplit_once('@').map(|(user, _)| user);
        if url.password().is_some()
            || user.is_some_and(|user| {
                user.is_empty() || user.contains(['@', ':']) || token_user(user) || !matches!(scheme, "ssh" | "https" | "http")
            })
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(
                "Use a repository URL without embedded credentials, a query or a fragment.".into(),
            );
        }
        if scheme == "file" {
            if !matches!(url.host_str(), None | Some("localhost"))
                || !rest.starts_with('/') && !rest.starts_with("localhost/")
            {
                return Err(INVALID.into());
            }
        } else if authority.is_empty()
            || url.host_str().is_none()
            || url.path().trim_matches('/').is_empty()
        {
            return Err(INVALID.into());
        }
        return Ok(());
    }
    let scp = if let Some((host, path)) = value.split_once("]:") {
        Some((format!("{host}]"), path))
    } else {
        value
            .split_once(':')
            .filter(|(host, _)| !host.contains('/'))
            .map(|(host, path)| (host.to_owned(), path))
    };
    if let Some((host, path)) = scp {
        if host.is_empty()
            || path.is_empty()
            || path.starts_with(':')
            || host.starts_with('-')
            || host.contains(['/', '\\', '?', '#'])
            || host.chars().any(char::is_whitespace)
        {
            return Err(INVALID.into());
        }
        let url = Url::parse(&format!("ssh://{host}/")).map_err(|_| INVALID)?;
        if url.host_str().is_none() || url.password().is_some() {
            return Err(INVALID.into());
        }
    }
    Ok(())
}

fn urls(repo: &Repository, name: &str, push: bool) -> Result<Vec<String>, String> {
    let mut cmd = repo.read_command();
    cmd.args(["remote", "get-url", "--all"]);
    if push {
        cmd.arg("--push");
    }
    let bytes =
        checked(cmd.args(["--", name])).map_err(|_| "Cannot read this remote's destination.")?;
    let output =
        std::str::from_utf8(&bytes).map_err(|_| "This remote's address cannot be displayed.")?;
    output
        .strip_suffix('\n')
        .unwrap_or(output)
        .split('\n')
        .map(|line| validate_url(line).map(|_| line.to_owned()))
        .collect()
}

pub(crate) fn list(repo: &Repository) -> Result<Vec<Remote>, String> {
    let bytes = checked(repo.read_command().arg("remote"))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|name| validate_name(name).is_ok())
        .map(|name| Remote {
            name: name.to_owned(),
            push_urls: urls(repo, name, true).unwrap_or_default(),
        })
        .collect())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Addresses {
    pub fetch: Vec<String>,
    pub push: Vec<String>,
}

impl Addresses {
    pub(crate) fn single(&self) -> Option<&str> {
        match self.fetch.as_slice() {
            [url] => Some(url),
            _ => None,
        }
    }

    pub(crate) fn separate_push(&self) -> Result<Option<&str>, ()> {
        match self.push.as_slice() {
            [] => Ok(None),
            [push] if Some(push.as_str()) == self.single() => Ok(None),
            [push] => Ok(Some(push)),
            _ => Err(()),
        }
    }
}

pub(crate) fn addresses(repo: &Repository) -> Result<std::collections::BTreeMap<String, Addresses>, String> {
    let output = repo
        .read_command()
        .args(["config", "--null", "--get-regexp", r"^remote\..*\.(url|pushurl)$"])
        .run()
        .map_err(|e| e.to_string())?;
    let mut found: std::collections::BTreeMap<String, Addresses> = std::collections::BTreeMap::new();
    if !output.status.success() {
        return match output.status.code() {
            Some(1) => Ok(found),
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
        };
    }
    for entry in output.stdout.split(|&b| b == 0) {
        let entry = String::from_utf8_lossy(entry);
        let Some((key, value)) = entry.split_once('\n') else { continue };
        let Some((name, field)) = key.strip_prefix("remote.").and_then(|key| key.rsplit_once('.')) else { continue };
        let addresses = found.entry(name.to_owned()).or_default();
        match field {
            "url" => addresses.fetch.push(value.to_owned()),
            _ => addresses.push.push(value.to_owned()),
        }
    }
    Ok(found)
}

fn token_user(user: &str) -> bool {
    ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-"].iter().any(|prefix| user.starts_with(prefix))
}

pub(crate) fn shown(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) if (parsed.password().is_some() || token_user(parsed.username())) && url.contains("://") => {
            let _ = parsed.set_password(None);
            if token_user(parsed.username()) {
                let _ = parsed.set_username("");
            }
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}

pub(crate) fn set_address(repo: &Repository, name: &str, seen: &Addresses, fetch: &str, push: Option<&str>) -> Result<bool, String> {
    validate_name(name)?;
    validate_url(fetch)?;
    let push = push.filter(|push| !push.is_empty() && *push != fetch);
    if let Some(push) = push {
        validate_url(push)?;
    }
    if seen.single().is_none() || seen.separate_push().is_err() {
        return Err(format!("{name} has more than one address. Change it with git remote set-url."));
    }
    check_transport(repo, name)?;
    let now = addresses(repo)?.remove(name);
    if now.as_ref() != Some(seen) {
        return Err(match now {
            None => format!("{name} is not a remote of this repository any more."),
            Some(_) => format!("{name}'s address changed since you opened it. Look at it again."),
        });
    }
    let unchanged = seen.single() == Some(fetch) && seen.separate_push() == Ok(push);
    if unchanged {
        return Ok(false);
    }
    if seen.single() != Some(fetch) {
        checked(repo.command().args(["remote", "set-url", "--", name, fetch]))?;
    }
    match push {
        Some(push) => drop(checked(repo.command().args(["remote", "set-url", "--push", "--", name, push]))?),
        None if !seen.push.is_empty() => {
            drop(checked(repo.command().args(["config", "--unset-all", "--", &format!("remote.{name}.pushurl")]))?)
        }
        None => {}
    }
    crate::auth::remote_edited(repo, name, None)
        .map_err(|e| format!("The address changed, but the GitHub sign-in for the old one could not be forgotten: {e}"))?;
    Ok(true)
}

pub(crate) fn check_transport(repo: &Repository, name: &str) -> Result<(), String> {
    let helper = repo
        .read_command()
        .args(["config", "--get", &format!("remote.{name}.vcs")])
        .run()
        .map_err(|e| e.to_string())?;
    match helper.status.code() {
        Some(1) => Ok(()),
        Some(0) => {
            Err("This remote uses a custom transport helper. Choose a standard Git remote.".into())
        }
        _ => Err("Cannot read the Git remote configuration.".into()),
    }
}

fn require_url(repo: &Repository, name: &str, expected: &str, push: bool) -> Result<(), String> {
    let endpoints = urls(repo, name, push)?;
    if endpoints.len() != 1 || endpoints[0] != expected {
        return Err(format!(
            "Remote {name} does not have the single selected {} destination. Review its URLs or choose a different remote name.",
            if push { "push" } else { "fetch" }
        ));
    }
    Ok(())
}

pub(crate) fn check_available(repo: &Repository, name: &str, url: &str) -> Result<(), String> {
    validate_name(name)?;
    validate_url(url)?;
    check_transport(repo, name)?;
    let existing = checked(repo.read_command().arg("remote"))?;
    if existing
        .split(|&b| b == b'\n')
        .any(|v| v == name.as_bytes())
    {
        require_url(repo, name, url, true)?;
        return require_url(repo, name, url, false);
    }
    let preview = checked(
        repo.read_command()
            .env("LC_ALL", "C")
            .arg("-c")
            .arg(format!("remote.{name}.url={url}"))
            .args(["remote", "-v"]),
    )
    .map_err(|_| "Cannot resolve the new remote’s Git destinations.")?;
    let prefix = format!("{name}\t");
    let endpoints: Vec<_> = preview
        .split(|&b| b == b'\n')
        .filter_map(|s| s.strip_prefix(prefix.as_bytes()))
        .collect();
    if endpoints
        != [
            format!("{url} (fetch)").as_bytes(),
            format!("{url} (push)").as_bytes(),
        ]
    {
        return Err("Git rewrites this address to another destination. Enter the effective repository URL or review Git’s URL configuration.".into());
    }
    Ok(())
}

pub(crate) fn add(repo: &Repository, name: &str, url: &str) -> Result<(), String> {
    check_available(repo, name, url)?;
    let result = repo
        .command()
        .args(["remote", "add", "--", name, url])
        .run()
        .map_err(|e| e.to_string())?;
    if !result.status.success() && result.status.code() != Some(3) {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_owned());
    }
    require_url(repo, name, url, true)?;
    require_url(repo, name, url, false)
}

pub(crate) fn add_with_push(repo: &Repository, name: &str, url: &str, push: Option<&str>) -> Result<(), String> {
    let push = push.filter(|push| !push.is_empty() && *push != url);
    if let Some(push) = push {
        validate_url(push)?;
    }
    add(repo, name, url)?;
    if let Some(push) = push {
        checked(repo.command().args(["remote", "set-url", "--push", "--", name, push]))?;
    }
    Ok(())
}

pub(crate) fn rename(repo: &Repository, old: &str, new: &str) -> Result<(), String> {
    validate_name(old)?;
    validate_name(new)?;
    if old == new {
        return Err("Enter a different remote name.".into());
    }
    checked(repo.command().args(["remote", "rename", "--", old, new]))?;
    Ok(())
}

pub(crate) fn remove(repo: &Repository, name: &str) -> Result<(), String> {
    validate_name(name)?;
    checked(repo.command().args(["remote", "remove", "--", name]))?;
    Ok(())
}

pub(crate) fn checked_branch(
    repo: &Repository,
    branch: &str,
    head: &str,
    current: bool,
) -> Result<Option<Upstream>, String> {
    validate_oid(head)?;
    let local_ref = format!("refs/heads/{branch}");
    let bytes = checked(repo.read_command().args([
        "for-each-ref", "--count=1",
        "--format=%(HEAD)%00%(refname)%00%(objectname)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
        "--", &local_ref,
    ]))?;
    let fields: Vec<_> = bytes
        .strip_suffix(b"\n")
        .unwrap_or(&bytes)
        .split(|&b| b == 0)
        .collect();
    let [checked_out, local, oid, remote, source, tracking] = fields.as_slice() else {
        return Err(if current {
            "The checked-out branch changed. Refresh and try again.".into()
        } else {
            format!("{branch} no longer exists. Refresh and try again.")
        });
    };
    if current && (*checked_out != b"*" || *local != local_ref.as_bytes() || *oid != head.as_bytes()) {
        return Err("The checked-out branch or its tip changed. Refresh and try again.".into());
    }
    if *local != local_ref.as_bytes() {
        return Err(format!("{branch} no longer exists. Refresh and try again."));
    }
    if *oid != head.as_bytes() {
        return Err(format!("{branch} changed since you chose it. Refresh and try again."));
    }
    if remote.is_empty() || !source.starts_with(b"refs/heads/") || !tracking.starts_with(b"refs/") {
        return Ok(None);
    }
    Ok(Some(Upstream {
        remote: OsString::from_vec(remote.to_vec()),
        remote_ref: OsString::from_vec(source.to_vec()),
        tracking_ref: OsString::from_vec(tracking.to_vec()),
    }))
}

#[derive(Clone, Debug)]
pub(crate) struct PushRequest {
    pub remote: OsString,
    pub refspecs: Vec<OsString>,
    pub lease: Option<Lease>,
    pub set_upstream: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Lease {
    pub remote_ref: OsString,
    pub expect: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PushReport {
    pub refs: Vec<PushedRef>,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PushedRef {
    pub flag: Flag,
    pub source: OsString,
    pub destination: OsString,
    pub summary: String,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flag {
    FastForward,
    Forced,
    Deleted,
    Created,
    Rejected,
    UpToDate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Rejection {
    NonFastForward,
    FetchFirst,
    Stale,
    AlreadyExists,
    Remote(String),
    Other(String),
}

impl PushedRef {
    pub(crate) fn rejection(&self) -> Option<Rejection> {
        if self.flag != Flag::Rejected {
            return None;
        }
        let reason = self.reason.clone().unwrap_or_default();
        Some(match (self.summary.as_str(), reason.as_str()) {
            ("[rejected]", "non-fast-forward") => Rejection::NonFastForward,
            ("[rejected]", "fetch first") => Rejection::FetchFirst,
            ("[rejected]", "stale info") => Rejection::Stale,
            ("[rejected]", "already exists") => Rejection::AlreadyExists,
            ("[remote rejected]", _) => Rejection::Remote(reason),
            _ => Rejection::Other(if reason.is_empty() { self.summary.clone() } else { reason }),
        })
    }
}

impl PushReport {
    fn parse(stdout: &[u8], stderr: &[u8]) -> Self {
        let refs = stdout
            .split(|&b| b == b'\n')
            .filter_map(|line| {
                let mut fields = line.splitn(3, |&b| b == b'\t');
                let flag = match fields.next()? {
                    b" " => Flag::FastForward,
                    b"+" => Flag::Forced,
                    b"-" => Flag::Deleted,
                    b"*" => Flag::Created,
                    b"!" => Flag::Rejected,
                    b"=" => Flag::UpToDate,
                    _ => return None,
                };
                let names = fields.next()?;
                let colon = names.iter().position(|&b| b == b':')?;
                let summary = String::from_utf8_lossy(fields.next()?).into_owned();
                let (summary, reason) = match summary.split_once(" (") {
                    Some((summary, reason)) => (
                        summary.to_owned(),
                        Some(reason.strip_suffix(')').unwrap_or(reason).to_owned()),
                    ),
                    None => (summary, None),
                };
                let source = match &names[..colon] {
                    b"(delete)" => &[][..],
                    source => source,
                };
                Some(PushedRef {
                    flag,
                    source: OsString::from_vec(source.to_vec()),
                    destination: OsString::from_vec(names[colon + 1..].to_vec()),
                    summary,
                    reason,
                })
            })
            .collect();
        Self {
            refs,
            output: String::from_utf8_lossy(stderr).trim().to_owned(),
        }
    }

    pub(crate) fn said(&self) -> Said {
        let lines: Vec<String> = self
            .output
            .split(['\n', '\r'])
            .filter(|line| !crate::jobs::is_progress(line))
            .filter_map(|line| line.trim_start().strip_prefix("remote:"))
            .map(|line| crate::activity::redact(line.trim()))
            .filter(|line| !line.is_empty())
            .collect();
        let link = lines.iter().find_map(|line| link_in(line));
        let output = if lines.is_empty() { String::new() } else { crate::activity::redact(&self.output) };
        Said { lines, link, output }
    }

    pub(crate) fn rejection(&self) -> Option<(&PushedRef, Rejection)> {
        self.refs
            .iter()
            .find_map(|pushed| Some((pushed, pushed.rejection()?)))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Said {
    pub lines: Vec<String>,
    pub link: Option<String>,
    pub output: String,
}

fn link_in(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url = line[start..]
        .split(|c: char| c.is_whitespace() || "<>\"'`".contains(c))
        .next()?
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '!']);
    crate::handoff::openable(url).then(|| url.to_owned())
}

#[derive(Debug)]
pub(crate) enum PushError {
    Refused(PushReport),
    Failed(String),
}

impl PushError {
    pub(crate) fn rejection(&self) -> Option<Rejection> {
        match self {
            Self::Refused(report) => report.rejection().map(|(_, why)| why),
            Self::Failed(_) => None,
        }
    }
}

impl From<String> for PushError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<PushError> for String {
    fn from(error: PushError) -> Self {
        match error {
            PushError::Failed(message) => message,
            PushError::Refused(report) => {
                let mut lines: Vec<String> = report
                    .refs
                    .iter()
                    .filter(|pushed| pushed.flag == Flag::Rejected)
                    .map(|pushed| {
                        let name = crate::guard::display(&pushed.destination);
                        match &pushed.reason {
                            Some(reason) => format!("{name}: {} ({reason})", pushed.summary),
                            None => format!("{name}: {}", pushed.summary),
                        }
                    })
                    .collect();
                if !report.output.is_empty() {
                    lines.push(report.output);
                }
                lines.join("\n")
            }
        }
    }
}

pub(crate) fn push(
    repo: &Repository,
    request: &PushRequest,
    recheck: impl FnOnce(&Repository) -> Result<(), String>,
) -> Result<PushReport, PushError> {
    if request.refspecs.is_empty() {
        return Err("Choose something to push.".to_owned().into());
    }
    if request.refspecs.iter().any(|refspec| {
        let bytes = refspec.as_encoded_bytes();
        bytes.is_empty() || matches!(bytes[0], b'+' | b'-') || !bytes.contains(&b':')
    }) {
        return Err("A push names each ref it sends and never forces one.".to_owned().into());
    }
    crate::auth::check_push_destination(repo, &request.remote)?;
    let mut mirror_override = OsString::from("remote.");
    mirror_override.push(&request.remote);
    mirror_override.push(".mirror=false");
    let mut command = repo.network_command()?;
    command.arg("-c").arg(mirror_override).args([
        "push",
        "--porcelain",
        "--progress",
        "--no-follow-tags",
        "--recurse-submodules=no",
    ]);
    match &request.lease {
        Some(lease) => {
            if !lease.expect.is_empty() {
                validate_oid(&lease.expect)?;
            }
            if !lease.remote_ref.as_encoded_bytes().starts_with(b"refs/") {
                return Err("Name the remote ref a lease protects in full.".to_owned().into());
            }
            let mut option = OsString::from("--force-with-lease=");
            option.push(&lease.remote_ref);
            option.push(":");
            option.push(&lease.expect);
            command.arg(option);
        }
        None => {
            command.arg("--no-force");
        }
    }
    if request.set_upstream {
        command.arg("--set-upstream");
    }
    recheck(repo)?;
    let (output, ran) = crate::repository::run_watching_hooks(command.arg("--").arg(&request.remote).args(&request.refspecs));
    let output = output.map_err(|e| format!("Run bundled Git: {e}"))?;
    let report = PushReport::parse(&output.stdout, &output.stderr);
    if output.status.success() {
        return Ok(report);
    }
    if report.refs.iter().any(|pushed| pushed.flag == Flag::Rejected) {
        return Err(PushError::Refused(report));
    }
    let hook_failed = ran.is_some_and(|ran| ran.iter().any(|ran| ran.hook == "pre-push" && ran.code.is_some_and(|code| code != 0)));
    let printed = if hook_failed { hook_printed(&output.stdout) } else { String::new() };
    let said = [printed.as_str(), report.output.as_str()].into_iter().filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n");
    Err(PushError::Failed(if said.is_empty() { format!("Git exited with {}", output.status) } else { said }))
}

fn hook_printed(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| {
            let porcelain = line.split_once('\t').is_some_and(|(flag, _)| matches!(flag, " " | "+" | "-" | "*" | "!" | "="));
            !porcelain && !line.starts_with("To ") && *line != "Done" && !line.trim().is_empty()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn push_branch(
    repo: &Repository,
    branch: &str,
    head: &str,
    remote: &OsStr,
    remote_ref: &OsStr,
    set_upstream: bool,
    current: bool,
) -> Result<PushReport, PushError> {
    let mut refspec = OsString::from(format!("refs/heads/{branch}:"));
    refspec.push(remote_ref);
    let request = PushRequest {
        remote: remote.to_owned(),
        refspecs: vec![refspec],
        lease: None,
        set_upstream,
    };
    push(repo, &request, |repo| {
        let upstream = checked_branch(repo, branch, head, current)?;
        if !set_upstream
            && !upstream.is_some_and(|u| u.remote == remote && u.remote_ref == remote_ref)
        {
            return Err("The configured upstream changed. Refresh and try again.".into());
        }
        Ok(())
    })
}

fn validate_target(
    repo: &Repository,
    branch: &str,
    head: &str,
    remote: &str,
    expected: &str,
    destination: &str,
) -> Result<String, String> {
    validate_name(remote)?;
    validate_url(expected)?;
    checked_branch(repo, branch, head, true)?;
    let dest_ref = format!("refs/heads/{destination}");
    if destination.is_empty() || destination.starts_with('-') || destination == "HEAD" {
        return Err("Enter a valid destination branch.".into());
    }
    checked(repo.read_command().args(["check-ref-format", &dest_ref]))
        .map_err(|_| "Enter a valid destination branch.")?;
    check_transport(repo, remote)?;
    require_url(repo, remote, expected, true)?;
    check_fetch_mapping(repo, remote, &dest_ref, false)?;
    Ok(dest_ref)
}

pub(crate) fn refspec_match<'a>(pattern: &[u8], source: &'a [u8]) -> Option<&'a [u8]> {
    match pattern.iter().position(|&b| b == b'*') {
        Some(i) => source
            .strip_prefix(&pattern[..i])?
            .strip_suffix(&pattern[i + 1..]),
        None => (source == pattern).then_some(&[]),
    }
}

pub(crate) fn mapped(repo: &Repository, name: &str, source: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let entries = repo
        .read_command()
        .args(["config", "--null", "--get-all", &format!("remote.{name}.fetch")])
        .run()
        .map_err(|e| format!("Run bundled Git: {e}"))?;
    if entries.status.code() == Some(1) {
        return Ok(Vec::new());
    }
    if !entries.status.success() {
        return Err(format!("Cannot read the fetch mapping of {name}."));
    }
    let mut local = Vec::new();
    for entry in entries.stdout.split(|&b| b == 0).filter(|entry| !entry.starts_with(b"^")) {
        let entry = entry.strip_prefix(b"+").unwrap_or(entry);
        let Some(colon) = entry.iter().position(|&b| b == b':') else { continue };
        let Some(matched) = refspec_match(&entry[..colon], source) else { continue };
        let configured = &entry[colon + 1..];
        let target = match configured.iter().position(|&b| b == b'*') {
            Some(i) => [&configured[..i], matched, &configured[i + 1..]].concat(),
            None => configured.to_vec(),
        };
        if !target.is_empty() && !local.contains(&target) {
            local.push(target);
        }
    }
    Ok(local)
}

pub(crate) fn check_fetch_mapping(
    repo: &Repository,
    name: &str,
    source: &str,
    require_included: bool,
) -> Result<(), String> {
    let entries = repo
        .read_command()
        .args([
            "config",
            "--null",
            "--get-all",
            &format!("remote.{name}.fetch"),
        ])
        .run()
        .map_err(|e| e.to_string())?;
    if entries.status.code() == Some(1) {
        return Ok(());
    }
    if !entries.status.success() {
        return Err("Cannot read the configured fetch mapping.".into());
    }
    for entry in entries.stdout.split(|&b| b == 0) {
        if let Some(excluded) = entry.strip_prefix(b"^") {
            if require_included && refspec_match(excluded, source.as_bytes()).is_some() {
                return Err("The remote’s fetch configuration explicitly excludes the selected branch. Review its fetch mapping.".into());
            }
            continue;
        }
        let entry = entry.strip_prefix(b"+").unwrap_or(entry);
        let Some(colon) = entry.iter().position(|&b| b == b':') else {
            continue;
        };
        let Some(matched) = refspec_match(&entry[..colon], source.as_bytes()) else {
            continue;
        };
        let configured = &entry[colon + 1..];
        if configured.is_empty() {
            continue;
        }
        let tracking = match configured.iter().position(|&b| b == b'*') {
            Some(i) => [&configured[..i], matched, &configured[i + 1..]].concat(),
            None => configured.to_vec(),
        };
        if !tracking.starts_with(b"refs/remotes/") {
            return Err("This remote maps the selected branch outside refs/remotes/. Review its fetch mapping before publishing.".into());
        }
        let symbolic = repo
            .read_command()
            .args(["symbolic-ref", "--quiet", "--no-recurse"])
            .arg(OsString::from_vec(tracking))
            .run()
            .map_err(|e| e.to_string())?;
        if symbolic.status.code() != Some(1) {
            return Err("The mapped tracking ref is symbolic or cannot be checked. Choose a direct remote-tracking ref.".into());
        }
    }
    Ok(())
}

fn verify_tracking(
    repo: &Repository,
    branch: &str,
    head: &str,
    remote: &str,
    dest_ref: &str,
) -> Result<(), String> {
    check_fetch_mapping(repo, remote, dest_ref, true)?;
    let Some(target) = checked_branch(repo, branch, head, true)? else {
        return Err("Git did not record the branch's upstream.".into());
    };
    if target.remote != OsStr::new(remote) || target.remote_ref != OsStr::new(dest_ref) {
        return Err("The branch's upstream does not match the selected destination.".into());
    }
    let tracking = checked(
        repo.read_command()
            .args(["rev-parse", "--verify", "--end-of-options"])
            .arg(target.tracking_ref),
    )
    .map_err(|_| "The configured fetch mapping has no tracking ref for the published branch.")?;
    if tracking.strip_suffix(b"\n") != Some(head.as_bytes()) {
        return Err("The local tracking ref does not yet match the published tip.".into());
    }
    Ok(())
}

pub(crate) fn first_push(
    repo: &Repository,
    branch: &str,
    head: &str,
    remote: &str,
    expected_url: &str,
    destination_branch: &str,
) -> Result<PushOutcome, String> {
    let dest_ref = validate_target(repo, branch, head, remote, expected_url, destination_branch)?;
    let report = push_branch(
        repo,
        branch,
        head,
        OsStr::new(remote),
        OsStr::new(&dest_ref),
        true,
        true,
    )?;
    Ok(
        match verify_tracking(repo, branch, head, remote, &dest_ref) {
            Ok(()) => PushOutcome::Tracking(report.said()),
            Err(reason) => PushOutcome::PublishedWithoutTracking(reason),
        },
    )
}

pub(crate) fn repair_tracking(
    repo: &Repository,
    branch: &str,
    head: &str,
    remote: &str,
    expected_url: &str,
    destination_branch: &str,
) -> Result<(), String> {
    let dest_ref = validate_target(repo, branch, head, remote, expected_url, destination_branch)?;
    check_fetch_mapping(repo, remote, &dest_ref, true)?;
    require_url(repo, remote, expected_url, false)?;
    let plan = checked(repo.network_command()?.args([
        "fetch",
        "--dry-run",
        "--verbose",
        "--porcelain",
        "--no-tags",
        "--no-prune",
        "--no-recurse-submodules",
        "--",
        remote,
        &dest_ref,
    ]))?;
    let mut tracking = None;
    let mut found = false;
    for line in std::str::from_utf8(&plan)
        .map_err(|_| "Unsupported tracking ref name.")?
        .lines()
    {
        let mut fields = line.rsplit(' ');
        let name = fields.next().unwrap_or_default();
        let oid = fields.next().unwrap_or_default();
        if oid != head {
            return Err("The remote tip changed. Refresh before setting up tracking.".into());
        }
        if name == "FETCH_HEAD" {
            found = true;
        } else if name.starts_with("refs/remotes/") && tracking.is_none() {
            tracking = Some(name.to_owned());
        } else {
            return Err("The fetch mapping is ambiguous or targets a local branch or tag. Review the remote's fetch configuration.".into());
        }
    }
    if !found {
        return Err("Git could not verify the tip of the published branch.".into());
    }
    checked_branch(repo, branch, head, true)?;
    let tracking = match tracking {
        Some(tracking) => tracking,
        None => {
            let tracking = format!("refs/remotes/{remote}/{destination_branch}");
            let existing = repo
                .read_command()
                .args(["show-ref", "--exists", &tracking])
                .run()
                .map_err(|e| e.to_string())?;
            if existing.status.code() != Some(2) {
                return Err("The proposed tracking ref already exists or cannot be checked. Review the remote's fetch configuration.".into());
            }
            checked(repo.command().args([
                "remote",
                "set-branches",
                "--add",
                "--",
                remote,
                destination_branch,
            ]))?;
            tracking
        }
    };
    let refspec = format!("{dest_ref}:{tracking}");
    require_url(repo, remote, expected_url, false)?;
    checked(repo.network_command()?.args([
        "fetch",
        "--no-tags",
        "--no-prune",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--refmap=",
        "--",
        remote,
        &refspec,
    ]))?;
    let actual = checked(repo.read_command().args([
        "rev-parse",
        "--verify",
        "--end-of-options",
        &tracking,
    ]))?;
    if actual.strip_suffix(b"\n") != Some(head.as_bytes()) {
        return Err("The remote tip changed during tracking setup. Refresh and try again.".into());
    }
    checked_branch(repo, branch, head, true)?;
    checked(
        repo.command()
            .arg("branch")
            .arg(format!("--set-upstream-to={tracking}"))
            .args(["--", branch]),
    )?;
    verify_tracking(repo, branch, head, remote, &dest_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn bare(f: &Fixture, path: &std::path::Path) -> String {
        let path = path.to_str().unwrap();
        f.git(&["init", "-q", "--bare", "-b", "release", path]);
        path.into()
    }

    fn commit(f: &Fixture) -> String {
        f.git(&["commit", "--allow-empty", "-qm", "Start project"]);
        String::from_utf8(f.git(&["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .into()
    }

    fn request(refspecs: &[&str], lease: Option<(&str, &str)>) -> PushRequest {
        PushRequest {
            remote: "origin".into(),
            refspecs: refspecs.iter().map(OsString::from).collect(),
            lease: lease.map(|(remote_ref, expect)| Lease {
                remote_ref: remote_ref.into(),
                expect: expect.into(),
            }),
            set_upstream: false,
        }
    }

    fn remote_refs(url: &str, f: &Fixture) -> String {
        String::from_utf8(f.git(&[
            "--git-dir",
            url,
            "for-each-ref",
            "--format=%(refname) %(objecttype) %(objectname)",
        ]))
        .unwrap()
    }

    fn rev(f: &Fixture, name: &str) -> String {
        String::from_utf8(f.git(&["rev-parse", name])).unwrap().trim().into()
    }

    #[test]
    fn porcelain_lines_give_each_ref_its_flag_and_reason() {
        let report = PushReport::parse(
            b"To ../bare.git\n\
              *\trefs/tags/v1:refs/tags/v1\t[new tag]\n\
              -\t:refs/heads/x\t[deleted]\n\
              +\trefs/heads/main:refs/heads/main\t8cb323b...50cc6d4 (forced update)\n\
              \x20\trefs/heads/a:refs/heads/a\t8cb323b..50cc6d4\n\
              =\trefs/heads/b:refs/heads/b\t[up to date]\n\
              !\trefs/heads/c:refs/heads/c\t[rejected] (non-fast-forward)\n\
              !\trefs/heads/d:refs/heads/d\t[rejected] (fetch first)\n\
              !\trefs/heads/e:refs/heads/e\t[rejected] (stale info)\n\
              !\trefs/tags/v2:refs/tags/v2\t[rejected] (already exists)\n\
              !\t:refs/heads/main\t[remote rejected] (deletion of the current branch prohibited)\n\
              Done\n",
            b"error: failed to push some refs\n",
        );
        let flags: Vec<_> = report.refs.iter().map(|r| r.flag).collect();
        assert_eq!(
            flags,
            [
                Flag::Created,
                Flag::Deleted,
                Flag::Forced,
                Flag::FastForward,
                Flag::UpToDate,
                Flag::Rejected,
                Flag::Rejected,
                Flag::Rejected,
                Flag::Rejected,
                Flag::Rejected,
            ]
        );
        assert_eq!(report.refs[1].source, OsString::new());
        assert_eq!(report.refs[1].destination, OsString::from("refs/heads/x"));
        assert_eq!(report.refs[2].summary, "8cb323b...50cc6d4");
        assert_eq!(report.refs[2].reason.as_deref(), Some("forced update"));
        assert_eq!(report.refs[2].rejection(), None);
        let why: Vec<_> = report.refs[5..].iter().map(|r| r.rejection().unwrap()).collect();
        assert_eq!(
            why,
            [
                Rejection::NonFastForward,
                Rejection::FetchFirst,
                Rejection::Stale,
                Rejection::AlreadyExists,
                Rejection::Remote("deletion of the current branch prohibited".into()),
            ]
        );
        assert_eq!(report.rejection().unwrap().1, Rejection::NonFastForward);
        assert_eq!(report.output, "error: failed to push some refs");
        let text = String::from(PushError::Refused(report));
        assert!(text.starts_with("c: [rejected] (non-fast-forward)\nd: [rejected] (fetch first)"), "{text}");
        assert!(text.ends_with("error: failed to push some refs"), "{text}");
    }

    #[test]
    fn a_push_keeps_what_the_server_said_and_its_link_with_credentials_masked() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("hosted.git"));
        let hook = dir.path().join("hosted.git/hooks/post-receive");
        fs::write(
            &hook,
            "#!/bin/sh\necho\necho \"Create a pull request for 'topic' by visiting:\"\necho \"     https://example.invalid/cedar/hosted/pull/new/topic.\"\necho \"Mirrored to https://scout:hunter2@mirror.invalid/hosted.git\"\n",
        )
        .unwrap();
        fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        add(&f.repo, "origin", &url).unwrap();
        let tip = commit(&f);
        let report = push(&f.repo, &request(&["refs/heads/main:refs/heads/topic"], None), |_| Ok(())).unwrap();
        assert_eq!(report.refs[0].flag, Flag::Created);
        let said = report.said();
        assert_eq!(
            said.lines,
            [
                "Create a pull request for 'topic' by visiting:",
                "https://example.invalid/cedar/hosted/pull/new/topic.",
                "Mirrored to https://•••@mirror.invalid/hosted.git",
            ]
        );
        assert_eq!(said.link.as_deref(), Some("https://example.invalid/cedar/hosted/pull/new/topic"));
        assert!(said.output.contains("remote: Create a pull request") && !said.output.contains("hunter2"), "{}", said.output);
        let logged = crate::activity::entries()
            .into_iter()
            .rev()
            .find(|entry| entry.argv.iter().any(|arg| arg == "refs/heads/main:refs/heads/topic") && entry.dir == f.repo.root)
            .unwrap();
        assert_eq!((logged.exit, logged.read_only), (Some(0), false));
        assert!(logged.stderr.contains("mirror.invalid") && !logged.stderr.contains("hunter2"), "{}", logged.stderr);
        assert_eq!(remote_refs(&url, &f), format!("refs/heads/topic commit {tip}\n"));
        fs::remove_file(&hook).unwrap();
        let quiet = push(&f.repo, &request(&["refs/heads/main:refs/heads/quiet"], None), |_| Ok(())).unwrap();
        assert_eq!(quiet.said(), Said::default());
    }

    #[test]
    fn push_sends_named_refs_creates_tags_and_deletes_branches() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("shared.git"));
        add(&f.repo, "origin", &url).unwrap();
        let tip = commit(&f);
        f.git(&["tag", "-a", "-m", "First release", "v1"]);
        f.git(&["tag", "-a", "-m", "Not asked for", "unasked"]);
        let tag = rev(&f, "refs/tags/v1");
        let report = push(
            &f.repo,
            &request(&["refs/heads/main:refs/heads/doomed", "refs/tags/v1:refs/tags/v1"], None),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            report.refs.iter().map(|r| (r.flag, r.destination.clone())).collect::<Vec<_>>(),
            [
                (Flag::Created, "refs/heads/doomed".into()),
                (Flag::Created, "refs/tags/v1".into()),
            ]
        );
        assert_eq!(
            remote_refs(&url, &f),
            format!("refs/heads/doomed commit {tip}\nrefs/tags/v1 tag {tag}\n")
        );

        let again = push(&f.repo, &request(&["refs/tags/v1:refs/tags/v1"], None), |_| Ok(())).unwrap();
        assert_eq!(again.refs[0].flag, Flag::UpToDate);
        let report = push(&f.repo, &request(&[":refs/heads/doomed"], None), |_| Ok(())).unwrap();
        assert_eq!(report.refs[0].flag, Flag::Deleted);
        assert_eq!(remote_refs(&url, &f), format!("refs/tags/v1 tag {tag}\n"));
        assert_eq!(rev(&f, "HEAD"), tip);
        assert_eq!(rev(&f, "refs/tags/v1"), tag);
    }

    #[test]
    fn push_replaces_a_ref_only_under_a_lease_that_still_holds() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("shared.git"));
        add(&f.repo, "origin", &url).unwrap();
        let first = commit(&f);
        f.git(&["push", "-q", "origin", "main:refs/heads/release"]);
        f.git(&["commit", "--allow-empty", "-qm", "Published later"]);
        let published = rev(&f, "HEAD");
        f.git(&["push", "-q", "origin", "main:refs/heads/release"]);
        f.git(&["reset", "-q", "--hard", &first]);
        f.git(&["commit", "--allow-empty", "-qm", "Rewritten"]);
        let rewritten = rev(&f, "HEAD");
        let main = ["refs/heads/main:refs/heads/release"];

        let err = push(&f.repo, &request(&main, None), |_| Ok(())).unwrap_err();
        assert_eq!(err.rejection(), Some(Rejection::NonFastForward));
        let stale = Some(("refs/heads/release", first.as_str()));
        let err = push(&f.repo, &request(&main, stale), |_| Ok(())).unwrap_err();
        assert_eq!(err.rejection(), Some(Rejection::Stale));
        let absent = Some(("refs/heads/release", ""));
        let err = push(&f.repo, &request(&main, absent), |_| Ok(())).unwrap_err();
        assert_eq!(err.rejection(), Some(Rejection::Stale));
        assert_eq!(remote_refs(&url, &f), format!("refs/heads/release commit {published}\n"));

        let held = Some(("refs/heads/release", published.as_str()));
        let report = push(&f.repo, &request(&main, held), |_| Ok(())).unwrap();
        assert_eq!(report.refs[0].flag, Flag::Forced);
        assert_eq!(remote_refs(&url, &f), format!("refs/heads/release commit {rewritten}\n"));

        f.git(&["reset", "-q", "--hard", &first]);
        for bad in [
            request(&["+refs/heads/main:refs/heads/release"], None),
            request(&["--force"], None),
            request(&["refs/heads/main"], None),
            request(&[], None),
            request(&main, Some(("release", rewritten.as_str()))),
            request(&main, Some(("refs/heads/release", "nonsense"))),
        ] {
            assert!(matches!(push(&f.repo, &bad, |_| Ok(())), Err(PushError::Failed(_))));
        }
        assert_eq!(remote_refs(&url, &f), format!("refs/heads/release commit {rewritten}\n"));

        let stale = Some(("refs/heads/release", published.as_str()));
        let err = push(&f.repo, &request(&[":refs/heads/release"], stale), |_| Ok(())).unwrap_err();
        let PushError::Refused(report) = &err else { panic!("{err:?}") };
        assert_eq!(report.refs[0].source, OsString::new());
        assert_eq!(report.refs[0].destination, OsString::from("refs/heads/release"));
        assert_eq!(err.rejection(), Some(Rejection::Stale));
        assert_eq!(remote_refs(&url, &f), format!("refs/heads/release commit {rewritten}\n"));
    }

    #[test]
    fn push_reports_a_partial_refusal_and_the_remote_s_own_reason() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("shared.git"));
        add(&f.repo, "origin", &url).unwrap();
        let tip = commit(&f);
        f.git(&["tag", "v1"]);
        f.git(&["push", "-q", "origin", "refs/tags/v1:refs/tags/v1"]);
        f.git(&["commit", "--allow-empty", "-qm", "Moved tag"]);
        let moved = rev(&f, "HEAD");
        f.git(&["tag", "-f", "v1"]);
        let err = push(
            &f.repo,
            &request(&["refs/heads/main:refs/heads/release", "refs/tags/v1:refs/tags/v1"], None),
            |_| Ok(()),
        )
        .unwrap_err();
        let PushError::Refused(report) = &err else { panic!("{err:?}") };
        assert_eq!(report.refs[0].flag, Flag::Created);
        assert_eq!(err.rejection(), Some(Rejection::AlreadyExists));
        assert_eq!(
            remote_refs(&url, &f),
            format!("refs/heads/release commit {moved}\nrefs/tags/v1 commit {tip}\n")
        );

        let hooks = dir.path().join("shared.git/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-receive");
        fs::write(&hook, "#!/bin/sh\necho Releases are frozen >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let err = push(&f.repo, &request(&[":refs/heads/release"], None), |_| Ok(())).unwrap_err();
        assert_eq!(err.rejection(), Some(Rejection::Remote("pre-receive hook declined".into())));
        assert!(String::from(err).contains("Releases are frozen"));
        assert_eq!(rev(&f, "HEAD"), moved);
        assert!(remote_refs(&url, &f).contains("refs/heads/release"));
    }

    #[test]
    fn push_rechecks_before_git_runs_and_reports_a_hook_that_stops_it() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("shared.git"));
        add(&f.repo, "origin", &url).unwrap();
        commit(&f);
        let main = request(&["refs/heads/main:refs/heads/release"], None);
        let err = push(&f.repo, &main, |repo| {
            assert_eq!(repo.root, f.repo.root);
            Err("The branch moved. Reopen the action.".into())
        })
        .unwrap_err();
        assert!(matches!(&err, PushError::Failed(text) if text == "The branch moved. Reopen the action."));
        assert!(remote_refs(&url, &f).is_empty());

        let hooks = dir.path().join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-push");
        fs::write(&hook, "#!/bin/sh\necho Not today >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        f.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        let err = push(&f.repo, &main, |_| Ok(())).unwrap_err();
        assert_eq!(err.rejection(), None);
        assert!(matches!(&err, PushError::Failed(text) if text.contains("Not today")), "{err:?}");
        assert!(remote_refs(&url, &f).is_empty());

        fs::write(&hook, "#!/bin/sh\necho 'pre-push: 2 tests failed'\necho 'see the log' >&2\nexit 3\n").unwrap();
        let link = crate::jobs::Link::new(true);
        let err = link.enter(|| push(&f.repo, &main, |_| Ok(()))).unwrap_err();
        let PushError::Failed(said) = &err else { panic!("{err:?}") };
        assert!(said.starts_with("pre-push: 2 tests failed\nsee the log\n"), "{said}");
        let failure = crate::hooks::named(crate::notice::Failure::from(format!("Push Failed — {said}")), &link.ran(), Some(&crate::hooks::PUSHING)).notice();
        assert_eq!((failure.title.as_str(), failure.detail.as_str()), ("pre-push stopped the push", "Nothing was pushed. It exited with code 3."));
        assert!(failure.output.as_deref().is_some_and(|output| output.starts_with("pre-push: 2 tests failed")));
        assert!(remote_refs(&url, &f).is_empty());
        fs::remove_file(&hook).unwrap();
        push(&f.repo, &main, |_| Ok(())).unwrap();
        assert!(!remote_refs(&url, &f).is_empty());
        assert_eq!(hook_printed(b"*\trefs/heads/main:refs/heads/release\t[new branch]\nTo ../x.git\nDone\nlint ok\n"), "lint ok");
    }

    #[test]
    fn rename_and_remove_remote_reconcile_tracking_and_leave_the_server_and_local_work_intact() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("primary.git"));
        let other = bare(&f, &dir.path().join("other.git"));
        add(&f.repo, "origin", &url).unwrap();
        add(&f.repo, "backup", &other).unwrap();
        let tip = commit(&f);
        f.git(&["push", "--set-upstream", "origin", "main:release"]);
        f.git(&["config", "branch.main.pushRemote", "origin"]);
        f.git(&["config", "remote.pushDefault", "origin"]);
        f.git(&["tag", "milestone"]);
        fs::write(f.dir.join("local-draft"), b"stay here").unwrap();

        assert!(rename(&f.repo, "origin", "backup").is_err());
        assert!(rename(&f.repo, "origin", "--invalid").is_err());
        rename(&f.repo, "origin", "team/upstream").unwrap();
        assert_eq!(f.git(&["remote", "get-url", "team/upstream"]), format!("{url}\n").as_bytes());
        assert_eq!(f.git(&["rev-parse", "--abbrev-ref", "@{upstream}"]), b"team/upstream/release\n");
        assert_eq!(f.git(&["config", "branch.main.pushRemote"]), b"team/upstream\n");
        assert_eq!(f.git(&["config", "remote.pushDefault"]), b"team/upstream\n");
        assert!(remove(&f.repo, "origin").is_err());

        remove(&f.repo, "team/upstream").unwrap();
        assert_eq!(f.git(&["remote"]), b"backup\n");
        assert!(f.git(&["for-each-ref", "refs/remotes"]).is_empty());
        assert!(f.repo.status().unwrap().upstream.is_none());
        assert_eq!(f.git(&["rev-parse", "HEAD", "refs/tags/milestone"]), format!("{tip}\n{tip}\n").as_bytes());
        assert_eq!(f.git(&["--git-dir", &url, "rev-parse", "refs/heads/release"]), format!("{tip}\n").as_bytes());
        assert_eq!(fs::read(f.dir.join("local-draft")).unwrap(), b"stay here");
    }

    #[test]
    fn add_while_unborn_then_publish_named_branch_and_keep_pending_changes() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("upstream.git"));
        add(&f.repo, "company", &url).unwrap();
        add(&f.repo, "company", &url).unwrap();
        assert_eq!(
            list(&f.repo).unwrap(),
            vec![Remote {
                name: "company".into(),
                push_urls: vec![url.clone()]
            }]
        );
        assert!(first_push(&f.repo, "main", &"0".repeat(40), "company", &url, "release").is_err());
        f.git(&["switch", "-qc", "topic/first"]);
        let tip = commit(&f);
        fs::write(f.dir.join("note"), b"draft").unwrap();
        f.git(&["add", "note"]);
        fs::write(f.dir.join("note"), b"edited").unwrap();
        let status = f.git(&["status", "--porcelain=v1"]);
        assert_eq!(
            first_push(&f.repo, "topic/first", &tip, "company", &url, "release").unwrap(),
            PushOutcome::Tracking(Said::default())
        );
        assert_eq!(status, f.git(&["status", "--porcelain=v1"]));
        assert_eq!(
            f.git(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
            b"company/release\n"
        );
        assert_eq!(
            f.git(&[
                "--git-dir",
                &url,
                "for-each-ref",
                "--format=%(objectname) %(refname)"
            ]),
            format!("{tip} refs/heads/release\n").as_bytes()
        );
        assert_eq!(
            first_push(&f.repo, "topic/first", &tip, "company", &url, "release").unwrap(),
            PushOutcome::Tracking(Said::default())
        );
        f.git(&["switch", "--detach", "-q"]);
        assert!(first_push(&f.repo, "topic/first", &tip, "company", &url, "release").is_err());
    }

    #[test]
    fn destination_checks_fail_before_upload_and_hide_embedded_credentials() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("first.git"));
        let other = bare(&f, &dir.path().join("second.git"));
        add(&f.repo, "origin", &url).unwrap();
        assert!(add(&f.repo, "origin", &other).is_err());
        f.git(&["config", "remote.origin.pushurl", &other]);
        assert!(check_available(&f.repo, "origin", &other).is_err());
        f.git(&["config", "--unset", "remote.origin.pushurl"]);
        f.git(&["config", "remote.origin.vcs", "unsupported"]);
        assert!(check_available(&f.repo, "origin", &url).is_err());
        f.git(&["config", "--unset", "remote.origin.vcs"]);
        let tip = commit(&f);
        f.git(&["config", "--add", "remote.origin.pushurl", &url]);
        f.git(&["config", "--add", "remote.origin.pushurl", &other]);
        assert!(first_push(&f.repo, "main", &tip, "origin", &url, "release").is_err());
        f.git(&["config", "--unset-all", "remote.origin.pushurl"]);
        f.git(&["config", &format!("url.{other}.pushInsteadOf"), &url]);
        assert!(first_push(&f.repo, "main", &tip, "origin", &url, "release").is_err());
        assert!(add(&f.repo, "another", &url).is_err());
        assert_eq!(f.git(&["remote"]), b"origin\n");
        assert!(f.git(&["--git-dir", &url, "for-each-ref"]).is_empty());
        assert!(f.git(&["--git-dir", &other, "for-each-ref"]).is_empty());
        let secret = "https://someone:synthetic-secret@example.invalid/repo.git";
        f.git(&["remote", "add", "legacy", secret]);
        assert!(
            list(&f.repo)
                .unwrap()
                .iter()
                .find(|r| r.name == "legacy")
                .unwrap()
                .push_urls
                .is_empty()
        );
        assert!(
            !add(&f.repo, "legacy", secret)
                .unwrap_err()
                .contains("synthetic-secret")
        );
    }

    #[test]
    fn published_while_config_locked_can_repair_without_another_push() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("upstream.git"));
        add(&f.repo, "origin", &url).unwrap();
        let tip = commit(&f);
        let lock = f.repo.git_dir().join("config.lock");
        fs::write(&lock, []).unwrap();
        assert!(matches!(
            first_push(&f.repo, "main", &tip, "origin", &url, "release").unwrap(),
            PushOutcome::PublishedWithoutTracking(_)
        ));
        assert_eq!(
            f.git(&["--git-dir", &url, "rev-parse", "refs/heads/release"]),
            format!("{tip}\n").as_bytes()
        );
        fs::remove_file(lock).unwrap();
        f.git(&["config", "remote.origin.receivepack", "false"]);
        repair_tracking(&f.repo, "main", &tip, "origin", &url, "release").unwrap();
        assert_eq!(
            f.git(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
            b"origin/release\n"
        );
    }

    #[test]
    fn repair_extends_a_single_branch_fetch_mapping_without_replacing_it() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("upstream.git"));
        add(&f.repo, "origin", &url).unwrap();
        f.git(&["remote", "set-branches", "origin", "old-branch"]);
        let tip = commit(&f);
        assert!(matches!(
            first_push(&f.repo, "main", &tip, "origin", &url, "release").unwrap(),
            PushOutcome::PublishedWithoutTracking(_)
        ));
        repair_tracking(&f.repo, "main", &tip, "origin", &url, "release").unwrap();
        let config =
            String::from_utf8(f.git(&["config", "--get-all", "remote.origin.fetch"])).unwrap();
        assert_eq!(
            config,
            "+refs/heads/old-branch:refs/remotes/origin/old-branch\n+refs/heads/release:refs/remotes/origin/release\n"
        );
    }

    #[test]
    fn repair_respects_deliberately_excluded_branches() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("upstream.git"));
        add(&f.repo, "origin", &url).unwrap();
        f.git(&[
            "config",
            "--add",
            "remote.origin.fetch",
            "^refs/heads/rel*se",
        ]);
        let tip = commit(&f);
        assert!(matches!(
            first_push(&f.repo, "main", &tip, "origin", &url, "release").unwrap(),
            PushOutcome::PublishedWithoutTracking(_)
        ));
        let config = f.git(&["config", "--get-all", "remote.origin.fetch"]);
        let err = repair_tracking(&f.repo, "main", &tip, "origin", &url, "release").unwrap_err();
        assert!(err.contains("excludes"));
        assert_eq!(
            config,
            f.git(&["config", "--get-all", "remote.origin.fetch"])
        );
    }

    #[test]
    fn first_push_cannot_move_a_local_branch_through_the_fetch_mapping() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("upstream.git"));
        add(&f.repo, "origin", &url).unwrap();
        let old = commit(&f);
        f.git(&["branch", "release"]);
        let tip = commit(&f);
        f.git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ]);
        assert!(first_push(&f.repo, "main", &tip, "origin", &url, "release").is_err());
        f.git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]);
        f.git(&[
            "symbolic-ref",
            "refs/remotes/origin/release",
            "refs/heads/release",
        ]);
        assert!(first_push(&f.repo, "main", &tip, "origin", &url, "release").is_err());
        assert_eq!(
            f.git(&["rev-parse", "release"]),
            format!("{old}\n").as_bytes()
        );
        assert!(f.git(&["--git-dir", &url, "for-each-ref"]).is_empty());
    }

    #[test]
    fn a_remote_can_be_added_with_a_separate_push_address() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let url = bare(&f, &dir.path().join("fetch.git"));
        let push = bare(&f, &dir.path().join("push.git"));
        add_with_push(&f.repo, "origin", &url, Some(&push)).unwrap();
        assert_eq!(f.git(&["remote", "get-url", "origin"]), format!("{url}\n").as_bytes());
        assert_eq!(f.git(&["remote", "get-url", "--push", "origin"]), format!("{push}\n").as_bytes());
        add_with_push(&f.repo, "plain", &url, Some(&url)).unwrap();
        add_with_push(&f.repo, "bare", &url, None).unwrap();
        assert!(add_with_push(&f.repo, "broken", &url, Some("ext::sh -c touch")).is_err());
        assert_eq!(f.git(&["remote"]), b"bare\norigin\nplain\n");
        assert!(f.git(&["for-each-ref"]).is_empty());
        let shown = addresses(&f.repo).unwrap();
        assert_eq!(shown["origin"], Addresses { fetch: vec![url.clone()], push: vec![push.clone()] });
        for name in ["plain", "bare"] {
            assert_eq!(shown[name], Addresses { fetch: vec![url.clone()], push: vec![] }, "{name}");
        }
    }

    #[test]
    fn accepts_standard_addresses_and_rejects_helpers_and_secrets() {
        for url in [
            "https://github.com/team/repo.git",
            "ssh://git@host.example:2222/proj",
            "git@github.com:team/project.git",
            "git@[::1]:team/project.git",
            "/tmp/my repo.git",
            "../peer.git",
            "file:///tmp/peer.git",
            "https://user@host/repo",
            "https://dev-team@dev.azure.com/team/project/_git/repo",
            "http://cedar@127.0.0.1:8080/trail.git",
        ] {
            assert!(validate_url(url).is_ok(), "{url}");
        }
        for url in [
            "ext::thing",
            "arbitrary::thing",
            "custom://host/repo",
            "https://a:b@host/repo",
            "https://a:@host/repo",
            "https://@host/repo",
            "https://a@b@host/repo",
            "git://user@host/repo",
            "https://ghp_example@github.com/team/repo.git",
            "https://github_pat_11ABCDEFG@github.com/team/repo.git",
            "https://glpat-xxxxxxxxxxxxxxxxxxxx@gitlab.com/team/repo.git",
            "https://host/repo?token=hidden",
            "ssh://a:b@host/repo",
            "-option",
            "a\nb",
            "file://other/repo",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }
}
