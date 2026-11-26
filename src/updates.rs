use crate::{
    Tab, handoff,
    commands::{Availability, Bar, Command, Group, Target},
    notice::Notice,
    settings::{self, Memory, Settings},
    tabs::Workspace,
    view::{self, Tone, Ui},
};
use gpui::{App, Context, Global, Window};
use serde_json::Value;
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(crate) const REPOSITORY: &str = "bitshielddev/FastKraken";
const API: &str = "https://api.github.com";
const DAY: i64 = 86_400;
const RETRY: i64 = 3_600;
const FIRST_LOOK: Duration = Duration::from_secs(30);
const LOOK_EVERY: Duration = Duration::from_secs(15 * 60);
const LARGEST_ANSWER: u64 = 1024 * 1024;
const UNREACHABLE: &str = "GitHub could not be reached. Check your connection and try again.";
pub(crate) const CHECKING: &str = "Checking for updates…";
pub(crate) const OPEN_PAGE: &str = "Open release page";
pub(crate) const AUTOMATIC_TITLE: &str = "Check for updates automatically";
pub(crate) const AUTOMATIC_HELP: &str = "Once a day, asks GitHub (api.github.com) for the latest FastKraken release and tells you when a newer version is out. GitHub sees the request and where it came from. Off, FastKraken asks only when you choose FastKraken > Check for Updates….";

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "app.updates",
    Group::App,
    |_, _| "Check for updates…".into(),
    available,
    |_, _| Some(view::Action::Updates(Action::Check)),
)
.words(&["update", "upgrade", "new version", "latest release", "download"])
.icon("arrow-down")
.bar(Bar::App)];

