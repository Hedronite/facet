use std::fs;

use probe_core::{
    Authentication, AuthenticationKind, AuthenticationValue, Body, Collection, CollectionItem,
    CollectionMetadata, EnvironmentResolutionError, FormField, Header, HttpRequest, ItemMetadata,
    QueryParameter, RequestBody, RequestUpdate, resolve_environment,
};
use probe_opencollection::{
    CreateError, SaveError, StructureError, StructureOperation, create_bundled_workspace,
    create_bundled_workspace_from_collection, load_workspace, load_workspace_from_str,
};

mod support;

use support::{copy_directory, fixture, temporary_path};

#[path = "repository/environment_persistence.rs"]
mod environment_persistence;
#[path = "repository/request_persistence.rs"]
mod request_persistence;
#[path = "repository/structure_persistence.rs"]
mod structure_persistence;
#[path = "repository/workspace_creation.rs"]
mod workspace_creation;

#[test]
fn bundled_directory_loads_opencollection_file_and_unbundled_root_stays_directory() {
    let bundled = temporary_path("bundled-root");
    fs::create_dir(&bundled).unwrap();
    fs::write(
        &*bundled.join("opencollection.yml"),
        "opencollection: 1.0.0\ninfo: { name: Bundled }\nbundled: true\nitems: []\n",
    )
    .unwrap();
    let loaded = load_workspace(&*bundled).expect("bundled directory should load");
    assert_eq!(
        loaded
            .source_path()
            .map(fs::canonicalize)
            .transpose()
            .unwrap(),
        Some(fs::canonicalize(bundled.join("opencollection.yml")).unwrap())
    );

    let unbundled = temporary_path("unbundled-root");
    fs::create_dir(&unbundled).unwrap();
    fs::write(
        &*unbundled.join("opencollection.yml"),
        "opencollection: 1.0.0\ninfo: { name: Unbundled }\nbundled: false\nitems: []\n",
    )
    .unwrap();
    let loaded = load_workspace(&*unbundled).expect("unbundled root should load");
    assert_eq!(
        loaded
            .source_path()
            .map(fs::canonicalize)
            .transpose()
            .unwrap(),
        Some(fs::canonicalize(&*unbundled).unwrap())
    );
}
