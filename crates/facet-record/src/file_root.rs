//! H7: file bodies stay under the collection root.
//!
//! `probe-http` opens a file body, or a multipart file part, at whatever
//! path the resolved request names: relative to `base_directory`, or
//! absolute. [`confine`] runs before every send. It resolves each such path
//! the way `probe-http` would, canonicalizes it and the root, and refuses a
//! path whose canonical form is not a regular file inside the canonical
//! root, compared by component. It hands back a copy of the request with
//! those canonical paths, which is what gets sent. No error carries a path.
//!
//! Residual: the check and `probe-http`'s open are two steps. Someone who
//! can write inside the root can swap the file between them; see
//! `docs/FACET.md`.

use std::path::{Path, PathBuf};

use probe_core::{Body, HttpRequest, MultipartPartKind, MultipartValue, RequestBody};

/// What [`confine`] does with a path outside the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outside {
    /// Refuse it. The TUI and MCP always pass this.
    Refuse,
    /// Let it through, still canonicalized and rewritten:
    /// `--allow-file-outside-root` on `request run` and `replay` only.
    Allow,
}

/// Why a file body was refused before the send. Never names a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRootError {
    /// The canonical path is outside the canonical root (`..`, an absolute
    /// path, or a symlink).
    Outside,
    /// The file does not exist, is not a regular file, or its canonical
    /// path is not UTF-8.
    Missing,
    /// There is no root on disk to compare against.
    NoBase,
}

impl FileRootError {
    /// Stable programmatic category, the same for every reason.
    #[must_use]
    pub fn category(self) -> &'static str {
        "file_body_outside_root"
    }

    /// Which check refused the path.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Outside => "outside_root",
            Self::Missing => "missing",
            Self::NoBase => "no_base",
        }
    }
}

impl std::fmt::Display for FileRootError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Outside => "a file body resolves outside the collection root",
            Self::Missing => "a file body is missing or is not a regular file",
            Self::NoBase => "a file body needs a collection on disk to resolve against",
        })
    }
}

impl std::error::Error for FileRootError {}

/// `request` with every file path `probe-http` would open rewritten to its
/// canonical absolute form, or the reason one is refused. Checked: the
/// single body, or every selected variant; in it, every selected file
/// reference and every enabled, named multipart file part. A request with
/// no file path comes back unchanged. The caller sends the returned copy
/// and keeps `request` for its hash, preview and record.
///
/// # Errors
///
/// [`FileRootError::NoBase`] with no canonical `base_directory`,
/// [`FileRootError::Missing`] for a path that is not a regular file, and
/// [`FileRootError::Outside`] for one outside the root. With
/// [`Outside::Allow`], only `Missing` is returned.
pub fn confine(
    request: &HttpRequest,
    base_directory: Option<&Path>,
    outside: Outside,
) -> Result<HttpRequest, FileRootError> {
    let mut confined = request.clone();
    let root = Root {
        base: base_directory,
        canonical: base_directory.and_then(|base| base.canonicalize().ok()),
        outside,
    };
    match confined.body.as_mut() {
        None => {}
        Some(RequestBody::Single(body)) => root.body(body)?,
        Some(RequestBody::Variants(variants)) => {
            for variant in variants.iter_mut().filter(|variant| variant.selected) {
                root.body(&mut variant.body)?;
            }
        }
    }
    Ok(confined)
}

struct Root<'a> {
    base: Option<&'a Path>,
    canonical: Option<PathBuf>,
    outside: Outside,
}

