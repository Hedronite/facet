//! Workspace store: `.facet/lattice.db` beside the OpenCollection YAML.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use crate::database::{Connection, Row};
use rusqlite::{params, types::Value};
use serde::Deserialize;

use crate::{
    LatticeConfig, LatticeError, Retention,
    blobs::{self, BodyInput},
    io_error, now_ms, ulid,
};

/// Directory beside the collection that holds Lattice state.
pub const FACET_DIR: &str = ".facet";
/// SQLite file name inside [`FACET_DIR`] (and the machine data directory).
pub const DB_FILE: &str = "lattice.db";
/// Content-addressed blob directory inside [`FACET_DIR`].
pub const BLOBS_DIR: &str = "blobs";
/// Workspace identity file inside [`FACET_DIR`]. Committed, not ignored.
pub const WORKSPACE_FILE: &str = "workspace.toml";
/// Lattice configuration file name (workspace and machine).
pub const CONFIG_FILE: &str = "config.toml";

const WORKSPACE_MIGRATIONS: &[&str] = &[
    include_str!("../migrations/workspace/0001_init.sql"),
    include_str!("../migrations/workspace/0002_hash_only_req.sql"),
    include_str!("../migrations/workspace/0003_replay_lineage.sql"),
];

const GITIGNORE: &str = "# Lattice run history is machine-local. workspace.toml and config.toml are shared.\n\
lattice.db\n\
lattice.db.engine\n\
lattice.db-wal\n\
lattice.db-shm\n\
lattice.db-journal\n\
blobs/\n";

const RUN_COLUMNS: &str = "id, started_at, duration_ms, request_path, request_hash, environment, method, url, \
status, error, req_headers, res_headers, req_body_len, req_body_hash, \
res_body_len, res_body_hash, res_body IS NOT NULL, res_content_type, session_id, actor, tags, \
replayed_from, var_names";

#[derive(Debug, Deserialize)]
struct WorkspaceFile {
    id: String,
}

/// Where a body lives, per the reader rule: a hash means a blob file,
/// otherwise the inline column. Never branch on length.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BodyRef {
    /// Body length in bytes, when known.
    pub len: Option<u64>,
    /// Blob file hash when the body is a file.
    pub hash: Option<String>,
    /// Whether the inline column is non-NULL.
    pub inline_present: bool,
}

impl BodyRef {
    /// `blob`, `inline`, or `none`.
    #[must_use]
    pub fn retention(&self) -> &'static str {
        if self.hash.is_some() {
            "blob"
        } else if self.inline_present {
            "inline"
        } else {
            "none"
        }
    }
}

/// One row of `runs`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRow {
    /// ULID.
    pub id: String,
    /// Unix milliseconds UTC.
    pub started_at: i64,
    /// Wall-clock duration.
    pub duration_ms: Option<i64>,
    /// Repository selector.
    pub request_path: String,
    /// SHA-256 of the resolved request.
    pub request_hash: String,
    /// Environment name used.
    pub environment: Option<String>,
    /// HTTP method.
    pub method: String,
    /// Resolved URL, secrets redacted.
    pub url: String,
    /// HTTP status; `None` on transport failure.
    pub status: Option<i64>,
    /// Transport or timeout error text.
    pub error: Option<String>,
    /// Request headers as JSON, secrets redacted.
    pub req_headers: Option<String>,
    /// Response headers as JSON.
    pub res_headers: Option<String>,
    /// Request body placement.
    pub req_body: BodyRef,
    /// Response body placement.
    pub res_body: BodyRef,
    /// Response content type.
    pub res_content_type: Option<String>,
    /// Machine-store session id.
    pub session_id: Option<String>,
    /// `human` or an agent name.
    pub actor: String,
    /// JSON array of tags.
    pub tags: Option<String>,
    /// `runs.id` this run replayed, when it came from `facet replay`.
    pub replayed_from: Option<String>,
    /// JSON array of `--var` names used at resolve time (never values).
    /// `None` on rows recorded before migration 0003 (unknown); `[]` when
    /// no overrides were passed.
    pub var_names: Option<String>,
}

