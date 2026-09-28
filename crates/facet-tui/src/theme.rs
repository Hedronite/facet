//! Semantic design tokens for the Facet TUI.
//!
//! Maps the Porcelain Honey (light) and Graphite Honey (dark) palettes
//! onto a role-based token model. Their palette values retain historical
//! provenance from `crates/desktop/src/theme.rs` at audited source SHA
//! `2c6ecfd` (Apache-2.0), as recorded in the root NOTICE. Components consume
//! `Theme`/`Style` values rather than embedding raw colors so appearance can
//! change without touching call sites.
//!
//! The palette constants remain intentionally duplicated; this TUI has no
//! dependency on the retired GPUI desktop crate.
//!
//! Three resolvers are provided per token, selected by the terminal's
//! color depth:
//! 1. Truecolor — `Color::Rgb` from the canonical hex.
//! 2. 256-color — `Color::Indexed` chosen to keep the domain colors
//!    distinct (GET teal, success jade, POST/honey distinct from DELETE).
//! 3. 16-color — named ANSI. The resolver never collapses GET/POST/DELETE
//!    into the same channel; on 16-color GET is Cyan, POST is Yellow+Bold,
//!    DELETE is LightRed, and 2xx is LightGreen.

use ratatui::style::{Color, Modifier, Style};

/// Light or dark palette selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Appearance {
    /// Porcelain Honey — warm porcelain light, golden-orange accent.
    Light,
    /// Graphite Honey — deep carbon dark, golden-orange accent.
    Dark,
}

impl Appearance {
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Porcelain Honey",
            Self::Dark => "Graphite Honey",
        }
    }

    /// Parses `--appearance` values. Graphite is the dark default;
    /// Porcelain is the light appearance.
    pub fn from_flag(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "graphite" | "dark" | "graphite-honey" => Some(Self::Dark),
            "porcelain" | "light" | "porcelain-honey" => Some(Self::Light),
            _ => None,
        }
    }
}

/// One role's foreground/background pair.
#[derive(Clone, Copy, Debug)]
pub struct Role {
    pub fg: Color,
    pub bg: Color,
}

impl Role {
    pub const fn new(fg: Color, bg: Color) -> Self {
        Self { fg, bg }
    }
}

/// Full semantic palette for one appearance.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub appearance: Appearance,
    pub window: Role,
    pub sidebar: Role,
    pub editor: Role,
    pub raised: Role,
    pub overlay: Role,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub text_placeholder: Color,
    pub text_inverse: Color,
    pub border_subtle: Color,
    pub border_standard: Color,
    pub border_strong: Color,
    pub border_focused: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    pub accent_disabled: Color,
    pub accent_disabled_fg: Color,
    pub selection_active_bg: Color,
    pub selection_active_fg: Color,
    pub selection_inactive_bg: Color,
    pub selection_inactive_fg: Color,
    pub status_success: Color,
    pub status_warning: Color,
    pub status_error: Color,
    pub status_informational: Color,
    pub method_get: Color,
    pub method_post: Color,
    pub method_put: Color,
    pub method_patch: Color,
    pub method_delete: Color,
    pub method_other: Color,
    pub response_informational: Color,
    pub response_success: Color,
    pub response_redirect: Color,
    pub response_client_error: Color,
    pub response_server_error: Color,
    pub syntax_property: Color,
    pub syntax_string: Color,
    pub syntax_number: Color,
    pub syntax_boolean: Color,
    pub syntax_null: Color,
    pub syntax_punctuation: Color,
}

