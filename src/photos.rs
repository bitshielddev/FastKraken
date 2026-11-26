use crate::{Tab, image_diff, repository::Repository, settings};
use gpui::{App, Context, Global, Image, RenderImage};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const SIZE: u32 = 80;
const LARGEST: u64 = 1024 * 1024;
const GITHUB_AVATARS: &str = "avatars.githubusercontent.com";
const NOREPLY: &str = "@users.noreply.github.com";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Pictures {
    #[default]
    Initials,
    GitHub,
    Gravatar,
}

impl Pictures {
    pub(crate) const ALL: [Pictures; 3] = [Pictures::Initials, Pictures::GitHub, Pictures::Gravatar];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Pictures::Initials => "Initials",
            Pictures::GitHub => "GitHub",
            Pictures::Gravatar => "GitHub and Gravatar",
        }
    }

    pub(crate) fn help(self) -> &'static str {
        match self {
            Pictures::Initials => {
                "Authors are shown by their initials, and FastKraken asks no one for a picture. Photos from GitHub or Gravatar tell that service which authors you are looking at."
            }
            Pictures::GitHub => {
                "Asks GitHub for authors' pictures: by the user ID in a GitHub noreply address, and, for a remote connected to your GitHub account, by the authors of its latest commits. GitHub learns whose pictures you look at. Pictures are kept in FastKraken's own folder."
            }
            Pictures::Gravatar => {
                "As GitHub, and for every other author sends Gravatar (gravatar.com) a SHA-256 hash of their email address, so Gravatar learns which addresses you look at. Pictures are kept in FastKraken's own folder."
            }
        }
    }
}

pub(crate) trait Fetch: Send + Sync {
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String>;
}

struct Network(ureq::Agent);

impl Fetch for Network {
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        if cfg!(test) || crate::acceptance::replaying() {
            return Err("Tests and replays never fetch pictures.".into());
        }
        let reply = self.0.get(url).call().map_err(|error| error.to_string())?;
        match reply.status().as_u16() {
            200 => {
                let mut bytes = Vec::new();
                reply.into_body().into_reader().take(LARGEST + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
                Ok((bytes.len() as u64 <= LARGEST).then_some(bytes))
            }
            404 => Ok(None),
            status => Err(format!("The picture server answered {status}.")),
        }
    }
}

#[derive(Default)]
pub(crate) struct Recorder {
    asked: Mutex<Vec<String>>,
    answers: Mutex<HashMap<String, Vec<u8>>>,
}

impl Recorder {
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn answer(&self, url: &str, bytes: Vec<u8>) {
        self.answers.lock().unwrap_or_else(|e| e.into_inner()).insert(url.to_owned(), bytes);
    }
}

impl Fetch for Recorder {
    fn get(&self, url: &str) -> Result<Option<Vec<u8>>, String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_owned());
        Ok(self.answers.lock().unwrap_or_else(|e| e.into_inner()).get(url).cloned())
    }
}

#[derive(Clone)]
enum Look {
    Asking,
    Shown(Arc<RenderImage>),
    Missing,
}

pub(crate) struct Photos {
    folder: Option<PathBuf>,
    fetch: Arc<dyn Fetch>,
    recorder: Option<Arc<Recorder>>,
    allowed: Arc<AtomicBool>,
    looks: HashMap<String, Look>,
    hosted: HashMap<String, String>,
    logins: HashMap<String, String>,
    wanted: Vec<(String, String)>,
    draining: bool,
    looked: HashSet<PathBuf>,
}

impl Global for Photos {}

