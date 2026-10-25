//! G5a: World contract + in-tree fixture exports cluster, intent and calls.

use std::path::PathBuf;

use hedron_ncl::eval::{export, Error, Input};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ncl/fixtures")
}

fn export_fixture() -> Result<serde_json::Value, Error> {
    let path = fixture_root().join("world.ncl");
    export(Input::Path(&path), &[], &[])
}

#[test]
fn world_fixture_exports_three_projections() {
    let v = export_fixture().unwrap();
    let cluster = v["cluster"].as_array().expect("cluster array");
    let intent = v["intent"].as_array().expect("intent array");
    assert_eq!(cluster.len(), 1);
    assert_eq!(intent.len(), 1);
    assert_eq!(v["calls"]["opencollection"], "1.0.0");
    assert_eq!(
        v["calls"]["config"]["environments"][0]["variables"][0]["secret_ref"],
        "kr:probe"
    );
}

#[test]
fn cluster_projection_matches_supported_pod_bar() {
    let v = export_fixture().unwrap();
    let pod = &v["cluster"][0];
    assert_eq!(pod["apiVersion"], "v1");
    assert_eq!(pod["kind"], "Pod");
    assert_eq!(pod["metadata"]["name"], "supported-pod");
    assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
    assert_eq!(pod["spec"]["enableServiceLinks"], false);
    assert_eq!(pod["spec"]["dnsPolicy"], "Default");
    let c = &pod["spec"]["containers"][0];
    assert_eq!(c["name"], "pause");
    assert_eq!(c["image"], "registry.k8s.io/pause:3.10");
    let sc = &c["securityContext"];
    assert_eq!(sc["runAsNonRoot"], true);
    assert_eq!(sc["runAsUser"], 65534);
    assert_eq!(sc["allowPrivilegeEscalation"], false);
    assert_eq!(sc["capabilities"]["drop"], serde_json::json!(["ALL"]));
    assert_eq!(sc["seccompProfile"]["type"], "RuntimeDefault");
    assert_eq!(pod["spec"]["securityContext"]["runAsNonRoot"], true);
}

#[test]
fn intent_projection_is_docs_eod_desired_state() {
    let row = &export_fixture().unwrap()["intent"][0];
    assert_eq!(row["name"], "nickel-g5-docs-eod");
    assert_eq!(row["importance"], 0.5);
    assert_eq!(row["spec"]["kind"], "docs_eod");
    assert_eq!(row["spec"]["date"], "2026-09-12");
    assert!(row["spec"]["required_briefs"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("docs/CHECKLIST-nickel")));
}

#[test]
fn world_contract_rejects_cluster_only_shape() {
    let path = fixture_root().join("world_cluster_only.ncl");
    let err = export(Input::Path(&path), &[], &[]).unwrap_err();
    assert!(err.to_string().contains("contract"), "{err}");
}
