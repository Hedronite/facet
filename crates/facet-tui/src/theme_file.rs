//! Versioned, human-editable theme files (the Theme files section in
//! `docs/FACET.md`), for `facet tui` only. No GPUI desktop theme files are
//! shipped in this workspace.
//!
//! A theme file is TOML at `<config dir>/themes/<name>.toml` (the config
//! dir is `FACET_CONFIG_DIR` or the platform config dir for `facet`):
//!
//! ```toml
//! version = 1
//! name = "midnight-honey"
//! extends = "graphite"          # built-in base: graphite | porcelain
//!
//! [colors]
//! accent = "#e7821b"
//! window_bg = "#1d1e22"
//! ```
//!
//! Rules, per DESIGN.md: parsing and validation live here, outside every
//! component; unsupported versions and invalid fields reject the **whole
//! file**; missing tokens merge with the built-in base; a rejected file
//! leaves a complete built-in theme in place; errors name the source file
//! and the offending field. Unknown top-level keys are tolerated (the
//! format grows additively inside a version); unknown `[colors]` tokens
//! are rejected — a typo'd token silently doing nothing is the worse
//! failure for a color file. Colors are `#rrggbb` only.

use std::fmt;
use std::path::{Path, PathBuf};

use ratatui::style::Color;

use crate::theme::{Appearance, Depth, Palette, Theme};

/// The only schema version this binary reads.
pub const THEME_FILE_VERSION: i64 = 1;

/// A parsed, validated theme file: the built-in base it extends plus the
/// merged palette (overrides applied, everything else from the base).
#[derive(Clone, Debug)]
pub struct ThemeFile {
    name: String,
    extends: Appearance,
    palette: Palette,
    /// Number of `[colors]` overrides the file set.
    overrides: usize,
}

