use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use probe_core::{
    AuthenticationKind, Body, CollectionItem, EnvironmentVariable, MultipartPartKind, RequestBody,
    VariableValue, VariableValueSet, VariableValueType, WorkspaceItemRef,
};
use probe_opencollection::create_bundled_workspace_from_collection;

use super::{
    COLLECTION_VARIABLES_ENVIRONMENT, PostmanImportError, PostmanSourceFormat,
    inspect_postman_source,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/postman")
        .join(name)
}

fn temporary_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "probe-postman-{}-{unique}-{name}",
        std::process::id()
    ))
}

#[test]
fn imports_v21_collection_into_domain_model() {
    let preview = inspect_postman_source(fixture("collection-v2.1.json")).unwrap();
    assert_eq!(preview.format(), PostmanSourceFormat::CollectionV2_1);
    let imported = preview.convert(false).unwrap();
    assert_eq!(imported.source.id.as_deref(), Some("pm_v21"));
    assert_eq!(
        imported.collection.metadata.name.as_deref(),
        Some("Postman Pets")
    );
    assert_eq!(
        imported.collection.metadata.version.as_deref(),
        Some("1.2.3-beta")
    );
    assert_eq!(imported.collection.items.len(), 1);
    let CollectionItem::Folder(folder) = &imported.collection.items[0] else {
        panic!("first item should be a folder");
    };
    let CollectionItem::HttpRequest(request) = &folder.items[0] else {
        panic!("folder should contain a request");
    };
    assert_eq!(request.url.as_deref(), Some("{{baseUrl}}/pets/:petId"));
    assert_eq!(request.query_parameters[0].name, "expand");
    assert_eq!(request.path_parameters[0].name, "petId");
    assert_eq!(
        request.authentication.as_ref().map(|auth| &auth.kind),
        Some(&AuthenticationKind::Bearer)
    );
    assert!(matches!(
        request.body,
        Some(RequestBody::Single(Body::Raw(_)))
    ));
    assert_eq!(
        imported.collection.environments[0].name,
        COLLECTION_VARIABLES_ENVIRONMENT
    );
    let probe_core::EnvironmentVariable::Plain(variable) =
        &imported.collection.environments[0].variables[0]
    else {
        panic!("collection variable should be plain");
    };
    assert!(matches!(
        variable.value,
        Some(VariableValueSet::Single(VariableValue::String(_)))
    ));
    let variables = &imported.collection.environments[0].variables;
    assert!(matches!(
        variables[2],
        EnvironmentVariable::Plain(probe_core::Variable {
            value: Some(VariableValueSet::Single(VariableValue::Typed {
                kind: VariableValueType::Number,
                ref data,
            })),
            ..
        }) if data == "25"
    ));
    assert!(matches!(
        variables[3],
        EnvironmentVariable::Plain(probe_core::Variable {
            value: Some(VariableValueSet::Single(VariableValue::Typed {
                kind: VariableValueType::Boolean,
                ref data,
            })),
            ..
        }) if data == "true"
    ));
    assert!(matches!(
        variables[4],
        EnvironmentVariable::Plain(probe_core::Variable {
            value: Some(VariableValueSet::Single(VariableValue::Typed {
                kind: VariableValueType::Object,
                ..
            })),
            ..
        })
    ));
    assert!(matches!(
        variables[5],
        EnvironmentVariable::Plain(probe_core::Variable {
            value: Some(VariableValueSet::Single(VariableValue::Typed {
                kind: VariableValueType::Null,
                ..
            })),
            ..
        })
    ));
    assert!(!imported.partial);
}

#[test]
fn imports_v2_object_authentication() {
    let preview = inspect_postman_source(fixture("collection-v2.json")).unwrap();
    assert_eq!(preview.format(), PostmanSourceFormat::CollectionV2);
    let imported = preview.convert(false).unwrap();
    let CollectionItem::HttpRequest(request) = &imported.collection.items[0] else {
        panic!("item should be a request");
    };
    let authentication = request.authentication.as_ref().unwrap();
    assert_eq!(authentication.kind, AuthenticationKind::Basic);
    assert_eq!(
        authentication.properties.get("username"),
        Some(&probe_core::AuthenticationValue::String("demo".to_owned()))
    );
}

