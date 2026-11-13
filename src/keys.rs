//! The client key store.
//!
//! `eggpass` issues the keys its clients use: `eggpass key add` mints one,
//! shows it exactly once, and stores a salted SHA-256 hash of it — never the
//! key itself. The store therefore cannot be read to impersonate a client,
//! and it lives in a `0600` file inside a `0700` directory besides.
//!
//! The store has exactly one writer, the `eggpass key` command. The server
//! only reads it, and re-reads it whenever the file changes underneath it, so
//! adding or revoking a key takes effect on the next request without a
//! restart.
//!
//! Keys are numbered. The number never changes and is never reused, so
//! `eggpass key revoke 2` always means the same key, even after other keys
//! have been revoked.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::restrict_permissions;
use crate::timeutil;

/// The marker every generated key starts with.
pub const KEY_PREFIX: &str = "sk-eggpass-";

/// How many characters of a key `eggpass key list` shows.
///
/// Enough to tell two keys apart in a list, not enough to use one.
const VISIBLE_CHARS: usize = 20;

/// Number of random bytes behind a key.
const KEY_BYTES: usize = 24;

/// Number of random bytes in a salt.
const SALT_BYTES: usize = 16;

/// Errors raised while reading or writing the key store.
#[derive(Debug)]
pub enum KeyError {
    /// The file could not be read or written.
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The file exists but does not hold keys we understand.
    Invalid {
        /// The file involved.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            KeyError::Invalid { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for KeyError {}

/// One issued key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    /// The number the key is revoked by.
    pub id: u32,
    /// What the key is for, when the user said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The visible part of the key.
    pub prefix: String,
    /// The salt the hash was made with.
    pub salt: String,
    /// The salted hash of the key.
    pub hash: String,
    /// When the key was created, in seconds since the unix epoch.
    pub created_at: i64,
}

impl KeyEntry {
    /// The label, or a placeholder when there is none.
    pub fn label_or_placeholder(&self) -> &str {
        self.label.as_deref().unwrap_or("(unlabelled)")
    }
}

/// The shape of the file on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
struct KeyFile {
    /// The number the next key gets.
    #[serde(default)]
    next_id: u32,
    /// The keys themselves.
    #[serde(default)]
    keys: Vec<KeyEntry>,
}

impl KeyFile {
    /// The number the next key should get.
    ///
    /// Never zero, and never lower than one past the highest number already
    /// used, so a number is never handed out twice.
    fn next_id(&self) -> u32 {
        let highest = self.keys.iter().map(|key| key.id).max().unwrap_or(0);
        self.next_id.max(highest + 1).max(1)
    }
}

/// The fingerprint of the file, used to notice that it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    /// The length of the file.
    len: u64,
    /// When it was last modified.
    modified: Option<SystemTime>,
    /// Which file it is.
    ///
    /// The store is written by renaming a temporary file over it, so the
    /// inode changes on every write even when the length and the timestamp do
    /// not.
    #[cfg(unix)]
    inode: Option<u64>,
}

/// The store itself.
#[derive(Debug)]
pub struct KeyStore {
    path: PathBuf,
    state: Mutex<State>,
}

/// The part of the store that is guarded by the lock.
#[derive(Debug)]
struct State {
    file: KeyFile,
    stamp: Option<Stamp>,
}

impl KeyStore {
    /// Open the store at a path.
    ///
    /// A file that does not exist is an empty store, not an error: the first
    /// `eggpass key add` creates it.
    pub fn open(path: impl Into<PathBuf>) -> Result<KeyStore, KeyError> {
        let path = path.into();
        let (file, stamp) = read_file(&path)?;
        Ok(KeyStore {
            path,
            state: Mutex::new(State { file, stamp }),
        })
    }

    /// Every key, oldest first.
    pub fn entries(&self) -> Vec<KeyEntry> {
        self.with_state(|state| state.file.keys.clone())
    }

    /// Whether any key has been issued.
    pub fn is_empty(&self) -> bool {
        self.with_state(|state| state.file.keys.is_empty())
    }

    /// How many keys have been issued.
    pub fn len(&self) -> usize {
        self.with_state(|state| state.file.keys.len())
    }

    /// Mint a key.
    ///
    /// Returns the number it was given and the key itself. The key is shown
    /// once and never stored: only its hash goes to disk.
    pub fn add(&self, label: Option<String>) -> Result<(u32, String), KeyError> {
        let mut state = self.lock();
        self.refresh(&mut state);

        let id = state.file.next_id();
        let plain = generate_key();
        let salt = hex(random_bytes(SALT_BYTES));

        state.file.keys.push(KeyEntry {
            id,
            label: normalise_label(label),
            prefix: visible_prefix(&plain),
            salt: salt.clone(),
            hash: hash_key(&plain, &salt),
            created_at: timeutil::now_unix(),
        });
        state.file.next_id = id + 1;

        self.write(&mut state)?;
        Ok((id, plain))
    }

