//! Thin wrapper over the Nickel VM: Nickel source in, frozen JSON out.
//!
//! This is the export boundary. `eval_full_for_export` skips fields marked
//! `| not_exported`, which is how plaintext secret fields stay out of every
//! frozen projection. Nothing here persists, hydrates or reads secrets.

use std::path::{Path, PathBuf};

use nickel_lang_core::error::report::{report_as_str, ColorOpt};
use nickel_lang_core::eval::cache::CacheImpl;
use nickel_lang_core::program::{Program, ProgramBuilder};
use nickel_lang_core::serialize::{to_string, ExportFormat};
use nickel_lang_core::term::{MergePriority, Number};
use nickel_lang_core::typecheck::TypecheckMode;

/// Merge priority given to caller-supplied overrides (`--var path=value`).
/// This is the **operator** rung of the SPEC ladder: above agent intent (plain),
/// below the release overlay (`priority 1000`) and platform `| force`.
pub const OVERRIDE_PRIORITY: i64 = 100;

/// Errors from building, evaluating or exporting a Nickel program.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The program could not be assembled (I/O, no input).
    #[error("nickel build: {0}")]
    Build(String),
    /// An override assignment was not `path.to.field=value`.
    #[error("nickel override: {0}")]
    Override(String),
    /// Parse, typecheck, import or contract (blame) failure. Carries the rendered
    /// Nickel diagnostic so callers can surface blame text verbatim.
    #[error("nickel: {0}")]
    Nickel(String),
    /// The evaluated value is not JSON-exportable (functions, opaque values).
    #[error("nickel export: {0}")]
    Export(String),
    /// The exported JSON text failed to parse back (should not happen).
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Where the main program comes from.
#[derive(Debug, Clone)]
pub enum Input<'a> {
    /// In-memory source, `name` only labels diagnostics. Relative imports do
    /// not resolve.
    Text { name: &'a str, source: &'a str },
    /// A file on disk; imports resolve relative to it.
    Path(&'a Path),
}

fn render<E: nickel_lang_core::error::IntoDiagnostics>(
    program: &Program<CacheImpl>,
    e: E,
) -> Error {
    let mut files = program.files();
    Error::Nickel(report_as_str(&mut files, e, ColorOpt::Never))
}

fn build(
    input: Input<'_>,
    overrides: &[String],
    import_paths: &[PathBuf],
) -> Result<Program<CacheImpl>, Error> {
    let builder = ProgramBuilder::new();
    let builder = if import_paths.is_empty() {
        builder
    } else {
        builder.add_import_paths(import_paths)
    };
    let builder = match input {
        Input::Text { name, source } => builder.add_source_string(source, name),
        Input::Path(path) => builder.add_path(path),
    };
    let mut program: Program<CacheImpl> = builder
        .build()
        .map_err(|e| Error::Build(format!("{e:?}")))?;

    let mut parsed = Vec::with_capacity(overrides.len());
    for assignment in overrides {
        if !assignment.contains('=') {
            return Err(Error::Override(format!(
                "expected `path.to.field=value`, got `{assignment}`"
            )));
        }
        let priority = MergePriority::Numeral(Number::from(OVERRIDE_PRIORITY));
        let field_override = program
            .parse_override(assignment.clone(), priority)
            .map_err(|e| render(&program, e))?;
        parsed.push(field_override);
    }
    program.add_overrides(parsed);
    Ok(program)
}

fn export_value(program: &mut Program<CacheImpl>) -> Result<serde_json::Value, Error> {
    let value = program
        .eval_full_for_export()
        .map_err(|e| render(program, e))?;
    let json =
        to_string(ExportFormat::Json, &value).map_err(|e| Error::Export(format!("{e:?}")))?;
    Ok(serde_json::from_str(&json)?)
}

/// Parse, typecheck and fully evaluate (so every contract fires) without
/// returning or persisting anything. `Ok(())` means the program would export.
pub fn check(
    input: Input<'_>,
    overrides: &[String],
    import_paths: &[PathBuf],
) -> Result<(), Error> {
    let mut program = build(input, overrides, import_paths)?;
    program
        .typecheck(TypecheckMode::Walk)
        .map_err(|e| render(&program, e))?;
    export_value(&mut program).map(drop)
}

/// Evaluate for export with caller overrides and return the frozen value as JSON.
pub fn export(
    input: Input<'_>,
    overrides: &[String],
    import_paths: &[PathBuf],
) -> Result<serde_json::Value, Error> {
    let mut program = build(input, overrides, import_paths)?;
    export_value(&mut program)
}

/// Evaluate `source` (named `name` in diagnostics) for export and return the
/// frozen value as JSON.
pub fn eval_export(name: &str, source: &str) -> Result<serde_json::Value, Error> {
    export(Input::Text { name, source }, &[], &[])
}

/// Like [`eval_export`], with `platform` and `overlay` bound in scope
/// (see [`crate::overlay::prelude`]).
pub fn eval_export_with_prelude(name: &str, body: &str) -> Result<serde_json::Value, Error> {
    let source = format!("{}{body}", crate::overlay::prelude());
    eval_export(name, &source)
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    #[test]
    fn exports_json() {
        let v = eval_export("t", r#"{ a = 1, b = "x", c | not_exported = "hidden" }"#).unwrap();
        assert_eq!(v, serde_json::json!({ "a": 1, "b": "x" }));
    }

    #[test]
    fn blame_is_an_error() {
        let err = eval_export("t", r#"("x" | Number)"#).unwrap_err();
        assert!(matches!(err, Error::Nickel(_)), "{err}");
    }

    #[test]
    fn override_is_operator_priority() {
        let src = r#"{ replicas = 1, image | priority 1000 = "pinned" }"#;
        let v = export(
            Input::Text {
                name: "t",
                source: src,
            },
            &["replicas=3".into(), "image=\"mine\"".into()],
            &[],
        )
        .unwrap();
        assert_eq!(v["replicas"], 3, "override beats plain agent value");
        assert_eq!(
            v["image"], "pinned",
            "release overlay priority beats override"
        );
    }

    #[test]
    fn bad_override_shape() {
        let err = export(
            Input::Text {
                name: "t",
                source: "{}",
            },
            &["nope".into()],
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, Error::Override(_)), "{err}");
    }

    #[test]
    fn check_reports_type_errors_without_output() {
        let err = check(
            Input::Text {
                name: "t",
                source: r#"(1 + "a" : Number)"#,
            },
            &[],
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, Error::Nickel(_)), "{err}");
        check(
            Input::Text {
                name: "t",
                source: "{ ok = true }",
            },
            &[],
            &[],
        )
        .unwrap();
    }

    #[test]
    fn pathological_program_finishes_within_declared_bound() {
        let mut source = String::with_capacity(64 * 1024);
        source.push('{');
        for index in 0..4_096 {
            let _ = std::fmt::Write::write_fmt(&mut source, format_args!("field_{index} = 0, "));
        }
        source.push('}');

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(eval_export("pathological.ncl", &source));
        });
        let _outcome = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("pathological Nickel program exceeded the 2-second bound");
    }
}
