//! Persist a reservation before running a commit. A reservation left by an
//! interrupted process is evidence of an uncertain outcome, never an instruction
//! to execute again.
use super::{Error, Result, grants::digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{ErrorKind, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 128;
// Keep every outcome longer than the lifetime of the review that authorized it.
const RETAIN_MS: u64 = 60 * 60 * 1000;
const MAX_BYTES: u64 = 2 * 1024 * 1024;

fn unknown() -> Error {
    Error::new(
        "outcome_unknown",
        "Check the local branch history for the proposed commit before starting a fresh review.",
    )
}

fn busy() -> Error {
    Error::new(
        "busy",
        "Another local commit is in progress. Retry with the same request ID and payload.",
    )
}

fn mismatch() -> Error {
    Error::new(
        "request_id_conflict",
        "This request ID already belongs to a different staged commit. Use its original payload or start a new request.",
    )
}

#[derive(Serialize, Deserialize)]
enum State {
    Pending,
    Retryable,
    Done(Result<Value>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    key: String,
    intent: String,
    time: u64,
    state: State,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    records: Vec<Record>,
    checksum: String,
}

fn checksum(records: &[Record]) -> String {
    digest(&serde_json::to_vec(records).expect("the ledger contains only JSON values"))
}

fn private_file(file: &File) -> Result<u64> {
    let meta = file.metadata().map_err(|_| unknown())?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(unknown());
    }
    Ok(meta.len())
}

fn read(dir: &Path, name: &str, created: bool) -> Result<Vec<Record>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(name))
    {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound && created => return Ok(Vec::new()),
        Err(_) => return Err(unknown()),
    };
    if private_file(&file)? > MAX_BYTES {
        return Err(unknown());
    }
    let ledger: Ledger = serde_json::from_reader(file.take(MAX_BYTES + 1)).map_err(|_| unknown())?;
    let unique: HashSet<_> = ledger.records.iter().map(|r| &r.key).collect();
    if ledger.records.len() > LIMIT
        || unique.len() != ledger.records.len()
        || checksum(&ledger.records) != ledger.checksum
    {
        return Err(unknown());
    }
    Ok(ledger.records)
}

fn save(dir: &Path, name: &str, records: Vec<Record>) -> Result<Vec<Record>> {
    let ledger = Ledger {
        checksum: checksum(&records),
        records,
    };
    crate::storage::write(dir, name, "local commit request ledger", &ledger).map_err(|_| unknown())?;
    // Persist the rename as well as the new file contents before publication.
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| unknown())?;
    Ok(ledger.records)
}

struct Flight {
    intent: String,
    outcome: Mutex<Option<Result<Value>>>,
    completed: Condvar,
}

impl Flight {
    fn await_result(&self) -> Result<Value> {
        let outcome = self.outcome.lock().unwrap();
        let (outcome, _) = self
            .completed
            .wait_timeout_while(outcome, Duration::from_secs(25), |v| v.is_none())
            .unwrap();
        outcome.clone().unwrap_or_else(|| Err(busy()))
    }
}

pub(super) struct Requests {
    flights: Mutex<HashMap<(String, String), Arc<Flight>>>,
    started: Instant,
    epoch_ms: u64,
}

