use crate::github::Tokens;

const GITHUB: &str = "GitHub sign-in";

fn service(client_id: &str) -> String {
    format!("dev.fastkraken.gitclient.github.{client_id}")
}

pub(super) fn get(client_id: &str, account: u64) -> Result<Option<Tokens>, String> {
    #[cfg(feature = "publish-replay")]
    if let Some(session) = super::replay::SESSION.get() {
        return Ok(session
            .tokens
            .lock()
            .unwrap()
            .get(&(client_id.into(), account))
            .cloned());
    }
    let saved = if secret::kept_apart() {
        secret::get(&service(client_id), &account.to_string())?
    } else {
        platform::read(&service(client_id), &account.to_string(), GITHUB)?
    };
    saved
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| {
                "Could not read your stored GitHub sign-in. Reconnect in Remotes.".into()
            })
        })
        .transpose()
}

pub(super) fn put(client_id: &str, account: u64, tokens: &Tokens) -> Result<(), String> {
    #[cfg(feature = "publish-replay")]
    if let Some(session) = super::replay::SESSION.get() {
        session
            .tokens
            .lock()
            .unwrap()
            .insert((client_id.into(), account), tokens.clone());
        return Ok(());
    }
    let bytes = serde_json::to_vec(tokens).map_err(|_| "Could not encode your GitHub sign-in.")?;
    if secret::kept_apart() {
        return secret::put(&service(client_id), &account.to_string(), &bytes);
    }
    platform::write(&service(client_id), &account.to_string(), &bytes, GITHUB)
}

pub(super) fn remove(client_id: &str, account: u64) -> Result<(), String> {
    #[cfg(feature = "publish-replay")]
    if let Some(session) = super::replay::SESSION.get() {
        session
            .tokens
            .lock()
            .unwrap()
            .remove(&(client_id.into(), account));
        return Ok(());
    }
    if secret::kept_apart() {
        return secret::remove(&service(client_id), &account.to_string());
    }
    platform::delete(&service(client_id), &account.to_string(), GITHUB)
}

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const ITEM_NOT_FOUND: i32 = -25300;

    fn keychain_error(action: &str, subject: &str, error: security_framework::base::Error) -> String {
        if error.code() == -25308 {
            return if subject == super::GITHUB {
                "Keychain needs your approval. Unlock your login keychain in macOS and reconnect in Remotes.".into()
            } else {
                "Keychain needs your approval. Unlock your login keychain in macOS and try again.".into()
            };
        }
        format!("{action} {subject} in Keychain: {error}")
    }

    pub(super) fn read(service: &str, account: &str, subject: &str) -> Result<Option<Vec<u8>>, String> {
        match get_generic_password(service, account) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.code() == ITEM_NOT_FOUND => Ok(None),
            Err(err) => Err(keychain_error("Read", subject, err)),
        }
    }

    pub(super) fn write(service: &str, account: &str, bytes: &[u8], subject: &str) -> Result<(), String> {
        set_generic_password(service, account, bytes).map_err(|err| keychain_error("Save", subject, err))
    }

    pub(super) fn delete(service: &str, account: &str, subject: &str) -> Result<(), String> {
        match delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == ITEM_NOT_FOUND => Ok(()),
            Err(err) => Err(keychain_error("Remove", subject, err)),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::{
        io::Write,
        process::{Command, Output, Stdio},
    };

    fn invoke(args: &[&str], input: Option<&[u8]>) -> Result<Output, String> {
        let mut child = Command::new("secret-tool")
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Start Secret Service (libsecret's secret-tool): {e}"))?;
        if let Some(bytes) = input {
            let result = child.stdin.take().unwrap().write_all(bytes);
            if result.is_err() {
                let _ = child.wait();
                return Err("Cannot write to Secret Service.".into());
            }
        }
        child
            .wait_with_output()
            .map_err(|e| format!("Wait for Secret Service: {e}"))
    }

    fn missing(output: &Output) -> bool {
        output.status.code() == Some(1) && output.stderr.is_empty()
    }

    pub(super) fn read(service: &str, account: &str, subject: &str) -> Result<Option<Vec<u8>>, String> {
        let output = invoke(&["lookup", "service", service, "account", account], None)?;
        if output.status.success() {
            Ok(Some(output.stdout))
        } else if missing(&output) {
            Ok(None)
        } else {
            Err(format!("Could not read {subject} from Secret Service. Unlock your keyring and try again."))
        }
    }

    pub(super) fn write(service: &str, account: &str, bytes: &[u8], subject: &str) -> Result<(), String> {
        let label = format!("--label=FastKraken {subject}");
        let output = invoke(
            &[
                "store",
                &label,
                "service",
                service,
                "account",
                account,
            ],
            Some(bytes),
        )?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("Could not save {subject} to Secret Service."))
    }

    pub(super) fn delete(service: &str, account: &str, subject: &str) -> Result<(), String> {
        let output = invoke(&["clear", "service", service, "account", account], None)?;
        (output.status.success() || missing(&output))
            .then_some(())
            .ok_or_else(|| format!("Could not remove {subject} from Secret Service."))
    }
}

