//! Workspace-store behavior: schema, threshold placement, the reader rule,
//! read-only SQL, and mark-and-sweep gc.

use std::fs;

use lattice::{
    BodyInput, HistoryQuery, LatticeConfig, LatticeError, MachineStore, NewRun, Retention,
    SecretConfig, SessionQuery, SqlValue, WORKSPACE_SCHEMA_VERSION, WorkspaceStore, now_ms,
    sha256_hex,
};

fn config(threshold: u64) -> LatticeConfig {
    LatticeConfig {
        inline_body_max: threshold,
        ..LatticeConfig::default()
    }
}

fn run<'a>(path: &'a str, body: &'a [u8]) -> NewRun<'a> {
    NewRun {
        started_at: now_ms(),
        duration_ms: Some(12),
        request_path: path,
        request_hash: "deadbeef",
        method: "GET",
        url: "http://127.0.0.1/x",
        status: Some(200),
        res_body: BodyInput::Bytes(body),
        res_content_type: Some("text/plain"),
        ..NewRun::default()
    }
}

#[test]
fn open_connection_honors_wal_flag() {
    let wal_dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(wal_dir.path(), LatticeConfig::default()).unwrap();
    let wal_db = wal_dir.path().join(".facet/lattice.db");
    // WAL is a file-mode: a fresh connection sees it.
    let wal_mode: String = rusqlite::Connection::open(&wal_db)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(wal_mode, "wal");
    assert_eq!(
        pragma_busy_timeout(&store),
        5000,
        "default busy_timeout is 5 s"
    );

    let rollback_dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(
        rollback_dir.path(),
        LatticeConfig {
            wal: false,
            busy_timeout_ms: 1234,
            ..LatticeConfig::default()
        },
    )
    .unwrap();
    let rollback_db = rollback_dir.path().join(".facet/lattice.db");
    let rollback_mode: String = rusqlite::Connection::open(&rollback_db)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rollback_mode, "delete");
    assert_eq!(pragma_busy_timeout(&store), 1234);
}

fn pragma_busy_timeout(store: &WorkspaceStore) -> i64 {
    // busy_timeout is per-connection; WorkspaceStore::query opens a
    // read-only connection and applies config.busy_timeout_ms to it.
    store
        .query("PRAGMA busy_timeout")
        .unwrap()
        .rows
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .and_then(|value| match value {
            SqlValue::Integer(n) => Some(n),
            _ => None,
        })
        .expect("busy_timeout row")
}

#[test]
fn open_lays_down_the_facet_directory_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), LatticeConfig::default()).unwrap();

    let facet = dir.path().join(".facet");
    assert!(facet.join("lattice.db").is_file());
    assert!(facet.join(".gitignore").is_file());
    assert!(facet.join("workspace.toml").is_file());
    assert!(lattice::is_ulid(store.workspace_id()));
    assert_eq!(store.schema_version().unwrap(), WORKSPACE_SCHEMA_VERSION);

    // Reopening keeps the identity and does not re-run migrations.
    let again = WorkspaceStore::open(dir.path(), LatticeConfig::default()).unwrap();
    assert_eq!(again.workspace_id(), store.workspace_id());
    assert_eq!(again.schema_version().unwrap(), WORKSPACE_SCHEMA_VERSION);

    let tables = again
        .query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let names: Vec<String> = tables
        .rows
        .iter()
        .map(|row| match &row[0] {
            SqlValue::Text(text) => text.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(names, ["blobs", "runs", "schema_version"]);
}

#[test]
fn discover_walks_up_from_a_nested_path() {
    let dir = tempfile::tempdir().unwrap();
    WorkspaceStore::open(dir.path(), LatticeConfig::default()).unwrap();
    let nested = dir.path().join("users/deep");
    fs::create_dir_all(&nested).unwrap();
    let file = nested.join("list.yml");
    fs::write(&file, "x").unwrap();

    let found = WorkspaceStore::discover(&file).unwrap();
    assert_eq!(found, std::path::absolute(dir.path()).unwrap());
    assert!(WorkspaceStore::discover(&std::env::temp_dir().join("nope-facet")).is_none());
}

#[test]
fn bodies_land_inline_or_as_blobs_by_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(8)).unwrap();

    let small = store.record_run(&run("a.yml", b"tiny")).unwrap();
    assert_eq!(small.res_body.retention(), "inline");
    assert_eq!(small.res_body.len, Some(4));
    assert!(small.res_body.hash.is_none());
    assert_eq!(store.response_body(&small).unwrap().unwrap(), b"tiny");

    let big = store.record_run(&run("a.yml", b"not tiny at all")).unwrap();
    assert_eq!(big.res_body.retention(), "blob");
    let hash = big.res_body.hash.clone().unwrap();
    assert_eq!(hash, sha256_hex(b"not tiny at all"));
    assert!(store.blobs_dir().join(&hash).is_file());
    assert_eq!(
        store.response_body(&big).unwrap().unwrap(),
        b"not tiny at all"
    );

    let blob = store.blob(&hash).unwrap().unwrap();
    assert_eq!(blob.len, 15);
    assert_eq!(blob.content_type.as_deref(), Some("text/plain"));
    assert!(store.blob("0000").unwrap().is_none());

    // Identical bodies share one file and one registry row.
    let again = store.record_run(&run("b.yml", b"not tiny at all")).unwrap();
    assert_eq!(again.res_body.hash.as_deref(), Some(hash.as_str()));
    assert_eq!(fs::read_dir(store.blobs_dir()).unwrap().count(), 1);
    let registry = store.query("SELECT count(*) FROM blobs").unwrap();
    assert_eq!(registry.rows[0][0], SqlValue::Integer(1));

    // Unretained bodies keep their length and nothing else.
    let unretained = store
        .record_run(&NewRun {
            res_body: BodyInput::Unretained { len: 99 },
            ..run("c.yml", b"")
        })
        .unwrap();
    assert_eq!(unretained.res_body.retention(), "none");
    assert_eq!(unretained.res_body.len, Some(99));
    assert!(store.response_body(&unretained).unwrap().is_none());
}

