use crate::{
    Tab, clone,
    notice::Notice,
    tabs::{self, Request},
    view::Tone,
};
use gpui::{App, Context, Window};
use std::path::{Path, PathBuf};
use url::Url;

pub(crate) const SCHEME: &str = "fastkraken";
pub(crate) const CANNOT_FOLLOW: &str = "FastKraken cannot follow this link";
pub(crate) const OPEN: &str = "Open";
pub(crate) const FOLDERS_ONLY: &str = "FastKraken opens folders";
const LINKS: &str = "FastKraken links open a repository, as fastkraken://open?path=…, or start a clone, as fastkraken://clone?url=….";
const SHOWN: usize = 80;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    Folder(PathBuf),
    Open(PathBuf),
    Clone(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Open(PathBuf),
}

fn shown(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(SHOWN) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

fn only(url: &Url, name: &str) -> Result<String, String> {
    let values: Vec<String> = url.query_pairs().filter(|(key, _)| key == name).map(|(_, value)| value.into_owned()).collect();
    match values.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("The link gives no {name}. {LINKS}")),
        _ => Err(format!("The link gives more than one {name}.")),
    }
}

pub(crate) fn read(text: &str) -> Result<Link, String> {
    let url = Url::parse(text.trim()).map_err(|_| format!("{} is not a link FastKraken can read.", shown(text)))?;
    if url.scheme() == "file" {
        return url.to_file_path().map(Link::Folder).map_err(|_| format!("{} does not name a folder on this Mac.", shown(text)));
    }
    if url.scheme() != SCHEME {
        return Err(format!("FastKraken follows {SCHEME}: links, not {}: links.", url.scheme()));
    }
    let verb = match url.host_str() {
        Some(host) if matches!(url.path(), "" | "/") => host.to_ascii_lowercase(),
        Some(_) => String::new(),
        None => url.path().trim_matches('/').to_ascii_lowercase(),
    };
    match verb.as_str() {
        "open" => {
            let path = only(&url, "path")?;
            if path.chars().any(char::is_control) || !Path::new(&path).is_absolute() {
                return Err("The link must name the repository's folder by its full path, starting with /.".to_owned());
            }
            Ok(Link::Open(PathBuf::from(path)))
        }
        "clone" => {
            let address = only(&url, "url")?.trim().to_owned();
            match clone::address_problem(&address) {
                None => Ok(Link::Clone(address)),
                Some(problem) if problem.is_empty() => Err("The link gives no address to clone.".to_owned()),
                Some(problem) => Err(format!("The link's address {} cannot be cloned. {problem}", shown(&address))),
            }
        }
        _ => Err(LINKS.to_owned()),
    }
}

pub(crate) fn refused(reason: impl Into<String>) -> Notice {
    Notice::new(Tone::Error, CANNOT_FOLLOW, reason).sticky()
}

pub(crate) fn offer(path: &Path) -> Notice {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    Notice::new(
        Tone::Info,
        format!("Open {name}?"),
        format!("A link asks FastKraken to open {}. Open it only if you expected this link.", path.display()),
    )
    .sticky()
    .offer(OPEN, crate::view::Action::Links(Action::Open(path.to_owned())))
}

pub(crate) fn request(url: &str) -> Request {
    match read(url) {
        Ok(Link::Folder(path)) => Request::Handed(path),
        Ok(Link::Open(path)) => Request::Linked(std::fs::canonicalize(&path).unwrap_or(path)),
        Ok(Link::Clone(address)) => Request::Clone(address),
        Err(reason) => Request::Tell(refused(reason)),
    }
}

pub(crate) fn follow(urls: Vec<String>, cx: &mut App) {
    for url in urls {
        tabs::ask(cx, request(&url));
    }
}

pub(crate) fn folders(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|path| path.is_dir()).cloned().collect()
}

