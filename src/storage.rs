//! Encrypted clip store (SQLite).
//!
//! On disk per row: keyed fingerprint, nonce, AES-GCM ciphertext, timestamps, flags.
//! Plaintext content never touches disk. Timestamps/flags stay readable on purpose:
//! retention cleanup must work without decrypting anything.
//!
//! Hardening:
//!   - PRAGMA secure_delete=ON  -> deleted rows are overwritten, not just unlinked
//!   - journal_mode=DELETE      -> no long-lived WAL file holding old pages
//!   - Unix: DB dir 0700, DB file 0600

use crate::crypto::{Cipher, CryptoError};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

pub const DEFAULT_MAX_AGE_SECS: i64 = 90 * 24 * 3600; // 90 days
pub const DEFAULT_MAX_ITEMS: i64 = 10_000;

#[derive(Debug)]
pub enum StoreError {
    Db(rusqlite::Error),
    Crypto(CryptoError),
    Io(std::io::Error),
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "database: {e}"),
            Self::Crypto(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError { fn from(e: rusqlite::Error) -> Self { Self::Db(e) } }
impl From<CryptoError> for StoreError { fn from(e: CryptoError) -> Self { Self::Crypto(e) } }
impl From<std::io::Error> for StoreError { fn from(e: std::io::Error) -> Self { Self::Io(e) } }

/// Decrypted clip, held in memory only.
#[allow(dead_code)] // consumed by the UI (step 4)
pub struct Item {
    pub id: i64,
    pub text: Zeroizing<String>,
    pub created_at: i64,   // unix ms
    pub last_used_at: i64, // unix ms
    pub pinned: bool,
    pub is_secret: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Insert {
    New(i64),
    Duplicate(i64), // already stored, moved to top
}

pub struct Store {
    conn: Connection,
    cipher: Cipher,
}

/// Unix time in milliseconds (ms precision keeps "most recent first" ordering exact).
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Default DB path: <OS data dir>/clipguard/clips.db
pub fn default_db_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "clipguard").map(|d| d.data_dir().join("clips.db"))
}