#[test]
fn request_bodies_are_hash_only_regardless_of_size() {
    // v2: request bodies always live in a blob file keyed by
    // req_body_hash, never inline — even a 4-byte body at a 1 MiB threshold.
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    let recorded = store
        .record_run(&NewRun {
            req_body: BodyInput::Bytes(b"tiny"),
            ..run("a.yml", b"response")
        })
        .unwrap();
    assert_eq!(recorded.req_body.retention(), "blob");
    assert_eq!(recorded.req_body.len, Some(4));
    let hash = recorded.req_body.hash.clone().unwrap();
    assert_eq!(hash, sha256_hex(b"tiny"));
    assert!(store.blobs_dir().join(&hash).is_file());
    assert_eq!(store.request_body(&recorded).unwrap().unwrap(), b"tiny");
    // The inline column is gone: a direct SELECT must not find req_body.
    let cols = store
        .query("SELECT count(*) FROM pragma_table_info('runs') WHERE name = 'req_body'")
        .unwrap();
    assert_eq!(cols.rows[0][0], SqlValue::Integer(0));
}

#[test]
fn v1_to_v2_migration_hydrates_inline_request_bodies() {
    // Build a v1 store by hand: schema 0001, an inline req_body row with no
    // req_body_hash. Reopening must hydrate that body to a blob file, set the
    // hash, and drop the req_body column — no body lost.
    let dir = tempfile::tempdir().unwrap();
    let facet = dir.path().join(".facet");
    fs::create_dir_all(&facet).unwrap();
    let db = facet.join("lattice.db");
    let v1 = include_str!("../migrations/workspace/0001_init.sql");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(v1)
        .unwrap();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, request_path, request_hash, method, url, actor, \
             req_body_len, req_body, res_body_len) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "01JAV1MIGR0000000001",
                now_ms(),
                "a.yml",
                "deadbeef",
                "GET",
                "http://x",
                "human",
                5_i64,
                b"hello",
                0_i64,
            ],
        )
        .unwrap();
    }

    // Reopen through the store: hydration + 0002 migration run on open.
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        lattice::WORKSPACE_SCHEMA_VERSION
    );
    let row = store.run("01JAV1MIGR0000000001").unwrap().unwrap();
    assert_eq!(row.req_body.retention(), "blob");
    let hash = row.req_body.hash.clone().unwrap();
    assert_eq!(hash, sha256_hex(b"hello"));
    assert!(store.blobs_dir().join(&hash).is_file());
    assert_eq!(store.request_body(&row).unwrap().unwrap(), b"hello");
}