/// A run to record.
#[derive(Clone, Copy, Debug)]
pub struct NewRun<'a> {
    /// Unix milliseconds UTC.
    pub started_at: i64,
    /// Wall-clock duration.
    pub duration_ms: Option<i64>,
    /// Repository selector.
    pub request_path: &'a str,
    /// SHA-256 of the resolved request.
    pub request_hash: &'a str,
    /// Environment name used.
    pub environment: Option<&'a str>,
    /// HTTP method.
    pub method: &'a str,
    /// Resolved URL, secrets redacted.
    pub url: &'a str,
    /// HTTP status; `None` on transport failure.
    pub status: Option<i64>,
    /// Transport or timeout error text.
    pub error: Option<&'a str>,
    /// Request headers as JSON, secrets redacted.
    pub req_headers: Option<&'a str>,
    /// Response headers as JSON.
    pub res_headers: Option<&'a str>,
    /// Request body.
    pub req_body: BodyInput<'a>,
    /// Response body.
    pub res_body: BodyInput<'a>,
    /// Response content type.
    pub res_content_type: Option<&'a str>,
    /// Machine-store session id.
    pub session_id: Option<&'a str>,
    /// `human` or an agent name.
    pub actor: &'a str,
    /// JSON array of tags.
    pub tags: Option<&'a str>,
    /// `runs.id` this run replayed, if any.
    pub replayed_from: Option<&'a str>,
    /// JSON array of `--var` names (never values); `[]` when none.
    pub var_names: Option<&'a str>,
}

impl Default for NewRun<'_> {
    fn default() -> Self {
        Self {
            started_at: 0,
            duration_ms: None,
            request_path: "",
            request_hash: "",
            environment: None,
            method: "GET",
            url: "",
            status: None,
            error: None,
            req_headers: None,
            res_headers: None,
            req_body: BodyInput::None,
            res_body: BodyInput::None,
            res_content_type: None,
            session_id: None,
            actor: "human",
            tags: None,
            replayed_from: None,
            var_names: None,
        }
    }
}

/// Filters for [`WorkspaceStore::history`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryQuery {
    /// Maximum rows, newest first.
    pub limit: usize,
    /// Only runs of this selector.
    pub request_path: Option<String>,
    /// Only runs with this HTTP status.
    pub status: Option<i64>,
    /// Only runs by this actor.
    pub actor: Option<String>,
    /// Only runs started at or after this time.
    pub since: Option<i64>,
    /// Only runs recorded under this session id.
    pub session_id: Option<String>,
    /// Only runs recorded under this environment name.
    pub environment: Option<String>,
    /// Only runs carrying every one of these tags (AND). Matched via
    /// `json_each(tags)`; a run with no tags never matches.
    pub tags: Vec<String>,
    /// Only runs whose `request_hash`, `req_body_hash`, or `res_body_hash`
    /// equals this value. Which column matched is reported by the CLI
    /// (`matchedHash`), not by the store.
    pub hash: Option<String>,
}

/// A blob registry row plus its bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Blob {
    /// SHA-256 hex.
    pub hash: String,
    /// Registered length.
    pub len: u64,
    /// Registered content type.
    pub content_type: Option<String>,
    /// File contents.
    pub bytes: Vec<u8>,
}

/// Result of a read-only `--sql` query.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlResult {
    /// Column names in select order.
    pub columns: Vec<String>,
    /// Rows of SQLite values. Secret columns are NULL.
    pub rows: Vec<Vec<Value>>,
    /// `table.column` for each secret column this statement read, sorted.
    pub redacted_columns: Vec<String>,
}

/// What `gc` found or did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GcReport {
    /// Whether deletions were applied.
    pub applied: bool,
    /// Retention window in force.
    pub retention: Retention,
    /// Runs older than the retention window.
    pub runs_expired: u64,
    /// Blob files no live run references: `(hash, bytes)`.
    pub orphans: Vec<(String, u64)>,
    /// Registry rows with no live reference.
    pub registry_orphans: u64,
    /// Sum of orphan file sizes.
    pub bytes_reclaimable: u64,
}

/// An open workspace store.
#[derive(Debug)]
pub struct WorkspaceStore {
    root: PathBuf,
    facet_dir: PathBuf,
    conn: Connection,
    config: LatticeConfig,
    workspace_id: String,
}