pub(crate) mod secret {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::Mutex,
    };

    const SUBJECT: &str = "saved password";

    enum Store {
        Keychain,
        File(PathBuf),
        Memory,
    }

    fn store() -> Result<Store, String> {
        if cfg!(test) {
            return Ok(Store::Memory);
        }
        if crate::acceptance::replaying() {
            return crate::acceptance::sandbox()
                .map(|sandbox| Store::File(sandbox.join("keychain.json")))
                .ok_or_else(|| "The acceptance replay keeps saved passwords inside its sandbox.".to_owned());
        }
        Ok(Store::Keychain)
    }

    pub(crate) fn kept_apart() -> bool {
        !matches!(store(), Ok(Store::Keychain))
    }

    static MEMORY: Mutex<BTreeMap<String, Vec<u8>>> = Mutex::new(BTreeMap::new());

    fn key(service: &str, account: &str) -> String {
        format!("{service}\0{account}")
    }

    fn change(file: &Path, change: impl FnOnce(&mut BTreeMap<String, Vec<u8>>)) -> Result<(), String> {
        let dir = file.parent().ok_or("The test keychain has no folder.")?;
        let name = file.file_name().and_then(|name| name.to_str()).ok_or("The test keychain has no name.")?;
        crate::storage::update(dir, name, &format!("{name}.lock"), "test keychain", |map: &mut BTreeMap<String, Vec<u8>>| {
            change(map);
            Ok(())
        })
    }

    pub(crate) fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>, String> {
        match store()? {
            Store::Keychain => super::platform::read(service, account, SUBJECT),
            Store::File(file) => {
                let dir = file.parent().ok_or("The test keychain has no folder.")?;
                let name = file.file_name().and_then(|name| name.to_str()).ok_or("The test keychain has no name.")?;
                let map: BTreeMap<String, Vec<u8>> = crate::storage::load(dir, name, "test keychain")?;
                Ok(map.get(&key(service, account)).cloned())
            }
            Store::Memory => Ok(MEMORY.lock().unwrap_or_else(|e| e.into_inner()).get(&key(service, account)).cloned()),
        }
    }

    pub(crate) fn put(service: &str, account: &str, secret: &[u8]) -> Result<(), String> {
        match store()? {
            Store::Keychain => super::platform::write(service, account, secret, SUBJECT),
            Store::File(file) => change(&file, |map| {
                map.insert(key(service, account), secret.to_owned());
            }),
            Store::Memory => {
                MEMORY.lock().unwrap_or_else(|e| e.into_inner()).insert(key(service, account), secret.to_owned());
                Ok(())
            }
        }
    }

    pub(crate) fn remove(service: &str, account: &str) -> Result<(), String> {
        match store()? {
            Store::Keychain => super::platform::delete(service, account, SUBJECT),
            Store::File(file) => change(&file, |map| {
                map.remove(&key(service, account));
            }),
            Store::Memory => {
                MEMORY.lock().unwrap_or_else(|e| e.into_inner()).remove(&key(service, account));
                Ok(())
            }
        }
    }
}