#[test]
fn reader_rule_never_branches_on_length() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    let recorded = store.record_run(&run("a.yml", b"short")).unwrap();
    assert_eq!(recorded.res_body.retention(), "inline");

    // A stale or misleading length must not steer reads away from the inline column.
    let db = dir.path().join(".facet/lattice.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE runs SET res_body_len = 999999 WHERE id = ?1",
        rusqlite::params![recorded.id],
    )
    .unwrap();
    drop(conn);

    let row = store.run(&recorded.id).unwrap().unwrap();
    assert_eq!(row.res_body.len, Some(999999));
    assert!(row.res_body.hash.is_none());
    assert_eq!(store.response_body(&row).unwrap().unwrap(), b"short");
}

#[test]
fn reader_rule_survives_a_threshold_change() {
    let dir = tempfile::tempdir().unwrap();
    // Written with a 4-byte threshold: 10 bytes becomes a blob.
    let blob_run = WorkspaceStore::open(dir.path(), config(4))
        .unwrap()
        .record_run(&run("a.yml", b"0123456789"))
        .unwrap();
    // Written with a 1 MiB threshold: the same 10 bytes stay inline.
    let inline_run = WorkspaceStore::open(dir.path(), config(1 << 20))
        .unwrap()
        .record_run(&run("a.yml", b"0123456789"))
        .unwrap();

    // A reader with yet another threshold reads both by the hash column.
    let reader = WorkspaceStore::open(dir.path(), config(0)).unwrap();
    let blob_row = reader.run(&blob_run.id).unwrap().unwrap();
    let inline_row = reader.run(&inline_run.id).unwrap().unwrap();
    assert_eq!(blob_row.res_body.len, inline_row.res_body.len);
    assert_eq!(blob_row.res_body.retention(), "blob");
    assert_eq!(inline_row.res_body.retention(), "inline");
    assert_eq!(
        reader.response_body(&blob_row).unwrap().unwrap(),
        b"0123456789"
    );
    assert_eq!(
        reader.response_body(&inline_row).unwrap().unwrap(),
        b"0123456789"
    );
}