impl WorkspaceStore {
    /// Walks up from `start` (a file or directory) to the nearest directory
    /// holding `.facet/lattice.db`.
    #[must_use]
    pub fn discover(start: &Path) -> Option<PathBuf> {
        let start = std::path::absolute(start).ok()?;
        let start = if start.is_file() {
            start.parent()?.to_owned()
        } else {
            start
        };
        start
            .ancestors()
            .find(|dir| dir.join(FACET_DIR).join(DB_FILE).is_file())
            .map(Path::to_owned)
    }

    /// Opens (creating when needed) the store for the workspace at `root`.
    pub fn open(root: &Path, config: LatticeConfig) -> Result<Self, LatticeError> {
        let root = std::path::absolute(root).map_err(|error| io_error(root, error))?;
        let facet_dir = root.join(FACET_DIR);
        fs::create_dir_all(&facet_dir).map_err(|error| io_error(&facet_dir, error))?;
        ensure_file(&facet_dir.join(".gitignore"), GITIGNORE)?;
        let workspace_id = ensure_workspace_id(&facet_dir)?;
        let conn = open_connection(&facet_dir.join(DB_FILE), &config)?;
        validate_schema(&conn, WORKSPACE_MIGRATIONS.len())?;
        // Before applying the v2 migration (which drops the inline req_body
        // column), hydrate any v1 inline request bodies into blob files so no
        // body is lost. No-op on fresh stores and stores already at v2.
        hydrate_inline_request_bodies(&conn, &facet_dir.join(BLOBS_DIR))?;
        migrate(&conn, WORKSPACE_MIGRATIONS)?;
        Ok(Self {
            root,
            facet_dir,
            conn,
            config,
            workspace_id,
        })
    }