impl Default for Requests {
    fn default() -> Self {
        Self {
            flights: Mutex::default(),
            started: Instant::now(),
            epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

impl Requests {
    fn now(&self) -> u64 {
        self.epoch_ms
            .saturating_add(self.started.elapsed().as_millis() as u64)
    }

    pub fn run(
        &self,
        dir: &Path,
        grant: &str,
        request_id: &str,
        review_id: &str,
        message: &str,
        execute: impl FnOnce() -> Result<Value>,
    ) -> Result<Value> {
        let key = (digest(grant.as_bytes()), digest(request_id.as_bytes()));
        // JSON tuple encoding preserves boundaries even with arbitrary message text.
        let intent = digest(&serde_json::to_vec(&(review_id, message)).unwrap());
        let flight = {
            let mut flights = self.flights.lock().unwrap();
            if let Some(active) = flights.get(&key).cloned() {
                drop(flights);
                if active.intent != intent {
                    return Err(mismatch());
                }
                return active.await_result();
            }
            // The listener already caps live work. This also bounds memory if a
            // broken filesystem cannot persist final outcomes on many grants.
            if flights.len() >= 1024 {
                return Err(unknown());
            }
            let active = Arc::new(Flight {
                intent: intent.clone(),
                outcome: Mutex::default(),
                completed: Condvar::new(),
            });
            flights.insert(key.clone(), active.clone());
            active
        };
        let mut keep_in_memory = false;
        let result = self.record(dir, &key, intent, &mut keep_in_memory, execute);
        *flight.outcome.lock().unwrap() = Some(result.clone());
        flight.completed.notify_all();
        if !keep_in_memory {
            self.flights.lock().unwrap().remove(&key);
        }
        result
    }

    fn record(
        &self,
        dir: &Path,
        (grant, key): &(String, String),
        intent: String,
        keep: &mut bool,
        execute: impl FnOnce() -> Result<Value>,
    ) -> Result<Value> {
        crate::agent_transport::validate_directory(dir).map_err(|_| unknown())?;
        let name = format!("requests-{grant}.json");
        let guard_path = dir.join(format!("requests-{grant}.lock"));
        let mut open = OpenOptions::new();
        open.read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let (lock, created) = match open.create_new(true).open(&guard_path) {
            Ok(f) => (f, true),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => (
                open.create_new(false).open(guard_path).map_err(|_| unknown())?,
                false,
            ),
            Err(_) => return Err(unknown()),
        };
        private_file(&lock)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(busy()),
            Err(std::fs::TryLockError::Error(_)) => return Err(unknown()),
        }
        // Creation of the private lock marks this grant as initialized. If a
        // previously used journal disappears, do not rerun any unseen intent.
        let mut records = read(dir, &name, created)?;
        let now = self.now();
        let position = if let Some(i) = records.iter().position(|r| r.key == *key) {
            let previous = &records[i];
            if previous.intent != intent {
                return Err(mismatch());
            }
            match &previous.state {
                State::Done(result) => return result.clone(),
                State::Pending => return Err(unknown()),
                State::Retryable => i,
            }
        } else {
            if records.len() == LIMIT {
                let expired = records.iter().position(|r| {
                    !matches!(r.state, State::Pending) && now.saturating_sub(r.time) >= RETAIN_MS
                });
                let Some(i) = expired else {
                    return Err(Error::new(
                        "busy",
                        "This grant has reached its retained request limit. Reconcile any uncertain commits and retry after an hour or create a new grant.",
                    ));
                };
                records.remove(i);
            }
            records.push(Record {
                key: key.clone(),
                intent,
                time: now,
                state: State::Pending,
            });
            records.len() - 1
        };
        records[position].state = State::Pending;
        let mut records = save(dir, &name, records)?;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(execute)).unwrap_or_else(|_| Err(unknown()));
        records[position].time = self.now();
        records[position].state = if result.as_ref().is_err_and(|e| e.code == "busy") {
            State::Retryable
        } else {
            State::Done(result.clone())
        };
        // Once published, the original outcome is authoritative even if the
        // disk fails. Existing callers and same-process retries can recover it;
        // after a restart the durable pending marker instructs reconciliation.
        if save(dir, &name, records).is_err() {
            *keep = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        sync::mpsc,
        thread,
    };

    fn private_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }

    fn name() -> String {
        format!("requests-{}.json", digest(b"client"))
    }

    fn success() -> Result<Value> {
        Ok(json!({"commit_id":"abc","tree_id":"123","parent":"parent","branch":"fixture"}))
    }

    #[test]
    fn simultaneous_duplicate_and_lost_reply_recover_one_outcome() {
        let dir = private_dir();
        let requests = Requests::default();
        let (started, running) = mpsc::channel();
        let (release, barrier) = mpsc::channel();
        thread::scope(|scope| {
            let original = scope.spawn(|| {
                requests.run(
                    dir.path(),
                    "client",
                    "id",
                    "review",
                    "fixture message",
                    move || {
                        started.send(()).unwrap();
                        barrier.recv_timeout(Duration::from_secs(3)).unwrap();
                        success()
                    },
                )
            });
            running.recv_timeout(Duration::from_secs(3)).unwrap();
            let duplicate = scope.spawn(|| {
                requests.run(dir.path(), "client", "id", "review", "fixture message", || {
                    panic!("ran twice")
                })
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while !requests
                .flights
                .lock()
                .unwrap()
                .values()
                .any(|f| Arc::strong_count(f) >= 3)
            {
                assert!(
                    Instant::now() < deadline,
                    "duplicate never joined the first transaction"
                );
                thread::yield_now();
            }
            let wrong = requests.run(
                dir.path(),
                "client",
                "id",
                "another-review",
                "fixture message",
                || panic!("reused ID"),
            );
            assert_eq!(wrong.unwrap_err().code, "request_id_conflict");
            release.send(()).unwrap();
            assert_eq!(original.join().unwrap(), success());
            assert_eq!(duplicate.join().unwrap(), success());
        });
        let resumed = Requests::default().run(dir.path(), "client", "id", "review", "fixture message", || {
            panic!("replayed after restart")
        });
        assert_eq!(resumed, success());
        let bytes = fs::read_to_string(dir.path().join(name())).unwrap();
        assert!(!bytes.contains("fixture message") && !bytes.contains("review"));
        assert_eq!(
            requests.run(
                dir.path(),
                "another-client",
                "id",
                "review",
                "fixture message",
                || Ok(json!({"other":true}))
            ),
            Ok(json!({"other":true}))
        );
    }

    #[test]
    fn busy_is_retryable_and_the_request_id_stays_bound() {
        let dir = private_dir();
        let requests = Requests::default();
        assert_eq!(
            requests
                .run(dir.path(), "client", "id", "review", "message", || Err(busy()))
                .unwrap_err()
                .code,
            "busy"
        );
        assert_eq!(
            requests
                .run(dir.path(), "client", "id", "review", "changed", success)
                .unwrap_err()
                .code,
            "request_id_conflict"
        );
        assert_eq!(
            requests.run(dir.path(), "client", "id", "review", "message", success),
            success()
        );
        let conflict = Requests::default().run(dir.path(), "client", "id", "different", "message", || {
            panic!("reused")
        });
        assert_eq!(conflict.unwrap_err().code, "request_id_conflict");
    }

    #[test]
    fn lost_final_journal_write_recovers_in_memory_and_requests_reconciliation_after_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let dir = fixture.path().join("access");
        let displaced = fixture.path().join("displaced");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let requests = Requests::default();
        let answer = requests.run(&dir, "client", "id", "review", "message", || {
            // Prevent atomic replacement at the original path, just after
            // the durable pending reservation and simulated publication.
            fs::rename(&dir, &displaced).unwrap();
            File::create(&dir).unwrap();
            success()
        });
        assert_eq!(answer, success());
        assert_eq!(
            requests.run(&dir, "client", "id", "review", "message", || panic!("reran")),
            success()
        );
        fs::remove_file(&dir).unwrap();
        fs::rename(&displaced, &dir).unwrap();
        let recovered = Requests::default().run(&dir, "client", "id", "review", "message", || {
            panic!("reran abandoned transaction")
        });
        assert_eq!(recovered.unwrap_err().code, "outcome_unknown");
    }

    #[test]
    fn damaged_missing_and_full_ledgers_fail_closed() {
        let dir = private_dir();
        let requests = Requests::default();
        assert_eq!(
            requests.run(dir.path(), "client", "id", "review", "message", success),
            success()
        );
        let journal = dir.path().join(name());
        let bytes = fs::read_to_string(&journal).unwrap();
        fs::write(&journal, bytes.replace("abc", "bad")).unwrap();
        assert_eq!(
            requests
                .run(dir.path(), "client", "id", "review", "message", || panic!(
                    "untrusted"
                ))
                .unwrap_err()
                .code,
            "outcome_unknown"
        );
        fs::remove_file(&journal).unwrap();
        assert_eq!(
            requests
                .run(dir.path(), "client", "id", "review", "message", || panic!(
                    "missing"
                ))
                .unwrap_err()
                .code,
            "outcome_unknown"
        );
        let mut records: Vec<_> = (0..LIMIT)
            .map(|i| Record {
                key: digest(&i.to_be_bytes()),
                intent: digest(b"message"),
                time: 0,
                state: State::Pending,
            })
            .collect();
        save(dir.path(), &name(), records).unwrap();
        assert_eq!(
            requests
                .run(dir.path(), "client", "another", "review", "message", || panic!(
                    "discarded pending"
                ))
                .unwrap_err()
                .code,
            "busy"
        );
        records = read(dir.path(), &name(), false).unwrap();
        records[0].state = State::Done(success());
        save(dir.path(), &name(), records).unwrap();
        assert_eq!(
            requests.run(dir.path(), "client", "another", "review", "message", success),
            success()
        );
        assert_eq!(read(dir.path(), &name(), false).unwrap().len(), LIMIT);
    }
}
