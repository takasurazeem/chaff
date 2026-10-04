//! The catalog: a local SQLite database describing the user's photographs.
//!
//! # What the catalog is, and what it is not
//!
//! It is **derived state plus irreplaceable state**, and the two must not be confused:
//!
//! * Derived: which files exist, their sizes, their mtimes, their grouping, their
//!   computed scores. All of this can be rebuilt by re-indexing, at the cost of time.
//! * Irreplaceable: the user's ratings, rejects, flags and person names. Re-indexing
//!   cannot recover these, because they exist nowhere else.
//!
//! That distinction is why [`migrate`] takes a backup before every schema upgrade. A
//! migration that goes wrong is recoverable from a re-index if only derived state is
//! lost, and is not recoverable at all if the user's three hours of culling are lost
//! with it.
//!
//! # Refusing to open a newer catalog
//!
//! An older build meeting a newer schema refuses to open it rather than guessing. The
//! alternative — running `CREATE TABLE IF NOT EXISTS` against a schema it does not
//! understand — is how a downgrade silently corrupts a catalog.

pub mod store;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("catalog i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "this catalog uses schema version {found}, but this build understands only up to \
         version {supported}. Refusing to open it — a newer catalog may contain columns \
         this build would silently drop. Update Chaff, or point it at a different catalog."
    )]
    TooNew { found: u32, supported: u32 },
}

/// Schema migrations, in order. Append only; never edit a shipped entry.
///
/// Editing a released migration means two machines at the same version have different
/// schemas, which is undetectable at runtime and produces data corruption that surfaces
/// weeks later. If a migration was wrong, add another one.
pub const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../../migrations/001_init.sql")),
    (2, include_str!("../../migrations/002_exif.sql")),
    (3, include_str!("../../migrations/003_decision.sql")),
];

/// Highest schema version this build understands.
pub fn supported_version() -> u32 {
    MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0)
}

/// Open a catalog file, applying any pending migrations.
pub fn open(path: &Path) -> Result<Connection, CatalogError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| CatalogError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }

    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    migrate(&mut conn, Some(path))?;
    Ok(conn)
}

/// An in-memory catalog. Used by tests; never by the application.
pub fn open_in_memory() -> Result<Connection, CatalogError> {
    let mut conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&mut conn, None)?;
    Ok(conn)
}

/// Connection-level settings.
///
/// WAL is the important one for the product, not for correctness: it lets the UI read
/// the catalog while the indexer writes to it, so the grid stays responsive during a
/// first index of a large library. Without it, every read blocks behind the writer and
/// the app appears frozen exactly when the user is most impatient.
fn configure(conn: &Connection) -> Result<(), CatalogError> {
    // A busy timeout rather than instant SQLITE_BUSY: with two connections in one
    // process, brief write contention is normal and must not surface as an error.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Journal mode is a file property and persists; setting it on every open is
    // harmless. In-memory databases do not support WAL, so fall back quietly.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    Ok(())
}

/// Apply pending migrations, backing up first when the catalog already has content.
///
/// Returns the schema version afterwards.
pub fn migrate(conn: &mut Connection, backup_path: Option<&Path>) -> Result<u32, CatalogError> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let target = supported_version();

    if current > target {
        return Err(CatalogError::TooNew { found: current, supported: target });
    }
    if current == target {
        return Ok(current);
    }

    // Only back up a catalog that has something to lose. A fresh file migrating from 0
    // is not worth a copy, and copying it would litter the app data directory with
    // meaningless .bak files on every first run.
    if current > 0 {
        if let Some(path) = backup_path {
            backup(conn, path, current)?;
        }
    }

    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", *version as i64)?;
        tx.commit()?;
    }

    Ok(target)
}

/// Copy the catalog to `<path>.v<from_version>.bak`.
///
/// The WAL is checkpointed first. Copying the main file while a WAL holds uncommitted
/// pages would produce a backup that looks valid and is missing recent writes — a backup
/// that fails only when it is needed.
fn backup(conn: &Connection, path: &Path, from_version: u32) -> Result<(), CatalogError> {
    let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");

    let mut dest = path.as_os_str().to_owned();
    dest.push(format!(".v{from_version}.bak"));
    let dest = PathBuf::from(dest);

    std::fs::copy(path, &dest).map_err(|source| CatalogError::Io { path: dest, source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_fresh_catalog_migrates_to_the_supported_version() {
        let conn = open_in_memory().expect("open");
        let v: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("read version");
        assert_eq!(v, supported_version());
        assert!(v >= 1, "there must be at least one migration");
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("catalog.db");

        let conn = open(&path).expect("first open");
        let first: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        drop(conn);

        let conn = open(&path).expect("second open");
        let second: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(first, second);

        // Re-running migrations must not duplicate tables or data.
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='photo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn all_expected_tables_exist() {
        let conn = open_in_memory().unwrap();
        for table in ["library", "photo", "file", "photo_review", "score"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} is missing from the schema");
        }
    }

    #[test]
    fn foreign_keys_are_enforced() {
        // Without `PRAGMA foreign_keys = ON`, SQLite silently accepts orphaned rows and
        // the cascade deletes in the schema do nothing. That would turn a paired delete
        // into a leak of dangling file rows.
        let conn = open_in_memory().unwrap();
        let result = conn.execute(
            "INSERT INTO photo (library_id, dir, stem, state, needs_review) \
             VALUES (9999, '/nowhere', 'x', 'pair', 0)",
            [],
        );
        assert!(result.is_err(), "inserting a photo for a nonexistent library must fail");
    }

    #[test]
    fn a_newer_catalog_is_refused_rather_than_opened() {
        let conn = open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 9999i64).unwrap();
        let mut conn = conn;
        let err = migrate(&mut conn, None).expect_err("must refuse");
        match err {
            CatalogError::TooNew { found, supported } => {
                assert_eq!(found, 9999);
                assert_eq!(supported, supported_version());
            }
            other => panic!("expected TooNew, got {other:?}"),
        }
    }

    #[test]
    fn wal_mode_is_enabled_on_a_file_catalog() {
        // The product depends on this: the grid must be readable while indexing writes.
        let dir = tempdir().unwrap();
        let conn = open(&dir.path().join("catalog.db")).unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