#[test]
fn history_filters_and_orders_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    for (index, path) in ["a.yml", "b.yml", "a.yml"].iter().enumerate() {
        store
            .record_run(&NewRun {
                started_at: 1_000 + index as i64,
                status: Some(if index == 1 { 500 } else { 200 }),
                actor: if index == 2 { "agent-x" } else { "human" },
                ..run(path, b"body")
            })
            .unwrap();
    }

    let all = store
        .history(&HistoryQuery {
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].started_at, 1_002);
    assert_eq!(all[2].started_at, 1_000);

    let only_a = store
        .history(&HistoryQuery {
            limit: 10,
            request_path: Some("a.yml".into()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(only_a.len(), 2);

    let failures = store
        .history(&HistoryQuery {
            limit: 10,
            status: Some(500),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].request_path, "b.yml");

    let agent = store
        .history(&HistoryQuery {
            limit: 10,
            actor: Some("agent-x".into()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(agent.len(), 1);

    let limited = store
        .history(&HistoryQuery {
            limit: 1,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(limited.len(), 1);
}

#[test]
fn sql_escape_hatch_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    store.record_run(&run("a.yml", b"body")).unwrap();

    let result = store
        .query("SELECT request_path, status, res_body_len FROM runs")
        .unwrap();
    assert_eq!(result.columns, ["request_path", "status", "res_body_len"]);
    assert_eq!(result.rows[0][1], SqlValue::Integer(200));

    let write = store.query("DELETE FROM runs");
    assert!(matches!(write, Err(LatticeError::ReadOnlyQuery)));
    assert_eq!(store.count_runs().unwrap(), 1);

    let multi = store.query("SELECT 1; DELETE FROM runs");
    assert!(multi.is_err());
    assert_eq!(store.count_runs().unwrap(), 1);

    let bad = store.query("SELEC nope").unwrap_err();
    assert!(bad.is_query_error(), "{bad}");
}

const SQL_SENTINEL: &str = "sql-sentinel-secret-value";

#[test]
fn sql_authorizer_nulls_secret_columns_in_every_reference() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    store.record_run(&run("a.yml", b"placeholder")).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join(".facet/lattice.db")).unwrap();
    conn.execute("ALTER TABLE runs ADD COLUMN req_body BLOB", [])
        .unwrap();
    conn.execute(
        "UPDATE runs SET req_body = ?1, res_body = ?1, req_headers = ?1, res_headers = ?1",
        [SQL_SENTINEL],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE environments (key TEXT, value TEXT, secret_ref TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO environments (key, value, secret_ref) VALUES ('token', ?1, ?1)",
        [SQL_SENTINEL],
    )
    .unwrap();
    drop(conn);

    for sql in [
        "SELECT req_headers AS h FROM runs",
        "SELECT substr(res_body, 1, 500) FROM runs",
        "SELECT res_body || '' FROM runs",
        "WITH c AS (SELECT req_headers AS h FROM runs) SELECT h FROM c",
        "SELECT h FROM (SELECT res_headers AS h FROM runs)",
        "SELECT json_extract(json_object('v', req_headers), '$.v') FROM runs",
        "SELECT length(res_body) FROM runs",
        "SELECT value AS v FROM environments",
        "SELECT substr(secret_ref, 1, 20) FROM environments",
    ] {
        let result = store
            .query(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_no_sentinel(sql, &result);
        assert!(
            result
                .rows
                .iter()
                .all(|row| row.iter().all(|value| *value == SqlValue::Null)),
            "{sql} did not null the secret reference: {result:?}"
        );
    }
    // This SQLite's hex() turns a NULL argument into '' rather than NULL.
    // The secret bytes are still absent.
    let sentinel_hex: String = SQL_SENTINEL
        .bytes()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    for sql in [
        "SELECT hex(res_body) FROM runs",
        "SELECT hex(req_body) FROM runs",
    ] {
        let result = store.query(sql).unwrap();
        assert_no_sentinel(sql, &result);
        let dumped = format!("{result:?}");
        assert!(!dumped.contains(&sentinel_hex), "{sql}: {dumped}");
        assert!(
            result.rows.iter().all(|row| {
                row.iter().all(|value| match value {
                    SqlValue::Null => true,
                    SqlValue::Text(text) => text.is_empty(),
                    _ => false,
                })
            }),
            "{sql}: {result:?}"
        );
    }

    let star = store.query("SELECT * FROM runs").unwrap();
    assert_no_sentinel("SELECT *", &star);
    for name in ["req_body", "res_body", "req_headers", "res_headers"] {
        let index = star
            .columns
            .iter()
            .position(|column| column == name)
            .unwrap_or_else(|| panic!("{name} missing from {:?}", star.columns));
        assert_eq!(star.rows[0][index], SqlValue::Null, "{name}");
    }
    assert_eq!(
        star.redacted_columns,
        vec![
            "runs.req_body".to_string(),
            "runs.req_headers".to_string(),
            "runs.res_body".to_string(),
            "runs.res_headers".to_string(),
        ]
    );

    let length = store
        .query("SELECT length(res_body), length(req_headers) FROM runs")
        .unwrap();
    assert_eq!(length.rows[0][0], SqlValue::Null);
    assert_eq!(length.rows[0][1], SqlValue::Null);

    let alias = store
        .query("SELECT req_headers AS h, request_path FROM runs")
        .unwrap();
    assert_eq!(alias.rows[0][0], SqlValue::Null);
    assert_eq!(alias.rows[0][1], SqlValue::Text("a.yml".into()));
    assert_eq!(alias.redacted_columns, vec!["runs.req_headers".to_string()]);
    assert_no_sentinel("alias", &alias);

    let env = store
        .query("SELECT key, value, secret_ref FROM environments")
        .unwrap();
    assert_eq!(env.rows[0][0], SqlValue::Text("token".into()));
    assert_eq!(env.rows[0][1], SqlValue::Null);
    assert_eq!(env.rows[0][2], SqlValue::Null);
    assert_eq!(
        env.redacted_columns,
        vec![
            "environments.secret_ref".to_string(),
            "environments.value".to_string()
        ]
    );

    let view = store.query("CREATE VIEW leak AS SELECT req_headers FROM runs");
    assert!(
        matches!(view, Err(LatticeError::ReadOnlyQuery)),
        "creating a view must fail on the read-only connection: {view:?}"
    );
    assert!(matches!(
        store.query("PRAGMA user_version = 7"),
        Err(LatticeError::ReadOnlyQuery)
    ));
    let info = store.query("PRAGMA table_info(runs)").unwrap();
    assert_no_sentinel("table_info", &info);
    assert!(
        store
            .query("SELECT load_extension('not-a-library')")
            .is_err()
    );
}

fn assert_no_sentinel(sql: &str, result: &lattice::SqlResult) {
    let dumped = format!("{result:?}");
    assert!(
        !dumped.contains(SQL_SENTINEL),
        "{sql} leaked the sentinel: {dumped}"
    );
}

#[test]
fn gc_is_mark_and_sweep_with_a_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(2)).unwrap();
    let kept = store.record_run(&run("a.yml", b"kept body")).unwrap();
    let orphan_hash = sha256_hex(b"orphan body");
    fs::write(store.blobs_dir().join(&orphan_hash), b"orphan body").unwrap();

    let dry = store.gc(false).unwrap();
    assert!(!dry.applied);
    assert_eq!(dry.retention, Retention::Unlimited);
    assert_eq!(dry.runs_expired, 0);
    assert_eq!(dry.orphans, [(orphan_hash.clone(), 11)]);
    assert_eq!(dry.bytes_reclaimable, 11);
    assert!(store.blobs_dir().join(&orphan_hash).is_file());

    let applied = store.gc(true).unwrap();
    assert!(applied.applied);
    assert!(!store.blobs_dir().join(&orphan_hash).exists());
    assert!(
        store
            .blobs_dir()
            .join(kept.res_body.hash.as_deref().unwrap())
            .is_file()
    );
    assert_eq!(store.count_runs().unwrap(), 1);
}

#[test]
fn gc_expires_runs_by_retention_and_sweeps_their_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(
        dir.path(),
        LatticeConfig {
            inline_body_max: 2,
            history_retention: Retention::Days(1),
            ..LatticeConfig::default()
        },
    )
    .unwrap();
    let old = store
        .record_run(&NewRun {
            started_at: now_ms() - 3 * 86_400_000,
            ..run("old.yml", b"old body")
        })
        .unwrap();
    let fresh = store.record_run(&run("new.yml", b"new body")).unwrap();

    let dry = store.gc(false).unwrap();
    assert_eq!(dry.runs_expired, 1);
    assert_eq!(dry.orphans.len(), 1);
    assert_eq!(dry.orphans[0].0, old.res_body.hash.clone().unwrap());

    store.gc(true).unwrap();
    assert_eq!(store.count_runs().unwrap(), 1);
    assert!(store.run(&old.id).unwrap().is_none());
    assert!(store.run(&fresh.id).unwrap().is_some());
    assert!(
        !store
            .blobs_dir()
            .join(old.res_body.hash.as_deref().unwrap())
            .exists()
    );
}

#[test]
fn machine_store_indexes_runs_by_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    let recorded = store.record_run(&run("a.yml", b"body")).unwrap();

    let machine = MachineStore::open_at(&dir.path().join("machine.db"), store.config()).unwrap();
    assert_eq!(
        machine.schema_version().unwrap(),
        lattice::MACHINE_SCHEMA_VERSION
    );
    machine
        .touch_workspace(store.workspace_id(), store.root(), Some("Pets"), now_ms())
        .unwrap();
    machine.index_run(&recorded, store.workspace_id()).unwrap();
    machine.index_run(&recorded, store.workspace_id()).unwrap();

    assert_eq!(machine.count_indexed_runs().unwrap(), 1);
    let workspaces = machine.workspaces().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, store.workspace_id());
    assert_eq!(workspaces[0].name.as_deref(), Some("Pets"));

    // v2: run_index carries duration_ms and actor.
    let conn = rusqlite::Connection::open(dir.path().join("machine.db")).unwrap();
    let (duration_ms, actor): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT duration_ms, actor FROM run_index WHERE run_id = ?1",
            rusqlite::params![recorded.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(duration_ms, Some(12));
    assert_eq!(actor.as_deref(), Some("human"));
}

#[test]
fn environments_round_trip_plain_and_secret() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1 << 20)).unwrap();
    let machine = MachineStore::open_at(&dir.path().join("machine.db"), store.config()).unwrap();
    let wsid = store.workspace_id();

    // Plain value: stored in the value column, secret_ref NULL.
    machine
        .set_environment_with(
            wsid,
            "local",
            "API_URL",
            "http://x",
            false,
            &SecretConfig::keyring(),
        )
        .unwrap();
    assert_eq!(
        machine
            .environment_with(wsid, "local", "API_URL", &SecretConfig::keyring())
            .unwrap()
            .as_deref(),
        Some("http://x")
    );

    // Secret value: stored via the encrypted backend, value NULL.
    let key_cfg = SecretConfig::encrypted(b"test-master-key");
    machine
        .set_environment_with(wsid, "local", "API_TOKEN", "tok-123", true, &key_cfg)
        .unwrap();
    assert_eq!(
        machine
            .environment_with(wsid, "local", "API_TOKEN", &key_cfg)
            .unwrap()
            .as_deref(),
        Some("tok-123")
    );

    // Listing returns metadata only; secrets are flagged, never returned.
    let mut rows = machine.environments(wsid).unwrap();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].key, "API_TOKEN");
    assert!(rows[0].secret);
    assert_eq!(rows[1].key, "API_URL");
    assert!(!rows[1].secret);

    // A secret cannot be read without the right key.
    assert!(
        machine
            .environment_with(
                wsid,
                "local",
                "API_TOKEN",
                &SecretConfig::encrypted(b"wrong")
            )
            .is_err()
    );

    // Replacing a secret with a plain value drops the old secret_ref.
    machine
        .set_environment_with(wsid, "local", "API_TOKEN", "plain-now", false, &key_cfg)
        .unwrap();
    assert_eq!(
        machine
            .environment_with(wsid, "local", "API_TOKEN", &key_cfg)
            .unwrap()
            .as_deref(),
        Some("plain-now")
    );
    assert!(
        !machine
            .environments(wsid)
            .unwrap()
            .iter()
            .any(|r| r.key == "API_TOKEN" && r.secret)
    );

    // Delete removes the row.
    assert!(
        machine
            .delete_environment_with(wsid, "local", "API_TOKEN", &key_cfg)
            .unwrap()
    );
    assert!(
        machine
            .environment_with(wsid, "local", "API_TOKEN", &key_cfg)
            .unwrap()
            .is_none()
    );
    assert!(
        !machine
            .delete_environment_with(wsid, "local", "API_TOKEN", &key_cfg)
            .unwrap()
    );
}