    /// Remove the key with this number.
    pub fn revoke(&self, id: u32) -> Result<KeyEntry, KeyError> {
        let mut state = self.lock();
        self.refresh(&mut state);

        let Some(index) = state.file.keys.iter().position(|key| key.id == id) else {
            return Err(KeyError::Invalid {
                path: self.path.clone(),
                message: format!("there is no key #{id}"),
            });
        };

        let entry = state.file.keys.remove(index);
        self.write(&mut state)?;
        Ok(entry)
    }

    /// Remove every key. Returns how many were removed.
    pub fn revoke_all(&self) -> Result<usize, KeyError> {
        let mut state = self.lock();
        self.refresh(&mut state);

        let removed = state.file.keys.len();
        state.file.keys.clear();
        self.write(&mut state)?;
        Ok(removed)
    }

    /// Whether this is one of our keys.
    ///
    /// Every key is checked, and the answer is not decided early, so the time
    /// this takes does not depend on which key matched.
    pub fn verify(&self, presented: &str) -> bool {
        self.with_state(|state| {
            let mut matched = false;
            for entry in &state.file.keys {
                let candidate = hash_key(presented, &entry.salt);
                matched |= constant_time_eq(candidate.as_bytes(), entry.hash.as_bytes());
            }
            matched
        })
    }

    /// Lock the store, refresh it, and hand the state to a closure.
    fn with_state<R>(&self, action: impl FnOnce(&State) -> R) -> R {
        let mut state = self.lock();
        self.refresh(&mut state);
        action(&state)
    }

    /// Take the lock, recovering from a poisoned lock.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Re-read the file when it changed on disk.
    fn refresh(&self, state: &mut State) {
        let current = stamp_of(&self.path);
        if current == state.stamp {
            return;
        }

        match read_file(&self.path) {
            Ok((file, stamp)) => {
                state.file = file;
                state.stamp = stamp;
            }
            Err(_) => {
                // The file is unreadable or broken. Keep serving what we
                // already have, and remember the stamp so a broken file is
                // not re-read on every request.
                state.stamp = current;
            }
        }
    }

    /// Write the store out.
    fn write(&self, state: &mut State) -> Result<(), KeyError> {
        let text = serde_json::to_string_pretty(&state.file).map_err(|err| KeyError::Invalid {
            path: self.path.clone(),
            message: err.to_string(),
        })?;

        write_private(&self.path, text.as_bytes())?;
        state.stamp = stamp_of(&self.path);
        Ok(())
    }
}

/// Read the store from disk.
///
/// A missing file is an empty store. A broken file is an error: silently
/// ignoring it would let a typo in the file hand out access.
fn read_file(path: &Path) -> Result<(KeyFile, Option<Stamp>), KeyError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((KeyFile::default(), None));
        }
        Err(source) => {
            return Err(KeyError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let file: KeyFile = serde_json::from_str(&text).map_err(|err| KeyError::Invalid {
        path: path.to_path_buf(),
        message: format!("could not parse the key store: {err}"),
    })?;

    Ok((file, stamp_of(path)))
}

/// The fingerprint of a file.
fn stamp_of(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.ino())
    };
    #[cfg(not(unix))]
    let inode = None;

    Some(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        inode,
    })
}

/// Write bytes to a file only the owner can read, atomically.
///
/// The bytes land in a temporary file first, so a crash mid-write cannot
/// leave a half-written store behind.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), KeyError> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|source| KeyError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    restrict_permissions(parent, 0o700).map_err(|source| KeyError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let temporary = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(source) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(KeyError::Io {
            path: path.to_path_buf(),
            source,
        });
    }

    restrict_permissions(&temporary, 0o600).map_err(|source| KeyError::Io {
        path: temporary.clone(),
        source,
    })?;

    if let Err(source) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(KeyError::Io {
            path: path.to_path_buf(),
            source,
        });
    }

    Ok(())
}

/// A fresh key.
fn generate_key() -> String {
    format!(
        "{KEY_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(random_bytes(KEY_BYTES))
    )
}

/// The part of a key that is safe to show.
fn visible_prefix(key: &str) -> String {
    key.chars().take(VISIBLE_CHARS).collect()
}

/// Fill a buffer with cryptographically secure random bytes.
fn random_bytes(length: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; length];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut buffer);
    buffer
}

