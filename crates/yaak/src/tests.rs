use crate::{ImportDiagnosticSeverity, YaakImportError, YaakSourceFormat, inspect_yaak_source};
use probe_core::{AuthenticationKind, Body, CollectionItem, RequestBody};
use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    time::SystemTime,
};

fn temporary_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("probe-yaak-{}-{nanos}-{name}", std::process::id()))
}

#[test]
fn converts_export_http_hierarchy_and_environment() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yaak/export-v4.json");

    let preview = inspect_yaak_source(&path).unwrap();
    assert_eq!(preview.format(), YaakSourceFormat::ExportJson);
    let imported = preview.convert(None, false).unwrap();
    assert_eq!(imported.collection.metadata.name.as_deref(), Some("Pets"));
    assert_eq!(imported.collection.environments[0].name, "Global Variables");
    let CollectionItem::Folder(folder) = &imported.collection.items[0] else {
        panic!("expected folder");
    };
    let CollectionItem::HttpRequest(request) = &folder.items[0] else {
        panic!("expected request");
    };
    assert_eq!(request.path_parameters[0].name, "id");
    assert_eq!(request.query_parameters[0].name, "page");
    assert_eq!(request.headers[0].value, "{{TOKEN}}");
    assert_eq!(
        request.authentication.as_ref().unwrap().kind,
        AuthenticationKind::Bearer
    );
    let Some(RequestBody::Single(Body::Raw(body))) = &request.body else {
        panic!("expected raw body");
    };
    assert!(body.data.contains("{{TOKEN}}"));
}

#[test]
fn converts_directory_sync_fixture() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yaak/sync");
    let preview = inspect_yaak_source(path).unwrap();
    assert_eq!(preview.format(), YaakSourceFormat::SyncDirectory);
    let imported = preview.convert(None, false).unwrap();
    assert_eq!(
        imported.collection.metadata.name.as_deref(),
        Some("Sync Pets")
    );
    assert_eq!(imported.collection.environments.len(), 1);
    assert_eq!(imported.collection.items.len(), 1);
}

#[test]
fn accepts_every_supported_export_schema() {
    for schema in 1..=4 {
        let path = temporary_path(&format!("schema-{schema}.json"));
        fs::write(
                &path,
                format!(
                    r#"{{"yaakSchema":{schema},"resources":{{"workspaces":[{{"model":"workspace","id":"wk_{schema}","name":"Schema {schema}"}}]}}}}"#
                ),
            )
            .unwrap();
        let imported = inspect_yaak_source(&path)
            .unwrap()
            .convert(None, false)
            .unwrap();
        assert_eq!(
            imported.collection.metadata.name.as_deref(),
            Some(format!("Schema {schema}").as_str())
        );
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn strict_mode_rejects_lossy_resources_and_partial_reports_them() {
    let path = temporary_path("lossy.json");
    fs::write(
        &path,
        r#"{
  "yaakSchema":4,
  "resources":{
    "workspaces":[{"model":"workspace","id":"wk_1","name":"Mixed"}],
    "grpcRequests":[{"model":"grpc_request","id":"gr_1","workspaceId":"wk_1"}]
  }
}"#,
    )
    .unwrap();
    let preview = inspect_yaak_source(&path).unwrap();
    assert!(matches!(
        preview.convert(None, false),
        Err(YaakImportError::Unsupported(_))
    ));
    let imported = preview.convert(None, true).unwrap();
    assert!(imported.partial);
    assert!(imported.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "unsupported_resource"
            && diagnostic.severity == ImportDiagnosticSeverity::Lossy
    }));
    fs::remove_file(path).unwrap();
}