#[test]
fn session_start_end_list_show_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let machine =
        MachineStore::open_at(&dir.path().join("machine.db"), &LatticeConfig::default()).unwrap();

    let started = machine
        .start_session(
            "agent.alpha",
            Some(r#"{"tool":{"tab":"workspace:task"}}"#),
            1_000,
        )
        .unwrap();
    assert!(lattice::is_ulid(&started.id));
    assert_eq!(started.actor, "agent.alpha");
    assert_eq!(started.started_at, 1_000);
    assert!(started.ended_at.is_none());
    assert_eq!(
        started.meta.as_deref(),
        Some(r#"{"tool":{"tab":"workspace:task"}}"#)
    );

    // show
    let fetched = machine.session(&started.id).unwrap().unwrap();
    assert_eq!(fetched, started);

    // list: one open session
    let open = machine
        .sessions(&SessionQuery {
            limit: 10,
            open_only: true,
            ..SessionQuery::default()
        })
        .unwrap();
    assert_eq!(open, std::slice::from_ref(&started));

    // end is idempotent: ended_at set once, second call leaves it unchanged.
    let ended = machine.end_session(&started.id, 2_000).unwrap().unwrap();
    assert_eq!(ended.ended_at, Some(2_000));
    let again = machine.end_session(&started.id, 9_999).unwrap().unwrap();
    assert_eq!(
        again.ended_at,
        Some(2_000),
        "second end must not move ended_at"
    );

    // open_only now excludes it; full list still has it.
    let open_after = machine
        .sessions(&SessionQuery {
            limit: 10,
            open_only: true,
            ..SessionQuery::default()
        })
        .unwrap();
    assert!(open_after.is_empty());
    let all = machine
        .sessions(&SessionQuery {
            limit: 10,
            ..SessionQuery::default()
        })
        .unwrap();
    assert_eq!(all, [ended]);

    // missing id -> None (CLI surfaces session_not_found).
    assert!(
        machine
            .session("01K4NOPE00000000000000000")
            .unwrap()
            .is_none()
    );
    assert!(
        machine
            .end_session("01K4NOPE00000000000000000", 3_000)
            .unwrap()
            .is_none()
    );
}

#[test]
fn session_list_filters_by_actor_and_orders_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let machine =
        MachineStore::open_at(&dir.path().join("machine.db"), &LatticeConfig::default()).unwrap();
    let a = machine.start_session("agent-a", None, 1_000).unwrap();
    let b = machine.start_session("agent-b", None, 2_000).unwrap();
    let c = machine.start_session("agent-a", None, 3_000).unwrap();

    let all = machine
        .sessions(&SessionQuery {
            limit: 10,
            ..SessionQuery::default()
        })
        .unwrap();
    assert_eq!(
        all.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
        [c.id.clone(), b.id.clone(), a.id.clone()]
    );

    let agent_a = machine
        .sessions(&SessionQuery {
            limit: 10,
            actor: Some("agent-a".into()),
            ..SessionQuery::default()
        })
        .unwrap();
    assert_eq!(
        agent_a.iter().map(|row| row.started_at).collect::<Vec<_>>(),
        [3_000, 1_000]
    );

    let limited = machine
        .sessions(&SessionQuery {
            limit: 1,
            ..SessionQuery::default()
        })
        .unwrap();
    assert_eq!(limited, [c]);
}