impl Palette {
    /// Porcelain Honey — warm porcelain light theme.
    pub const fn porcelain_honey() -> Self {
        Self {
            appearance: Appearance::Light,
            window: Role::new(rgb(0x4b4641), rgb(0xf8f6f2)),
            sidebar: Role::new(rgb(0x4b4641), rgb(0xf1eee9)),
            editor: Role::new(rgb(0x4b4641), rgb(0xfaf8f5)),
            raised: Role::new(rgb(0x4b4641), rgb(0xfcf9f5)),
            overlay: Role::new(rgb(0x4b4641), rgb(0xfdfbf8)),
            text_primary: rgb(0x4b4641),
            text_secondary: rgb(0x6c655e),
            text_muted: rgb(0x8b837a),
            text_placeholder: rgb(0xa9a199),
            text_inverse: rgb(0xffffff),
            border_subtle: rgb(0xe7e1d9),
            border_standard: rgb(0xdcd4cb),
            border_strong: rgb(0xc5b9ad),
            border_focused: rgb(0xe7821b),
            accent: rgb(0xe7821b),
            accent_hover: rgb(0xd96f10),
            accent_pressed: rgb(0xb9570c),
            accent_disabled: rgb(0xe7e1d9),
            accent_disabled_fg: rgb(0xa9a199),
            selection_active_bg: rgb(0xe7821b),
            selection_active_fg: rgb(0xffffff),
            selection_inactive_bg: rgb(0xebe5de),
            selection_inactive_fg: rgb(0x4b4641),
            status_success: rgb(0x2d8a5b),
            status_warning: rgb(0xe7821b),
            status_error: rgb(0xc43d3d),
            status_informational: rgb(0x227c8f),
            method_get: rgb(0x1f8a70),
            method_post: rgb(0xe7821b),
            method_put: rgb(0xaa7d24),
            method_patch: rgb(0x7c5bbd),
            method_delete: rgb(0xc43d3d),
            method_other: rgb(0x7a7469),
            response_informational: rgb(0x227c8f),
            response_success: rgb(0x2d8a5b),
            response_redirect: rgb(0xaa7d24),
            response_client_error: rgb(0xe7821b),
            response_server_error: rgb(0xc43d3d),
            syntax_property: rgb(0x227c8f),
            syntax_string: rgb(0x2d8a5b),
            syntax_number: rgb(0xe7821b),
            syntax_boolean: rgb(0x7c5bbd),
            syntax_null: rgb(0x7a7469),
            syntax_punctuation: rgb(0xa9a199),
        }
    }

    /// Graphite Honey — deep carbon dark theme, paired with Porcelain Honey.
    pub const fn graphite_honey() -> Self {
        Self {
            appearance: Appearance::Dark,
            window: Role::new(rgb(0xe9e6e0), rgb(0x1d1e22)),
            sidebar: Role::new(rgb(0xe9e6e0), rgb(0x1a1b1f)),
            editor: Role::new(rgb(0xe9e6e0), rgb(0x222327)),
            raised: Role::new(rgb(0xe9e6e0), rgb(0x222327)),
            overlay: Role::new(rgb(0xe9e6e0), rgb(0x292a2e)),
            text_primary: rgb(0xe9e6e0),
            text_secondary: rgb(0xcac5bd),
            text_muted: rgb(0xa7a19a),
            text_placeholder: rgb(0x807c76),
            text_inverse: rgb(0x1d1e22),
            border_subtle: rgb(0x2b2c31),
            border_standard: rgb(0x3c3e43),
            border_strong: rgb(0x57585d),
            border_focused: rgb(0xd98e26),
            accent: rgb(0xd98e26),
            accent_hover: rgb(0xf0a338),
            accent_pressed: rgb(0xb97620),
            accent_disabled: rgb(0x2b2c31),
            accent_disabled_fg: rgb(0x807c76),
            selection_active_bg: rgb(0xd98e26),
            selection_active_fg: rgb(0x1d1e22),
            selection_inactive_bg: rgb(0x2b2c31),
            selection_inactive_fg: rgb(0xe9e6e0),
            status_success: rgb(0x79d19a),
            status_warning: rgb(0xe39a32),
            status_error: rgb(0xff7f7f),
            status_informational: rgb(0x75c6d4),
            method_get: rgb(0x72d6c2),
            method_post: rgb(0xe39a32),
            method_put: rgb(0xcaa95f),
            method_patch: rgb(0xb89af7),
            method_delete: rgb(0xff7f7f),
            method_other: rgb(0xa6a39e),
            response_informational: rgb(0x75c6d4),
            response_success: rgb(0x79d19a),
            response_redirect: rgb(0xcaa95f),
            response_client_error: rgb(0xe39a32),
            response_server_error: rgb(0xff7f7f),
            syntax_property: rgb(0x75c6d4),
            syntax_string: rgb(0x79d19a),
            syntax_number: rgb(0xe39a32),
            syntax_boolean: rgb(0xb89af7),
            syntax_null: rgb(0xa6a39e),
            syntax_punctuation: rgb(0x85817b),
        }
    }