impl ThemeFile {
    /// Reads and validates a theme file.
    pub fn load(path: &Path) -> Result<Self, ThemeFileError> {
        let text = std::fs::read_to_string(path).map_err(|error| ThemeFileError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        Self::parse(path, &text)
    }

    /// Parses and validates. `path` is used for error reporting and the
    /// default name (file stem); the file is never read here.
    pub fn parse(path: &Path, text: &str) -> Result<Self, ThemeFileError> {
        let document: toml::Value =
            toml::from_str(text).map_err(|error| ThemeFileError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        let table = document.as_table().ok_or_else(|| ThemeFileError::Parse {
            path: path.to_path_buf(),
            message: "theme file must be a TOML table".to_string(),
        })?;

        let version = table
            .get("version")
            .ok_or_else(|| ThemeFileError::MissingField {
                path: path.to_path_buf(),
                field: "version",
            })?;
        let version = version
            .as_integer()
            .ok_or_else(|| ThemeFileError::InvalidField {
                path: path.to_path_buf(),
                field: "version".to_string(),
                reason: "expected an integer".to_string(),
            })?;
        if version != THEME_FILE_VERSION {
            return Err(ThemeFileError::UnsupportedVersion {
                path: path.to_path_buf(),
                found: version,
            });
        }

        let extends_value = table
            .get("extends")
            .ok_or_else(|| ThemeFileError::MissingField {
                path: path.to_path_buf(),
                field: "extends",
            })?;
        let extends_str = extends_value
            .as_str()
            .ok_or_else(|| ThemeFileError::InvalidField {
                path: path.to_path_buf(),
                field: "extends".to_string(),
                reason: "expected \"graphite\" or \"porcelain\"".to_string(),
            })?;
        let extends =
            Appearance::from_flag(extends_str).ok_or_else(|| ThemeFileError::InvalidField {
                path: path.to_path_buf(),
                field: "extends".to_string(),
                reason: format!("expected \"graphite\" or \"porcelain\", got {extends_str:?}"),
            })?;

        let name = match table.get("name") {
            None => path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "theme".to_string()),
            Some(value) => {
                let name = value.as_str().ok_or_else(|| ThemeFileError::InvalidField {
                    path: path.to_path_buf(),
                    field: "name".to_string(),
                    reason: "expected a string".to_string(),
                })?;
                if name.is_empty() {
                    return Err(ThemeFileError::InvalidField {
                        path: path.to_path_buf(),
                        field: "name".to_string(),
                        reason: "must not be empty".to_string(),
                    });
                }
                name.to_string()
            }
        };

        let mut palette = Palette::for_appearance(extends);
        let mut overrides = 0;
        if let Some(colors) = table.get("colors") {
            let colors = colors
                .as_table()
                .ok_or_else(|| ThemeFileError::InvalidField {
                    path: path.to_path_buf(),
                    field: "colors".to_string(),
                    reason: "expected a table of token = \"#rrggbb\"".to_string(),
                })?;
            for (token, value) in colors {
                let field = format!("colors.{token}");
                let hex = value.as_str().ok_or_else(|| ThemeFileError::InvalidField {
                    path: path.to_path_buf(),
                    field: field.clone(),
                    reason: "expected a \"#rrggbb\" string".to_string(),
                })?;
                let color = parse_hex(hex).ok_or_else(|| ThemeFileError::InvalidField {
                    path: path.to_path_buf(),
                    field: field.clone(),
                    reason: format!("expected \"#rrggbb\", got {hex:?}"),
                })?;
                if !set_token(&mut palette, token, color) {
                    return Err(ThemeFileError::UnknownField {
                        path: path.to_path_buf(),
                        field,
                    });
                }
                overrides += 1;
            }
        }

        Ok(Self {
            name,
            extends,
            palette,
            overrides,
        })
    }

    /// Parses an Omarchy colors.toml palette.
    pub fn parse_omarchy(path: &Path, name: &str, text: &str) -> Result<Self, ThemeFileError> {
        let document: toml::Value =
            toml::from_str(text).map_err(|error| ThemeFileError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
        let table = document.as_table().ok_or_else(|| ThemeFileError::Parse {
            path: path.to_path_buf(),
            message: "Omarchy colors.toml must be a TOML table".to_owned(),
        })?;
        let extends = table
            .get("mode")
            .and_then(toml::Value::as_str)
            .and_then(Appearance::from_flag)
            .unwrap_or(Appearance::Dark);
        let mut palette = Palette::for_appearance(extends);
        let mappings = [
            ("accent", "accent"),
            ("selection", "selection_active_bg"),
            ("muted", "text_muted"),
            ("background", "window_bg"),
            ("dark_background", "sidebar_bg"),
            ("lighter_background", "editor_bg"),
            ("foreground", "text_primary"),
            ("dark_foreground", "text_muted"),
            ("light_foreground", "text_secondary"),
            ("bright_foreground", "text_primary"),
        ];
        let mut overrides = 0;
        for (source, target) in mappings {
            let Some(value) = table.get(source).and_then(toml::Value::as_str) else {
                continue;
            };
            let color = parse_hex(value).ok_or_else(|| ThemeFileError::InvalidField {
                path: path.to_path_buf(),
                field: source.to_owned(),
                reason: format!("expected #rrggbb, got {value:?}"),
            })?;
            let _ = set_token(&mut palette, target, color);
            overrides += 1;
        }
        if let Some(value) = table.get("selection").and_then(toml::Value::as_str)
            && let Some(color) = parse_hex(value)
        {
            let _ = set_token(&mut palette, "selection_inactive_bg", color);
        }
        Ok(Self {
            name: name.to_owned(),
            extends,
            palette,
            overrides,
        })
    }

    /// Theme name (`name =` or the file stem).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The built-in base this theme extends.
    pub fn extends(&self) -> Appearance {
        self.extends
    }

    /// How many `[colors]` tokens the file set.
    pub fn override_count(&self) -> usize {
        self.overrides
    }

    /// The merged palette (overrides over the built-in base).
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// Builds the active theme at a color depth.
    pub fn theme(&self, depth: Depth) -> Theme {
        Theme::from_palette(self.extends, self.palette).with_depth(depth)
    }
}

/// Why a theme file was rejected. Every variant names the file; field
/// errors name the field (DESIGN.md: report source and field, never crash).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThemeFileError {
    /// Unreadable file.
    Io { path: PathBuf, message: String },
    /// Not TOML / not a table.
    Parse { path: PathBuf, message: String },
    /// `version` is absent.
    MissingField { path: PathBuf, field: &'static str },
    /// `version` is not 1.
    UnsupportedVersion { path: PathBuf, found: i64 },
    /// A field has the wrong type or an invalid value.
    InvalidField {
        path: PathBuf,
        field: String,
        reason: String,
    },
    /// A `[colors]` token this version does not know.
    UnknownField { path: PathBuf, field: String },
}

impl ThemeFileError {
    /// The source file.
    pub fn path(&self) -> &Path {
        match self {
            Self::Io { path, .. }
            | Self::Parse { path, .. }
            | Self::MissingField { path, .. }
            | Self::UnsupportedVersion { path, .. }
            | Self::InvalidField { path, .. }
            | Self::UnknownField { path, .. } => path,
        }
    }

    /// The offending field, when the error has one.
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::MissingField { field, .. } => Some(field),
            Self::InvalidField { field, .. } | Self::UnknownField { field, .. } => Some(field),
            _ => None,
        }
    }
}