#[test]
fn imports_all_supported_bodies_and_expands_authentication_inheritance() {
    let preview = inspect_postman_source(fixture("collection-bodies-v2.1.json")).unwrap();
    let imported = preview.convert(false).unwrap();
    assert!(!imported.partial);
    let CollectionItem::Folder(folder) = &imported.collection.items[0] else {
        panic!("first item should be a folder");
    };
    assert_eq!(folder.items.len(), 5);
    let requests = folder
        .items
        .iter()
        .map(|item| match item {
            CollectionItem::HttpRequest(request) => request,
            CollectionItem::Folder(_) => panic!("payload item should be a request"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests[0].url.as_deref(),
        Some("https://api.example.com/pets/:petId")
    );
    assert_eq!(requests[0].query_parameters[0].value, "right");
    assert!(requests[0].query_parameters[0].disabled);
    assert!(requests[0].path_parameters[0].disabled);
    assert_eq!(
        requests[0].authentication.as_ref().map(|auth| &auth.kind),
        Some(&AuthenticationKind::Bearer)
    );
    assert!(requests[1].authentication.is_none());
    assert!(matches!(
        requests[0].body,
        Some(RequestBody::Single(Body::Raw(_)))
    ));
    assert!(matches!(
        requests[1].body,
        Some(RequestBody::Single(Body::FormUrlEncoded(_)))
    ));
    let Some(RequestBody::Single(Body::Multipart(parts))) = &requests[2].body else {
        panic!("third request should be multipart");
    };
    assert_eq!(parts[1].kind, MultipartPartKind::File);
    assert!(matches!(
        requests[3].body,
        Some(RequestBody::Single(Body::File(_)))
    ));
    assert!(matches!(
        requests[4].body,
        Some(RequestBody::Single(Body::Raw(_)))
    ));
    let CollectionItem::HttpRequest(string_request) = &imported.collection.items[1] else {
        panic!("second root item should be a request");
    };
    assert_eq!(
        string_request
            .authentication
            .as_ref()
            .map(|auth| &auth.kind),
        Some(&AuthenticationKind::Basic)
    );
}

#[test]
fn strict_mode_rejects_lossy_data_and_partial_is_explicit() {
    let preview = inspect_postman_source(fixture("collection-lossy.json")).unwrap();
    let diagnostics = match preview.convert(false) {
        Err(PostmanImportError::Unsupported(diagnostics)) => diagnostics,
        other => panic!("expected strict compatibility failure, got {other:?}"),
    };
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "unsupported_scripts")
    );
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "unsupported_examples")
    );
    assert!(diagnostics.iter().any(|item| item.code == "unknown_field"));
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "unsupported_variable_scope")
    );
    let imported = preview.convert(true).unwrap();
    assert!(imported.partial);
    assert!(
        imported
            .diagnostics
            .iter()
            .any(|item| item.code == "dynamic_variable_unsupported")
    );
    assert!(
        imported
            .diagnostics
            .iter()
            .any(|item| item.code == "execution_unsupported")
    );
}

#[test]
fn rejects_malformed_json_and_unknown_schema() {
    let malformed = inspect_postman_source(fixture("malformed.json")).unwrap_err();
    assert!(matches!(malformed, PostmanImportError::Invalid(_)));
    let error = inspect_postman_source(fixture("collection-v3.json")).unwrap_err();
    assert!(matches!(error, PostmanImportError::Invalid(_)));
}

#[test]
fn bundled_save_reload_preserves_imported_semantics_and_environment() {
    let imported = inspect_postman_source(fixture("collection-v2.1.json"))
        .unwrap()
        .convert(false)
        .unwrap();
    let destination = temporary_path("roundtrip.yml");
    let loaded =
        create_bundled_workspace_from_collection(&destination, &imported.collection).unwrap();
    let workspace = loaded.workspace();
    assert_eq!(workspace.metadata(), &imported.collection.metadata);
    assert_eq!(workspace.request_count(), 1);
    assert_eq!(workspace.folder_count(), 1);
    assert_eq!(workspace.environments(), imported.collection.environments);
    let WorkspaceItemRef::Folder(folder_key) = workspace.root_items()[0] else {
        panic!("round-tripped root item should be a folder");
    };
    let folder = workspace.folder(folder_key).unwrap();
    let WorkspaceItemRef::Request(request_key) = folder.children[0] else {
        panic!("round-tripped folder item should be a request");
    };
    let request = workspace.request(request_key).unwrap();
    let CollectionItem::Folder(source_folder) = &imported.collection.items[0] else {
        panic!("source item should be a folder");
    };
    let CollectionItem::HttpRequest(source_request) = &source_folder.items[0] else {
        panic!("source folder item should be a request");
    };
    assert_eq!(request, source_request);
    fs::remove_file(destination).unwrap();
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
        2 => "{".to_owned(),
        3 => "[]".to_owned(),
        4 => {
            let depth = 1 + (next_totality(seed) % 64) as usize;
            format!("{}0{}", "[".repeat(depth), "]".repeat(depth))
        }
        5 => "{\"a\":1,\"a\":2}".to_owned(),
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
fn inspect_postman_source_is_total_over_fixed_seed_corpus() {
    let path = temporary_path("totality.json");
    let mut seed = 0x504f_5354_4d41_4e31;
    for index in 0..2_048 {
        fs::write(&path, totality_case(index, &mut seed)).unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| inspect_postman_source(&path)));
        assert!(
            result.is_ok(),
            "Postman inspect panicked for corpus case {index}"
        );
    }
    fs::remove_file(path).unwrap();
}