    /// Resolves the palette for the chosen appearance.
    pub const fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::porcelain_honey(),
            Appearance::Dark => Self::graphite_honey(),
        }
    }

    /// Returns the method color for an HTTP method name.
    pub fn method(&self, method: &str) -> Color {
        match method.to_ascii_uppercase().as_str() {
            "GET" => self.method_get,
            "POST" => self.method_post,
            "PUT" => self.method_put,
            "PATCH" => self.method_patch,
            "DELETE" => self.method_delete,
            _ => self.method_other,
        }
    }

    /// Returns the response color for a numeric HTTP status code.
    pub fn response_status(&self, status: u16) -> Color {
        match status {
            100..=199 => self.response_informational,
            200..=299 => self.response_success,
            300..=399 => self.response_redirect,
            400..=499 => self.response_client_error,
            500..=599 => self.response_server_error,
            _ => self.text_muted,
        }
    }
}

/// Color depth advertised by the terminal. Drives the resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Depth {
    Truecolor,
    Indexed,
    Ansi16,
}

impl Depth {
    /// Picks a depth from the `COLORTERM` / `TERM` environment the way
    /// `ratatui` and most TUIs do.
    pub fn from_env() -> Self {
        let color_term = std::env::var("COLORTERM")
            .ok()
            .map(|value| value.to_ascii_lowercase());
        if color_term.is_some_and(|value| value.contains("truecolor") || value.contains("24bit")) {
            return Self::Truecolor;
        }
        let term = std::env::var("TERM").ok().unwrap_or_default();
        if term.contains("256color") || term.contains("256") {
            return Self::Indexed;
        }
        if term.is_empty() {
            return Self::Truecolor;
        }
        Self::Ansi16
    }
}

/// Active theme binding the palette to a concrete appearance + depth.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    appearance: Appearance,
    palette: Palette,
    depth: Depth,
}

impl Theme {
    pub const fn new(appearance: Appearance) -> Self {
        Self {
            appearance,
            palette: Palette::for_appearance(appearance),
            depth: Depth::Truecolor,
        }
    }

    /// A theme over a custom palette (a theme file's overrides merged into
    /// a built-in base). The appearance stays the base's, so lower depths
    /// resolve through the built-in role channels.
    pub const fn from_palette(appearance: Appearance, palette: Palette) -> Self {
        Self {
            appearance,
            palette,
            depth: Depth::Truecolor,
        }
    }

    pub fn with_depth(mut self, depth: Depth) -> Self {
        self.depth = depth;
        self
    }

    pub const fn appearance(self) -> Appearance {
        self.appearance
    }

    pub const fn palette(self) -> Palette {
        self.palette
    }

    pub const fn depth(self) -> Depth {
        self.depth
    }

    pub fn toggle(self) -> Self {
        let next = match self.appearance {
            Appearance::Light => Appearance::Dark,
            Appearance::Dark => Appearance::Light,
        };
        Self {
            appearance: next,
            palette: Palette::for_appearance(next),
            depth: self.depth,
        }
    }

    /// Returns a `Theme` chosen from `COLORFGBG` when set: light background
    /// implies Porcelain, dark background implies Graphite. Falls back to
    /// Graphite Honey (the dark default per the flavor note).
    pub fn detect_default() -> Self {
        let appearance = std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| {
                let last = value.rsplit(';').next()?;
                match last {
                    "black" | "0" | "15" => Some(Appearance::Dark),
                    "white" | "7" | "7;" | "15;" => Some(Appearance::Light),
                    _ => None,
                }
            })
            .unwrap_or(Appearance::Dark);
        Self::new(appearance).with_depth(Depth::from_env())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::detect_default()
    }
}