    /// Opens the store at `root` only when one already exists.
    pub fn open_existing(root: &Path, config: LatticeConfig) -> Result<Option<Self>, LatticeError> {
        if root.join(FACET_DIR).join(DB_FILE).is_file() {
            Self::open(root, config).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Workspace root (the directory beside the collection).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The `.facet` directory.
    #[must_use]
    pub fn facet_dir(&self) -> &Path {
        &self.facet_dir
    }

    /// The blob directory.
    #[must_use]
    pub fn blobs_dir(&self) -> PathBuf {
        self.facet_dir.join(BLOBS_DIR)
    }

    /// Effective configuration.
    #[must_use]
    pub fn config(&self) -> &LatticeConfig {
        &self.config
    }

    /// Workspace ULID from `.facet/workspace.toml`.
    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
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

    /// Records one run. Blob files are written before the short write
    /// transaction that inserts the registry and run rows.
    pub fn record_run(&self, run: &NewRun<'_>) -> Result<RunRow, LatticeError> {
        let id = ulid();
        let blobs_dir = self.blobs_dir();
        let threshold = self.config.inline_body_max;
        // Request bodies are hash-only (v2): always a blob file,
        // never inline. Response bodies keep the inline threshold.
        let req_body = blobs::place_hashed(&blobs_dir, run.req_body)?;
        let res_body = blobs::place(&blobs_dir, run.res_body, threshold)?;
        let now = now_ms();

        let tx = self.conn.transaction()?;
        for (stored, content_type) in [(&req_body, None), (&res_body, run.res_content_type)] {
            if let Some(hash) = &stored.hash {
                tx.execute(
                    "INSERT OR IGNORE INTO blobs (hash, len, content_type, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![hash, stored.len.map(to_i64), content_type, now],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO runs (id, started_at, duration_ms, request_path, request_hash, environment, method, url, \
             status, error, req_headers, res_headers, req_body_len, req_body_hash, \
             res_body_len, res_body, res_body_hash, res_content_type, session_id, actor, tags, \
             replayed_from, var_names) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, \
             ?22, ?23)",
            params![
                id,
                run.started_at,
                run.duration_ms,
                run.request_path,
                run.request_hash,
                run.environment,
                run.method,
                run.url,
                run.status,
                run.error,
                run.req_headers,
                run.res_headers,
                req_body.len.map(to_i64),
                req_body.hash,
                res_body.len.map(to_i64),
                res_body.inline,
                res_body.hash,
                run.res_content_type,
                run.session_id,
                run.actor,
                run.tags,
                run.replayed_from,
                run.var_names,
            ],
        )?;
        tx.commit()?;
        self.run(&id)?
            .ok_or_else(|| LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }

    /// Fetches one run by id.
    pub fn run(&self, id: &str) -> Result<Option<RunRow>, LatticeError> {
        let mut statement = self
            .conn
            .prepare(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"))?;
        let mut rows = statement.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_run(row)?)),
            None => Ok(None),
        }
    }

    /// Lists runs newest first.
    pub fn history(&self, query: &HistoryQuery) -> Result<Vec<RunRow>, LatticeError> {
        let mut sql = format!("SELECT {RUN_COLUMNS} FROM runs");
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(path) = &query.request_path {
            clauses.push("request_path = ?");
            values.push(Value::Text(path.clone()));
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            values.push(Value::Integer(status));
        }
        if let Some(actor) = &query.actor {
            clauses.push("actor = ?");
            values.push(Value::Text(actor.clone()));
        }
        if let Some(since) = query.since {
            clauses.push("started_at >= ?");
            values.push(Value::Integer(since));
        }
        if let Some(session_id) = &query.session_id {
            clauses.push("session_id = ?");
            values.push(Value::Text(session_id.clone()));
        }
        if let Some(environment) = &query.environment {
            clauses.push("environment = ?");
            values.push(Value::Text(environment.clone()));
        }
        for tag in &query.tags {
            // AND: every tag must appear in the run's tags array. A NULL or
            // empty tags column yields no rows from json_each, so it never
            // matches, which is the correct absence semantics.
            clauses.push("EXISTS (SELECT 1 FROM json_each(tags) WHERE value = ?)");
            values.push(Value::Text(tag.clone()));
        }
        if let Some(hash) = &query.hash {
            // One column-agnostic flag across the three hash columns.
            clauses.push("(request_hash = ? OR req_body_hash = ? OR res_body_hash = ?)");
            let hash_value = Value::Text(hash.clone());
            values.push(hash_value.clone());
            values.push(hash_value.clone());
            values.push(hash_value);
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY started_at DESC, id DESC LIMIT ?");
        values.push(Value::Integer(to_i64(query.limit as u64)));

        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query(values)?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            runs.push(row_to_run(row)?);
        }
        Ok(runs)
    }

    /// Total run rows.
    pub fn count_runs(&self) -> Result<i64, LatticeError> {
        self.conn
            .query_row("SELECT count(*) FROM runs", [], |row| row.get(0))
    }

    /// Reads a run's request body per the reader rule. Request bodies are
    /// hash-only (v2): the body lives in `.facet/blobs/<req_body_hash>`, or
    /// there is no body. There is no inline fallback.
    pub fn request_body(&self, run: &RunRow) -> Result<Option<Vec<u8>>, LatticeError> {
        match &run.req_body.hash {
            Some(hash) => blobs::read_blob(&self.blobs_dir(), hash),
            None => Ok(None),
        }
    }

    /// Reads a run's response body per the reader rule.
    pub fn response_body(&self, run: &RunRow) -> Result<Option<Vec<u8>>, LatticeError> {
        self.body_bytes(&run.id, &run.res_body, "res_body")
    }

    fn body_bytes(
        &self,
        run_id: &str,
        body: &BodyRef,
        column: &str,
    ) -> Result<Option<Vec<u8>>, LatticeError> {
        if let Some(hash) = &body.hash {
            return blobs::read_blob(&self.blobs_dir(), hash);
        }
        let bytes: Option<Vec<u8>> = self.conn.query_row(
            &format!("SELECT {column} FROM runs WHERE id = ?1"),
            params![run_id],
            |row| row.get(0),
        )?;
        Ok(bytes)
    }

    /// Fetches a blob file and its registry row.
    pub fn blob(&self, hash: &str) -> Result<Option<Blob>, LatticeError> {
        let hash = hash.trim().to_ascii_lowercase();
        if !blobs::is_blob_name(&hash) {
            return Ok(None);
        }
        let Some(bytes) = blobs::read_blob(&self.blobs_dir(), &hash)? else {
            return Ok(None);
        };
        let registered: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT len, content_type FROM blobs WHERE hash = ?1",
                params![hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                LatticeError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                other => Err(other),
            })?;
        let (len, content_type) = registered
            .map(|(len, content_type)| (len.unsigned_abs(), content_type))
            .unwrap_or((bytes.len() as u64, None));
        Ok(Some(Blob {
            hash,
            len,
            content_type,
            bytes,
        }))
    }

    /// Runs one read-only SQL statement on a separate read-only connection.
    ///
    /// Secret columns (`runs` bodies and headers, `environments.value` and
    /// `environments.secret_ref`) are NULL wherever the statement references
    /// them. `redacted_columns` names the ones this statement read.
    pub fn query(&self, sql: &str) -> Result<SqlResult, LatticeError> {
        let conn = Connection::open(&self.facet_dir.join(DB_FILE), &self.config, true)?;
        let authorizer = conn.install_sql_authorizer()?;
        let mut statement = conn.prepare_read_only(sql)?;
        let columns = statement.column_names();
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(columns.len());
            for index in 0..columns.len() {
                values.push(row.get::<Value>(index)?);
            }
            out.push(values);
        }
        Ok(SqlResult {
            columns,
            rows: out,
            redacted_columns: authorizer.columns(),
        })
    }

    /// Mark-and-sweep garbage collection. Expired runs are those
    /// older than `history_retention`; orphans are blob files no surviving
    /// run references. Nothing is deleted unless `apply` is true.
    pub fn gc(&self, apply: bool) -> Result<GcReport, LatticeError> {
        let retention = self.config.history_retention;
        let cutoff = retention.window_ms().map(|window| now_ms() - window);
        let runs_expired: i64 = match cutoff {
            Some(cutoff) => self.conn.query_row(
                "SELECT count(*) FROM runs WHERE started_at < ?1",
                params![cutoff],
                |row| row.get(0),
            )?,
            None => 0,
        };

        let live_sql = "SELECT req_body_hash FROM runs WHERE req_body_hash IS NOT NULL AND started_at >= ?1 \
                        UNION SELECT res_body_hash FROM runs WHERE res_body_hash IS NOT NULL AND started_at >= ?1";
        let floor = cutoff.unwrap_or(i64::MIN);
        let mut live = BTreeSet::new();
        {
            let mut statement = self.conn.prepare(live_sql)?;
            let mut rows = statement.query(params![floor])?;
            while let Some(row) = rows.next()? {
                live.insert(row.get::<String>(0)?);
            }
        }

        let blobs_dir = self.blobs_dir();
        let mut orphans = Vec::new();
        if blobs_dir.is_dir() {
            for entry in fs::read_dir(&blobs_dir).map_err(|error| io_error(&blobs_dir, error))? {
                let entry = entry.map_err(|error| io_error(&blobs_dir, error))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if blobs::is_blob_name(&name) && !live.contains(&name) {
                    let len = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                    orphans.push((name, len));
                }
            }
        }
        orphans.sort();
        let bytes_reclaimable = orphans.iter().map(|(_, len)| *len).sum();

        let registry_orphans: i64 = {
            let mut statement = self.conn.prepare("SELECT hash FROM blobs")?;
            let mut rows = statement.query([])?;
            let mut count = 0;
            while let Some(row) = rows.next()? {
                if !live.contains(&row.get::<String>(0)?) {
                    count += 1;
                }
            }
            count
        };

        if apply {
            let tx = self.conn.transaction()?;
            if let Some(cutoff) = cutoff {
                tx.execute("DELETE FROM runs WHERE started_at < ?1", params![cutoff])?;
            }
            {
                let mut statement = tx.prepare("SELECT hash FROM blobs")?;
                let stale: Vec<String> = statement
                    .query_map([], |row| row.get::<String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .filter(|hash| !live.contains(hash))
                    .collect();
                for hash in stale {
                    tx.execute("DELETE FROM blobs WHERE hash = ?1", params![hash])?;
                }
            }
            tx.commit()?;
            for (hash, _) in &orphans {
                let path = blobs_dir.join(hash);
                fs::remove_file(&path).map_err(|error| io_error(path, error))?;
            }
        }

        Ok(GcReport {
            applied: apply,
            retention,
            runs_expired: runs_expired.unsigned_abs(),
            orphans,
            registry_orphans: registry_orphans.unsigned_abs(),
            bytes_reclaimable,
        })
    }
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn row_to_run(row: &Row) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: row.get(0)?,
        started_at: row.get(1)?,
        duration_ms: row.get(2)?,
        request_path: row.get(3)?,
        request_hash: row.get(4)?,
        environment: row.get(5)?,
        method: row.get(6)?,
        url: row.get(7)?,
        status: row.get(8)?,
        error: row.get(9)?,
        req_headers: row.get(10)?,
        res_headers: row.get(11)?,
        // Request bodies are hash-only (v2): no inline column, so
        // inline_present is always false. The reader only consults the hash.
        req_body: BodyRef {
            len: row.get::<Option<i64>>(12)?.map(i64::unsigned_abs),
            hash: row.get(13)?,
            inline_present: false,
        },
        res_body: BodyRef {
            len: row.get::<Option<i64>>(14)?.map(i64::unsigned_abs),
            hash: row.get(15)?,
            inline_present: row.get(16)?,
        },
        res_content_type: row.get(17)?,
        session_id: row.get(18)?,
        actor: row.get(19)?,
        tags: row.get(20)?,
        replayed_from: row.get(21)?,
        var_names: row.get(22)?,
    })
}