impl fmt::Display for ThemeFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(formatter, "{}: {message}", path.display())
            }
            Self::Parse { path, message } => {
                write!(formatter, "{}: {message}", path.display())
            }
            Self::MissingField { path, field } => {
                write!(
                    formatter,
                    "{}: missing required field `{field}`",
                    path.display()
                )
            }
            Self::UnsupportedVersion { path, found } => write!(
                formatter,
                "{}: unsupported theme version {found} (this facet reads version {THEME_FILE_VERSION})",
                path.display()
            ),
            Self::InvalidField {
                path,
                field,
                reason,
            } => write!(formatter, "{}: {field}: {reason}", path.display()),
            Self::UnknownField { path, field } => write!(
                formatter,
                "{}: unknown token `{field}` (see docs/FACET.md § Theme files)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ThemeFileError {}

/// The themes directory: `<config dir>/themes`.
pub fn themes_dir() -> Option<PathBuf> {
    lattice::machine_config_dir().map(|dir| dir.join("themes"))
}

/// Resolves a `--theme` / `:theme` argument to a file path: an explicit
/// path (contains a separator or ends in `.toml`) is used as-is; a bare
/// name resolves to `<themes dir>/<name>.toml`.
pub fn resolve_theme_path(argument: &str) -> Result<PathBuf, ThemeFileError> {
    let looks_like_path =
        argument.ends_with(".toml") || argument.contains('/') || argument.contains('\\');
    if looks_like_path {
        return Ok(PathBuf::from(argument));
    }
    let path = themes_dir()
        .map(|dir| dir.join(format!("{argument}.toml")))
        .ok_or_else(|| ThemeFileError::Io {
            path: PathBuf::from(argument),
            message: "no config directory for theme discovery".to_string(),
        })?;
    Ok(path)
}

/// Loads a named vendored Omarchy colors.toml palette.
pub fn load_named(name: &str) -> Result<ThemeFile, ThemeFileError> {
    let (path, text) = match name {
        "hedron" => (
            "themes/omarchy/hedron/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/hedron/colors.toml"
            )),
        ),
        "hedron-light" => (
            "themes/omarchy/hedron-light/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/hedron-light/colors.toml"
            )),
        ),
        "catppuccin" => (
            "themes/omarchy/catppuccin/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/catppuccin/colors.toml"
            )),
        ),
        "gruvbox" => (
            "themes/omarchy/gruvbox/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/gruvbox/colors.toml"
            )),
        ),
        "nord" => (
            "themes/omarchy/nord/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/nord/colors.toml"
            )),
        ),
        "rose-pine" => (
            "themes/omarchy/rose-pine/colors.toml",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../themes/omarchy/rose-pine/colors.toml"
            )),
        ),
        _ => {
            return Err(ThemeFileError::Io {
                path: PathBuf::from(name),
                message: "unknown vendored theme".to_owned(),
            });
        }
    };
    ThemeFile::parse_omarchy(Path::new(path), name, text)
}

/// Reads the live Omarchy theme, when the desktop state exists.
pub fn load_omarchy() -> Result<Option<ThemeFile>, ThemeFileError> {
    let Some(home) = std::env::home_dir() else {
        return Ok(None);
    };
    let root = home.join(".local/state/omarchy/current/theme");
    let colors = root.join("colors.toml");
    if !colors.is_file() {
        return Ok(None);
    }
    let name = std::fs::read_to_string(root.join("../theme.name"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "omarchy".to_owned());
    let text = std::fs::read_to_string(&colors).map_err(|error| ThemeFileError::Io {
        path: colors.clone(),
        message: error.to_string(),
    })?;
    ThemeFile::parse_omarchy(&colors, &name, &text).map(Some)
}

/// One entry of `facet theme list`: a built-in or a file on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThemeEntry {
    /// Theme name (`graphite` / `porcelain` for built-ins).
    pub name: String,
    /// `built-in` or `file`.
    pub source: &'static str,
    /// File path for file themes.
    pub path: Option<PathBuf>,
    /// Base appearance for file themes that parse.
    pub extends: Option<Appearance>,
    /// Validation error text for files that do not parse.
    pub error: Option<String>,
}