/// 256-color resolver. Keep GET, success, POST, and DELETE in distinct
/// channels; near-truecolor hexes collapse gracefully but never blur the
/// method families.
pub fn indexed(appearance: Appearance, kind: Role256) -> Color {
    use Role256::*;
    match (appearance, kind) {
        (Appearance::Light, Window) => Color::Indexed(255),
        (Appearance::Light, Sidebar) => Color::Indexed(255),
        (Appearance::Light, Editor) => Color::Indexed(231),
        (Appearance::Light, Raised) => Color::Indexed(231),
        (Appearance::Light, Overlay) => Color::Indexed(231),
        (Appearance::Light, TextPrimary) => Color::Indexed(239),
        (Appearance::Light, TextMuted) => Color::Indexed(245),
        (Appearance::Light, Border) => Color::Indexed(252),
        (Appearance::Light, BorderStrong) => Color::Indexed(251),
        (Appearance::Light, Accent) => Color::Indexed(208),
        (Appearance::Light, AccentInverse) => Color::Indexed(231),
        (Appearance::Light, MethodGet) => Color::Indexed(36),
        (Appearance::Light, MethodPost) => Color::Indexed(208),
        (Appearance::Light, MethodPut) => Color::Indexed(136),
        (Appearance::Light, MethodPatch) => Color::Indexed(97),
        (Appearance::Light, MethodDelete) => Color::Indexed(167),
        (Appearance::Light, MethodOther) => Color::Indexed(244),
        (Appearance::Light, StatusSuccess) => Color::Indexed(29),
        (Appearance::Light, StatusInformational) => Color::Indexed(30),
        (Appearance::Light, StatusError) => Color::Indexed(167),

        (Appearance::Dark, Window) => Color::Indexed(234),
        (Appearance::Dark, Sidebar) => Color::Indexed(234),
        (Appearance::Dark, Editor) => Color::Indexed(235),
        (Appearance::Dark, Raised) => Color::Indexed(235),
        (Appearance::Dark, Overlay) => Color::Indexed(236),
        (Appearance::Dark, TextPrimary) => Color::Indexed(254),
        (Appearance::Dark, TextMuted) => Color::Indexed(247),
        (Appearance::Dark, Border) => Color::Indexed(238),
        (Appearance::Dark, BorderStrong) => Color::Indexed(239),
        (Appearance::Dark, Accent) => Color::Indexed(172),
        (Appearance::Dark, AccentInverse) => Color::Indexed(234),
        (Appearance::Dark, MethodGet) => Color::Indexed(79),
        (Appearance::Dark, MethodPost) => Color::Indexed(214),
        (Appearance::Dark, MethodPut) => Color::Indexed(179),
        (Appearance::Dark, MethodPatch) => Color::Indexed(147),
        (Appearance::Dark, MethodDelete) => Color::Indexed(210),
        (Appearance::Dark, MethodOther) => Color::Indexed(247),
        (Appearance::Dark, StatusSuccess) => Color::Indexed(114),
        (Appearance::Dark, StatusInformational) => Color::Indexed(116),
        (Appearance::Dark, StatusError) => Color::Indexed(210),
    }
}

