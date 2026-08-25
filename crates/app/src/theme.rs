//! Ember palette — dark & light themes for the Dragon desktop app.

use gpui::Hsla;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: ThemeName,
    /// window background
    pub bg: Hsla,
    /// sidebar / chrome
    pub panel: Hsla,
    /// cards & bubbles
    pub surface: Hsla,
    /// inputs, hovers
    pub elevated: Hsla,
    /// hairlines & borders
    pub line: Hsla,
    /// primary text
    pub text: Hsla,
    /// secondary text
    pub muted: Hsla,
    /// tertiary text
    pub faint: Hsla,
    /// brand accent (dragon fire)
    pub ember: Hsla,
    /// accent readable as small text on this theme's background
    pub ember_text: Hsla,
    pub flame: Hsla,
    pub gold: Hsla,
    pub jade: Hsla,
    pub sky: Hsla,
    pub violet: Hsla,
    pub blood: Hsla,
}

pub const EMBER: u32 = 0xFF6347;
pub const FLAME: u32 = 0xFF984A;
pub const GOLD: u32 = 0xFFCD70;
pub const JADE: u32 = 0x69D296;
pub const SKY: u32 = 0x6CAAF5;
pub const VIOLET: u32 = 0xAC8CFA;
pub const BLOOD: u32 = 0xF05454;

impl Theme {
    pub fn new(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => Self {
                name,
                bg: rgb(0x100E14),
                panel: rgb(0x161320),
                surface: rgb(0x1D1928),
                elevated: rgb(0x272233),
                line: rgb(0x2C2739),
                text: rgb(0xEDEBF2),
                muted: rgb(0x8F8A9E),
                faint: rgb(0x5F5A70),
                ember: rgb(EMBER),
                ember_text: rgb(0xFF7A5C),
                flame: rgb(FLAME),
                gold: rgb(GOLD),
                jade: rgb(JADE),
                sky: rgb(SKY),
                violet: rgb(VIOLET),
                blood: rgb(BLOOD),
            },
            ThemeName::Light => Self {
                name,
                bg: rgb(0xF7F5FA),
                panel: rgb(0xFFFFFF),
                surface: rgb(0xF1EEF7),
                elevated: rgb(0xE7E3EF),
                line: rgb(0xDCD7E6),
                text: rgb(0x24202C),
                muted: rgb(0x6B6579),
                faint: rgb(0x9A94A8),
                ember: rgb(EMBER),
                ember_text: rgb(0xC43D22),
                flame: rgb(0xE07E28),
                gold: rgb(0xB8860B),
                jade: rgb(0x2E8B57),
                sky: rgb(0x3D6FD1),
                violet: rgb(0x7A57D4),
                blood: rgb(0xD0342C),
            },
        }
    }

    pub fn is_dark(&self) -> bool {
        self.name == ThemeName::Dark
    }

    /// Soft accent wash behind selected text inside inputs.
    pub fn selection_fill(&self) -> Hsla {
        match self.name {
            ThemeName::Dark => gpui::rgba(0xFF63472E).into(),
            ThemeName::Light => gpui::rgba(0xC43D2226).into(),
        }
    }

    /// Tinted bubble behind the user's own messages.
    pub fn user_bubble(&self) -> Hsla {
        match self.name {
            ThemeName::Dark => gpui::rgba(0x6CAAF526).into(),
            ThemeName::Light => gpui::rgba(0x3D6FD11A).into(),
        }
    }
}

fn rgb(hex: u32) -> Hsla {
    gpui::rgb(hex).into()
}

// ---- global access ---------------------------------------------------------

struct GlobalTheme(Theme);
impl gpui::Global for GlobalTheme {}

/// Install the active theme as a GPUI global.
pub fn set_global_theme(cx: &mut gpui::App, theme: Theme) {
    cx.set_global(GlobalTheme(theme));
}

/// Read the active theme from any context that derefs to `App`.
pub fn theme(cx: &gpui::App) -> Theme {
    cx.try_global::<GlobalTheme>()
        .map(|g| g.0)
        .unwrap_or_else(|| Theme::new(ThemeName::Dark))
}