fn ensure_file(path: &Path, contents: &str) -> Result<(), LatticeError> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, contents).map_err(|error| io_error(path, error))
}

fn ensure_workspace_id(facet_dir: &Path) -> Result<String, LatticeError> {
    let path = facet_dir.join(WORKSPACE_FILE);
    if let Ok(source) = fs::read_to_string(&path)
        && let Ok(file) = toml::from_str::<WorkspaceFile>(&source)
        && crate::is_ulid(&file.id)
    {
        return Ok(file.id);
    }
    let id = ulid();
    fs::write(
        &path,
        format!("# Facet workspace identity. Commit this file; it keys the machine-wide run index.\nid = \"{id}\"\n"),
    )
    .map_err(|error| io_error(&path, error))?;
    Ok(id)
}

/// Opens a connection with WAL, busy timeout, and NORMAL synchronous defaults.
pub(crate) fn open_connection(
    path: &Path,
    config: &LatticeConfig,
) -> Result<Connection, LatticeError> {
    Connection::open(path, config, false)
}

/// v1 -> v2 data migration: before the SQL migration drops the inline
/// `req_body` column, write any inline request bodies to content-addressed
/// blob files and set `req_body_hash`, so no body is lost. No-op on fresh
/// stores (no `runs` table or no inline rows) and on stores already at v2
/// (the column is gone). Idempotent: identical content shares one file.
fn hydrate_inline_request_bodies(conn: &Connection, blobs_dir: &Path) -> Result<(), LatticeError> {
    // Only meaningful when the v1 `runs` table still has `req_body`.
    let has_req_body: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('runs') WHERE name = 'req_body'",
        [],
        |row| row.get(0),
    )?;
    if has_req_body == 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT id, req_body FROM runs WHERE req_body IS NOT NULL AND req_body_hash IS NULL",
    )?;
    let rows: Vec<(String, Vec<u8>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if rows.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(blobs_dir).map_err(|error| io_error(blobs_dir, error))?;
    let now = now_ms();
    let tx = conn.transaction()?;
    for (id, bytes) in rows {
        let len = bytes.len() as u64;
        let hash = blobs::sha256_hex(&bytes);
        blobs::write_blob(blobs_dir, &hash, |sink| sink.write_all(&bytes))?;
        tx.execute(
            "INSERT OR IGNORE INTO blobs (hash, len, content_type, created_at) VALUES (?1, ?2, NULL, ?3)",
            params![hash, to_i64(len), now],
        )?;
        tx.execute(
            "UPDATE runs SET req_body_hash = ?1, req_body_len = coalesce(req_body_len, ?2) WHERE id = ?3",
            params![hash, to_i64(len), id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Applies numbered migrations above the current `schema_version` inside one
/// immediate transaction, so concurrent first opens serialize.
pub(crate) fn migrate(conn: &Connection, migrations: &[&str]) -> Result<(), LatticeError> {
    let tx = conn.transaction()?;
    let current = validate_schema(&tx, migrations.len())?;
    for (index, sql) in migrations.iter().enumerate() {
        let version = to_i64(index as u64 + 1);
        if version > current {
            tx.execute_batch(sql)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn validate_schema(conn: &Connection, maximum: usize) -> Result<i64, LatticeError> {
    let has_table: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    let current: i64 = if has_table > 0 {
        conn.query_row(
            "SELECT coalesce(max(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )?
    } else {
        0
    };
    if current < 0 || current > maximum as i64 {
        return Err(LatticeError::Engine(format!(
            "unsupported schema version {current}; maximum is {}",
            maximum
        )));
    }
    Ok(current)
}
