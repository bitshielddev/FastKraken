use super::*;
use std::sync::{Mutex, OnceLock};

pub(super) struct Session {
    pub(super) directory: PathBuf,
    pub(super) client: Client,
    pub(super) tokens: Mutex<BTreeMap<(String, u64), Tokens>>,
    pub(super) git_destination: PathBuf,
}

pub(super) static SESSION: OnceLock<Session> = OnceLock::new();

pub(crate) fn begin_replay(directory: PathBuf, client: Client, git_destination: PathBuf) {
    assert!(
        SESSION
            .set(Session {
                directory,
                client,
                git_destination,
                tokens: Mutex::default()
            })
            .is_ok()
    );
}
