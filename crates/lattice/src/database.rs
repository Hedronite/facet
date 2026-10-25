//! Narrow synchronous storage boundary shared by both Lattice stores.
//!
//! SQL, migrations, bodies and application semantics stay in store/machine.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    ops::Deref,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{
    ToSql,
    hooks::{AuthAction, AuthContext, Authorization},
    types::{FromSql, FromSqlError, ToSqlOutput, Value, ValueRef},
};

use crate::{Engine, LatticeConfig, LatticeError, io_error};

type Result<T> = std::result::Result<T, LatticeError>;

pub(crate) enum Connection {
    Sqlite(rusqlite::Connection),
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("engine", &self.engine())
            .finish_non_exhaustive()
    }
}

impl Connection {
    pub(crate) fn open(path: &Path, config: &LatticeConfig, read_only: bool) -> Result<Self> {
        if config.engine == Engine::Turso {
            return Err(LatticeError::Engine(
                "Turso engine is not supported in this build".into(),
            ));
        }
        verify_engine(path, config.engine, read_only)?;
        let flags = if read_only {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        };
        let conn = rusqlite::Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(Duration::from_millis(config.busy_timeout_ms))?;
        if read_only {
            conn.pragma_update(None, "query_only", true)?;
            // Replaced by [`Connection::install_sql_authorizer`] for `--sql`.
            // This default still blocks ATTACH on any other read-only open.
            conn.authorizer(Some(|ctx: AuthContext<'_>| {
                if matches!(
                    ctx.action,
                    AuthAction::Attach { .. } | AuthAction::Detach { .. }
                ) {
                    Authorization::Deny
                } else {
                    Authorization::Allow
                }
            }))?;
        } else if config.wal {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        Ok(Self::Sqlite(conn))
    }

    pub(crate) fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    pub(crate) fn execute(&self, sql: &str, params: impl Bind) -> Result<usize> {
        let values = params.values()?;
        match self {
            Self::Sqlite(conn) => Ok(conn.execute(sql, rusqlite::params_from_iter(values))?),
        }
    }

    pub(crate) fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            Self::Sqlite(conn) => Ok(conn.execute_batch(sql)?),
        }
    }

    pub(crate) fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        match self {
            Self::Sqlite(conn) => Ok(Statement::Sqlite(conn.prepare_cached(sql)?)),
        }
    }

    /// Authorizer for `history --sql`. Secret columns are read as NULL.
    /// ATTACH, load_extension, and pragma writes are denied.
    pub(crate) fn install_sql_authorizer(&self) -> Result<SqlAuthorizer> {
        let redacted = Arc::new(Mutex::new(BTreeSet::new()));
        let recorded = Arc::clone(&redacted);
        match self {
            Self::Sqlite(conn) => {
                conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                    authorize_sql(ctx, &recorded)
                }))?;
            }
        }
        Ok(SqlAuthorizer { redacted })
    }

    pub(crate) fn prepare_read_only(&self, sql: &str) -> Result<Statement<'_>> {
        let statement = self.prepare(sql).map_err(|error| {
            if let LatticeError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) = &error
                && failure.code == rusqlite::ErrorCode::AuthorizationForStatementDenied
            {
                return LatticeError::ReadOnlyQuery;
            }
            error
        })?;
        if let Statement::Sqlite(inner) = &statement
            && !inner.readonly()
        {
            return Err(LatticeError::ReadOnlyQuery);
        }
        Ok(statement)
    }

    pub(crate) fn query_row<T>(
        &self,
        sql: &str,
        params: impl Bind,
        f: impl FnOnce(&Row) -> rusqlite::Result<T>,
    ) -> Result<T> {
        let mut statement = self.prepare(sql)?;
        let mut rows = statement.query(params)?;
        let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok(f(row)?)
    }

    pub(crate) fn transaction(&self) -> Result<Transaction<'_>> {
        self.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Transaction {
            conn: self,
            committed: false,
        })
    }
}

/// Columns the `--sql` authorizer reads as NULL.
///
/// Workspace `runs`: bodies and headers. `req_body` was dropped in migration
/// 0002; it stays listed so a store that still has the column cannot return it.
/// Blob *files* are not columns (`blobs` stores hash, length, and content type).
///
/// Machine-store `environments`: plain values and secret references. That
/// database is not attached (`ATTACH` is denied). The names are still matched
/// so a visible table of the same shape cannot return them.
///
/// `runs.url` and `runs.var_names` are not listed: recording scrubs secrets
/// out of the URL, and `var_names` stores names only.
const SECRET_SQL_COLUMNS: &[(&str, &str)] = &[
    ("runs", "req_body"),
    ("runs", "res_body"),
    ("runs", "req_headers"),
    ("runs", "res_headers"),
    ("environments", "value"),
    ("environments", "secret_ref"),
];

/// Raw page virtual tables. Column `Ignore` does not cover their `data` blob.
const RAW_PAGE_TABLES: &[&str] = &["sqlite_dbpage", "sqlite_dbdata"];