fn available(_: &Ui, target: &Target, cx: &App) -> Availability {
    if *target != Target::Repo {
        return Availability::Hidden;
    }
    let by_hand = cx.try_global::<Updates>().is_some_and(|updates| updates.by_hand);
    Availability::unless((by_hand && checking(cx)).then(|| "FastKraken is already checking for updates".to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Check,
    Automatically(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version([u64; 3]);

impl Version {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let mut parts = [0; 3];
        let mut count = 0;
        for part in text.split('.') {
            if count == parts.len() || part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            parts[count] = part.parse().ok()?;
            count += 1;
        }
        (count >= 2).then_some(Self(parts))
    }

    pub(crate) fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self([0; 3]))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(f, "{major}.{minor}.{patch}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: Version,
    pub(crate) page: String,
}

pub(crate) type Answer = Result<Option<Release>, String>;

pub(crate) fn releases_page() -> String {
    format!("https://github.com/{REPOSITORY}/releases/")
}

fn page_of(listed: Option<&str>, tag: &str) -> String {
    let releases = releases_page();
    match listed {
        Some(url) if url.starts_with(&releases) && url.len() > releases.len() && handoff::openable(url) => url.to_owned(),
        _ => format!("{releases}tag/{tag}"),
    }
}

pub(crate) fn release_of(json: &Value) -> Answer {
    if json["draft"].as_bool() == Some(true) || json["prerelease"].as_bool() == Some(true) {
        return Ok(None);
    }
    let tag = json["tag_name"].as_str().ok_or("GitHub's answer names no release.")?;
    let version = Version::parse(tag)
        .ok_or_else(|| format!("The latest release, “{}”, has no version number FastKraken can read.", tag.chars().take(40).collect::<String>()))?;
    Ok(Some(Release { version, page: page_of(json["html_url"].as_str(), tag.trim()) }))
}

pub(crate) fn latest(agent: &ureq::Agent, base: &str) -> Answer {
    let mut reply = agent
        .get(format!("{base}/repos/{REPOSITORY}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|_| UNREACHABLE.to_owned())?;
    match reply.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        403 | 429 => return Err("GitHub is limiting how often it answers. Try again later.".into()),
        status => return Err(format!("GitHub answered {status}. Try again later.")),
    }
    let bytes = reply
        .body_mut()
        .with_config()
        .limit(LARGEST_ANSWER)
        .read_to_vec()
        .map_err(|_| UNREACHABLE.to_owned())?;
    let json: Value = serde_json::from_slice(&bytes).map_err(|_| "GitHub's answer could not be read.".to_owned())?;
    release_of(&json)
}

fn ask() -> Answer {
    if crate::acceptance::replaying() {
        let base = crate::github::replay_api().ok_or("Replays ask only a stand-in for GitHub, and none is serving.")?;
        return latest(&crate::github::http_agent(true), &base);
    }
    if cfg!(test) {
        return Err("Tests never ask GitHub for releases.".into());
    }
    latest(&crate::github::http_agent(false), API)
}

pub(crate) fn due(enabled: bool, checked: Option<i64>, tried: Option<i64>, now: i64) -> bool {
    let waited = |at: Option<i64>, wait: i64| at.is_none_or(|at| now < at || now - at >= wait);
    enabled && waited(checked, DAY) && waited(tried, RETRY)
}

pub(crate) fn notice(answer: &Answer, current: Version, asked: bool) -> Option<Notice> {
    match answer {
        Ok(Some(release)) if release.version > current => Some(
            Notice::new(
                Tone::Info,
                format!("FastKraken {} is available", release.version),
                format!("You have {current}. The release page has the download and what changed."),
            )
            .sticky()
            .offer(OPEN_PAGE, view::Action::Handoff(handoff::Action::OpenUrl(release.page.clone()))),
        ),
        _ if !asked => None,
        Ok(Some(release)) if release.version == current => {
            Some(Notice::new(Tone::Success, "FastKraken is up to date", format!("{current} is the latest release.")))
        }
        Ok(Some(release)) => Some(Notice::new(
            Tone::Success,
            "FastKraken is up to date",
            format!("You have {current}, which is newer than the latest release, {}.", release.version),
        )),
        Ok(None) => Some(Notice::new(Tone::Info, "No FastKraken release has been published yet", "")),
        Err(reason) => Some(Notice::new(Tone::Error, "Could not check for updates", reason.clone())),
    }
}

#[derive(Default)]
struct Updates {
    checking: Arc<AtomicBool>,
    by_hand: bool,
    tried: Option<i64>,
    announced: Option<Version>,
}

impl Global for Updates {}

pub(crate) fn checking(cx: &App) -> bool {
    cx.try_global::<Updates>().is_some_and(|updates| updates.checking.load(Ordering::SeqCst))
}

fn due_now(cx: &App) -> bool {
    let enabled = settings::peek(cx, |settings| settings.check_updates);
    let checked = settings::memory(cx).and_then(|memory| memory.updates_checked);
    let tried = cx.try_global::<Updates>().and_then(|updates| updates.tried);
    due(enabled, checked, tried, view::now())
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Check => check(tab, true, cx),
        Action::Automatically(on) => {
            Settings::update(cx, |settings| settings.check_updates = on);
            if on && due_now(cx) {
                check(tab, false, cx);
            }
        }
    }
    cx.notify();
}

pub(crate) fn check(tab: &mut Tab, asked: bool, cx: &mut Context<Tab>) {
    let flag = cx.default_global::<Updates>().checking.clone();
    if flag.swap(true, Ordering::SeqCst) {
        if asked {
            cx.global_mut::<Updates>().by_hand = true;
            tab.ui.notices.show(Notice::new(Tone::Info, "FastKraken is already checking for updates", ""));
        }
        return;
    }
    let updates = cx.global_mut::<Updates>();
    updates.tried = Some(view::now());
    updates.by_hand = asked;
    let progress = asked.then(|| tab.ui.notices.progress(CHECKING));
    let work = move || {
        let answer = ask();
        flag.store(false, Ordering::SeqCst);
        answer
    };
    Tab::work(cx, work, move |tab, answer, cx| answered(tab, answer, asked, progress, cx));
}

fn answered(tab: &mut Tab, answer: Answer, asked: bool, progress: Option<u64>, cx: &mut Context<Tab>) {
    let asked = std::mem::take(&mut cx.default_global::<Updates>().by_hand) || asked;
    if answer.is_ok() {
        Memory::update(cx, |memory| memory.updates_checked = Some(view::now()));
    }
    let found = answer.as_ref().ok().and_then(Option::as_ref).map(|release| release.version);
    let told = cx.default_global::<Updates>().announced;
    let repeated = !asked && found.is_some() && found == told;
    let shown = if repeated { None } else { notice(&answer, Version::current(), asked) };
    if shown.as_ref().is_some_and(|notice| !notice.actions.is_empty()) {
        cx.global_mut::<Updates>().announced = found;
    }
    match (shown, progress) {
        (Some(shown), Some(id)) => {
            tab.ui.notices.finish(id, shown);
        }
        (Some(shown), None) => {
            tab.ui.notices.show(shown);
        }
        (None, Some(id)) => {
            tab.ui.notices.remove(id);
        }
        (None, None) => {}
    }
    cx.notify();
}

pub(crate) fn arm(cx: &mut Context<Workspace>) {
    if cfg!(test) || crate::acceptance::replaying() {
        return;
    }
    cx.spawn(async move |desk, cx| {
        cx.background_executor().timer(FIRST_LOOK).await;
        loop {
            let looked = desk.update(cx, |desk, cx| {
                if due_now(cx) {
                    desk.front().clone().update(cx, |tab, cx| check(tab, false, cx));
                }
            });
            if looked.is_err() {
                break;
            }
            cx.background_executor().timer(LOOK_EVERY).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::tests::{listen, response};
    use serde_json::json;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_are_read_from_tags_and_compared_by_number() {
        assert_eq!(version("v0.8.0"), Version([0, 8, 0]));
        assert_eq!(version("0.9"), Version([0, 9, 0]));
        assert_eq!(version(" V1.2.3 "), Version([1, 2, 3]));
        assert_eq!(version("v0.10.0").to_string(), "0.10.0");
        assert!(version("v0.10.0") > version("v0.9.9"));
        assert!(version("1.0") > version("0.99.99"));
        assert!(version("0.8.1") > version("0.8"));
        assert_eq!(version("0.8"), version("0.8.0"));
        for tag in ["", "v", "1", "v1.", "1..2", "1.2.3.4", "v0.9.0-rc.1", "0.9 beta", "x1.2", "1.2.3a", "1234567890.0"] {
            assert_eq!(Version::parse(tag), None, "{tag:?}");
        }
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn a_release_takes_its_page_only_from_the_project_s_own_releases() {
        let release = release_of(&json!({
            "tag_name": "v0.9.0",
            "html_url": "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0",
            "draft": false,
            "prerelease": false
        }))
        .unwrap()
        .unwrap();
        assert_eq!(release, Release { version: version("0.9.0"), page: "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0".into() });
        for listed in [
            json!("https://evil.example.invalid/bitshielddev/FastKraken/releases/tag/v0.9.0"),
            json!("http://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0"),
            json!("https://github.com/bitshielddev/FastKraken/releases/"),
            json!("https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0 extra"),
            json!(42),
            Value::Null,
        ] {
            let release = release_of(&json!({"tag_name": "v0.9.0", "html_url": listed})).unwrap().unwrap();
            assert_eq!(release.page, "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0", "{listed}");
        }
        assert_eq!(release_of(&json!({"tag_name": "v1.0.0", "draft": true})), Ok(None));
        assert_eq!(release_of(&json!({"tag_name": "v1.0.0", "prerelease": true})), Ok(None));
        assert!(release_of(&json!({"name": "Nameless"})).unwrap_err().contains("names no release"));
        let unreadable = release_of(&json!({"tag_name": "nightly"})).unwrap_err();
        assert!(unreadable.contains("“nightly”"), "{unreadable}");
    }

    #[test]
    fn the_latest_release_is_asked_of_the_project_s_public_releases() {
        let answers = vec![
            response(200, json!({"tag_name": "v0.9.0", "html_url": "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0"})),
            response(404, json!({"message": "Not Found"})),
            response(403, json!({"message": "API rate limit exceeded"})),
            response(500, json!({})),
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot json!".to_owned(),
        ];
        let (base, server) = listen(answers);
        let agent = crate::github::http_agent(true);
        let found = latest(&agent, &base).unwrap().unwrap();
        assert_eq!(found.version, version("0.9.0"));
        assert_eq!(latest(&agent, &base), Ok(None));
        assert!(latest(&agent, &base).unwrap_err().contains("limiting"));
        assert!(latest(&agent, &base).unwrap_err().contains("500"));
        assert!(latest(&agent, &base).unwrap_err().contains("could not be read"));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        for request in &requests {
            assert_eq!(request.line, "GET /repos/bitshielddev/FastKraken/releases/latest HTTP/1.1");
            assert!(request.headers.iter().any(|header| header.eq_ignore_ascii_case("accept: application/vnd.github+json")), "{:?}", request.headers);
            assert!(!request.headers.iter().any(|header| header.to_ascii_lowercase().starts_with("authorization")), "{:?}", request.headers);
        }
        assert_eq!(ask(), Err("Tests never ask GitHub for releases.".into()));
    }

    #[test]
    fn a_newer_release_is_announced_with_its_page_and_the_rest_only_when_asked() {
        let current = version("0.8.0");
        let release = |text: &str| Ok(Some(Release { version: version(text), page: format!("https://github.com/bitshielddev/FastKraken/releases/tag/v{text}") }));
        for asked in [true, false] {
            let shown = notice(&release("0.9.0"), current, asked).unwrap();
            assert_eq!(shown.title, "FastKraken 0.9.0 is available");
            assert!(shown.detail.contains("You have 0.8.0"), "{}", shown.detail);
            assert_eq!(shown.tone, Tone::Info);
            let [(label, view::Action::Handoff(handoff::Action::OpenUrl(url)))] = shown.actions.as_slice() else {
                panic!("{:?}", shown.actions);
            };
            assert_eq!(label.as_ref(), OPEN_PAGE);
            assert_eq!(url, "https://github.com/bitshielddev/FastKraken/releases/tag/v0.9.0");
        }
        for answer in [release("0.8.0"), release("0.7.9"), Ok(None), Err("GitHub answered 500.".into())] {
            assert!(notice(&answer, current, false).is_none(), "{answer:?}");
        }
        let same = notice(&release("0.8.0"), current, true).unwrap();
        assert_eq!((same.tone, same.title.as_str(), same.detail.as_str()), (Tone::Success, "FastKraken is up to date", "0.8.0 is the latest release."));
        assert!(same.actions.is_empty());
        let older = notice(&release("0.7.9"), current, true).unwrap();
        assert!(older.detail.contains("newer than the latest release, 0.7.9"), "{}", older.detail);
        let none = notice(&Ok(None), current, true).unwrap();
        assert_eq!(none.title, "No FastKraken release has been published yet");
        let failed = notice(&Err(UNREACHABLE.into()), current, true).unwrap();
        assert_eq!((failed.tone, failed.title.as_str(), failed.detail.as_str()), (Tone::Error, "Could not check for updates", UNREACHABLE));
    }

    #[test]
    fn the_automatic_check_is_off_until_chosen_and_then_runs_once_a_day() {
        let now = 1_800_000_000;
        assert!(!Settings::default().check_updates);
        assert!(!due(false, None, None, now));
        assert!(due(true, None, None, now));
        assert!(!due(true, Some(now - DAY + 60), None, now));
        assert!(due(true, Some(now - DAY), None, now));
        assert!(due(true, Some(now + 600), None, now));
        assert!(!due(true, None, Some(now - 60), now));
        assert!(due(true, None, Some(now - RETRY), now));
        assert!(!due(true, Some(now - 2 * DAY), Some(now - 60), now));
        assert!(!due(false, Some(now - 2 * DAY), Some(now - 2 * RETRY), now));
    }

    #[test]
    fn the_command_sits_after_about_in_the_fastkraken_menu() {
        let command = crate::commands::find("app.updates").unwrap();
        assert_eq!(command.bar, Some(Bar::App));
        assert_eq!(command.shortcut, None);
        assert_eq!(crate::commands::title_case("Check for updates…"), "Check for Updates…");
        let app: Vec<_> = crate::commands::in_bar(Bar::App).into_iter().map(|command| command.id).collect();
        assert_eq!(app[..2], ["app.about", "app.updates"]);
        assert_eq!(crate::commands::bar_index(Bar::App, "app.updates"), Some(1));
    }
}