pub(crate) fn key_of(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn sized(url: &str) -> String {
    let joint = if url.contains('?') { '&' } else { '?' };
    format!("{url}{joint}s={SIZE}")
}

pub(crate) fn github_avatar(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    let clean = url.scheme() == "https"
        && url.host_str() == Some(GITHUB_AVATARS)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none();
    clean.then(|| url.to_string())
}

pub(crate) fn noreply_id(key: &str) -> Option<u64> {
    let (id, _) = key.strip_suffix(NOREPLY)?.split_once('+')?;
    id.parse().ok().filter(|id| *id > 0)
}

fn noreply_login(key: &str) -> Option<&str> {
    let login = key.strip_suffix(NOREPLY)?;
    (!login.contains('+') && !login.is_empty()).then_some(login)
}

pub(crate) fn gravatar(key: &str) -> String {
    let hash: String = sha256(key.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    format!("https://gravatar.com/avatar/{hash}?s={SIZE}&d=404")
}

pub(crate) fn address(key: &str, pictures: Pictures, hosted: &HashMap<String, String>, logins: &HashMap<String, String>) -> Option<String> {
    if pictures == Pictures::Initials || key.is_empty() {
        return None;
    }
    if let Some(url) = hosted.get(key) {
        return Some(sized(url));
    }
    if let Some(id) = noreply_id(key) {
        return Some(format!("https://{GITHUB_AVATARS}/u/{id}?s={SIZE}&v=4"));
    }
    if let Some(url) = noreply_login(key).and_then(|login| logins.get(login)) {
        return Some(sized(url));
    }
    (pictures == Pictures::Gravatar).then(|| gravatar(key))
}

pub(crate) fn start(cx: &mut App) {
    if cx.has_global::<Photos>() {
        allow(cx);
        return;
    }
    let sandboxed = cfg!(test) || crate::acceptance::replaying();
    let recorder = sandboxed.then(|| Arc::new(Recorder::default()));
    let fetch: Arc<dyn Fetch> = match &recorder {
        Some(recorder) => recorder.clone(),
        None => Arc::new(Network(crate::github::http_agent(false))),
    };
    let folder = settings::folder(cx).map(|directory| directory.join("photos"));
    cx.set_global(Photos {
        folder,
        fetch,
        recorder,
        allowed: Arc::new(AtomicBool::new(false)),
        looks: HashMap::new(),
        hosted: HashMap::new(),
        logins: HashMap::new(),
        wanted: Vec::new(),
        draining: false,
        looked: HashSet::new(),
    });
    allow(cx);
}

pub(crate) fn recorder(cx: &App) -> Option<Arc<Recorder>> {
    cx.try_global::<Photos>().and_then(|photos| photos.recorder.clone())
}

pub(crate) fn folder(cx: &App) -> Option<PathBuf> {
    cx.try_global::<Photos>().and_then(|photos| photos.folder.clone())
}

pub(crate) fn shown(email: &str, cx: &App) -> bool {
    cx.try_global::<Photos>().is_some_and(|photos| matches!(photos.looks.get(&key_of(email)), Some(Look::Shown(_))))
}

pub(crate) fn chosen(cx: &mut App) {
    allow(cx);
    if cx.has_global::<Photos>() {
        let photos = cx.global_mut::<Photos>();
        photos.looks.retain(|_, look| matches!(look, Look::Shown(_)));
        photos.looked.clear();
    }
    cx.refresh_windows();
}

fn allow(cx: &mut App) {
    let on = settings::peek(cx, |settings| settings.pictures) != Pictures::Initials;
    if let Some(photos) = cx.try_global::<Photos>() {
        photos.allowed.store(on, Ordering::Release);
    }
}

pub(crate) fn photo(email: &str, cx: &mut App) -> Option<Arc<RenderImage>> {
    let pictures = settings::peek(cx, |settings| settings.pictures);
    if pictures == Pictures::Initials || email.is_empty() || !cx.has_global::<Photos>() {
        return None;
    }
    let key = key_of(email);
    let photos = cx.global_mut::<Photos>();
    match photos.looks.get(&key) {
        Some(Look::Shown(image)) => return Some(image.clone()),
        Some(_) => return None,
        None => {}
    }
    let Some(url) = address(&key, pictures, &photos.hosted, &photos.logins) else {
        photos.looks.insert(key, Look::Missing);
        return None;
    };
    photos.looks.insert(key.clone(), Look::Asking);
    photos.wanted.push((key, url));
    if !photos.draining {
        photos.draining = true;
        cx.defer(drain);
    }
    None
}

enum Found {
    Picture(Arc<RenderImage>),
    Missing,
    Skipped,
}

fn drain(cx: &mut App) {
    let photos = cx.global_mut::<Photos>();
    let batch = std::mem::take(&mut photos.wanted);
    if batch.is_empty() {
        photos.draining = false;
        return;
    }
    let (fetch, folder, allowed) = (photos.fetch.clone(), photos.folder.clone(), photos.allowed.clone());
    let renderer = cx.svg_renderer();
    let task = cx.background_executor().spawn(async move {
        batch
            .into_iter()
            .map(|(key, url)| {
                if !allowed.load(Ordering::Acquire) {
                    return (key, Found::Skipped);
                }
                let decoded = load(fetch.as_ref(), folder.as_deref(), &url).and_then(|bytes| {
                    let format = image_diff::sniff(&bytes)?;
                    Image::from_bytes(format, bytes).to_image_data(renderer.clone()).ok()
                });
                (key, decoded.map_or(Found::Missing, Found::Picture))
            })
            .collect::<Vec<_>>()
    });
    cx.spawn(async move |cx| {
        let found = task.await;
        let _ = cx.update(|cx| {
            let photos = cx.global_mut::<Photos>();
            for (key, found) in found {
                match found {
                    Found::Picture(image) => photos.looks.insert(key, Look::Shown(image)),
                    Found::Missing => photos.looks.insert(key, Look::Missing),
                    Found::Skipped => photos.looks.remove(&key),
                };
            }
            if photos.wanted.is_empty() {
                photos.draining = false;
            } else {
                cx.defer(drain);
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

pub(crate) fn cache_path(folder: &Path, url: &str) -> PathBuf {
    let name: String = sha256(url.as_bytes()).iter().take(20).map(|byte| format!("{byte:02x}")).collect();
    folder.join(name)
}

pub(crate) fn load(fetch: &dyn Fetch, folder: Option<&Path>, url: &str) -> Option<Vec<u8>> {
    let cached = folder.map(|folder| cache_path(folder, url));
    if let Some(bytes) = cached.as_ref().and_then(|path| fs::read(path).ok()).filter(|bytes| image_diff::sniff(bytes).is_some()) {
        return Some(bytes);
    }
    let bytes = fetch.get(url).ok().flatten().filter(|bytes| image_diff::sniff(bytes).is_some())?;
    if let (Some(folder), Some(path)) = (folder, cached) {
        let _ = fs::create_dir_all(folder);
        if let Ok(mut file) = tempfile::NamedTempFile::new_in(folder) {
            use std::io::Write;
            if file.write_all(&bytes).is_ok() {
                let _ = file.persist(path);
            }
        }
    }
    Some(bytes)
}

pub(crate) fn hosted_authors(repo: &Repository) -> Vec<(String, String)> {
    let addresses = crate::remotes::addresses(repo).unwrap_or_default();
    let mut found = Vec::new();
    for (remote, url) in crate::auth::bindings(repo).unwrap_or_default() {
        if addresses.get(&remote).and_then(crate::remotes::Addresses::single) != Some(url.as_str()) {
            continue;
        }
        let Some((owner, name)) = crate::auth::github_repo(&url) else { continue };
        let Ok(Some(connection)) = crate::auth::connection(repo, &remote) else { continue };
        if let Ok(authors) = connection.client.commit_authors(&connection.token, &owner, &name) {
            found.extend(authors);
        }
    }
    found
}

pub(crate) fn repository_shown(tab: &mut Tab, cx: &mut Context<Tab>) {
    if settings::peek(cx, |settings| settings.pictures) == Pictures::Initials || !cx.has_global::<Photos>() {
        return;
    }
    let Some(repo) = tab.repo.clone() else { return };
    let directory = repo.common_dir().to_path_buf();
    if !cx.global_mut::<Photos>().looked.insert(directory) {
        return;
    }
    Tab::work(
        cx,
        move || hosted_authors(&repo),
        move |_, authors, cx| {
            if authors.is_empty() || settings::peek(cx, |settings| settings.pictures) == Pictures::Initials {
                return;
            }
            let photos = cx.global_mut::<Photos>();
            for (email, url) in authors {
                let key = key_of(&email);
                if matches!(photos.looks.get(&key), Some(Look::Missing)) {
                    photos.looks.remove(&key);
                }
                photos.hosted.insert(key, url);
            }
            cx.refresh_windows();
        },
    );
}

pub(crate) fn pull_authors<'a>(authors: impl Iterator<Item = (&'a str, &'a str)>, cx: &mut App) {
    if !cx.has_global::<Photos>() {
        return;
    }
    let photos = cx.global_mut::<Photos>();
    for (login, url) in authors {
        let login = login.to_ascii_lowercase();
        if photos.logins.get(&login).is_some_and(|known| known == url) {
            continue;
        }
        let key = format!("{login}{NOREPLY}");
        if matches!(photos.looks.get(&key), Some(Look::Missing)) {
            photos.looks.remove(&key);
        }
        photos.logins.insert(login, url.to_owned());
    }
}

pub(crate) fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((input.len() as u64).wrapping_mul(8)).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let (a, b) = (words[index - 15], words[index - 2]);
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[index] = words[index - 16].wrapping_add(s0).wrapping_add(words[index - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let first = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(K[index]).wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut digest = [0u8; 32];
    for (index, value) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture";

    #[test]
    fn the_hash_matches_the_published_sha256_values() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(&sha256(long)), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(hex(&sha256(&[b'a'; 1000])), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn initials_never_give_an_address_and_each_source_is_used_only_when_chosen() {
        let (hosted, logins) = (HashMap::new(), HashMap::new());
        for key in ["583231+octocat@users.noreply.github.com", "cedar@example.invalid", ""] {
            assert_eq!(address(key, Pictures::Initials, &hosted, &logins), None, "{key}");
        }
        let noreply = "583231+octocat@users.noreply.github.com";
        assert_eq!(
            address(noreply, Pictures::GitHub, &hosted, &logins).as_deref(),
            Some("https://avatars.githubusercontent.com/u/583231?s=80&v=4")
        );
        assert_eq!(address("cedar@example.invalid", Pictures::GitHub, &hosted, &logins), None);
        let gravatar = address("cedar@example.invalid", Pictures::Gravatar, &hosted, &logins).unwrap();
        assert_eq!(gravatar, format!("https://gravatar.com/avatar/{}?s=80&d=404", hex(&sha256(b"cedar@example.invalid"))));
        assert_eq!(key_of("  Cedar@Example.INVALID "), "cedar@example.invalid");
        let hosted = HashMap::from([("cedar@example.invalid".to_owned(), "https://avatars.githubusercontent.com/u/7?v=4".to_owned())]);
        assert_eq!(
            address("cedar@example.invalid", Pictures::GitHub, &hosted, &logins).as_deref(),
            Some("https://avatars.githubusercontent.com/u/7?v=4&s=80")
        );
        let logins = HashMap::from([("fern".to_owned(), "https://avatars.githubusercontent.com/u/9?v=4".to_owned())]);
        assert_eq!(
            address("fern@users.noreply.github.com", Pictures::GitHub, &HashMap::new(), &logins).as_deref(),
            Some("https://avatars.githubusercontent.com/u/9?v=4&s=80")
        );
        assert_eq!(address("fern@users.noreply.github.com", Pictures::GitHub, &HashMap::new(), &HashMap::new()), None);
    }

    #[test]
    fn only_github_avatar_addresses_are_taken_from_github_answers() {
        assert_eq!(
            github_avatar("https://avatars.githubusercontent.com/u/583231?v=4").as_deref(),
            Some("https://avatars.githubusercontent.com/u/583231?v=4")
        );
        for refused in [
            "http://avatars.githubusercontent.com/u/1",
            "https://avatars.githubusercontent.com.example.invalid/u/1",
            "https://user:pass@avatars.githubusercontent.com/u/1",
            "https://avatars.githubusercontent.com:8443/u/1",
            "https://example.invalid/u/1",
            "not a url",
        ] {
            assert_eq!(github_avatar(refused), None, "{refused}");
        }
        assert_eq!(noreply_id("12+fern@users.noreply.github.com"), Some(12));
        assert_eq!(noreply_id("x+fern@users.noreply.github.com"), None);
        assert_eq!(noreply_id("fern@example.invalid"), None);
    }

    #[test]
    fn a_picture_is_fetched_once_and_then_read_from_the_folder() {
        let folder = tempfile::tempdir().unwrap();
        let recorder = Recorder::default();
        let url = "https://avatars.githubusercontent.com/u/1?s=80&v=4";
        recorder.answer(url, PNG.to_vec());
        assert_eq!(load(&recorder, Some(folder.path()), url).as_deref(), Some(PNG));
        assert!(cache_path(folder.path(), url).is_file());
        assert_eq!(load(&recorder, Some(folder.path()), url).as_deref(), Some(PNG));
        assert_eq!(recorder.asked(), vec![url.to_owned()]);
        let missing = "https://gravatar.com/avatar/00?s=80&d=404";
        assert_eq!(load(&recorder, Some(folder.path()), missing), None);
        assert!(!cache_path(folder.path(), missing).exists());
        recorder.answer("https://gravatar.com/avatar/11", b"<html>not a picture</html>".to_vec());
        assert_eq!(load(&recorder, Some(folder.path()), "https://gravatar.com/avatar/11"), None);
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
    }

    #[test]
    fn the_choice_is_saved_by_name_and_starts_on_initials() {
        assert_eq!(serde_json::to_value(Pictures::GitHub).unwrap(), "github");
        assert_eq!(serde_json::to_value(Pictures::Gravatar).unwrap(), "gravatar");
        assert_eq!(settings::Settings::default().pictures, Pictures::Initials);
        let read: settings::Settings = serde_json::from_str(r#"{"pictures": "flickr"}"#).unwrap();
        assert_eq!(read.pictures, Pictures::Initials);
        for pictures in Pictures::ALL {
            assert!(!pictures.help().is_empty());
        }
        assert!(Pictures::Gravatar.help().contains("SHA-256 hash of their email address"));
        assert!(Pictures::GitHub.help().contains("GitHub learns"));
    }
}