/// Built-ins first, then every `*.toml` in the themes directory (sorted),
/// validated but never fatal — invalid files list with their error.
pub fn list_themes() -> Vec<ThemeEntry> {
    let mut entries = vec![
        ThemeEntry {
            name: "graphite".to_string(),
            source: "built-in",
            path: None,
            extends: Some(Appearance::Dark),
            error: None,
        },
        ThemeEntry {
            name: "porcelain".to_string(),
            source: "built-in",
            path: None,
            extends: Some(Appearance::Light),
            error: None,
        },
    ];

    let Some(dir) = themes_dir() else {
        return entries;
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return entries;
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    for path in files {
        let entry = match ThemeFile::load(&path) {
            Ok(theme) => ThemeEntry {
                name: theme.name().to_string(),
                source: "file",
                path: Some(path),
                extends: Some(theme.extends()),
                error: None,
            },
            Err(error) => ThemeEntry {
                name: path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "theme".to_string()),
                source: "file",
                path: Some(path),
                extends: None,
                error: Some(error.to_string()),
            },
        };
        entries.push(entry);
    }
    entries
}

/// `#rrggbb` → RGB. The only accepted color literal.
fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// Applies one `[colors]` token. Returns `false` for a token this schema
/// version does not define. The token names are the public contract;
/// docs/FACET.md § Theme files lists them in the same groups.
fn set_token(palette: &mut Palette, token: &str, color: Color) -> bool {
    match token {
        // Surfaces (role fg/bg pairs).
        "window_fg" => palette.window.fg = color,
        "window_bg" => palette.window.bg = color,
        "sidebar_fg" => palette.sidebar.fg = color,
        "sidebar_bg" => palette.sidebar.bg = color,
        "editor_fg" => palette.editor.fg = color,
        "editor_bg" => palette.editor.bg = color,
        "raised_fg" => palette.raised.fg = color,
        "raised_bg" => palette.raised.bg = color,
        "overlay_fg" => palette.overlay.fg = color,
        "overlay_bg" => palette.overlay.bg = color,
        // Text.
        "text_primary" => palette.text_primary = color,
        "text_secondary" => palette.text_secondary = color,
        "text_muted" => palette.text_muted = color,
        "text_placeholder" => palette.text_placeholder = color,
        "text_inverse" => palette.text_inverse = color,
        // Borders.
        "border_subtle" => palette.border_subtle = color,
        "border_standard" => palette.border_standard = color,
        "border_strong" => palette.border_strong = color,
        "border_focused" => palette.border_focused = color,
        // Accent.
        "accent" => palette.accent = color,
        "accent_hover" => palette.accent_hover = color,
        "accent_pressed" => palette.accent_pressed = color,
        "accent_disabled" => palette.accent_disabled = color,
        "accent_disabled_fg" => palette.accent_disabled_fg = color,
        // Selection.
        "selection_active_bg" => palette.selection_active_bg = color,
        "selection_active_fg" => palette.selection_active_fg = color,
        "selection_inactive_bg" => palette.selection_inactive_bg = color,
        "selection_inactive_fg" => palette.selection_inactive_fg = color,
        // Status.
        "status_success" => palette.status_success = color,
        "status_warning" => palette.status_warning = color,
        "status_error" => palette.status_error = color,
        "status_informational" => palette.status_informational = color,
        // HTTP methods.
        "method_get" => palette.method_get = color,
        "method_post" => palette.method_post = color,
        "method_put" => palette.method_put = color,
        "method_patch" => palette.method_patch = color,
        "method_delete" => palette.method_delete = color,
        "method_other" => palette.method_other = color,
        // Response status families.
        "response_informational" => palette.response_informational = color,
        "response_success" => palette.response_success = color,
        "response_redirect" => palette.response_redirect = color,
        "response_client_error" => palette.response_client_error = color,
        "response_server_error" => palette.response_server_error = color,
        // Syntax.
        "syntax_property" => palette.syntax_property = color,
        "syntax_string" => palette.syntax_string = color,
        "syntax_number" => palette.syntax_number = color,
        "syntax_boolean" => palette.syntax_boolean = color,
        "syntax_null" => palette.syntax_null = color,
        "syntax_punctuation" => palette.syntax_punctuation = color,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> &'static Path {
        Path::new("midnight-honey.toml")
    }

    #[test]
    fn valid_file_merges_overrides_with_the_base() {
        let theme = ThemeFile::parse(
            path(),
            r##"
version = 1
name = "midnight-honey"
extends = "graphite"

[colors]
accent = "#ff0000"
window_bg = "#101012"
"##,
        )
        .expect("valid theme");
        assert_eq!(theme.name(), "midnight-honey");
        assert_eq!(theme.extends(), Appearance::Dark);
        assert_eq!(theme.override_count(), 2);
        assert_eq!(theme.palette().accent, Color::Rgb(255, 0, 0));
        assert_eq!(theme.palette().window.bg, Color::Rgb(16, 16, 18));
        // Untouched tokens come from Graphite Honey.
        assert_eq!(
            theme.palette().text_primary,
            Palette::graphite_honey().text_primary
        );
    }

    #[test]
    fn name_defaults_to_the_file_stem() {
        let theme = ThemeFile::parse(path(), "version = 1\nextends = \"porcelain\"\n").unwrap();
        assert_eq!(theme.name(), "midnight-honey");
        assert_eq!(theme.extends(), Appearance::Light);
        assert_eq!(theme.override_count(), 0);
    }

    #[test]
    fn unsupported_version_rejects_the_file() {
        let error = ThemeFile::parse(path(), "version = 2\nextends = \"graphite\"\n").unwrap_err();
        assert_eq!(
            error,
            ThemeFileError::UnsupportedVersion {
                path: path().to_path_buf(),
                found: 2,
            }
        );
    }

    #[test]
    fn missing_version_rejects_the_file() {
        let error = ThemeFile::parse(path(), "extends = \"graphite\"\n").unwrap_err();
        assert_eq!(
            error,
            ThemeFileError::MissingField {
                path: path().to_path_buf(),
                field: "version",
            }
        );
    }

    #[test]
    fn bad_hex_names_the_field() {
        let error = ThemeFile::parse(
            path(),
            "version = 1\nextends = \"graphite\"\n[colors]\naccent = \"red\"\n",
        )
        .unwrap_err();
        assert_eq!(
            error,
            ThemeFileError::InvalidField {
                path: path().to_path_buf(),
                field: "colors.accent".to_string(),
                reason: "expected \"#rrggbb\", got \"red\"".to_string(),
            }
        );
    }

    #[test]
    fn unknown_color_token_rejects_the_file() {
        let error = ThemeFile::parse(
            path(),
            "version = 1\nextends = \"graphite\"\n[colors]\naccnet = \"#ff0000\"\n",
        )
        .unwrap_err();
        assert_eq!(
            error,
            ThemeFileError::UnknownField {
                path: path().to_path_buf(),
                field: "colors.accnet".to_string(),
            }
        );
    }

    #[test]
    fn unknown_top_level_keys_are_tolerated() {
        // Additive growth inside a version: a future optional key must not
        // break this binary.
        let theme = ThemeFile::parse(
            path(),
            "version = 1\nextends = \"graphite\"\nauthor = \"evan\"\n",
        )
        .unwrap();
        assert_eq!(theme.extends(), Appearance::Dark);
    }

    #[test]
    fn invalid_extends_rejects_the_file() {
        let error = ThemeFile::parse(path(), "version = 1\nextends = \"neon\"\n").unwrap_err();
        assert!(matches!(
            error,
            ThemeFileError::InvalidField { field, .. } if field == "extends"
        ));
    }

    #[test]
    fn syntax_error_is_a_parse_error() {
        let error = ThemeFile::parse(path(), "version = = 1\n").unwrap_err();
        assert!(matches!(error, ThemeFileError::Parse { .. }));
    }

    #[test]
    fn uppercase_hex_is_accepted() {
        let theme = ThemeFile::parse(
            path(),
            "version = 1\nextends = \"graphite\"\n[colors]\naccent = \"#E7821B\"\n",
        )
        .unwrap();
        assert_eq!(theme.palette().accent, Color::Rgb(0xe7, 0x82, 0x1b));
    }

    #[test]
    fn theme_builds_at_any_depth() {
        let file = ThemeFile::parse(path(), "version = 1\nextends = \"graphite\"\n").unwrap();
        let theme = file.theme(Depth::Indexed);
        assert_eq!(theme.appearance(), Appearance::Dark);
        assert_eq!(theme.depth(), Depth::Indexed);
    }

    #[test]
    fn omarchy_palette_maps_known_keys_and_keeps_missing_defaults() {
        let file = ThemeFile::parse_omarchy(
            Path::new("colors.toml"),
            "Omahedron",
            r##"mode = "light"
accent = "#010203"
foreground = "#040506"
"##,
        )
        .unwrap();
        assert_eq!(file.name(), "Omahedron");
        assert_eq!(file.extends(), Appearance::Light);
        assert_eq!(file.palette().accent, Color::Rgb(1, 2, 3));
        assert_eq!(file.palette().text_primary, Color::Rgb(4, 5, 6));
        assert_eq!(
            file.palette().text_muted,
            Palette::porcelain_honey().text_muted
        );
    }

    #[test]
    fn vendored_omarchy_theme_is_selectable() {
        let theme = super::load_named("hedron").unwrap();
        assert_eq!(theme.name(), "hedron");
        assert_eq!(theme.extends(), Appearance::Dark);
    }
}