impl Root<'_> {
    fn body(&self, body: &mut Body) -> Result<(), FileRootError> {
        match body {
            Body::File(files) => {
                for file in files.iter_mut().filter(|file| file.selected) {
                    self.place(&mut file.file_path)?;
                }
            }
            Body::Multipart(parts) => {
                // The parts `probe-http` sends: enabled and named.
                for part in parts.iter_mut().filter(|part| {
                    part.kind == MultipartPartKind::File
                        && !part.disabled
                        && !part.name.trim().is_empty()
                }) {
                    match &mut part.value {
                        MultipartValue::Single(path) => self.place(path)?,
                        MultipartValue::Multiple(paths) => {
                            for path in paths {
                                self.place(path)?;
                            }
                        }
                    }
                }
            }
            Body::Raw(_) | Body::FormUrlEncoded(_) => {}
        }
        Ok(())
    }

    /// Resolves `path` as `probe-http`'s `resolve_path` does, checks it,
    /// and rewrites it to the canonical form.
    fn place(&self, path: &mut String) -> Result<(), FileRootError> {
        if self.canonical.is_none() && self.outside == Outside::Refuse {
            return Err(FileRootError::NoBase);
        }
        let given = Path::new(path.as_str());
        let resolved = match (self.base, given.is_absolute()) {
            (Some(base), false) => base.join(given),
            _ => given.to_owned(),
        };
        let canonical = resolved
            .canonicalize()
            .map_err(|_| FileRootError::Missing)?;
        if !canonical.is_file() {
            return Err(FileRootError::Missing);
        }
        let inside = self
            .canonical
            .as_deref()
            .is_some_and(|root| canonical.starts_with(root));
        if !inside && self.outside == Outside::Refuse {
            return Err(FileRootError::Outside);
        }
        *path = canonical
            .into_os_string()
            .into_string()
            .map_err(|_| FileRootError::Missing)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use probe_core::{BodyVariant, FileReference, MultipartPart};

    use super::*;

    /// A root `<tmp>/root` holding `in.bin`, a sibling `<tmp>/root-x` (a
    /// prefix of the root's name) holding `out.bin`, and `<tmp>/out.bin`.
    struct Tree {
        tmp: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            fs::create_dir(tmp.path().join("root")).unwrap();
            fs::create_dir(tmp.path().join("root/sub")).unwrap();
            fs::create_dir(tmp.path().join("root-x")).unwrap();
            fs::write(tmp.path().join("root/in.bin"), b"inside").unwrap();
            fs::write(tmp.path().join("root/sub/deep.bin"), b"deep").unwrap();
            fs::write(tmp.path().join("root-x/out.bin"), b"sibling").unwrap();
            fs::write(tmp.path().join("out.bin"), b"outside").unwrap();
            Self { tmp }
        }

        fn root(&self) -> PathBuf {
            self.tmp.path().join("root")
        }

        fn canonical(&self, relative: &str) -> String {
            self.tmp
                .path()
                .join(relative)
                .canonicalize()
                .unwrap()
                .into_os_string()
                .into_string()
                .unwrap()
        }
    }

    fn file_body(path: &str) -> HttpRequest {
        HttpRequest {
            body: Some(RequestBody::Single(Body::File(vec![FileReference {
                file_path: path.to_owned(),
                content_type: "application/octet-stream".to_owned(),
                selected: true,
            }]))),
            ..HttpRequest::default()
        }
    }

    fn multipart(value: MultipartValue, disabled: bool) -> HttpRequest {
        HttpRequest {
            body: Some(RequestBody::Single(Body::Multipart(vec![MultipartPart {
                name: "upload".to_owned(),
                kind: MultipartPartKind::File,
                value,
                content_type: None,
                disabled,
            }]))),
            ..HttpRequest::default()
        }
    }

    fn sent_path(request: &HttpRequest) -> &str {
        match request.body.as_ref() {
            Some(RequestBody::Single(Body::File(files))) => &files[0].file_path,
            Some(RequestBody::Single(Body::Multipart(parts))) => match &parts[0].value {
                MultipartValue::Single(path) => path,
                MultipartValue::Multiple(paths) => &paths[0],
            },
            _ => panic!("no file body"),
        }
    }

    fn refuse(tree: &Tree, request: &HttpRequest) -> Result<HttpRequest, FileRootError> {
        confine(request, Some(&tree.root()), Outside::Refuse)
    }

    #[test]
    fn relative_inside_is_rewritten_to_the_canonical_path() {
        let tree = Tree::new();
        for given in ["in.bin", "./in.bin", "sub/../in.bin"] {
            let request = file_body(given);
            let sent = refuse(&tree, &request).unwrap();
            assert_eq!(sent_path(&sent), tree.canonical("root/in.bin"), "{given}");
            assert_eq!(sent_path(&request), given, "the original is untouched");
        }
        let sent = refuse(&tree, &file_body("sub/deep.bin")).unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("root/sub/deep.bin"));
    }

    #[test]
    fn absolute_inside_is_rewritten_and_absolute_outside_refused() {
        let tree = Tree::new();
        let inside = tree.root().join("in.bin");
        let sent = refuse(&tree, &file_body(inside.to_str().unwrap())).unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("root/in.bin"));
        let outside = tree.tmp.path().join("out.bin");
        assert_eq!(
            refuse(&tree, &file_body(outside.to_str().unwrap())).unwrap_err(),
            FileRootError::Outside
        );
    }

    #[test]
    fn the_root_is_compared_by_component_not_by_prefix() {
        let tree = Tree::new();
        // `<tmp>/root-x/out.bin` starts with the string `<tmp>/root`.
        let sibling = tree.tmp.path().join("root-x/out.bin");
        assert!(
            sibling
                .to_str()
                .unwrap()
                .starts_with(tree.root().to_str().unwrap())
        );
        assert_eq!(
            refuse(&tree, &file_body(sibling.to_str().unwrap())).unwrap_err(),
            FileRootError::Outside
        );
        assert_eq!(
            refuse(&tree, &file_body("../root-x/out.bin")).unwrap_err(),
            FileRootError::Outside
        );
    }

    #[test]
    fn dotdot_escape_is_refused() {
        let tree = Tree::new();
        for given in ["../out.bin", "sub/../../out.bin"] {
            assert_eq!(
                refuse(&tree, &file_body(given)).unwrap_err(),
                FileRootError::Outside,
                "{given}"
            );
        }
    }

    #[test]
    fn symlinks_are_judged_by_their_target() {
        let tree = Tree::new();
        std::os::unix::fs::symlink(
            tree.tmp.path().join("out.bin"),
            tree.root().join("link-out"),
        )
        .unwrap();
        std::os::unix::fs::symlink(tree.root().join("in.bin"), tree.root().join("link-in"))
            .unwrap();
        assert_eq!(
            refuse(&tree, &file_body("link-out")).unwrap_err(),
            FileRootError::Outside
        );
        let sent = refuse(&tree, &file_body("link-in")).unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("root/in.bin"));
    }

    #[test]
    fn missing_a_directory_and_no_base_are_refused() {
        let tree = Tree::new();
        assert_eq!(
            refuse(&tree, &file_body("nope.bin")).unwrap_err(),
            FileRootError::Missing
        );
        assert_eq!(
            refuse(&tree, &file_body("sub")).unwrap_err(),
            FileRootError::Missing
        );
        assert_eq!(
            confine(&file_body("in.bin"), None, Outside::Refuse).unwrap_err(),
            FileRootError::NoBase
        );
        let missing_root = tree.tmp.path().join("no-such-root");
        assert_eq!(
            confine(&file_body("in.bin"), Some(&missing_root), Outside::Refuse).unwrap_err(),
            FileRootError::NoBase
        );
    }

    #[test]
    fn allow_lets_outside_through_canonicalized_but_never_a_missing_file() {
        let tree = Tree::new();
        let allow = |request: &HttpRequest| confine(request, Some(&tree.root()), Outside::Allow);
        let sent = allow(&file_body("../out.bin")).unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("out.bin"));
        let outside = tree.tmp.path().join("out.bin");
        let sent = confine(&file_body(outside.to_str().unwrap()), None, Outside::Allow).unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("out.bin"));
        assert_eq!(
            allow(&file_body("../nope.bin")).unwrap_err(),
            FileRootError::Missing
        );
    }

    #[test]
    fn multipart_file_parts_are_checked_and_rewritten() {
        let tree = Tree::new();
        let sent = refuse(
            &tree,
            &multipart(MultipartValue::Single("in.bin".into()), false),
        )
        .unwrap();
        assert_eq!(sent_path(&sent), tree.canonical("root/in.bin"));
        assert_eq!(
            refuse(
                &tree,
                &multipart(MultipartValue::Single("../out.bin".into()), false)
            )
            .unwrap_err(),
            FileRootError::Outside
        );
        let both = MultipartValue::Multiple(vec!["in.bin".into(), "../out.bin".into()]);
        assert_eq!(
            refuse(&tree, &multipart(both, false)).unwrap_err(),
            FileRootError::Outside
        );
        // A disabled part is never sent, so it is not checked.
        let disabled = multipart(MultipartValue::Single("../out.bin".into()), true);
        assert_eq!(refuse(&tree, &disabled).unwrap(), disabled);
    }

    #[test]
    fn only_selected_files_and_variants_are_checked() {
        let tree = Tree::new();
        let request = HttpRequest {
            body: Some(RequestBody::Variants(vec![
                BodyVariant {
                    title: "unused".to_owned(),
                    selected: false,
                    body: Body::File(vec![FileReference {
                        file_path: "../out.bin".to_owned(),
                        content_type: "text/plain".to_owned(),
                        selected: true,
                    }]),
                },
                BodyVariant {
                    title: "used".to_owned(),
                    selected: true,
                    body: Body::File(vec![
                        FileReference {
                            file_path: "../out.bin".to_owned(),
                            content_type: "text/plain".to_owned(),
                            selected: false,
                        },
                        FileReference {
                            file_path: "in.bin".to_owned(),
                            content_type: "text/plain".to_owned(),
                            selected: true,
                        },
                    ]),
                },
            ])),
            ..HttpRequest::default()
        };
        let sent = refuse(&tree, &request).unwrap();
        let Some(RequestBody::Variants(variants)) = sent.body else {
            panic!("variants");
        };
        let Body::File(files) = &variants[1].body else {
            panic!("file body");
        };
        assert_eq!(files[1].file_path, tree.canonical("root/in.bin"));
        assert_eq!(files[0].file_path, "../out.bin");
    }

    #[test]
    fn a_request_without_a_file_body_is_unchanged_without_a_base() {
        let request = HttpRequest::default();
        assert_eq!(confine(&request, None, Outside::Refuse).unwrap(), request);
    }

    #[test]
    fn no_error_names_a_path() {
        for error in [
            FileRootError::Outside,
            FileRootError::Missing,
            FileRootError::NoBase,
        ] {
            let text = error.to_string();
            assert!(!text.contains('/'), "{text}");
            assert_eq!(error.category(), "file_body_outside_root");
        }
    }
}