#[test]
fn ensure_session_is_idempotent_mint_if_missing() {
    let dir = tempfile::tempdir().unwrap();
    let machine =
        MachineStore::open_at(&dir.path().join("machine.db"), &LatticeConfig::default()).unwrap();

    // First call mints the row.
    let created = machine
        .ensure_session("01K4ENSURE0000000000000001", "agent-x", 1_000)
        .unwrap();
    assert!(created);
    let row = machine
        .session("01K4ENSURE0000000000000001")
        .unwrap()
        .unwrap();
    assert_eq!(row.actor, "agent-x");
    assert_eq!(row.started_at, 1_000);
    assert!(row.ended_at.is_none());
    assert!(row.meta.is_none());

    // Second call with the same id is a no-op: created is false, actor and
    // started_at are unchanged even when different arguments are passed.
    let created_again = machine
        .ensure_session("01K4ENSURE0000000000000001", "someone-else", 9_999)
        .unwrap();
    assert!(!created_again);
    let row_after = machine
        .session("01K4ENSURE0000000000000001")
        .unwrap()
        .unwrap();
    assert_eq!(row_after.actor, "agent-x");
    assert_eq!(row_after.started_at, 1_000);

    // A different id mints a new row.
    let created_other = machine
        .ensure_session("01K4ENSURE0000000000000002", "agent-x", 2_000)
        .unwrap();
    assert!(created_other);
}

