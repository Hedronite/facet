//! P1b G3 / H12: `Hydration` holds secret values in `Zeroizing` buffers, and
//! its hand-written `Debug` shows names and counts, never a value.
//!
//! Each case has a control that proves the canary is in the input, so a
//! clean `{:?}` is not just an empty `Hydration`. Every value below is a
//! canary, not a credential, and no assertion message prints one.

use facet_record::{Hydration, Zeroizing, overlay_secrets};
use probe_core::{Environment, EnvironmentVariable, Header, HttpRequest, SecretVariable};

const CANARY: &str = "h12-debug-canary-value";
const VAR_CANARY: &str = "h12-debug-var-canary";

/// `{:?}` and `{:#?}` hold neither canary.
fn assert_clean(what: &str, hydration: &Hydration) {
    for debug in [format!("{hydration:?}"), format!("{hydration:#?}")] {
        assert!(!debug.contains(CANARY), "{what}: a canary is in Debug");
        assert!(!debug.contains(VAR_CANARY), "{what}: a canary is in Debug");
    }
}

#[test]
fn debug_shows_names_and_counts_never_values() {
    let hydration = Hydration {
        overrides: vec![("token".to_owned(), Zeroizing::new(CANARY.to_owned()))],
        hydrated: vec!["token".to_owned()],
        secrets: 1,
        source: Some("encrypted"),
        redact: vec![
            Zeroizing::new(CANARY.to_owned()),
            Zeroizing::new(VAR_CANARY.to_owned()),
        ],
        ..Hydration::default()
    };
    // Control: both canaries are in the values Debug must hide.
    assert_eq!(hydration.overrides[0].1.as_str(), CANARY);
    assert!(
        hydration
            .redact
            .iter()
            .any(|value| value.as_str() == VAR_CANARY)
    );
    assert_clean("literal", &hydration);
    let debug = format!("{hydration:?}");
    assert!(
        debug.contains("token"),
        "Debug should still name the variable"
    );

    // A clone carries the values and hides them the same way.
    let copy = hydration.clone();
    assert_eq!(copy.overrides[0].1.as_str(), CANARY);
    assert_clean("clone", &copy);
}

#[test]
fn a_var_secret_from_the_overlay_stays_out_of_debug() {
    let environments = [Environment {
        name: "local".to_owned(),
        color: None,
        extends: None,
        dot_env_file_path: None,
        variables: vec![EnvironmentVariable::Secret(SecretVariable {
            name: Some("token".to_owned()),
            value_type: None,
            disabled: false,
        })],
    }];
    let request = HttpRequest {
        url: Some("http://127.0.0.1:9/h12".to_owned()),
        headers: vec![Header {
            name: "X-Token".to_owned(),
            value: "{{token}}".to_owned(),
            disabled: false,
        }],
        ..HttpRequest::default()
    };
    let var = [("token".to_owned(), VAR_CANARY.to_owned())];
    let dir = tempfile::tempdir().unwrap();
    let hydration = overlay_secrets(
        Some(dir.path()),
        Some("local"),
        &request,
        &environments,
        &var,
    )
    .expect("hydration");
    // Control: the --var value of a declared secret is one the run redacts.
    assert!(
        hydration
            .redact
            .iter()
            .any(|value| value.as_str() == VAR_CANARY),
        "the --var secret should be in redact"
    );
    assert_clean("overlay", &hydration);
}