/// 16-color resolver. The dark/light split picks the background tone
/// (Black vs White); the rest of the slots are chosen so GET, POST,
/// DELETE, 2xx, and 4xx remain distinct without collapsing to Yellow.
pub fn ansi(appearance: Appearance, kind: Role16) -> Color {
    use Role16::*;
    match (appearance, kind) {
        (Appearance::Dark, Background) => Color::Black,
        (Appearance::Dark, TextPrimary) => Color::White,
        (Appearance::Dark, TextMuted) => Color::DarkGray,
        (Appearance::Dark, Border) => Color::DarkGray,
        (Appearance::Dark, Accent) => Color::Yellow,
        (Appearance::Dark, AccentInverse) => Color::Black,
        (Appearance::Dark, MethodGet) => Color::LightCyan,
        (Appearance::Dark, MethodPost) => Color::Yellow,
        (Appearance::Dark, MethodPut) => Color::White,
        (Appearance::Dark, MethodPatch) => Color::Magenta,
        (Appearance::Dark, MethodDelete) => Color::LightRed,
        (Appearance::Dark, MethodOther) => Color::Gray,
        (Appearance::Dark, StatusSuccess) => Color::LightGreen,
        (Appearance::Dark, StatusInformational) => Color::Cyan,
        (Appearance::Dark, StatusError) => Color::LightRed,

        (Appearance::Light, Background) => Color::White,
        (Appearance::Light, TextPrimary) => Color::Black,
        (Appearance::Light, TextMuted) => Color::DarkGray,
        (Appearance::Light, Border) => Color::Gray,
        (Appearance::Light, Accent) => Color::Yellow,
        (Appearance::Light, AccentInverse) => Color::White,
        (Appearance::Light, MethodGet) => Color::Cyan,
        (Appearance::Light, MethodPost) => Color::Yellow,
        (Appearance::Light, MethodPut) => Color::Black,
        (Appearance::Light, MethodPatch) => Color::Magenta,
        (Appearance::Light, MethodDelete) => Color::Red,
        (Appearance::Light, MethodOther) => Color::DarkGray,
        (Appearance::Light, StatusSuccess) => Color::Green,
        (Appearance::Light, StatusInformational) => Color::Blue,
        (Appearance::Light, StatusError) => Color::Red,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role256 {
    Window,
    Sidebar,
    #[allow(dead_code)] // Retained palette role; current resolver shares Window.
    Editor,
    Raised,
    Overlay,
    TextPrimary,
    TextMuted,
    Border,
    BorderStrong,
    Accent,
    AccentInverse,
    MethodGet,
    #[allow(dead_code)] // Retained palette role; current resolver shares Accent.
    MethodPost,
    MethodPut,
    MethodPatch,
    MethodDelete,
    MethodOther,
    StatusSuccess,
    StatusInformational,
    StatusError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role16 {
    Background,
    TextPrimary,
    TextMuted,
    Border,
    Accent,
    AccentInverse,
    MethodGet,
    #[allow(dead_code)] // Retained palette role; current resolver shares Accent.
    MethodPost,
    MethodPut,
    MethodPatch,
    MethodDelete,
    MethodOther,
    StatusSuccess,
    StatusInformational,
    StatusError,
}

/// Resolves a semantic token through the active depth. `palette` is the
/// canonical truecolor palette; this maps it to indexed/16-color when
/// the terminal cannot render truecolor.
pub fn resolve(theme: Theme, palette_token: PaletteToken) -> Color {
    let palette = theme.palette();
    let truth = palette_token.truth(&palette);
    match theme.depth() {
        Depth::Truecolor => truth,
        Depth::Indexed => indexed(theme.appearance(), palette_token.into_role256(truth)),
        Depth::Ansi16 => ansi(theme.appearance(), palette_token.into_role16(truth)),
    }
}

/// Identifies which role a token is, so resolvers can pick a sensible
/// channel without re-implementing the palette mapping per widget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteToken {
    WindowFg,
    WindowBg,
    SidebarBg,
    EditorBg,
    RaisedBg,
    OverlayBg,
    TextPrimary,
    TextSecondary,
    TextMuted,
    TextPlaceholder,
    TextInverse,
    BorderSubtle,
    BorderStandard,
    BorderStrong,
    Accent,
    AccentInverse,
    MethodGet,
    MethodPost,
    MethodPut,
    MethodPatch,
    MethodDelete,
    MethodOther,
    StatusSuccess,
    StatusInformational,
    StatusError,
    StatusWarning,
    SelectionInactiveBg,
    SelectionInactiveFg,
}

impl PaletteToken {
    fn truth(self, palette: &Palette) -> Color {
        match self {
            Self::WindowFg => palette.window.fg,
            Self::WindowBg => palette.window.bg,
            Self::SidebarBg => palette.sidebar.bg,
            Self::EditorBg => palette.editor.bg,
            Self::RaisedBg => palette.raised.bg,
            Self::OverlayBg => palette.overlay.bg,
            Self::TextPrimary => palette.text_primary,
            Self::TextSecondary => palette.text_secondary,
            Self::TextMuted => palette.text_muted,
            Self::TextPlaceholder => palette.text_placeholder,
            Self::TextInverse => palette.text_inverse,
            Self::BorderSubtle => palette.border_subtle,
            Self::BorderStandard => palette.border_standard,
            Self::BorderStrong => palette.border_strong,
            Self::Accent => palette.accent,
            Self::AccentInverse => palette.text_inverse,
            Self::MethodGet => palette.method_get,
            Self::MethodPost => palette.method_post,
            Self::MethodPut => palette.method_put,
            Self::MethodPatch => palette.method_patch,
            Self::MethodDelete => palette.method_delete,
            Self::MethodOther => palette.method_other,
            Self::StatusSuccess => palette.status_success,
            Self::StatusInformational => palette.status_informational,
            Self::StatusError => palette.status_error,
            Self::StatusWarning => palette.status_warning,
            Self::SelectionInactiveBg => palette.selection_inactive_bg,
            Self::SelectionInactiveFg => palette.selection_inactive_fg,
        }
    }

    fn into_role256(self, _truth: Color) -> Role256 {
        match self {
            Self::WindowBg | Self::EditorBg => Role256::Window,
            Self::SidebarBg => Role256::Sidebar,
            Self::RaisedBg => Role256::Raised,
            Self::OverlayBg => Role256::Overlay,
            Self::TextPrimary | Self::TextSecondary => Role256::TextPrimary,
            Self::TextMuted | Self::TextPlaceholder => Role256::TextMuted,
            Self::BorderSubtle | Self::BorderStandard => Role256::Border,
            Self::BorderStrong => Role256::BorderStrong,
            Self::Accent | Self::MethodPost | Self::StatusWarning => Role256::Accent,
            Self::TextInverse | Self::AccentInverse => Role256::AccentInverse,
            Self::MethodGet => Role256::MethodGet,
            Self::MethodPut => Role256::MethodPut,
            Self::MethodPatch => Role256::MethodPatch,
            Self::MethodDelete => Role256::MethodDelete,
            Self::MethodOther => Role256::MethodOther,
            Self::StatusSuccess => Role256::StatusSuccess,
            Self::StatusInformational => Role256::StatusInformational,
            Self::StatusError => Role256::StatusError,
            Self::WindowFg => Role256::TextPrimary,
            Self::SelectionInactiveBg => Role256::Overlay,
            Self::SelectionInactiveFg => Role256::TextPrimary,
        }
    }

    fn into_role16(self, _truth: Color) -> Role16 {
        match self {
            Self::WindowBg
            | Self::EditorBg
            | Self::SidebarBg
            | Self::RaisedBg
            | Self::OverlayBg => Role16::Background,
            Self::TextPrimary | Self::TextSecondary => Role16::TextPrimary,
            Self::TextMuted | Self::TextPlaceholder => Role16::TextMuted,
            Self::BorderSubtle | Self::BorderStandard | Self::BorderStrong => Role16::Border,
            Self::Accent | Self::MethodPost | Self::StatusWarning => Role16::Accent,
            Self::TextInverse | Self::AccentInverse => Role16::AccentInverse,
            Self::MethodGet => Role16::MethodGet,
            Self::MethodPut => Role16::MethodPut,
            Self::MethodPatch => Role16::MethodPatch,
            Self::MethodDelete => Role16::MethodDelete,
            Self::MethodOther => Role16::MethodOther,
            Self::StatusSuccess => Role16::StatusSuccess,
            Self::StatusInformational => Role16::StatusInformational,
            Self::StatusError => Role16::StatusError,
            Self::WindowFg => Role16::TextPrimary,
            Self::SelectionInactiveBg => Role16::Border,
            Self::SelectionInactiveFg => Role16::TextPrimary,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Styles {
    pub base: Style,
    pub title: Style,
    pub border: Style,
    pub title_focused: Style,
    pub status_idle: Style,
    pub status_running: Style,
    pub status_success: Style,
    pub status_error: Style,
    pub selected_row: Style,
    pub muted: Style,
    pub method: Style,
    pub url: Style,
    pub sidebar: Style,
    pub editor: Style,
    pub stone_pill: Style,
    pub placeholder: Style,
    pub accent_text: Style,
    /// Wordmark and footer brackets: bold accent on the window ground.
    pub brand: Style,
    /// Lattice nodes on the splash.
    pub brand_bright: Style,
    /// Lattice edges on the splash.
    pub brand_line: Style,
    /// Filled brand band: carbon-on-gold (text inverse on accent), bold.
    /// Reserved for the splash hexagon outline — the one place the brand
    /// gets a filled frame instead of a line.
    pub brand_fill: Style,
}

impl Styles {
    /// Builds a coherent set of `Style` values for the given theme. The
    /// chrome here never paints a pane border in the accent color;
    /// accent is reserved for fills (selected row, Send) and the title.
    pub fn for_theme(theme: Theme) -> Self {
        let palette = theme.palette();
        let window_bg = resolve(theme, PaletteToken::WindowBg);
        let text_primary = resolve(theme, PaletteToken::TextPrimary);
        let text_inverse = resolve(theme, PaletteToken::TextInverse);
        let border = resolve(theme, PaletteToken::BorderStandard);
        let accent = resolve(theme, PaletteToken::Accent);
        let muted = resolve(theme, PaletteToken::TextMuted);

        let base = Style::new().fg(text_primary).bg(window_bg);
        let title = Style::new()
            .fg(text_inverse)
            .bg(accent)
            .add_modifier(Modifier::BOLD);
        let title_focused = Style::new()
            .fg(accent)
            .bg(window_bg)
            .add_modifier(Modifier::BOLD);
        let status_idle = base.fg(muted);
        let status_running = base
            .fg(resolve(theme, PaletteToken::StatusInformational))
            .add_modifier(Modifier::BOLD);
        let status_success = base
            .fg(resolve(theme, PaletteToken::StatusSuccess))
            .add_modifier(Modifier::BOLD);
        let status_error = base
            .fg(resolve(theme, PaletteToken::StatusError))
            .add_modifier(Modifier::BOLD);
        let selected_row = base
            .fg(text_inverse)
            .bg(accent)
            .add_modifier(Modifier::BOLD);
        let muted_style = base.fg(muted);
        let method = base.add_modifier(Modifier::BOLD);
        let url = base.fg(resolve(theme, PaletteToken::TextSecondary));
        let sidebar_bg = resolve(theme, PaletteToken::SidebarBg);
        let editor_bg = resolve(theme, PaletteToken::EditorBg);
        let sidebar = Style::new().fg(text_primary).bg(sidebar_bg);
        let editor = Style::new().fg(text_primary).bg(editor_bg);
        let stone_pill = Style::new()
            .fg(resolve(theme, PaletteToken::SelectionInactiveFg))
            .bg(resolve(theme, PaletteToken::SelectionInactiveBg))
            .add_modifier(Modifier::BOLD);
        let placeholder = base.fg(resolve(theme, PaletteToken::TextPlaceholder));
        let accent_text = base.fg(accent).add_modifier(Modifier::BOLD);
        let brand_line = base.fg(accent);
        let brand = brand_line.add_modifier(Modifier::BOLD);
        let brand_bright = brand;
        let brand_fill = Style::new()
            .fg(text_inverse)
            .bg(accent)
            .add_modifier(Modifier::BOLD);

        let _ = palette;
        Self {
            base,
            title,
            border: base.fg(border),
            title_focused,
            status_idle,
            status_running,
            status_success,
            status_error,
            selected_row,
            muted: muted_style,
            method,
            url,
            sidebar,
            editor,
            stone_pill,
            placeholder,
            accent_text,
            brand,
            brand_bright,
            brand_line,
            brand_fill,
        }
    }
}

const fn rgb(value: u32) -> Color {
    Color::Rgb(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_appearances_round_trip() {
        let light = Theme::new(Appearance::Light);
        let dark = Theme::new(Appearance::Dark);
        assert_eq!(light.appearance(), Appearance::Light);
        assert_eq!(dark.appearance(), Appearance::Dark);
        assert_ne!(
            light.palette().window.bg,
            dark.palette().window.bg,
            "light and dark must differ on window background",
        );
    }

    #[test]
    fn accent_is_golden_orange_in_both_themes() {
        let light = Theme::new(Appearance::Light).palette();
        let dark = Theme::new(Appearance::Dark).palette();
        let (lr, lg, lb) = rgb_channels(light.accent);
        let (dr, dg, db) = rgb_channels(dark.accent);
        for (r, g, b) in [(lr, lg, lb), (dr, dg, db)] {
            assert!(r > 180, "accent red channel should be high, got {r}");
            assert!(g > 100, "accent green channel should be moderate, got {g}");
            assert!(b < 80, "accent blue channel should be low, got {b}");
        }
    }

    #[test]
    fn method_color_uses_families() {
        let palette = Theme::new(Appearance::Light).palette();
        assert_eq!(palette.method("GET"), palette.method_get);
        assert_eq!(palette.method("get"), palette.method_get);
        assert_eq!(palette.method("PATCH"), palette.method_patch);
        assert_eq!(palette.method("WAT"), palette.method_other);
    }

    #[test]
    fn response_status_buckets() {
        let palette = Theme::new(Appearance::Light).palette();
        assert_eq!(palette.response_status(101), palette.response_informational);
        assert_eq!(palette.response_status(204), palette.response_success);
        assert_eq!(palette.response_status(307), palette.response_redirect);
        assert_eq!(palette.response_status(404), palette.response_client_error);
        assert_eq!(palette.response_status(503), palette.response_server_error);
    }

    #[test]
    fn ansi_keeps_methods_distinct() {
        let dark = Theme::new(Appearance::Dark);
        let get = ansi(dark.appearance(), Role16::MethodGet);
        let post = ansi(dark.appearance(), Role16::MethodPost);
        let delete = ansi(dark.appearance(), Role16::MethodDelete);
        let success = ansi(dark.appearance(), Role16::StatusSuccess);
        assert_ne!(get, post, "GET and POST must differ in 16-color dark");
        assert_ne!(get, success, "GET and 2xx must differ in 16-color dark");
        assert_ne!(post, delete, "POST and DELETE must differ in 16-color dark");
    }

    #[test]
    fn indexed_keeps_methods_distinct() {
        let dark = Theme::new(Appearance::Dark);
        let get = indexed(dark.appearance(), Role256::MethodGet);
        let post = indexed(dark.appearance(), Role256::MethodPost);
        let delete = indexed(dark.appearance(), Role256::MethodDelete);
        assert_ne!(get, post);
        assert_ne!(post, delete);
    }

    #[test]
    fn appearance_flag_parses_aliases() {
        assert_eq!(Appearance::from_flag("graphite"), Some(Appearance::Dark));
        assert_eq!(Appearance::from_flag("Dark"), Some(Appearance::Dark));
        assert_eq!(
            Appearance::from_flag("porcelain-honey"),
            Some(Appearance::Light)
        );
        assert_eq!(Appearance::from_flag("light"), Some(Appearance::Light));
        assert_eq!(Appearance::from_flag("neon"), None);
    }

    #[test]
    fn stone_pill_is_not_honey_fill() {
        let dark = Theme::new(Appearance::Dark);
        let styles = Styles::for_theme(dark);
        assert_ne!(
            styles.stone_pill.bg, styles.selected_row.bg,
            "section tabs must not use the tree's honey fill"
        );
        assert_eq!(
            styles.selected_row.fg,
            Some(dark.palette().text_inverse),
            "dark selected row / Send is carbon on gold"
        );
    }

    #[test]
    fn brand_fill_is_the_filled_brand_band() {
        // The splash hexagon outline: text-inverse on accent, bold, in
        // both appearances. Distinct from the line/bright brand styles.
        for appearance in [Appearance::Dark, Appearance::Light] {
            let theme = Theme::new(appearance);
            let styles = Styles::for_theme(theme);
            let palette = theme.palette();
            assert_eq!(styles.brand_fill.fg, Some(palette.text_inverse));
            assert_eq!(styles.brand_fill.bg, Some(palette.accent));
            assert_ne!(
                styles.brand_fill.bg, styles.brand_line.bg,
                "fill must stand apart from the line style"
            );
        }
    }

    #[test]
    fn dark_filled_uses_carbon_foreground() {
        // The selected-row pair in Graphite is carbon (#1D1E22) on
        // gold (#D98E26). Light uses white on orange. The flavor note
        // calls this out as a load-bearing detail.
        let dark = Theme::new(Appearance::Dark).palette();
        let light = Theme::new(Appearance::Light).palette();
        assert_eq!(dark.text_inverse, rgb(0x1d1e22));
        assert_eq!(dark.selection_active_bg, dark.accent);
        assert_eq!(dark.selection_active_fg, dark.text_inverse);
        assert_eq!(light.text_inverse, rgb(0xffffff));
        assert_eq!(light.selection_active_fg, light.text_inverse);
    }

    fn rgb_channels(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected Color::Rgb, got {other:?}"),
        }
    }
}