#[test]
fn history_filters_by_session_environment_tag_and_hash() {
    let dir = tempfile::tempdir().unwrap();
    // Tiny threshold so the 5-byte response body lands as a blob and gets a
    // res_body_hash; request bodies are hash-only regardless of size.
    let store = WorkspaceStore::open(dir.path(), config(2)).unwrap();

    let req_hash = sha256_hex(b"req-a");
    let res_hash = sha256_hex(b"res-a");
    let req_body_hash = sha256_hex(b"reqbody-a");

    let in_session = store
        .record_run(&NewRun {
            started_at: 1_000,
            request_path: "Pets/List",
            request_hash: &req_hash,
            environment: Some("local"),
            method: "GET",
            url: "http://x/pets",
            status: Some(200),
            req_body: BodyInput::Bytes(b"reqbody-a"),
            res_body: BodyInput::Bytes(b"res-a"),
            session_id: Some("01K4SESS0000000000000000A"),
            actor: "agent-x",
            tags: Some(r#"["smoke","regression"]"#),
            ..NewRun::default()
        })
        .unwrap();
    assert_eq!(
        in_session.req_body.hash.as_deref(),
        Some(req_body_hash.as_str())
    );
    assert_eq!(in_session.res_body.hash.as_deref(), Some(res_hash.as_str()));

    let other_session = store
        .record_run(&NewRun {
            started_at: 2_000,
            request_path: "Pets/List",
            request_hash: "deadbeef",
            environment: Some("ci"),
            method: "GET",
            url: "http://x/pets",
            status: Some(500),
            session_id: Some("01K4SESS0000000000000000B"),
            actor: "agent-x",
            tags: Some(r#"["smoke"]"#),
            ..NewRun::default()
        })
        .unwrap();

    let no_session = store
        .record_run(&NewRun {
            started_at: 3_000,
            request_path: "Pets/List",
            request_hash: "deadbeef",
            environment: Some("local"),
            method: "GET",
            url: "http://x/pets",
            status: Some(200),
            actor: "human",
            ..NewRun::default()
        })
        .unwrap();

    // session_id filter
    let session_a = store
        .history(&HistoryQuery {
            limit: 10,
            session_id: Some("01K4SESS0000000000000000A".into()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        session_a.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // environment filter
    let local = store
        .history(&HistoryQuery {
            limit: 10,
            environment: Some("local".into()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(local.len(), 2);
    assert!(
        local
            .iter()
            .all(|r| r.environment.as_deref() == Some("local"))
    );

    // tag AND: a run with both smoke and regression
    let tagged = store
        .history(&HistoryQuery {
            limit: 10,
            tags: vec!["smoke".into(), "regression".into()],
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        tagged.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // tag that only one run has, but the other lacks
    let regression = store
        .history(&HistoryQuery {
            limit: 10,
            tags: vec!["regression".into()],
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        regression.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // a run with no tags never matches any tag filter
    let no_tags_match = store
        .history(&HistoryQuery {
            limit: 10,
            tags: vec!["smoke".into()],
            ..HistoryQuery::default()
        })
        .unwrap();
    assert!(!no_tags_match.iter().any(|r| r.id == no_session.id));

    // hash matches any of the three columns: response body hash
    let by_res_hash = store
        .history(&HistoryQuery {
            limit: 10,
            hash: Some(res_hash.clone()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        by_res_hash.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // request body hash
    let by_req_body_hash = store
        .history(&HistoryQuery {
            limit: 10,
            hash: Some(req_body_hash.clone()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        by_req_body_hash
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // request hash
    let by_req_hash = store
        .history(&HistoryQuery {
            limit: 10,
            hash: Some(req_hash.clone()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        by_req_hash.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    // a hash that no run carries
    let by_missing_hash = store
        .history(&HistoryQuery {
            limit: 10,
            hash: Some(sha256_hex(b"nowhere")),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert!(by_missing_hash.is_empty());

    // filters compose: session + tag
    let session_a_smoke = store
        .history(&HistoryQuery {
            limit: 10,
            session_id: Some("01K4SESS0000000000000000A".into()),
            tags: vec!["smoke".into()],
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        session_a_smoke
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&in_session.id)
    );

    let _ = other_session;
}

#[test]
fn replay_lineage_columns_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1024)).unwrap();
    assert_eq!(store.schema_version().unwrap(), 3);
    let plain = store.record_run(&run("a.yml", b"x")).unwrap();
    assert!(plain.replayed_from.is_none());
    assert!(
        plain.var_names.is_none(),
        "NewRun default leaves var_names unknown"
    );
    let replayed = store
        .record_run(&NewRun {
            replayed_from: Some(&plain.id),
            var_names: Some(r#"["token"]"#),
            ..run("a.yml", b"x")
        })
        .unwrap();
    assert_eq!(replayed.replayed_from.as_deref(), Some(plain.id.as_str()));
    assert_eq!(replayed.var_names.as_deref(), Some(r#"["token"]"#));
    let lineage = store
        .query(&format!(
            "SELECT id FROM runs WHERE replayed_from = '{}'",
            plain.id
        ))
        .unwrap();
    assert_eq!(lineage.rows.len(), 1);
    assert_eq!(lineage.rows[0][0], SqlValue::Text(replayed.id.clone()));
}

#[test]
fn machine_index_answers_which_workspace_holds_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::open(dir.path(), config(1024)).unwrap();
    let machine = MachineStore::open_at(&dir.path().join("machine.db"), store.config()).unwrap();
    let row = store.record_run(&run("a.yml", b"x")).unwrap();
    assert_eq!(machine.indexed_run(&row.id).unwrap(), None);
    machine
        .touch_workspace(store.workspace_id(), store.root(), None, now_ms())
        .unwrap();
    machine.index_run(&row, store.workspace_id()).unwrap();
    let (workspace_id, path) = machine.indexed_run(&row.id).unwrap().expect("indexed");
    assert_eq!(workspace_id, store.workspace_id());
    assert_eq!(path, store.root().to_string_lossy());
}

#[test]
fn preferences_round_trip_by_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let machine =
        MachineStore::open_at(&dir.path().join("machine.db"), &LatticeConfig::default()).unwrap();
    assert_eq!(machine.preference("pin.auth").unwrap(), None);
    machine
        .set_preference("pin.auth", r#"{"runId":"a"}"#)
        .unwrap();
    machine.set_preference("pin.b", r#"{"runId":"b"}"#).unwrap();
    machine
        .set_preference("tui.theme", r#""graphite""#)
        .unwrap();
    machine
        .set_preference("pin.auth", r#"{"runId":"a2"}"#)
        .unwrap();
    assert_eq!(
        machine.preference("pin.auth").unwrap().as_deref(),
        Some(r#"{"runId":"a2"}"#)
    );
    let pins = machine.preferences("pin.").unwrap();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins[0].0, "pin.auth");
    assert!(machine.delete_preference("pin.b").unwrap());
    assert!(!machine.delete_preference("pin.b").unwrap());
    assert_eq!(machine.preferences("pin.").unwrap().len(), 1);
}