#[cfg(unix)]
fn restrict_permissions(dir: &Path, file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if file.exists() {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn restrict_permissions(_: &Path, _: &Path) -> std::io::Result<()> {
    Ok(()) // Windows: %APPDATA% is already per-user ACL'd
}

#[allow(dead_code)] // pin/delete/count are UI actions (step 4)
impl Store {
    pub fn open(path: &Path, cipher: Cipher) -> Result<Self, StoreError> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let conn = Connection::open(path)?;
        restrict_permissions(dir, path)?;
        Self::init(conn, cipher)
    }

    pub fn open_in_memory(cipher: Cipher) -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, cipher)
    }

    fn init(conn: Connection, cipher: Cipher) -> Result<Self, StoreError> {
        conn.execute_batch(
            "PRAGMA secure_delete = ON;
             PRAGMA journal_mode = DELETE;
             CREATE TABLE IF NOT EXISTS clips (
                 id           INTEGER PRIMARY KEY,
                 fingerprint  BLOB    NOT NULL UNIQUE,
                 nonce        BLOB    NOT NULL,
                 ciphertext   BLOB    NOT NULL,
                 created_at   INTEGER NOT NULL,
                 last_used_at INTEGER NOT NULL,
                 pinned       INTEGER NOT NULL DEFAULT 0,
                 is_secret    INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_clips_last_used ON clips(last_used_at);",
        )?;
        Ok(Self { conn, cipher })
    }

    pub fn insert(&self, text: &str, is_secret: bool) -> Result<Insert, StoreError> {
        let ts = now_ms();
        let fp = self.cipher.fingerprint(text.as_bytes());

        let existing: Option<i64> = self
            .conn
            .query_row("SELECT id FROM clips WHERE fingerprint = ?1", params![&fp[..]], |r| r.get(0))
            .optional()?;
        if let Some(id) = existing {
            self.conn.execute("UPDATE clips SET last_used_at = ?1 WHERE id = ?2", params![ts, id])?;
            return Ok(Insert::Duplicate(id));
        }

        let (nonce, ct) = self.cipher.encrypt(text.as_bytes(), &fp)?;
        self.conn.execute(
            "INSERT INTO clips (fingerprint, nonce, ciphertext, created_at, last_used_at, is_secret)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![&fp[..], &nonce[..], ct, ts, is_secret as i64],
        )?;
        Ok(Insert::New(self.conn.last_insert_rowid()))
    }

    /// All items, newest-used first, decrypted into memory.
    /// Rows that fail to decrypt (tampered / foreign key) are skipped and counted.
    pub fn load_all(&self) -> Result<(Vec<Item>, usize), StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, fingerprint, nonce, ciphertext, created_at, last_used_at, pinned, is_secret
             FROM clips ORDER BY pinned DESC, last_used_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
            ))
        })?;

        let mut items = Vec::new();
        let mut bad = 0usize;
        for row in rows {
            let (id, fp, nonce, ct, created_at, last_used_at, pinned, is_secret) = row?;
            let Ok(pt) = self.cipher.decrypt(&nonce, &ct, &fp) else { bad += 1; continue };
            let Ok(s) = std::str::from_utf8(&pt) else { bad += 1; continue };
            items.push(Item {
                id,
                text: Zeroizing::new(s.to_owned()),
                created_at,
                last_used_at,
                pinned: pinned != 0,
                is_secret: is_secret != 0,
            });
        }
        Ok((items, bad))
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<(), StoreError> {
        self.conn.execute("UPDATE clips SET pinned = ?1 WHERE id = ?2", params![pinned as i64, id])?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?)
    }

    /// Keyed fingerprint of `text` (same value the DB uses for dedup).
    pub fn fingerprint(&self, text: &str) -> [u8; 32] {
        self.cipher.fingerprint(text.as_bytes())
    }

    /// Secret rule: secrets live `ttl_secs` after their last copy, pinned or not.
    /// (Pinning a secret does not exempt it; that would defeat the rule.)
    pub fn purge_secrets(&self, ttl_secs: i64) -> Result<usize, StoreError> {
        let cutoff = now_ms() - ttl_secs * 1000;
        Ok(self
            .conn
            .execute("DELETE FROM clips WHERE is_secret = 1 AND last_used_at < ?1", params![cutoff])?)
    }

    /// Retention: drop unpinned items older than `max_age_secs`, then keep only the
    /// newest `max_items` unpinned. Pinned items are never touched. Returns rows removed.
    pub fn purge(&self, max_age_secs: i64, max_items: i64) -> Result<usize, StoreError> {
        let cutoff = now_ms() - max_age_secs * 1000;
        let mut removed = self
            .conn
            .execute("DELETE FROM clips WHERE pinned = 0 AND last_used_at < ?1", params![cutoff])?;
        removed += self.conn.execute(
            "DELETE FROM clips WHERE pinned = 0 AND id NOT IN (
                 SELECT id FROM clips WHERE pinned = 0 ORDER BY last_used_at DESC, id DESC LIMIT ?1)",
            params![max_items],
        )?;
        Ok(removed)
    }

    #[cfg(test)]
    fn backdate(&self, id: i64, secs: i64) {
        self.conn
            .execute("UPDATE clips SET last_used_at = last_used_at - ?1 WHERE id = ?2", params![secs * 1000, id])
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory(Cipher::from_master(&[9u8; 32])).unwrap()
    }

    #[test]
    fn insert_load_dedup() {
        let s = store();
        let a = s.insert("SELECT 1;", false).unwrap();
        assert!(matches!(a, Insert::New(_)));
        assert!(matches!(s.insert("SELECT 1;", false).unwrap(), Insert::Duplicate(_)));
        s.insert("second", false).unwrap();
        let (items, bad) = s.load_all().unwrap();
        assert_eq!((items.len(), bad), (2, 0));
        assert_eq!(&*items[0].text, "second");
    }

    #[test]
    fn plaintext_not_on_disk() {
        let dir = std::env::temp_dir().join(format!("clipguard-test-{}", std::process::id()));
        let path = dir.join("clips.db");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let s = Store::open(&path, Cipher::from_master(&[3u8; 32])).unwrap();
            s.insert("SUPER-SECRET-MARKER-123", false).unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        let needle = b"SUPER-SECRET-MARKER-123";
        assert!(!raw.windows(needle.len()).any(|w| w == needle), "plaintext leaked to disk");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrong_key_rows_skipped() {
        let dir = std::env::temp_dir().join(format!("clipguard-test2-{}", std::process::id()));
        let path = dir.join("clips.db");
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(&path, Cipher::from_master(&[1u8; 32])).unwrap().insert("x", false).unwrap();
        let (items, bad) = Store::open(&path, Cipher::from_master(&[2u8; 32])).unwrap().load_all().unwrap();
        assert_eq!((items.len(), bad), (0, 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secrets_expire_even_if_pinned() {
        let s = store();
        let Insert::New(sec) = s.insert("AKIAIOSFODNN7EXAMPLE", true).unwrap() else { panic!() };
        s.insert("normal", false).unwrap();
        s.set_pinned(sec, true).unwrap();
        assert_eq!(s.purge_secrets(120).unwrap(), 0, "fresh secret stays");
        s.backdate(sec, 121);
        assert_eq!(s.purge_secrets(120).unwrap(), 1);
        let (items, _) = s.load_all().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(&*items[0].text, "normal");
    }

    #[test]
    fn purge_respects_age_limit_and_pins() {
        let s = store();
        let Insert::New(old) = s.insert("old", false).unwrap() else { panic!() };
        let Insert::New(old_pinned) = s.insert("old-pinned", false).unwrap() else { panic!() };
        s.insert("fresh", false).unwrap();
        s.backdate(old, DEFAULT_MAX_AGE_SECS + 10);
        s.backdate(old_pinned, DEFAULT_MAX_AGE_SECS + 10);
        s.set_pinned(old_pinned, true).unwrap();

        assert_eq!(s.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS).unwrap(), 1);
        let (items, _) = s.load_all().unwrap();
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, vec!["old-pinned", "fresh"]);

        for i in 0..5 { s.insert(&format!("n{i}"), false).unwrap(); }
        s.purge(DEFAULT_MAX_AGE_SECS, 3).unwrap();
        assert_eq!(s.count().unwrap(), 4); // 3 newest unpinned + 1 pinned
    }
}