/// Read pragmas whose parenthesized argument is not an assignment.
const READ_PRAGMAS_WITH_ARG: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_info",
    "index_xinfo",
    "index_list",
    "foreign_key_list",
    "foreign_key_check",
    "table_list",
];

/// Pragmas that write or checkpoint even with no `= value`.
const WRITE_PRAGMAS: &[&str] = &[
    "optimize",
    "wal_checkpoint",
    "incremental_vacuum",
    "writable_schema",
];

/// Source columns a `--sql` statement caused SQLite to read as NULL.
pub(crate) struct SqlAuthorizer {
    redacted: Arc<Mutex<BTreeSet<String>>>,
}

impl SqlAuthorizer {
    /// `table.column`, sorted, one entry per secret column the statement read.
    pub(crate) fn columns(&self) -> Vec<String> {
        self.redacted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

fn authorize_sql(ctx: AuthContext<'_>, redacted: &Mutex<BTreeSet<String>>) -> Authorization {
    match ctx.action {
        AuthAction::Read {
            table_name,
            column_name,
        } => authorize_read(table_name, column_name, redacted),
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("load_extension") =>
        {
            Authorization::Deny
        }
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } if pragma_writes(pragma_name, pragma_value) => Authorization::Deny,
        AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::AlterTable { .. }
        | AuthAction::Analyze { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::CreateVtable { .. }
        | AuthAction::Delete { .. }
        | AuthAction::DropIndex { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropView { .. }
        | AuthAction::DropVtable { .. }
        | AuthAction::Insert { .. }
        | AuthAction::Reindex { .. }
        | AuthAction::Update { .. } => Authorization::Deny,
        // Bundled SQLite has no load_extension action code. An unknown code
        // from another build is denied rather than treated as a read.
        AuthAction::Unknown { .. } => Authorization::Deny,
        _ => Authorization::Allow,
    }
}

fn authorize_read(
    table_name: &str,
    column_name: &str,
    redacted: &Mutex<BTreeSet<String>>,
) -> Authorization {
    // `SELECT count(*)` reads the table once with an empty column name.
    if column_name.is_empty() {
        return Authorization::Allow;
    }
    let table = base_ident(table_name);
    if RAW_PAGE_TABLES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(table))
    {
        return Authorization::Deny;
    }
    let column = base_ident(column_name);
    if !SECRET_SQL_COLUMNS
        .iter()
        .any(|(secret_table, secret_column)| {
            secret_table.eq_ignore_ascii_case(table) && secret_column.eq_ignore_ascii_case(column)
        })
    {
        return Authorization::Allow;
    }
    // SQLITE_IGNORE substitutes NULL for this column reference wherever it
    // appears: aliases, functions, CTEs, subqueries, and views.
    if let Ok(mut guard) = redacted.lock() {
        guard.insert(format!(
            "{}.{}",
            table.to_ascii_lowercase(),
            column.to_ascii_lowercase()
        ));
        Authorization::Ignore
    } else {
        Authorization::Deny
    }
}

fn base_ident(name: &str) -> &str {
    name.rsplit_once('.').map(|(_, tail)| tail).unwrap_or(name)
}

fn pragma_writes(name: &str, value: Option<&str>) -> bool {
    if WRITE_PRAGMAS
        .iter()
        .any(|pragma| pragma.eq_ignore_ascii_case(name))
    {
        return true;
    }
    // `PRAGMA name = value` is a write. `PRAGMA table_info(runs)` is not.
    value.is_some()
        && !READ_PRAGMAS_WITH_ARG
            .iter()
            .any(|pragma| pragma.eq_ignore_ascii_case(name))
}

/// Never infer Turso compatibility from a SQLite header. Legacy unmarked files
/// belong to SQLite; changing configuration cannot silently convert them.
fn verify_engine(path: &Path, engine: Engine, read_only: bool) -> Result<()> {
    let marker = path.with_added_extension("engine");
    match fs::read_to_string(&marker) {
        Ok(value) if value == format!("{}\n", engine.as_str()) => return Ok(()),
        Ok(value) => {
            return Err(LatticeError::Engine(format!(
                "{} belongs to {:?}, requested {}; use a separate store or explicit conversion",
                path.display(),
                value.trim(),
                engine.as_str()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&marker, error)),
    }
    if path.exists() && engine != Engine::Sqlite {
        return Err(LatticeError::Engine(format!(
            "{} is an unmarked SQLite store; refusing a Turso open",
            path.display()
        )));
    }
    if read_only {
        if engine == Engine::Sqlite {
            return Ok(());
        }
        return Err(LatticeError::Engine(
            "Turso store is missing its engine marker".into(),
        ));
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(mut file) => {
            writeln!(file, "{}", engine.as_str()).map_err(|error| io_error(&marker, error))?;
            file.sync_all().map_err(|error| io_error(&marker, error))?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_engine(path, engine, true)
        }
        Err(error) => Err(io_error(&marker, error)),
    }
}

pub(crate) trait Bind {
    fn values(self) -> rusqlite::Result<Vec<Value>>;
}
impl Bind for [Value; 0] {
    fn values(self) -> rusqlite::Result<Vec<Value>> {
        Ok(Vec::new())
    }
}
impl Bind for Vec<Value> {
    fn values(self) -> rusqlite::Result<Vec<Value>> {
        Ok(self)
    }
}
impl Bind for &[&dyn ToSql] {
    fn values(self) -> rusqlite::Result<Vec<Value>> {
        self.iter()
            .map(|value| match value.to_sql()? {
                ToSqlOutput::Borrowed(value) => Value::column_result(value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error))),
                ToSqlOutput::Owned(value) => Ok(value),
                _ => Err(rusqlite::Error::InvalidParameterName(
                    "unsupported Lattice parameter type".into(),
                )),
            })
            .collect()
    }
}

pub(crate) enum Statement<'conn> {
    Sqlite(rusqlite::CachedStatement<'conn>),
}
impl Statement<'_> {
    pub(crate) fn column_names(&self) -> Vec<String> {
        match self {
            Self::Sqlite(stmt) => stmt.column_names().into_iter().map(str::to_owned).collect(),
        }
    }
    pub(crate) fn query(&mut self, params: impl Bind) -> Result<Rows<'_>> {
        let values = params.values()?;
        let inner = match self {
            Self::Sqlite(stmt) => {
                RowsInner::Sqlite(stmt.query(rusqlite::params_from_iter(values))?)
            }
        };
        Ok(Rows {
            inner,
            current: None,
        })
    }
    pub(crate) fn query_map<'a, T, F: FnMut(&Row) -> rusqlite::Result<T> + 'a>(
        &'a mut self,
        params: impl Bind,
        f: F,
    ) -> Result<impl Iterator<Item = Result<T>> + 'a> {
        let mut rows = self.query(params)?;
        let mut f = f;
        let mut ended = false;
        Ok(std::iter::from_fn(move || {
            if ended {
                return None;
            }
            match rows.next() {
                Ok(Some(row)) => Some(f(row).map_err(Into::into)),
                Ok(None) => {
                    ended = true;
                    None
                }
                Err(error) => {
                    ended = true;
                    Some(Err(error))
                }
            }
        }))
    }
}