/// The salted hash of a key.
fn hash_key(key: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(unhex(salt));
    hasher.update(key.as_bytes());
    hex(hasher.finalize())
}

/// A label that is not empty.
fn normalise_label(label: Option<String>) -> Option<String> {
    let label = label?.trim().to_string();
    if label.is_empty() { None } else { Some(label) }
}

/// Compare two secrets without leaking their length through timing.
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Encode bytes as lower-case hex.
fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// Decode lower-case hex.
fn unhex(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
        let low = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((high << 4) | low);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store(dir: &Path) -> KeyStore {
        KeyStore::open(dir.join("keys.json")).unwrap()
    }

    #[test]
    fn a_minted_key_is_returned_once_and_never_stored_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let (id, plain) = store.add(None).unwrap();
        assert_eq!(id, 1);
        assert!(plain.starts_with(KEY_PREFIX), "{plain}");

        let text = std::fs::read_to_string(dir.path().join("keys.json")).unwrap();
        assert!(
            !text.contains(&plain),
            "the key was written down in the clear"
        );
        assert!(text.contains("\"hash\""), "{text}");
    }

    #[test]
    fn a_minted_key_verifies_and_anything_else_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, plain) = store.add(None).unwrap();

        assert!(store.verify(&plain));
        assert!(!store.verify("sk-eggpass-wrong"));
        assert!(!store.verify(""));
        assert!(!store.verify(&plain[..plain.len() - 1]));
    }

    #[test]
    fn the_file_is_not_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        store.add(None).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("keys.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    #[test]
    fn keys_are_numbered_and_numbers_are_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let (first, _) = store.add(None).unwrap();
        let (second, _) = store.add(None).unwrap();
        assert_eq!((first, second), (1, 2));

        store.revoke(1).unwrap();
        let (third, _) = store.add(None).unwrap();
        assert_eq!(third, 3, "a number must not be handed out twice");
    }

    #[test]
    fn revoking_removes_the_key_and_leaves_the_others_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, first) = store.add(None).unwrap();
        let (_, second) = store.add(None).unwrap();

        let removed = store.revoke(1).unwrap();
        assert_eq!(removed.id, 1);
        assert!(!store.verify(&first));
        assert!(store.verify(&second));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn revoking_a_number_that_is_not_there_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        store.add(None).unwrap();

        let err = store.revoke(7).unwrap_err();
        assert!(err.to_string().contains("no key #7"), "{err}");
    }

    #[test]
    fn every_key_can_be_revoked_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, first) = store.add(None).unwrap();
        let (_, second) = store.add(None).unwrap();

        assert_eq!(store.revoke_all().unwrap(), 2);
        assert!(store.is_empty());
        assert!(!store.verify(&first));
        assert!(!store.verify(&second));
    }

    #[test]
    fn a_key_added_behind_our_back_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let other = open_store(dir.path());
        let (_, plain) = other.add(None).unwrap();

        assert!(
            store.verify(&plain),
            "the store did not notice the new file"
        );
    }

    #[test]
    fn a_key_revoked_behind_our_back_stops_working() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, plain) = store.add(None).unwrap();
        assert!(store.verify(&plain));

        let other = open_store(dir.path());
        other.revoke(1).unwrap();

        assert!(!store.verify(&plain), "the store did not notice the change");
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn a_broken_file_is_reported_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(&path, "{not json").unwrap();

        let err = KeyStore::open(&path).unwrap_err();
        assert!(err.to_string().contains("could not parse"), "{err}");
    }

    #[test]
    fn labels_are_kept_and_empty_ones_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());

        let (id, _) = store.add(Some("laptop".to_string())).unwrap();
        let (other, _) = store.add(Some("   ".to_string())).unwrap();

        let entries = store.entries();
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].label.as_deref(), Some("laptop"));
        assert_eq!(entries[1].label, None);
        assert_eq!(entries[1].label_or_placeholder(), "(unlabelled)");
        assert_eq!(other, 2);
    }

    #[test]
    fn the_visible_prefix_is_a_prefix_and_not_the_whole_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, plain) = store.add(None).unwrap();

        let entry = &store.entries()[0];
        assert!(plain.starts_with(&entry.prefix), "{}", entry.prefix);
        assert!(entry.prefix.len() < plain.len());
    }

    #[test]
    fn keys_are_long_enough_and_random_enough() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let (_, first) = store.add(None).unwrap();
        let (_, second) = store.add(None).unwrap();

        assert_eq!(first.len(), KEY_PREFIX.len() + 32, "{first}");
        assert_ne!(first, second);
    }
}
