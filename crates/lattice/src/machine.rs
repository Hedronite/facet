//! Machine store: `~/.local/share/facet/lattice.db` (XDG data dir).

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::database::{Connection, Row};
use rusqlite::{params, types::Value};

use crate::{
    DB_FILE, LatticeConfig, LatticeError, RunRow, io_error,
    store::{migrate, open_connection},
};

use crate::secrets::{
    SecretConfig, StoredSecret, backend_of, delete_secret_with, get_secret_with, put_secret_with,
};
use zeroize::Zeroizing;

const MACHINE_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/machine/0001_init.sql"),
    include_str!("../migrations/machine/0002_run_index_cols.sql"),
];

/// Machine data directory: `FACET_DATA_DIR` or the platform data dir for `facet`.
#[must_use]
pub fn machine_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FACET_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "facet").map(|dirs| dirs.data_dir().to_owned())
}

/// Machine config directory: `FACET_CONFIG_DIR` or the platform config dir for `facet`.
#[must_use]
pub fn machine_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FACET_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "facet").map(|dirs| dirs.config_dir().to_owned())
}

/// A workspace registry row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRow {
    /// Workspace ULID.
    pub id: String,
    /// Last known absolute path.
    pub path: String,
    /// Display name.
    pub name: Option<String>,
    /// Last time a run was indexed.
    pub last_seen: i64,
}

/// One row of `sessions` (cross-workspace, machine store only).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRow {
    /// ULID.
    pub id: String,
    /// `human` or an agent name.
    pub actor: String,
    /// Unix milliseconds UTC.
    pub started_at: i64,
    /// Unix milliseconds UTC, `None` while the session is open.
    pub ended_at: Option<i64>,
    /// JSON metadata pointer (tool session ids, cwd, etc.). Never transcripts.
    pub meta: Option<String>,
}

/// Filters for [`MachineStore::sessions`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionQuery {
    /// Maximum rows, newest first.
    pub limit: usize,
    /// Only sessions by this actor.
    pub actor: Option<String>,
    /// Only sessions with `ended_at IS NULL`.
    pub open_only: bool,
}

/// An open machine store.
#[derive(Debug)]
pub struct MachineStore {
    path: PathBuf,
    conn: Connection,
}

impl MachineStore {
    /// Opens (creating when needed) the machine store in the data directory.
    pub fn open(config: &LatticeConfig) -> Result<Self, LatticeError> {
        let dir = machine_data_dir().ok_or(LatticeError::NoDataDir)?;
        fs::create_dir_all(&dir).map_err(|error| io_error(&dir, error))?;
        Self::open_at(&dir.join(DB_FILE), config)
    }

    /// Opens (creating when needed) a machine store at an explicit path.
    pub fn open_at(path: &Path, config: &LatticeConfig) -> Result<Self, LatticeError> {
        let conn = open_connection(path, config)?;
        migrate(&conn, MACHINE_MIGRATIONS)?;
        Ok(Self {
            path: path.to_owned(),
            conn,
        })
    }

    /// Database file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Engine executing operations for this store.
    #[must_use]
    pub fn engine(&self) -> crate::Engine {
        self.conn.engine()
    }