pub(crate) fn dropped(paths: &[PathBuf], cx: &mut App) {
    let folders = folders(paths);
    if folders.is_empty() {
        let notice = Notice::new(Tone::Info, FOLDERS_ONLY, "Drop a repository's folder on the window to open it.");
        tabs::ask(cx, Request::Tell(notice));
        return;
    }
    for folder in folders {
        tabs::ask(cx, Request::Handed(folder));
    }
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Open(path) => {
            if !tabs::ask(cx, Request::Handed(path.clone())) {
                tab.open(path, cx);
            }
        }
    }
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(text: &str) -> Link {
        read(text).unwrap_or_else(|why| panic!("{text}: {why}"))
    }

    fn refusal(text: &str) -> String {
        read(text).expect_err(text)
    }

    #[test]
    fn an_open_link_names_a_folder_by_its_full_path() {
        assert_eq!(link("fastkraken://open?path=/Users/cedar/Lantern"), Link::Open("/Users/cedar/Lantern".into()));
        assert_eq!(link("fastkraken://open?path=%2FUsers%2Fcedar%2FTwo%20Words"), Link::Open("/Users/cedar/Two Words".into()));
        assert_eq!(link("FastKraken://OPEN/?path=/tmp/a+b"), Link::Open("/tmp/a b".into()));
        assert_eq!(link("fastkraken:open?path=/tmp/lantern&commit=abc"), Link::Open("/tmp/lantern".into()));
        assert_eq!(link(" fastkraken://open?path=/tmp/lantern \n"), Link::Open("/tmp/lantern".into()));
        assert!(refusal("fastkraken://open?path=Lantern").contains("full path"));
        assert!(refusal("fastkraken://open?path=~/Lantern").contains("full path"));
        assert!(refusal("fastkraken://open?path=/tmp/a%0Ab").contains("full path"));
        assert!(refusal("fastkraken://open").contains("gives no path"));
        assert!(refusal("fastkraken://open?path=/a&path=/b").contains("more than one path"));
    }

    #[test]
    fn a_clone_link_carries_an_address_the_dialog_would_accept() {
        assert_eq!(
            link("fastkraken://clone?url=https%3A%2F%2Fgithub.com%2Fcedar%2Flantern.git"),
            Link::Clone("https://github.com/cedar/lantern.git".into())
        );
        assert_eq!(link("fastkraken://clone?url=git@github.com:cedar/lantern.git"), Link::Clone("git@github.com:cedar/lantern.git".into()));
        assert_eq!(link("fastkraken://clone?url=%20/srv/git/lantern.git%20"), Link::Clone("/srv/git/lantern.git".into()));
        assert_eq!(link("fastkraken://clone?url=file:///srv/git/lantern.git"), Link::Clone("file:///srv/git/lantern.git".into()));
        assert!(refusal("fastkraken://clone?url=").contains("no address"));
        assert!(refusal("fastkraken://clone").contains("gives no url"));
        assert!(refusal("fastkraken://clone?url=ext::sh%20-c%20touch%20%2Ftmp%2Fx").contains("cannot be cloned"));
        assert!(refusal("fastkraken://clone?url=-u%20evil").contains("cannot be cloned"));
        assert!(refusal("fastkraken://clone?url=https%3A%2F%2Fcedar%3Asecret%40example.invalid%2Fa.git").contains("cannot be cloned"));
        assert!(refusal("fastkraken://clone?url=lantern").contains("cannot be cloned"));
        assert!(refusal("fastkraken://clone?url=/a.git&url=/b.git").contains("more than one url"));
    }

    #[test]
    fn other_links_are_explained_and_folders_from_the_system_pass_through() {
        assert_eq!(link("file:///Users/cedar/Lantern/"), Link::Folder("/Users/cedar/Lantern/".into()));
        assert_eq!(link("file:///Users/cedar/Two%20Words"), Link::Folder("/Users/cedar/Two Words".into()));
        assert!(refusal("fastkraken://checkout?branch=main").starts_with("FastKraken links open a repository"));
        assert!(refusal("fastkraken://open/elsewhere?path=/tmp").starts_with("FastKraken links open a repository"));
        assert!(refusal("https://example.invalid/").contains("not https: links"));
        assert!(refusal("not a link").contains("is not a link"));
        let long = format!("fastkraken://{}", "x".repeat(300));
        assert!(refusal(&format!("{long} spaces")).len() < 140);
        assert_eq!(refused("why").title, CANNOT_FOLLOW);
        assert_eq!(refused("why").tone, Tone::Error);
    }

    #[test]
    fn an_open_link_asks_before_opening() {
        let notice = offer(Path::new("/tmp/Lantern"));
        assert_eq!(notice.title, "Open Lantern?");
        assert!(notice.detail.contains("/tmp/Lantern"), "{}", notice.detail);
        assert!(matches!(
            notice.actions.as_slice(),
            [(label, crate::view::Action::Links(Action::Open(path)))] if label.as_ref() == OPEN && path == Path::new("/tmp/Lantern")
        ));
        assert!(matches!(request("fastkraken://open?path=/nowhere/Lantern"), Request::Linked(path) if path == Path::new("/nowhere/Lantern")));
        assert!(matches!(request("file:///nowhere/Lantern"), Request::Handed(path) if path == Path::new("/nowhere/Lantern")));
        assert!(matches!(request("fastkraken://clone?url=/srv/a.git"), Request::Clone(address) if address == "/srv/a.git"));
        assert!(matches!(request("fastkraken://pull"), Request::Tell(notice) if notice.title == CANNOT_FOLLOW));
    }

    #[test]
    fn only_folders_dropped_on_the_window_are_opened() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Lantern");
        std::fs::create_dir(&folder).unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "notes\n").unwrap();
        let missing = dir.path().join("gone");
        assert_eq!(folders(&[file.clone(), folder.clone(), missing.clone()]), vec![folder]);
        assert!(folders(&[file, missing]).is_empty());
    }

    #[test]
    fn the_bundle_declares_the_link_scheme_and_folders() {
        let plist = include_str!("../packaging/macos/Info.plist");
        let schemes = plist.split("<key>CFBundleURLSchemes</key>").nth(1).expect("no URL schemes");
        let declared = schemes.split("</array>").next().unwrap();
        assert!(declared.contains(&format!("<string>{SCHEME}</string>")), "{declared}");
        assert!(plist.contains("<string>public.folder</string>"));
    }
}