#[test]
fn sync_directory_requires_valid_relationships() {
    let root = temporary_path("sync");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("yaak.wk_1.yaml"),
        "model: workspace\nid: wk_1\nname: Sync\n",
    )
    .unwrap();
    fs::write(
        root.join("yaak.rq_1.yaml"),
        "model: http_request\nid: rq_1\nworkspaceId: wk_1\nfolderId: missing\nname: Broken\n",
    )
    .unwrap();
    let preview = inspect_yaak_source(&root).unwrap();
    assert!(matches!(
        preview.convert(None, false),
        Err(YaakImportError::Invalid(_))
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_directory_allows_partial_import_with_unsupported_resources() {
    let root = temporary_path("sync-partial");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("yaak.wk_1.yaml"),
        "model: workspace\nid: wk_1\nname: Sync\n",
    )
    .unwrap();
    fs::write(
        root.join("yaak.sse_1.yaml"),
        "model: sse_request\nid: sse_1\nworkspaceId: wk_1\nname: Events\n",
    )
    .unwrap();

    let preview = inspect_yaak_source(&root).unwrap();
    assert!(matches!(
        preview.convert(None, false),
        Err(YaakImportError::Unsupported(_))
    ));
    let imported = preview.convert(None, true).unwrap();
    assert!(imported.partial);
    assert!(imported.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "unsupported_resource"
            && diagnostic.resource_type == "sse_request"
            && diagnostic.resource_id.as_deref() == Some("sse_1")
    }));
    fs::remove_dir_all(root).unwrap();
}

fn next_totality(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed
}

fn totality_case(index: usize, seed: &mut u64) -> String {
    match index % 9 {
        0 => String::new(),
        1 => "\n".to_owned(),
        2 => "---".to_owned(),
        3 => "{".to_owned(),
        4 => {
            let depth = 1 + (next_totality(seed) % 64) as usize;
            format!("{}0{}", "[".repeat(depth), "]".repeat(depth))
        }
        5 => "a: 1\na: 2\n".to_owned(),
        6 => {
            if next_totality(seed) & 1 == 0 {
                "yes\n".to_owned()
            } else {
                "on\n".to_owned()
            }
        }
        7 => "\t".repeat(1 + (next_totality(seed) % 16) as usize),
        _ => {
            let width = (next_totality(seed) % 48) as usize;
            let alphabet = b"{}[],:\"012xyz\n\t";
            (0..width)
                .map(|_| alphabet[(next_totality(seed) % alphabet.len() as u64) as usize] as char)
                .collect()
        }
    }
}

#[test]
fn inspect_yaak_json_and_yaml_is_total_over_fixed_seed_corpus() {
    let json_path = temporary_path("totality.json");
    let sync_path = temporary_path("totality-sync");
    fs::create_dir(&sync_path).unwrap();
    let yaml_path = sync_path.join("yaak.wk_1.yaml");
    let mut seed = 0x5941_414b_544f_5441;
    for index in 0..2_048 {
        let source = totality_case(index, &mut seed);
        fs::write(&json_path, &source).unwrap();
        let json_result = catch_unwind(AssertUnwindSafe(|| inspect_yaak_source(&json_path)));
        assert!(
            json_result.is_ok(),
            "Yaak JSON inspect panicked for corpus case {index}"
        );

        fs::write(&yaml_path, &source).unwrap();
        let yaml_result = catch_unwind(AssertUnwindSafe(|| inspect_yaak_source(&sync_path)));
        assert!(
            yaml_result.is_ok(),
            "Yaak YAML inspect panicked for corpus case {index}"
        );
    }
    fs::remove_file(json_path).unwrap();
    fs::remove_dir_all(sync_path).unwrap();
}

#[test]
fn rejects_unsupported_export_schemas() {
    for schema in [0, 5] {
        let path = temporary_path(&format!("unsupported-schema-{schema}.json"));
        fs::write(
            &path,
            format!(r#"{{"yaakSchema":{schema},"resources":{{"workspaces":[]}}}}"#),
        )
        .unwrap();
        let error = inspect_yaak_source(&path).unwrap_err();
        assert!(matches!(
            error,
            YaakImportError::Invalid(message)
                if message == format!(
                    "unsupported Yaak export schema {schema}; supported schemas are 1 through 4"
                )
        ));
        fs::remove_file(path).unwrap();
    }
}