    /// Highest applied migration.
    pub fn schema_version(&self) -> Result<i64, LatticeError> {
        self.conn.query_row(
            "SELECT coalesce(max(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
    }

    /// Registers or refreshes a workspace.
    pub fn touch_workspace(
        &self,
        id: &str,
        path: &Path,
        name: Option<&str>,
        now: i64,
    ) -> Result<(), LatticeError> {
        self.conn.execute(
            "INSERT INTO workspaces (id, path, name, last_seen) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET path = excluded.path, \
             name = coalesce(excluded.name, workspaces.name), last_seen = excluded.last_seen",
            params![id, path.to_string_lossy(), name, now],
        )?;
        Ok(())
    }

    /// Writes the cross-workspace pointer row for a run.
    pub fn index_run(&self, run: &RunRow, workspace_id: &str) -> Result<(), LatticeError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO run_index (run_id, workspace_id, started_at, request_path, status, duration_ms, actor) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.id,
                workspace_id,
                run.started_at,
                run.request_path,
                run.status,
                run.duration_ms,
                run.actor,
            ],
        )?;
        Ok(())
    }

    /// Registered workspaces, most recently seen first.
    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>, LatticeError> {
        let mut statement = self.conn.prepare(
            "SELECT id, path, name, last_seen FROM workspaces ORDER BY last_seen DESC, id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(WorkspaceRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    name: row.get(2)?,
                    last_seen: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The workspace (`id`, last known path) the index knows a run under,
    /// or `None`. One query; `facet replay` uses it as a breadcrumb when a
    /// run id belongs to another workspace.
    pub fn indexed_run(&self, run_id: &str) -> Result<Option<(String, String)>, LatticeError> {
        match self.conn.query_row(
            "SELECT run_index.workspace_id, coalesce(workspaces.path, '') FROM run_index \
             LEFT JOIN workspaces ON workspaces.id = run_index.workspace_id \
             WHERE run_index.run_id = ?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok(pair) => Ok(Some(pair)),
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Total pointer rows.
    pub fn count_indexed_runs(&self) -> Result<i64, LatticeError> {
        self.conn
            .query_row("SELECT count(*) FROM run_index", [], |row| row.get(0))
    }

    // ----- Sessions (Goal 1) --------------------------------------------

    /// Starts a new session, returning the row. The id is a fresh ULID.
    pub fn start_session(
        &self,
        actor: &str,
        meta: Option<&str>,
        now: i64,
    ) -> Result<SessionRow, LatticeError> {
        let id = crate::ulid();
        self.conn.execute(
            "INSERT INTO sessions (id, actor, started_at, ended_at, meta) \
             VALUES (?1, ?2, ?3, NULL, ?4)",
            params![id, actor, now, meta],
        )?;
        Ok(SessionRow {
            id,
            actor: actor.to_owned(),
            started_at: now,
            ended_at: None,
            meta: meta.map(str::to_owned),
        })
    }

    /// Mint-if-missing: inserts a session with the given id only when no row
    /// exists yet. Returns `true` when a row was created, `false` when one
    /// already existed (in which case nothing is written). Used by
    /// `facet-record` when `FACET_SESSION` is set on a run.
    pub fn ensure_session(&self, id: &str, actor: &str, now: i64) -> Result<bool, LatticeError> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO sessions (id, actor, started_at, ended_at, meta) \
             VALUES (?1, ?2, ?3, NULL, NULL)",
            params![id, actor, now],
        )?;
        Ok(inserted > 0)
    }

    /// Ends a session. Idempotent: calling this on an already-ended session
    /// leaves `ended_at` unchanged and returns the row. Returns `None` when
    /// no session with `id` exists.
    pub fn end_session(&self, id: &str, now: i64) -> Result<Option<SessionRow>, LatticeError> {
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
            params![id, now],
        )?;
        self.session(id)
    }

    /// Fetches one session by id, or `None` when absent.
    pub fn session(&self, id: &str) -> Result<Option<SessionRow>, LatticeError> {
        let row = match self.conn.query_row(
            "SELECT id, actor, started_at, ended_at, meta FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        ) {
            Ok(row) => Some(row),
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => None,
            Err(other) => return Err(other),
        };
        Ok(row)
    }

    /// Lists sessions newest first, optionally filtered by actor and open state.
    pub fn sessions(&self, query: &SessionQuery) -> Result<Vec<SessionRow>, LatticeError> {
        let mut sql = String::from("SELECT id, actor, started_at, ended_at, meta FROM sessions");
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(actor) = &query.actor {
            clauses.push("actor = ?");
            values.push(Value::Text(actor.clone()));
        }
        if query.open_only {
            clauses.push("ended_at IS NULL");
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
        values.push(Value::Integer(
            i64::try_from(query.limit as u64).unwrap_or(i64::MAX),
        ));

        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query(values)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_session(row)?);
        }
        Ok(out)
    }

    // ----- Preferences (pins first; TUI-private state later) -------------

    /// Sets one preference. `value` is JSON text; the caller owns its shape.
    pub fn set_preference(&self, key: &str, value: &str) -> Result<(), LatticeError> {
        self.conn.execute(
            "INSERT INTO preferences (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Reads one preference's JSON text, or `None`.
    pub fn preference(&self, key: &str) -> Result<Option<String>, LatticeError> {
        match self.conn.query_row(
            "SELECT value FROM preferences WHERE key = ?1",
            params![key],
            |row| row.get::<String>(0),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// All `(key, value)` pairs whose key starts with `prefix`, sorted by key.
    pub fn preferences(&self, prefix: &str) -> Result<Vec<(String, String)>, LatticeError> {
        let mut statement = self.conn.prepare(
            "SELECT key, value FROM preferences WHERE substr(key, 1, ?1) = ?2 ORDER BY key",
        )?;
        let rows = statement
            .query_map(
                params![i64::try_from(prefix.len()).unwrap_or(i64::MAX), prefix],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Deletes one preference; `false` when it was not set.
    pub fn delete_preference(&self, key: &str) -> Result<bool, LatticeError> {
        let removed = self
            .conn
            .execute("DELETE FROM preferences WHERE key = ?1", params![key])?;
        Ok(removed > 0)
    }

    // ----- Environments --------------------------------------------------

    /// Sets one environment value for a workspace. When `secret` is true the
    /// value is routed through the secrets layer (OS keyring by default,
    /// encrypted under `FACET_SECRET_KEY` in headless mode) and the row stores
    /// a `secret_ref`; otherwise the value is stored in plaintext in `value`.
    ///
    /// Replacing an existing entry cleans up the previous secret reference
    /// (keyring entry deleted, encrypted blob dropped with the old row).
    pub fn set_environment(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
        value: &str,
        secret: bool,
    ) -> Result<(), LatticeError> {
        self.set_environment_with(
            workspace_id,
            name,
            key,
            value,
            secret,
            &SecretConfig::from_env()?,
        )
    }

    /// [`Self::set_environment`] with an explicit secrets config (for tests).
    pub fn set_environment_with(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
        value: &str,
        secret: bool,
        config: &SecretConfig,
    ) -> Result<(), LatticeError> {
        let now = crate::now_ms();
        let old_ref: Option<String> = match self.conn.query_row(
            "SELECT secret_ref FROM environments WHERE workspace_id = ?1 AND name = ?2 AND key = ?3",
            params![workspace_id, name, key],
            |row| row.get::<Option<String>>(0),
        ) {
            Ok(value) => value,
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => None,
            Err(other) => return Err(other),
        };

        let (stored_value, stored_ref): (Option<String>, Option<String>) = if secret {
            let StoredSecret { reference, .. } = put_secret_with(value, config)?;
            // Drop the old keyring entry if the previous row held one.
            if let Some(previous) = &old_ref
                && backend_of(previous).is_some()
                && previous != &reference
            {
                let _ = delete_secret_with(previous, config);
            }
            (None, Some(reference))
        } else {
            // Downgrading from secret to plain: clean up the old secret.
            if let Some(previous) = &old_ref
                && backend_of(previous).is_some()
            {
                let _ = delete_secret_with(previous, config);
            }
            (Some(value.to_owned()), None)
        };

        self.conn.execute(
            "INSERT INTO environments (workspace_id, name, key, value, secret_ref, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(workspace_id, name, key) DO UPDATE SET \
             value = excluded.value, secret_ref = excluded.secret_ref, updated_at = excluded.updated_at",
            params![workspace_id, name, key, stored_value, stored_ref, now],
        )?;
        Ok(())
    }

    /// Reads one environment value, resolving secrets through the secrets
    /// layer. Returns `None` when the row is absent. Every value the store
    /// hands out is `Zeroizing`, whether the row is plaintext or a
    /// `secret_ref`. A secret whose backend is unavailable (for example an
    /// encrypted ref with no `FACET_SECRET_KEY`) surfaces as [`SecretError`]
    /// wrapped in [`LatticeError::Secret`].
    pub fn environment(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
    ) -> Result<Option<Zeroizing<String>>, LatticeError> {
        self.environment_with(workspace_id, name, key, &SecretConfig::from_env()?)
    }

    /// [`Self::environment`] with an explicit secrets config (for tests).
    pub fn environment_with(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
        config: &SecretConfig,
    ) -> Result<Option<Zeroizing<String>>, LatticeError> {
        let row = match self.conn.query_row(
            "SELECT value, secret_ref FROM environments \
             WHERE workspace_id = ?1 AND name = ?2 AND key = ?3",
            params![workspace_id, name, key],
            |row| Ok((row.get::<Option<String>>(0)?, row.get::<Option<String>>(1)?)),
        ) {
            Ok(pair) => Some(pair),
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => None,
            Err(other) => return Err(other),
        };
        let Some((value, secret_ref)) = row else {
            return Ok(None);
        };
        match secret_ref {
            Some(reference) if !reference.is_empty() => Ok(get_secret_with(&reference, config)?),
            _ => Ok(value.map(Zeroizing::new)),
        }
    }

    /// Lists environment entries for a workspace as metadata only: name, key,
    /// whether the value is a secret, and `updated_at`. Secret values are
    /// never returned here; resolve them with [`Self::environment`].
    pub fn environments(&self, workspace_id: &str) -> Result<Vec<EnvironmentRow>, LatticeError> {
        let mut statement = self.conn.prepare(
            "SELECT name, key, secret_ref IS NOT NULL AND secret_ref != '', updated_at \
             FROM environments WHERE workspace_id = ?1 \
             ORDER BY name, key",
        )?;
        let rows = statement
            .query_map(params![workspace_id], |row| {
                Ok(EnvironmentRow {
                    name: row.get(0)?,
                    key: row.get(1)?,
                    secret: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Deletes one environment entry, also removing its secret from the
    /// keyring when it was a keyring-backed secret. Missing rows are not an
    /// error.
    pub fn delete_environment(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
    ) -> Result<bool, LatticeError> {
        self.delete_environment_with(workspace_id, name, key, &SecretConfig::from_env()?)
    }

    /// [`Self::delete_environment`] with an explicit secrets config (for tests).
    pub fn delete_environment_with(
        &self,
        workspace_id: &str,
        name: &str,
        key: &str,
        config: &SecretConfig,
    ) -> Result<bool, LatticeError> {
        let secret_ref: Option<String> = match self.conn.query_row(
            "SELECT secret_ref FROM environments \
             WHERE workspace_id = ?1 AND name = ?2 AND key = ?3",
            params![workspace_id, name, key],
            |row| row.get::<Option<String>>(0),
        ) {
            Ok(value) => value,
            Err(LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => None,
            Err(other) => return Err(other),
        };
        let removed = self.conn.execute(
            "DELETE FROM environments WHERE workspace_id = ?1 AND name = ?2 AND key = ?3",
            params![workspace_id, name, key],
        )?;
        if removed > 0
            && let Some(reference) = &secret_ref
            && backend_of(reference).is_some()
        {
            // Best effort: a missing keyring entry is not an error.
            let _ = delete_secret_with(reference, config);
        }
        Ok(removed > 0)
    }
}

/// One row of `environments` as metadata (never carries a secret value).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentRow {
    /// Environment name.
    pub name: String,
    /// Variable key.
    pub key: String,
    /// Whether the value is a secret (stored via the secrets layer).
    pub secret: bool,
    /// Unix milliseconds UTC of the last update.
    pub updated_at: i64,
}

fn row_to_session(row: &Row) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        actor: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        meta: row.get(4)?,
    })
}