enum RowsInner<'stmt> {
    Sqlite(rusqlite::Rows<'stmt>),
}
pub(crate) struct Rows<'stmt> {
    inner: RowsInner<'stmt>,
    current: Option<Row>,
}
impl Rows<'_> {
    pub(crate) fn next(&mut self) -> Result<Option<&Row>> {
        self.current = match &mut self.inner {
            RowsInner::Sqlite(rows) => rows
                .next()?
                .map(|row| {
                    (0..row.as_ref().column_count())
                        .map(|index| row.get(index))
                        .collect::<rusqlite::Result<Vec<Value>>>()
                        .map(Row)
                })
                .transpose()?,
        };
        Ok(self.current.as_ref())
    }
}
pub(crate) struct Row(Vec<Value>);
impl Row {
    pub(crate) fn get<T: FromSql>(&self, index: usize) -> rusqlite::Result<T> {
        let value = self
            .0
            .get(index)
            .ok_or(rusqlite::Error::InvalidColumnIndex(index))?;
        T::column_result(ValueRef::from(value)).map_err(|error| match error {
            FromSqlError::InvalidType => {
                rusqlite::Error::InvalidColumnType(index, index.to_string(), value.data_type())
            }
            FromSqlError::OutOfRange(value) => {
                rusqlite::Error::IntegralValueOutOfRange(index, value)
            }
            other => {
                rusqlite::Error::FromSqlConversionFailure(index, value.data_type(), Box::new(other))
            }
        })
    }
}

pub(crate) struct Transaction<'conn> {
    conn: &'conn Connection,
    committed: bool,
}
impl Deref for Transaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
    }
}
impl Transaction<'_> {
    pub(crate) fn commit(mut self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_transaction_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(
            &dir.path().join("transaction.db"),
            &LatticeConfig::default(),
            false,
        )
        .unwrap();
        conn.execute_batch("CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        {
            let tx = conn.transaction().unwrap();
            tx.execute("INSERT INTO entries VALUES (1, 'first')", [])
                .unwrap();
            assert!(
                tx.execute("INSERT INTO entries VALUES (1, 'duplicate')", [])
                    .is_err()
            );
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entries", [], |row| row.get::<i64>(0))
                .unwrap(),
            0
        );
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO entries VALUES (2, 'committed')", [])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entries", [], |row| row.get::<i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn sqlite_authorizer_blocks_commented_attach() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::WorkspaceStore::open(dir.path(), LatticeConfig::default()).unwrap();
        for sql in [
            "/* prefix */ ATTACH ':memory:' AS other",
            "-- prefix\nATTACH ':memory:' AS other",
        ] {
            assert!(matches!(store.query(sql), Err(LatticeError::ReadOnlyQuery)));
        }
    }
}
