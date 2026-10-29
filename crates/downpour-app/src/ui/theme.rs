//! Palettes, fonts, and egui style: a dashboard look with soft cards in
//! several color themes, each in light and dark.
//!
//! Colors are read through functions (`theme::ink()`, …) so the whole UI
//! follows the palette chosen at run time.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use downpour_core::{CategoryLook, DownloadStatus};
use downpour_protocol::brand;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle,
};

use crate::app_settings::{Appearance, ColorTheme};

const SEMIBOLD: &str = "semibold";
const BOLD: &str = "bold";

/// Which palette is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// What the window is drawn in: a color theme in light or dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    pub colors: ColorTheme,
    pub mode: Mode,
}

impl Look {
    /// The look the user chose; `system` is what the operating system
    /// reports, if anything, for "match Windows".
    pub fn chosen(appearance: Appearance, colors: ColorTheme, system: Option<egui::Theme>) -> Look {
        Look {
            colors,
            mode: mode_for(appearance, system),
        }
    }

    /// One number for the whole look, so it is stored in one step: the
    /// theme's number, then one bit for the mode.
    const fn to_bits(self) -> u8 {
        let mode = match self.mode {
            Mode::Light => 0,
            Mode::Dark => 1,
        };
        (self.colors as u8) << 1 | mode
    }

    fn from_bits(bits: u8) -> Look {
        let colors = ColorTheme::ALL
            .get(usize::from(bits >> 1))
            .copied()
            .unwrap_or_default();
        let mode = match bits & 1 {
            1 => Mode::Dark,
            _ => Mode::Light,
        };
        Look { colors, mode }
    }
}

/// Every color the UI uses, by role.
struct Palette {
    backdrop: Color32,
    surface: Color32,
    sidebar: Color32,
    surface_muted: Color32,
    border: Color32,
    ink: Color32,
    ink_soft: Color32,
    ink_faint: Color32,
    primary: Color32,
    primary_hover: Color32,
    primary_soft: Color32,
    /// Text and icons drawn on the primary color.
    on_primary: Color32,
    accent: Color32,
    success: Color32,
    success_soft: Color32,
    danger: Color32,
    danger_soft: Color32,
    warning: Color32,
    warning_soft: Color32,
    tile_mint: Color32,
    tile_pink: Color32,
    tile_lavender: Color32,
    tile_peach: Color32,
    tile_violet: Color32,
    tile_sky: Color32,
    music: Color32,
    program: Color32,
    video: Color32,
    image: Color32,
    selected_row: Color32,
    /// Dims the window behind a dialog.
    modal_backdrop: Color32,
    shadow: Color32,
    /// The lit edge of a raised surface, opposite its shadow.
    highlight: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const LIGHT: Palette = Palette {
    backdrop: rgb(0xEE, 0xF0, 0xF7),
    surface: Color32::WHITE,
    sidebar: rgb(0xFB, 0xFB, 0xFE),
    surface_muted: rgb(0xF6, 0xF7, 0xFB),
    border: rgb(0xE7, 0xE9, 0xF3),
    ink: rgb(0x1E, 0x21, 0x42),
    ink_soft: rgb(0x6B, 0x70, 0x90),
    ink_faint: rgb(0xA3, 0xA8, 0xC3),
    primary: rgb(0x5B, 0x4F, 0xE0),
    primary_hover: rgb(0x4C, 0x40, 0xCF),
    primary_soft: rgb(0xEE, 0xEC, 0xFD),
    on_primary: Color32::WHITE,
    accent: rgb(0xFF, 0x7A, 0x45),
    success: rgb(0x1F, 0xA9, 0x7A),
    success_soft: rgb(0xE4, 0xF7, 0xEF),
    danger: rgb(0xE5, 0x48, 0x4D),
    danger_soft: rgb(0xFD, 0xEC, 0xEC),
    warning: rgb(0xD9, 0x8E, 0x1F),
    warning_soft: rgb(0xFF, 0xF4, 0xE0),
    tile_mint: rgb(0xE3, 0xF8, 0xF4),
    tile_pink: rgb(0xFD, 0xED, 0xEE),
    tile_lavender: rgb(0xEF, 0xEB, 0xFE),
    tile_peach: rgb(0xFF, 0xF2, 0xE9),
    tile_violet: rgb(0xF3, 0xEB, 0xFD),
    tile_sky: rgb(0xE6, 0xF2, 0xFD),
    music: rgb(0xE0, 0x4F, 0x8F),
    program: rgb(0x16, 0x9C, 0x92),
    video: rgb(0x8B, 0x55, 0xE0),
    image: rgb(0x2B, 0x8A, 0xD8),
    selected_row: rgb(0xF4, 0xF3, 0xFE),
    modal_backdrop: Color32::from_rgba_premultiplied(11, 12, 24, 90),
    shadow: Color32::from_rgba_premultiplied(2, 2, 4, 14),
    highlight: Color32::from_rgba_premultiplied(220, 220, 220, 220),
};

/// Deep blue-gray surfaces; colored roles are lifted for contrast on them,
/// and soft fills become dark tints of the same hues.
const DARK: Palette = Palette {
    backdrop: rgb(0x11, 0x13, 0x1E),
    surface: rgb(0x1B, 0x1E, 0x2E),
    sidebar: rgb(0x16, 0x18, 0x26),
    surface_muted: rgb(0x23, 0x27, 0x3A),
    border: rgb(0x30, 0x35, 0x4E),
    ink: rgb(0xE8, 0xEA, 0xF6),
    ink_soft: rgb(0xA8, 0xAD, 0xCA),
    ink_faint: rgb(0x70, 0x76, 0x96),
    primary: rgb(0x80, 0x75, 0xF2),
    // Darker, not lighter, under the pointer: white text must stay readable.
    primary_hover: rgb(0x6E, 0x62, 0xE8),
    primary_soft: rgb(0x2A, 0x27, 0x52),
    on_primary: Color32::WHITE,
    accent: rgb(0xFF, 0x8E, 0x62),
    success: rgb(0x3C, 0xCB, 0x98),
    success_soft: rgb(0x16, 0x35, 0x2C),
    danger: rgb(0xF2, 0x6B, 0x70),
    danger_soft: rgb(0x40, 0x1F, 0x25),
    warning: rgb(0xEC, 0xAA, 0x45),
    warning_soft: rgb(0x3B, 0x2F, 0x16),
    tile_mint: rgb(0x16, 0x33, 0x2E),
    tile_pink: rgb(0x3A, 0x1F, 0x2A),
    tile_lavender: rgb(0x27, 0x23, 0x47),
    tile_peach: rgb(0x3A, 0x29, 0x1F),
    tile_violet: rgb(0x2D, 0x22, 0x45),
    tile_sky: rgb(0x18, 0x2C, 0x42),
    music: rgb(0xF0, 0x6E, 0xA6),
    program: rgb(0x3C, 0xC2, 0xB6),
    video: rgb(0xA7, 0x7C, 0xF0),
    image: rgb(0x5B, 0xAA, 0xF0),
    selected_row: rgb(0x27, 0x25, 0x4A),
    modal_backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 140),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 70),
    highlight: Color32::from_rgba_premultiplied(14, 14, 16, 16),
};

// Sunset: warm orange buttons with violet accents, on warm-tinted surfaces.
const SUNSET_LIGHT: Palette = Palette {
    backdrop: rgb(0xF6, 0xF0, 0xEC),
    sidebar: rgb(0xFE, 0xFB, 0xF9),
    surface_muted: rgb(0xFA, 0xF5, 0xF2),
    border: rgb(0xF0, 0xE6, 0xE0),
    ink: rgb(0x2B, 0x1F, 0x24),
    ink_soft: rgb(0x7A, 0x69, 0x6E),
    ink_faint: rgb(0xB5, 0xA6, 0xA9),
    primary: rgb(0xE8, 0x62, 0x34),
    primary_hover: rgb(0xD4, 0x53, 0x27),
    primary_soft: rgb(0xFD, 0xEC, 0xE4),
    accent: rgb(0x5B, 0x4F, 0xE0),
    selected_row: rgb(0xFE, 0xF2, 0xEC),
    ..LIGHT
};

const SUNSET_DARK: Palette = Palette {
    backdrop: rgb(0x17, 0x12, 0x14),
    surface: rgb(0x23, 0x1C, 0x1F),
    sidebar: rgb(0x1D, 0x17, 0x1A),
    surface_muted: rgb(0x2D, 0x25, 0x28),
    border: rgb(0x3D, 0x32, 0x36),
    ink: rgb(0xF5, 0xEC, 0xEC),
    ink_soft: rgb(0xC4, 0xB3, 0xB5),
    ink_faint: rgb(0x85, 0x75, 0x79),
    primary: rgb(0xFF, 0x86, 0x57),
    primary_hover: rgb(0xFF, 0x9B, 0x73),
    primary_soft: rgb(0x47, 0x2B, 0x22),
    // White text is too faint on bright orange.
    on_primary: rgb(0x2A, 0x12, 0x08),
    accent: rgb(0x9D, 0x93, 0xF7),
    selected_row: rgb(0x3A, 0x27, 0x23),
    ..DARK
};

// Slate: muted navy, like a banking dashboard, with coral highlights.
const SLATE_LIGHT: Palette = Palette {
    backdrop: rgb(0xEC, 0xEF, 0xF5),
    sidebar: rgb(0xF9, 0xFA, 0xFC),
    surface_muted: rgb(0xF4, 0xF6, 0xFA),
    border: rgb(0xE2, 0xE6, 0xEF),
    ink: rgb(0x1F, 0x26, 0x3D),
    ink_soft: rgb(0x65, 0x6E, 0x88),
    ink_faint: rgb(0xA1, 0xA9, 0xBE),
    primary: rgb(0x48, 0x55, 0x8E),
    primary_hover: rgb(0x3B, 0x47, 0x7C),
    primary_soft: rgb(0xE6, 0xE9, 0xF5),
    accent: rgb(0xF3, 0x7F, 0x55),
    selected_row: rgb(0xEE, 0xF0, 0xF8),
    ..LIGHT
};

const SLATE_DARK: Palette = Palette {
    backdrop: rgb(0x10, 0x14, 0x1F),
    surface: rgb(0x19, 0x1F, 0x2E),
    sidebar: rgb(0x14, 0x19, 0x27),
    surface_muted: rgb(0x22, 0x29, 0x3B),
    border: rgb(0x2F, 0x38, 0x4F),
    primary: rgb(0x7E, 0x8D, 0xD8),
    primary_hover: rgb(0x6A, 0x79, 0xC9),
    primary_soft: rgb(0x25, 0x2D, 0x4A),
    accent: rgb(0xFF, 0x98, 0x70),
    selected_row: rgb(0x24, 0x2C, 0x46),
    ..DARK
};

// Ocean: teal with pink accents on cool, slightly green surfaces.
const OCEAN_LIGHT: Palette = Palette {
    backdrop: rgb(0xEA, 0xF3, 0xF3),
    sidebar: rgb(0xF8, 0xFC, 0xFC),
    surface_muted: rgb(0xF2, 0xF8, 0xF8),
    border: rgb(0xDD, 0xEB, 0xEB),
    ink: rgb(0x14, 0x2C, 0x30),
    ink_soft: rgb(0x58, 0x73, 0x77),
    ink_faint: rgb(0x98, 0xB1, 0xB3),
    primary: rgb(0x0E, 0x8F, 0x8A),
    primary_hover: rgb(0x0A, 0x7B, 0x77),
    primary_soft: rgb(0xDF, 0xF4, 0xF2),
    accent: rgb(0xE5, 0x4F, 0x86),
    selected_row: rgb(0xEA, 0xF7, 0xF6),
    ..LIGHT
};

const OCEAN_DARK: Palette = Palette {
    backdrop: rgb(0x0C, 0x16, 0x18),
    surface: rgb(0x14, 0x21, 0x24),
    sidebar: rgb(0x10, 0x1B, 0x1E),
    surface_muted: rgb(0x1C, 0x2C, 0x30),
    border: rgb(0x28, 0x3C, 0x41),
    ink: rgb(0xE4, 0xF2, 0xF2),
    ink_soft: rgb(0xA2, 0xBD, 0xBF),
    ink_faint: rgb(0x68, 0x84, 0x87),
    primary: rgb(0x2C, 0xC0, 0xB5),
    primary_hover: rgb(0x4B, 0xD0, 0xC6),
    primary_soft: rgb(0x15, 0x35, 0x36),
    // White text is too faint on bright teal.
    on_primary: rgb(0x05, 0x1E, 0x1F),
    accent: rgb(0xF5, 0x7A, 0xA5),
    selected_row: rgb(0x17, 0x32, 0x34),
    ..DARK
};

// Graphite: neutral grays, the calm look of a classic download manager.
const GRAPHITE_LIGHT: Palette = Palette {
    backdrop: rgb(0xF0, 0xF0, 0xF2),
    sidebar: rgb(0xFA, 0xFA, 0xFB),
    surface_muted: rgb(0xF5, 0xF5, 0xF7),
    border: rgb(0xE3, 0xE3, 0xE7),
    ink: rgb(0x1C, 0x1C, 0x22),
    ink_soft: rgb(0x66, 0x66, 0x71),
    ink_faint: rgb(0xA2, 0xA2, 0xAC),
    primary: rgb(0x68, 0x58, 0xE3),
    primary_hover: rgb(0x58, 0x48, 0xD2),
    primary_soft: rgb(0xEE, 0xEC, 0xFB),
    accent: rgb(0x2B, 0x8A, 0xD8),
    selected_row: rgb(0xF1, 0xF0, 0xFA),
    ..LIGHT
};

const GRAPHITE_DARK: Palette = Palette {
    backdrop: rgb(0x14, 0x14, 0x17),
    surface: rgb(0x1E, 0x1E, 0x22),
    sidebar: rgb(0x18, 0x18, 0x1B),
    surface_muted: rgb(0x27, 0x27, 0x2C),
    border: rgb(0x34, 0x34, 0x3B),
    ink: rgb(0xEC, 0xEC, 0xF0),
    ink_soft: rgb(0xA9, 0xA9, 0xB4),
    ink_faint: rgb(0x6F, 0x6F, 0x7A),
    primary: rgb(0x8C, 0x7D, 0xF6),
    primary_hover: rgb(0x78, 0x68, 0xEC),
    primary_soft: rgb(0x2C, 0x29, 0x45),
    accent: rgb(0x5B, 0xAA, 0xF0),
    selected_row: rgb(0x2A, 0x29, 0x36),
    ..DARK
};

// Cream: surfaces nearly the color of the page, lifted by a warm shadow and
// a lit edge, with peach and rose.
const CREAM_LIGHT: Palette = Palette {
    backdrop: rgb(0xEF, 0xEA, 0xE4),
    surface: rgb(0xF4, 0xF0, 0xEB),
    sidebar: rgb(0xF4, 0xF0, 0xEB),
    surface_muted: rgb(0xEA, 0xE4, 0xDD),
    border: rgb(0xE2, 0xDB, 0xD3),
    ink: rgb(0x34, 0x30, 0x2C),
    ink_soft: rgb(0x8C, 0x83, 0x7B),
    ink_faint: rgb(0xB9, 0xAF, 0xA6),
    primary: rgb(0xB0, 0x75, 0x60),
    primary_hover: rgb(0x9E, 0x65, 0x52),
    primary_soft: rgb(0xF1, 0xDF, 0xD5),
    accent: rgb(0xD4, 0x8C, 0x7E),
    selected_row: rgb(0xEF, 0xE3, 0xDA),
    shadow: Color32::from_rgba_premultiplied(24, 16, 8, 40),
    highlight: Color32::from_rgba_premultiplied(235, 235, 235, 235),
    ..LIGHT
};

const CREAM_DARK: Palette = Palette {
    backdrop: rgb(0x26, 0x22, 0x1F),
    surface: rgb(0x2B, 0x27, 0x24),
    sidebar: rgb(0x2B, 0x27, 0x24),
    surface_muted: rgb(0x34, 0x2F, 0x2B),
    border: rgb(0x3E, 0x38, 0x33),
    ink: rgb(0xF1, 0xEA, 0xE3),
    ink_soft: rgb(0xBF, 0xB3, 0xA8),
    ink_faint: rgb(0x85, 0x7A, 0x70),
    primary: rgb(0xE2, 0xAE, 0x96),
    primary_hover: rgb(0xEC, 0xC0, 0xAB),
    primary_soft: rgb(0x4A, 0x37, 0x2D),
    // White text is too faint on pale peach.
    on_primary: rgb(0x2B, 0x1C, 0x14),
    accent: rgb(0xEF, 0xA5, 0x97),
    selected_row: rgb(0x3B, 0x30, 0x2A),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 110),
    highlight: Color32::from_rgba_premultiplied(16, 15, 14, 16),
    ..DARK
};

// Ember: charcoal with amber, from a music player; red for the accent.
const EMBER_LIGHT: Palette = Palette {
    backdrop: rgb(0xE4, 0xE3, 0xE1),
    surface: rgb(0xFA, 0xFA, 0xF9),
    sidebar: rgb(0xF3, 0xF2, 0xF0),
    surface_muted: rgb(0xEF, 0xEE, 0xEC),
    border: rgb(0xDE, 0xDC, 0xD9),
    ink: rgb(0x1F, 0x1F, 0x21),
    ink_soft: rgb(0x64, 0x64, 0x68),
    ink_faint: rgb(0xA2, 0xA2, 0xA6),
    primary: rgb(0xE3, 0x8A, 0x12),
    primary_hover: rgb(0xCF, 0x7C, 0x0C),
    primary_soft: rgb(0xFB, 0xEB, 0xD3),
    // Amber is too light for white text.
    on_primary: rgb(0x21, 0x14, 0x02),
    accent: rgb(0xC8, 0x36, 0x3D),
    selected_row: rgb(0xFA, 0xF0, 0xE2),
    ..LIGHT
};

const EMBER_DARK: Palette = Palette {
    backdrop: rgb(0x1E, 0x1E, 0x20),
    surface: rgb(0x2E, 0x2E, 0x31),
    sidebar: rgb(0x25, 0x25, 0x27),
    surface_muted: rgb(0x3A, 0x3A, 0x3E),
    border: rgb(0x44, 0x44, 0x49),
    ink: rgb(0xF4, 0xF4, 0xF5),
    ink_soft: rgb(0xB4, 0xB4, 0xBA),
    ink_faint: rgb(0x7A, 0x7A, 0x80),
    primary: rgb(0xF4, 0xA2, 0x3C),
    primary_hover: rgb(0xFF, 0xB6, 0x5E),
    primary_soft: rgb(0x4A, 0x3A, 0x22),
    on_primary: rgb(0x24, 0x16, 0x04),
    accent: rgb(0xE5, 0x48, 0x4D),
    selected_row: rgb(0x3C, 0x36, 0x2C),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 120),
    ..DARK
};

// Coral: white cards on cool gray beside a near-black sidebar, with coral,
// orange, and teal blocks.
const CORAL_LIGHT: Palette = Palette {
    backdrop: rgb(0xE4, 0xE7, 0xEB),
    sidebar: rgb(0x24, 0x26, 0x2C),
    surface_muted: rgb(0xF2, 0xF4, 0xF6),
    border: rgb(0xE1, 0xE4, 0xE8),
    ink: rgb(0x20, 0x22, 0x28),
    ink_soft: rgb(0x64, 0x69, 0x73),
    ink_faint: rgb(0xA4, 0xA9, 0xB1),
    primary: rgb(0xE6, 0x45, 0x5A),
    primary_hover: rgb(0xD4, 0x38, 0x4C),
    primary_soft: rgb(0xFC, 0xE4, 0xE8),
    accent: rgb(0xF5, 0x9E, 0x2B),
    program: rgb(0x2E, 0xA8, 0x99),
    selected_row: rgb(0xFD, 0xEF, 0xF1),
    shadow: Color32::from_rgba_premultiplied(4, 6, 10, 22),
    ..LIGHT
};

const CORAL_DARK: Palette = Palette {
    backdrop: rgb(0x14, 0x15, 0x19),
    surface: rgb(0x1F, 0x20, 0x25),
    sidebar: rgb(0x0E, 0x0F, 0x12),
    surface_muted: rgb(0x2A, 0x2C, 0x32),
    border: rgb(0x37, 0x3A, 0x41),
    ink: rgb(0xEE, 0xF0, 0xF3),
    ink_soft: rgb(0xA9, 0xAE, 0xB7),
    ink_faint: rgb(0x6F, 0x74, 0x7D),
    primary: rgb(0xFF, 0x6B, 0x7D),
    primary_hover: rgb(0xFF, 0x84, 0x94),
    primary_soft: rgb(0x47, 0x22, 0x2A),
    on_primary: rgb(0x2A, 0x09, 0x10),
    accent: rgb(0xFF, 0xB2, 0x5C),
    program: rgb(0x4F, 0xD1, 0xC1),
    selected_row: rgb(0x38, 0x22, 0x28),
    ..DARK
};

// Violet: a vivid violet sidebar beside white cards, with orange.
const VIOLET_LIGHT: Palette = Palette {
    backdrop: rgb(0xEC, 0xEB, 0xF0),
    sidebar: rgb(0x84, 0x50, 0xEA),
    surface_muted: rgb(0xF5, 0xF4, 0xF8),
    border: rgb(0xE7, 0xE5, 0xEC),
    ink: rgb(0x1C, 0x1A, 0x22),
    ink_soft: rgb(0x69, 0x66, 0x7A),
    ink_faint: rgb(0xA9, 0xA6, 0xB6),
    primary: rgb(0x8A, 0x4D, 0xEB),
    primary_hover: rgb(0x7A, 0x3D, 0xDB),
    primary_soft: rgb(0xF0, 0xE7, 0xFE),
    accent: rgb(0xFF, 0x7B, 0x3A),
    selected_row: rgb(0xF5, 0xEF, 0xFE),
    shadow: Color32::from_rgba_premultiplied(10, 4, 20, 26),
    ..LIGHT
};

const VIOLET_DARK: Palette = Palette {
    backdrop: rgb(0x12, 0x10, 0x16),
    surface: rgb(0x1C, 0x19, 0x22),
    sidebar: rgb(0x5B, 0x2F, 0xB0),
    surface_muted: rgb(0x27, 0x23, 0x2E),
    border: rgb(0x35, 0x30, 0x40),
    ink: rgb(0xEE, 0xEA, 0xF5),
    ink_soft: rgb(0xAF, 0xA8, 0xC0),
    ink_faint: rgb(0x74, 0x6E, 0x83),
    primary: rgb(0xA5, 0x7B, 0xFF),
    primary_hover: rgb(0xB5, 0x91, 0xFF),
    primary_soft: rgb(0x32, 0x24, 0x50),
    on_primary: rgb(0x17, 0x0C, 0x2B),
    accent: rgb(0xFF, 0x92, 0x5C),
    selected_row: rgb(0x2C, 0x22, 0x40),
    ..DARK
};

const fn brand_color([r, g, b]: [u8; 3]) -> Color32 {
    rgb(r, g, b)
}

/// The app icon's slate, the sidebar in both modes, so the icon's square
/// melts into it and only the raindrop shows.
const ICON_SLATE: Color32 = brand_color(brand::SLATE);
/// The app icon's coral.
const ICON_CORAL: Color32 = brand_color(brand::CORAL);
/// The second accent beside coral, for charts and warnings.
const SUNNY_YELLOW: Color32 = rgb(0xF6, 0xE0, 0x4B);

// Downpour: the app icon's colors. Flat cards a shade apart from the page,
// controls a shade apart from the cards (darker in light mode, lighter in
// dark), no outlines, bold headings, and coral with sunny yellow.
const DOWNPOUR_LIGHT: Palette = Palette {
    backdrop: rgb(0xF2, 0xF3, 0xF5),
    sidebar: ICON_SLATE,
    surface_muted: rgb(0xE6, 0xE8, 0xEC),
    border: rgb(0xE1, 0xE3, 0xE7),
    ink: rgb(0x2A, 0x2D, 0x33),
    ink_soft: rgb(0x68, 0x6C, 0x75),
    ink_faint: rgb(0xA2, 0xA6, 0xAE),
    primary: ICON_CORAL,
    primary_hover: rgb(0xE6, 0x51, 0x4A),
    primary_soft: rgb(0xFE, 0xE8, 0xE6),
    // Pure sunny yellow fades into white; this keeps its warmth.
    accent: rgb(0xED, 0xBE, 0x12),
    warning: rgb(0xC9, 0x9A, 0x06),
    warning_soft: rgb(0xFF, 0xF6, 0xD6),
    tile_peach: rgb(0xFF, 0xF6, 0xDB),
    tile_pink: rgb(0xFE, 0xEC, 0xEB),
    selected_row: rgb(0xFE, 0xF0, 0xEF),
    shadow: Color32::from_rgba_premultiplied(6, 8, 12, 30),
    ..LIGHT
};

const DOWNPOUR_DARK: Palette = Palette {
    backdrop: rgb(0x38, 0x3C, 0x43),
    surface: rgb(0x46, 0x4A, 0x52),
    sidebar: ICON_SLATE,
    surface_muted: rgb(0x53, 0x57, 0x60),
    border: rgb(0x5A, 0x5E, 0x67),
    ink: rgb(0xF5, 0xF6, 0xF8),
    ink_soft: rgb(0xB3, 0xB7, 0xBF),
    ink_faint: rgb(0x8B, 0x90, 0x99),
    primary: ICON_CORAL,
    // Darker, not lighter, under the pointer: white text must stay readable.
    primary_hover: rgb(0xE9, 0x53, 0x4C),
    primary_soft: rgb(0x5E, 0x4A, 0x4F),
    on_primary: Color32::WHITE,
    accent: SUNNY_YELLOW,
    success_soft: rgb(0x3E, 0x55, 0x4F),
    danger_soft: rgb(0x5E, 0x44, 0x48),
    warning: SUNNY_YELLOW,
    warning_soft: rgb(0x5A, 0x58, 0x44),
    tile_mint: rgb(0x3E, 0x55, 0x50),
    tile_pink: rgb(0x5E, 0x47, 0x4E),
    tile_lavender: rgb(0x4C, 0x4B, 0x63),
    tile_peach: rgb(0x5B, 0x54, 0x44),
    tile_violet: rgb(0x52, 0x4A, 0x62),
    tile_sky: rgb(0x41, 0x52, 0x6A),
    selected_row: rgb(0x59, 0x4B, 0x52),
    modal_backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 120),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 90),
    ..DARK
};

fn palette_of(look: Look) -> &'static Palette {
    match (look.colors, look.mode) {
        (ColorTheme::Downpour, Mode::Light) => &DOWNPOUR_LIGHT,
        (ColorTheme::Downpour, Mode::Dark) => &DOWNPOUR_DARK,
        (ColorTheme::Lavender, Mode::Light) => &LIGHT,
        (ColorTheme::Lavender, Mode::Dark) => &DARK,
        (ColorTheme::Sunset, Mode::Light) => &SUNSET_LIGHT,
        (ColorTheme::Sunset, Mode::Dark) => &SUNSET_DARK,
        (ColorTheme::Slate, Mode::Light) => &SLATE_LIGHT,
        (ColorTheme::Slate, Mode::Dark) => &SLATE_DARK,
        (ColorTheme::Ocean, Mode::Light) => &OCEAN_LIGHT,
        (ColorTheme::Ocean, Mode::Dark) => &OCEAN_DARK,
        (ColorTheme::Graphite, Mode::Light) => &GRAPHITE_LIGHT,
        (ColorTheme::Graphite, Mode::Dark) => &GRAPHITE_DARK,
        (ColorTheme::Cream, Mode::Light) => &CREAM_LIGHT,
        (ColorTheme::Cream, Mode::Dark) => &CREAM_DARK,
        (ColorTheme::Ember, Mode::Light) => &EMBER_LIGHT,
        (ColorTheme::Ember, Mode::Dark) => &EMBER_DARK,
        (ColorTheme::Coral, Mode::Light) => &CORAL_LIGHT,
        (ColorTheme::Coral, Mode::Dark) => &CORAL_DARK,
        (ColorTheme::Violet, Mode::Light) => &VIOLET_LIGHT,
        (ColorTheme::Violet, Mode::Dark) => &VIOLET_DARK,
    }
}

// ---- Form: everything about a theme that is not a color ----

/// How a theme builds the window: shapes, depth, and how each kind of
/// element is drawn. Colors come from the palette; one form serves a
/// theme's light and dark palettes alike.
#[derive(Debug, PartialEq, Eq)]
pub struct Form {
    pub card_radius: u8,
    pub window_radius: u8,
    pub controls: ControlShape,
    pub depth: Depth,
    pub sidebar: SidebarFill,
    /// Where the sidebar and the details panel sit.
    pub panels: Placement,
    pub tiles: TileStyle,
    pub rows: RowStyle,
    pub progress: ProgressStyle,
    pub titles: TitleStyle,
    pub icon_buttons: IconButtonShape,
    pub main_button: MainButton,
    pub charts: ChartColors,
}

/// The outline of buttons, fields, and other controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlShape {
    Rounded {
        radius: u8,
    },
    /// Fully round ends, whatever the height.
    Pill,
}

/// What lifts a card off the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// A soft shadow below.
    Shadow { drop: i8, blur: u8 },
    /// A shadow below-right and a lit edge above-left, on surfaces nearly
    /// the page's own color, as if pressed out of it.
    Raised { distance: i8, blur: u8 },
    /// A thick dark frame around the card, and a deep shadow below.
    Bezel {
        width: u8,
        color: Color32,
        drop: i8,
        blur: u8,
    },
    /// No shadow and no outlines: cards are set apart from the page only by
    /// their fill, and controls by a shade apart from the card (darker in
    /// light mode, lighter in dark). Only what floats above the page
    /// (dialogs, menus) casts a shadow.
    Flat { drop: i8, blur: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarFill {
    /// The palette's light sidebar color, with the usual ink.
    Plain,
    /// A dark or saturated sidebar with light text.
    Contrast { selection: ContrastSelection },
}

/// How the chosen entry stands out in a contrast sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContrastSelection {
    /// Filled with the primary color.
    Primary,
    /// Filled with light, its text in the sidebar's own color, as if cut out.
    Light,
    /// No fill: a short bar of the primary color under its label, like the
    /// current tab of a web page.
    Underline,
}

/// How a navigation list draws its chosen entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavSelection {
    /// A block of `fill` behind the entry, its text and icon in `ink`.
    Fill { fill: Color32, ink: Color32 },
    /// A short `bar` under the label, its text and icon in `ink`.
    Underline { bar: Color32, ink: Color32 },
}

/// Where the side panels sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Flush against the window's edge.
    Edge,
    /// A card of its own with a gap around it.
    Floating,
}

/// The summary tiles above lists and on the statistics page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileStyle {
    /// A pastel fill per tile.
    Tinted,
    /// The card's surface, with the tile's color only on a round badge.
    Neutral,
    /// The tile's color as a solid block, with light text.
    Solid,
}

/// How the download list separates its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStyle {
    /// One card, rows parted by hairlines.
    Lines,
    /// Every row a card of its own, with a gap between.
    Cards,
    /// Full-width bands of alternating shade, no lines.
    Bands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressStyle {
    /// A thin rounded bar.
    Bar,
    /// A thick, fully rounded bar.
    Chunky,
    /// A slim track with a round knob where the progress ends.
    Knob,
    /// Upright bars of varying height, like an audio waveform.
    Waveform,
}

/// How page titles are set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleStyle {
    /// Bold, at the size asked for.
    Standard,
    /// Larger and in the regular weight, for a quiet, airy page.
    Light,
    /// Larger still and bold, for a poster-like page.
    Display,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconButtonShape {
    /// Shows a rounded square only on hover.
    Square,
    /// Always a filled circle.
    Circle,
}

/// What the one main action on a page (Add URL, OK) is filled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainButton {
    Primary,
    /// The ink color, with the surface color for its text.
    Ink,
}

/// The colors of the statistics charts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartColors {
    /// Every series in the primary color.
    Primary,
    /// The primary color for the main series and the accent for the
    /// second, as in a two-tone dashboard.
    PrimaryAndAccent,
}

/// Which of a chart page's series is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Series {
    Main,
    Second,
}

const DOWNPOUR: Form = Form {
    card_radius: 14,
    window_radius: 16,
    controls: ControlShape::Pill,
    depth: Depth::Flat { drop: 12, blur: 36 },
    sidebar: SidebarFill::Contrast {
        selection: ContrastSelection::Underline,
    },
    panels: Placement::Edge,
    tiles: TileStyle::Neutral,
    rows: RowStyle::Cards,
    progress: ProgressStyle::Bar,
    titles: TitleStyle::Standard,
    icon_buttons: IconButtonShape::Circle,
    main_button: MainButton::Primary,
    charts: ChartColors::PrimaryAndAccent,
};

/// The first five themes share the dashboard form; they differ in color.
const DASHBOARD: Form = Form {
    card_radius: 16,
    window_radius: 18,
    controls: ControlShape::Rounded { radius: 10 },
    depth: Depth::Shadow { drop: 6, blur: 24 },
    sidebar: SidebarFill::Plain,
    panels: Placement::Edge,
    tiles: TileStyle::Tinted,
    rows: RowStyle::Lines,
    progress: ProgressStyle::Bar,
    titles: TitleStyle::Standard,
    icon_buttons: IconButtonShape::Square,
    main_button: MainButton::Primary,
    charts: ChartColors::Primary,
};

const CREAM: Form = Form {
    card_radius: 26,
    window_radius: 26,
    controls: ControlShape::Pill,
    depth: Depth::Raised {
        distance: 6,
        blur: 18,
    },
    sidebar: SidebarFill::Plain,
    panels: Placement::Floating,
    tiles: TileStyle::Neutral,
    rows: RowStyle::Cards,
    progress: ProgressStyle::Knob,
    titles: TitleStyle::Light,
    icon_buttons: IconButtonShape::Circle,
    main_button: MainButton::Primary,
    charts: ChartColors::Primary,
};

const EMBER: Form = Form {
    card_radius: 24,
    window_radius: 24,
    controls: ControlShape::Pill,
    depth: Depth::Bezel {
        width: 5,
        color: rgb(0x16, 0x16, 0x18),
        drop: 10,
        blur: 30,
    },
    sidebar: SidebarFill::Plain,
    panels: Placement::Edge,
    tiles: TileStyle::Neutral,
    rows: RowStyle::Lines,
    progress: ProgressStyle::Waveform,
    titles: TitleStyle::Light,
    icon_buttons: IconButtonShape::Circle,
    main_button: MainButton::Primary,
    charts: ChartColors::Primary,
};

const CORAL: Form = Form {
    card_radius: 22,
    window_radius: 22,
    controls: ControlShape::Pill,
    depth: Depth::Shadow { drop: 8, blur: 28 },
    sidebar: SidebarFill::Contrast {
        selection: ContrastSelection::Primary,
    },
    panels: Placement::Edge,
    tiles: TileStyle::Solid,
    rows: RowStyle::Lines,
    progress: ProgressStyle::Chunky,
    titles: TitleStyle::Standard,
    icon_buttons: IconButtonShape::Circle,
    main_button: MainButton::Primary,
    charts: ChartColors::Primary,
};

const VIOLET: Form = Form {
    card_radius: 30,
    window_radius: 28,
    controls: ControlShape::Rounded { radius: 14 },
    depth: Depth::Shadow { drop: 14, blur: 40 },
    sidebar: SidebarFill::Contrast {
        selection: ContrastSelection::Light,
    },
    panels: Placement::Floating,
    tiles: TileStyle::Tinted,
    rows: RowStyle::Bands,
    progress: ProgressStyle::Chunky,
    titles: TitleStyle::Display,
    icon_buttons: IconButtonShape::Square,
    main_button: MainButton::Ink,
    charts: ChartColors::Primary,
};

/// The form of any theme, for previews of themes not being drawn.
pub fn form_for(colors: ColorTheme) -> &'static Form {
    match colors {
        ColorTheme::Downpour => &DOWNPOUR,
        ColorTheme::Lavender
        | ColorTheme::Sunset
        | ColorTheme::Slate
        | ColorTheme::Ocean
        | ColorTheme::Graphite => &DASHBOARD,
        ColorTheme::Cream => &CREAM,
        ColorTheme::Ember => &EMBER,
        ColorTheme::Coral => &CORAL,
        ColorTheme::Violet => &VIOLET,
    }
}

/// The form of the theme being drawn.
pub fn form() -> &'static Form {
    form_for(look().colors)
}

/// A theme's main colors, for its swatch in Settings.
pub struct Swatch {
    pub backdrop: Color32,
    pub surface: Color32,
    pub sidebar: Color32,
    /// A raised surface's lit edge and its shadow.
    pub highlight: Color32,
    pub shadow: Color32,
    pub primary: Color32,
    pub accent: Color32,
    pub ink_faint: Color32,
}

pub fn swatch(look: Look) -> Swatch {
    let palette = palette_of(look);
    Swatch {
        backdrop: palette.backdrop,
        surface: palette.surface,
        sidebar: palette.sidebar,
        highlight: palette.highlight,
        shadow: palette.shadow,
        primary: palette.primary,
        accent: palette.accent,
        ink_faint: palette.ink_faint,
    }
}

/// The palette for the chosen appearance; `system` is what the operating
/// system reports, if anything.
pub fn mode_for(appearance: Appearance, system: Option<egui::Theme>) -> Mode {
    match appearance {
        Appearance::Light => Mode::Light,
        Appearance::Dark => Mode::Dark,
        Appearance::MatchSystem => match system {
            Some(egui::Theme::Dark) => Mode::Dark,
            Some(egui::Theme::Light) | None => Mode::Light,
        },
    }
}

/// The look being drawn, as [`Look::to_bits`].
static LOOK: AtomicU8 = AtomicU8::new(
    Look {
        colors: ColorTheme::Downpour,
        mode: Mode::Light,
    }
    .to_bits(),
);

pub fn look() -> Look {
    Look::from_bits(LOOK.load(Ordering::Relaxed))
}

pub fn mode() -> Mode {
    look().mode
}

fn palette() -> &'static Palette {
    palette_of(look())
}

macro_rules! colors {
    ($($name:ident),* $(,)?) => {
        $(pub fn $name() -> Color32 { palette().$name })*
    };
}

colors!(
    backdrop,
    surface,
    sidebar,
    surface_muted,
    border,
    ink,
    ink_soft,
    ink_faint,
    primary,
    primary_soft,
    on_primary,
    accent,
    success,
    success_soft,
    danger,
    danger_soft,
    warning,
    warning_soft,
    tile_mint,
    tile_pink,
    tile_peach,
    selected_row,
    modal_backdrop,
);

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

/// A page title in the theme's title style; `size` is the standard size.
pub fn title(size: f32) -> FontId {
    match form().titles {
        TitleStyle::Standard => bold(size),
        TitleStyle::Light => regular(size * 1.25),
        TitleStyle::Display => bold(size * 1.3),
    }
}

pub fn card_radius() -> CornerRadius {
    CornerRadius::same(form().card_radius)
}

/// The corners of a block that sits inside a card (a tile, a notice, a
/// highlighted row): the card's, `tighter_by` points tighter, so the smaller
/// shape still follows the card's curve.
pub fn inner_card_radius(tighter_by: u8) -> CornerRadius {
    CornerRadius::same(form().card_radius.saturating_sub(tighter_by))
}

/// Fully round ends for a shape `height` points tall.
pub fn round_ends(height: f32) -> CornerRadius {
    CornerRadius::same((height / 2.0).clamp(0.0, f32::from(u8::MAX)) as u8)
}

/// The corners of a control `height` points tall.
pub fn control_radius(height: f32) -> CornerRadius {
    control_radius_of(form(), height)
}

fn control_radius_of(form: &Form, height: f32) -> CornerRadius {
    match form.controls {
        ControlShape::Rounded { radius } => CornerRadius::same(radius),
        ControlShape::Pill => round_ends(height),
    }
}

/// The corners of a control sitting `inset` points inside another control,
/// so the two outlines stay parallel.
pub fn nested_control_radius(height: f32, inset: f32) -> CornerRadius {
    match form().controls {
        ControlShape::Rounded { radius } => {
            CornerRadius::same(radius.saturating_sub(inset.clamp(0.0, 255.0) as u8))
        }
        ControlShape::Pill => control_radius(height),
    }
}

/// The one shadow egui draws under windows, menus, and frames.
pub fn card_shadow() -> Shadow {
    shadow_of(form(), palette())
}

fn shadow_of(form: &Form, palette: &Palette) -> Shadow {
    let (offset, blur) = match form.depth {
        Depth::Shadow { drop, blur }
        | Depth::Bezel { drop, blur, .. }
        | Depth::Flat { drop, blur } => ([0, drop], blur),
        Depth::Raised { distance, blur } => ([distance, distance], blur),
    };
    Shadow {
        offset,
        blur,
        spread: 0,
        color: palette.shadow,
    }
}

/// A frame in the card style, for egui areas and windows that need a
/// `Frame`. Raised themes get only their shadow here; [`card`] also draws
/// the lit edge. These float above the page, so flat themes keep the shadow.
pub fn card_frame() -> egui::Frame {
    let frame = egui::Frame::new()
        .fill(surface())
        .corner_radius(card_radius())
        .inner_margin(Margin::same(18))
        .shadow(card_shadow());
    match form().depth {
        Depth::Bezel { width, color, .. } => frame.stroke(Stroke::new(f32::from(width), color)),
        Depth::Shadow { .. } | Depth::Raised { .. } | Depth::Flat { .. } => frame,
    }
}

/// The shapes that draw a surface of `fill` at `rect` with the theme's depth:
/// its shadows (and lit edge, or bezel) and the fill itself.
pub fn surface_shapes(rect: egui::Rect, radius: CornerRadius, fill: Color32) -> egui::Shape {
    let palette = palette();
    let fill_shape = egui::Shape::rect_filled(rect, radius, fill);
    let shapes = match form().depth {
        Depth::Shadow { .. } => vec![card_shadow().as_shape(rect, radius).into(), fill_shape],
        Depth::Flat { .. } => vec![fill_shape],
        Depth::Raised { distance, blur } => {
            let lit = Shadow {
                offset: [-distance, -distance],
                blur,
                spread: 0,
                color: palette.highlight,
            };
            vec![
                lit.as_shape(rect, radius).into(),
                card_shadow().as_shape(rect, radius).into(),
                fill_shape,
            ]
        }
        Depth::Bezel { width, color, .. } => vec![
            card_shadow().as_shape(rect, radius).into(),
            fill_shape,
            egui::Shape::rect_stroke(
                rect,
                radius,
                Stroke::new(f32::from(width), color),
                egui::StrokeKind::Inside,
            ),
        ],
    };
    egui::Shape::Vec(shapes)
}

/// Paints a surface of `fill` with the theme's depth; see [`surface_shapes`].
pub fn paint_surface(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: CornerRadius,
    fill: Color32,
) {
    painter.add(surface_shapes(rect, radius, fill));
}

/// A card around `add_contents`, drawn with the theme's depth. The card is
/// painted behind the contents once their size is known.
pub fn card<R>(
    ui: &mut egui::Ui,
    margin: Margin,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::new()
        .inner_margin(margin)
        .show(ui, add_contents);
    ui.painter().set(
        background,
        surface_shapes(inner.response.rect, card_radius(), surface()),
    );
    inner
}

/// The colors of an icon button, at rest and under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconButtonInk {
    pub icon: Color32,
    /// Behind a round button at rest; square buttons have no fill at rest.
    pub fill: Color32,
    pub hover_icon: Color32,
    pub hover_fill: Color32,
}

impl IconButtonInk {
    /// On the page's own surfaces, with `icon` at rest.
    pub fn on_page(icon: Color32) -> Self {
        let palette = palette();
        Self {
            icon,
            fill: palette.surface_muted,
            hover_icon: palette.primary,
            hover_fill: palette.primary_soft,
        }
    }
}

/// The colors of an on/off switch, off and on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchInk {
    pub off_track: Color32,
    pub on_track: Color32,
    pub off_knob: Color32,
    pub on_knob: Color32,
}

impl SwitchInk {
    pub fn on_page() -> Self {
        let palette = palette();
        Self {
            off_track: palette.border,
            on_track: palette.primary,
            off_knob: palette.surface,
            on_knob: palette.surface,
        }
    }
}

/// Which icon colors a navigation list keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconColors {
    /// Every entry keeps its own icon color.
    Kept,
    /// An icon color that does not stand out from `fill` gives way to the
    /// list's soft ink (the primary-colored Documents icon on a violet
    /// sidebar, say).
    ReadableOn { fill: Color32 },
}

/// Below this contrast with what is behind it (WCAG's floor for icons and
/// other graphics), an icon is hard to see.
const MIN_ICON_CONTRAST: f32 = 3.0;

/// Everything drawn in a navigation list, which may be the sidebar (dark or
/// saturated on some themes) or a list on an ordinary card. Screens read
/// these colors; only [`sidebar_ink`] knows what kind of sidebar it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavInk {
    pub ink: Color32,
    pub ink_soft: Color32,
    pub ink_faint: Color32,
    pub selection: NavSelection,
    pub hover_fill: Color32,
    /// Behind an unselected entry's count.
    pub badge_fill: Color32,
    /// Small cards inside the list, like the sidebar's speed card.
    pub card_fill: Color32,
    /// Behind a setting shown in the list, like the auto-start switch.
    pub panel_fill: Color32,
    pub icon_button: IconButtonInk,
    pub switch: SwitchInk,
    pub icons: IconColors,
}

impl NavInk {
    /// `color` where it reads in this list, else the list's soft ink.
    pub fn readable(&self, color: Color32) -> Color32 {
        readable_icon(color, self.icons, self.ink_soft)
    }
}

/// The choice behind [`NavInk::readable`], with what it depends on passed in.
fn readable_icon(color: Color32, icons: IconColors, fallback: Color32) -> Color32 {
    match icons {
        IconColors::Kept => color,
        IconColors::ReadableOn { fill } if contrast_ratio(color, fill) >= MIN_ICON_CONTRAST => {
            color
        }
        IconColors::ReadableOn { .. } => fallback,
    }
}

/// Light text on a contrast sidebar: full, soft, and faint.
const CONTRAST_INK: Color32 = rgb(0xF7, 0xF7, 0xFA);
const CONTRAST_INK_SOFT: Color32 = Color32::from_rgba_premultiplied(184, 184, 186, 186);
const CONTRAST_INK_FAINT: Color32 = Color32::from_rgba_premultiplied(122, 122, 124, 124);
const CONTRAST_HOVER: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
const CONTRAST_CARD: Color32 = Color32::from_rgba_premultiplied(28, 28, 28, 28);

/// A navigation list on an ordinary surface.
pub fn plain_nav_ink() -> NavInk {
    let palette = palette();
    NavInk {
        ink: palette.ink,
        ink_soft: palette.ink_soft,
        ink_faint: palette.ink_faint,
        selection: NavSelection::Fill {
            fill: palette.primary_soft,
            ink: palette.primary,
        },
        hover_fill: palette.surface_muted,
        badge_fill: palette.border,
        card_fill: palette.primary_soft,
        panel_fill: palette.surface_muted,
        icon_button: IconButtonInk::on_page(palette.ink_soft),
        switch: SwitchInk::on_page(),
        icons: IconColors::Kept,
    }
}

/// The sidebar's navigation, on the theme's sidebar fill.
pub fn sidebar_ink() -> NavInk {
    let SidebarFill::Contrast { selection } = form().sidebar else {
        return plain_nav_ink();
    };
    contrast_nav_ink(palette(), selection)
}

/// A dark or saturated sidebar's navigation, chosen entries drawn as `selection` says.
fn contrast_nav_ink(palette: &Palette, selection: ContrastSelection) -> NavInk {
    // The solid pair fills what stands out in the sidebar: a pressed icon
    // button, a switch that is on, and a filled chosen entry.
    let (solid_fill, solid_ink, selection) = match selection {
        ContrastSelection::Primary => (
            palette.primary,
            palette.on_primary,
            NavSelection::Fill {
                fill: palette.primary,
                ink: palette.on_primary,
            },
        ),
        ContrastSelection::Light => (
            CONTRAST_INK,
            palette.sidebar,
            NavSelection::Fill {
                fill: CONTRAST_INK,
                ink: palette.sidebar,
            },
        ),
        // Only the bar is in the primary color: label-sized coral on the
        // sidebar is too faint to read.
        ContrastSelection::Underline => (
            palette.primary,
            palette.on_primary,
            NavSelection::Underline {
                bar: palette.primary,
                ink: CONTRAST_INK,
            },
        ),
    };
    NavInk {
        ink: CONTRAST_INK,
        ink_soft: CONTRAST_INK_SOFT,
        ink_faint: CONTRAST_INK_FAINT,
        selection,
        hover_fill: CONTRAST_HOVER,
        badge_fill: CONTRAST_CARD,
        card_fill: CONTRAST_CARD,
        panel_fill: CONTRAST_CARD,
        // The page's light fills would glare here.
        icon_button: IconButtonInk {
            icon: CONTRAST_INK_SOFT,
            fill: CONTRAST_CARD,
            hover_icon: solid_ink,
            hover_fill: solid_fill,
        },
        // A primary track could match the sidebar itself.
        switch: SwitchInk {
            off_track: CONTRAST_INK_FAINT,
            on_track: solid_fill,
            off_knob: CONTRAST_INK,
            on_knob: solid_ink,
        },
        icons: IconColors::ReadableOn {
            fill: palette.sidebar,
        },
    }
}

/// How much shallower a raised control is than a raised card: small
/// controls under a card's full shadow look smudged.
const CONTROL_DEPTH_SCALE: i8 = 2;

/// The lighter depth a control gets on raised themes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlRaise {
    distance: i8,
    blur: u8,
}

impl ControlRaise {
    /// `None` on themes whose controls are flat and outlined.
    fn of(depth: Depth) -> Option<Self> {
        let Depth::Raised { distance, blur } = depth else {
            return None;
        };
        Some(Self {
            distance: (distance / CONTROL_DEPTH_SCALE).max(1),
            blur: blur / CONTROL_DEPTH_SCALE as u8,
        })
    }

    /// How far its lit edge and shadow paint past the control: a shadow
    /// spreads half its blur past the shape it is offset from.
    fn reach(self) -> f32 {
        f32::from(self.distance) + f32::from(self.blur) / 2.0
    }
}

/// The shapes of a control's body: on raised themes a small raised surface
/// with no outline, elsewhere `fill` with `outline` (if any).
pub fn control_shapes(
    rect: egui::Rect,
    radius: CornerRadius,
    fill: Color32,
    outline: Option<Color32>,
) -> egui::Shape {
    let palette = palette();
    let fill_shape = egui::Shape::rect_filled(rect, radius, fill);
    let shapes = match (ControlRaise::of(form().depth), outline) {
        (Some(raise), _) => {
            let side = |offset: i8, color: Color32| -> egui::Shape {
                Shadow {
                    offset: [offset, offset],
                    blur: raise.blur,
                    spread: 0,
                    color,
                }
                .as_shape(rect, radius)
                .into()
            };
            vec![
                side(-raise.distance, palette.highlight),
                side(raise.distance, palette.shadow),
                fill_shape,
            ]
        }
        (None, Some(outline)) => vec![
            fill_shape,
            egui::Shape::rect_stroke(
                rect,
                radius,
                Stroke::new(1.0, outline),
                egui::StrokeKind::Inside,
            ),
        ],
        (None, None) => vec![fill_shape],
    };
    egui::Shape::Vec(shapes)
}

/// Whole points a control's depth paints past its edges (its lit edge or
/// shadow), for layouts that must leave room for it. Only raised themes
/// draw outside.
pub fn control_shadow_reach() -> f32 {
    ControlRaise::of(form().depth).map_or(0.0, |raise| raise.reach().ceil())
}

/// The fill of a block set inside a card or dialog (a metric tile, a
/// settings group): lifted out like a control on raised themes, sunk to the
/// page's shade on flat ones (their controls take the lighter shade), softly
/// tinted elsewhere.
pub fn inset_fill() -> Color32 {
    match form().depth {
        Depth::Raised { .. } => surface(),
        Depth::Flat { .. } => backdrop(),
        Depth::Shadow { .. } | Depth::Bezel { .. } => surface_muted(),
    }
}

/// How a control at rest (a secondary button, a dropdown, a stepper, a text
/// field) is set apart from the card or page it sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlLook {
    pub fill: Color32,
    pub outline: Option<Color32>,
}

impl ControlLook {
    /// The outline as a hairline stroke, or none.
    pub fn stroke(self) -> Stroke {
        self.outline
            .map_or(Stroke::NONE, |outline| Stroke::new(1.0, outline))
    }
}

/// The surface's own color with a hairline on most themes; on flat ones a
/// shade apart from the card (darker in light mode, lighter in dark), with
/// no outline.
pub fn control_look() -> ControlLook {
    control_look_of(form(), palette())
}

fn control_look_of(form: &Form, palette: &Palette) -> ControlLook {
    match form.depth {
        Depth::Flat { .. } => ControlLook {
            fill: palette.surface_muted,
            outline: None,
        },
        Depth::Shadow { .. } | Depth::Raised { .. } | Depth::Bezel { .. } => ControlLook {
            fill: palette.surface,
            outline: Some(palette.border),
        },
    }
}

/// Raised themes sink fields into the page instead of outlining them; flat
/// themes fill them like their other controls.
pub fn field_look() -> ControlLook {
    field_look_of(form(), palette())
}

fn field_look_of(form: &Form, palette: &Palette) -> ControlLook {
    match form.depth {
        Depth::Raised { .. } => ControlLook {
            fill: palette.surface_muted,
            outline: None,
        },
        Depth::Shadow { .. } | Depth::Bezel { .. } | Depth::Flat { .. } => {
            control_look_of(form, palette)
        }
    }
}

/// Paints a control's body; see [`control_shapes`].
pub fn paint_control(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: CornerRadius,
    fill: Color32,
    outline: Option<Color32>,
) {
    painter.add(control_shapes(rect, radius, fill, outline));
}

/// Text that reads on `fill`: near-white on dark or saturated fills, the
/// palette's ink on light ones.
pub fn ink_on(fill: Color32) -> Color32 {
    if contrast_ratio(CONTRAST_INK, fill) >= contrast_ratio(ink_for_light_fills(), fill) {
        CONTRAST_INK
    } else {
        ink_for_light_fills()
    }
}

/// The darker of the two inks, whichever mode is drawn.
fn ink_for_light_fills() -> Color32 {
    LIGHT.ink
}

/// WCAG relative luminance of an sRGB color.
fn luminance(color: Color32) -> f32 {
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG contrast ratio between two opaque colors, from 1 to 21.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (light, dark) = {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b), a.min(b))
    };
    (light + 0.05) / (dark + 0.05)
}

/// The blocks of a row of solid tiles, left to right: the primary color,
/// the accent, teal, and ink, as in the Coral design. Status colors are left
/// out: as whole blocks they read as warnings, and the danger red is nearly
/// Coral's own primary.
pub fn solid_blocks() -> [Color32; 4] {
    solid_blocks_of(palette())
}

fn solid_blocks_of(palette: &Palette) -> [Color32; 4] {
    [
        palette.primary,
        palette.accent,
        palette.program,
        palette.ink,
    ]
}

/// A chart series' color, at rest and lit under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeriesInk {
    pub color: Color32,
    pub lit: Color32,
}

/// How far a lit accent series leans toward the ink: lighter on dark
/// pages, deeper on light ones.
const ACCENT_LIT_BLEND: f32 = 0.25;

/// The colors of `series` on the statistics page.
pub fn series_ink(series: Series) -> SeriesInk {
    series_ink_of(form(), palette(), series)
}

fn series_ink_of(form: &Form, palette: &Palette, series: Series) -> SeriesInk {
    let primary = SeriesInk {
        color: palette.primary,
        lit: palette.primary_hover,
    };
    match (form.charts, series) {
        (ChartColors::Primary, _) | (ChartColors::PrimaryAndAccent, Series::Main) => primary,
        (ChartColors::PrimaryAndAccent, Series::Second) => SeriesInk {
            color: palette.accent,
            lit: palette.accent.lerp_to_gamma(palette.ink, ACCENT_LIT_BLEND),
        },
    }
}

/// The colors of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonInk {
    pub fill: Color32,
    pub hover_fill: Color32,
    pub text: Color32,
    pub outline: Option<Color32>,
}

/// How much an ink-filled main button lightens under the pointer.
const INK_BUTTON_HOVER: f32 = 0.35;

/// The page's one main button (Add URL, Save).
pub fn main_button_ink() -> ButtonInk {
    let palette = palette();
    match form().main_button {
        MainButton::Primary => ButtonInk {
            fill: palette.primary,
            hover_fill: palette.primary_hover,
            text: palette.on_primary,
            outline: None,
        },
        MainButton::Ink => ButtonInk {
            fill: palette.ink,
            hover_fill: palette
                .ink
                .lerp_to_gamma(palette.ink_soft, INK_BUTTON_HOVER),
            text: palette.surface,
            outline: None,
        },
    }
}

/// Foreground and soft background for a category's file tile and icon.
pub fn look_colors(look: CategoryLook) -> (Color32, Color32) {
    let palette = palette();
    match look {
        CategoryLook::Archive => (palette.accent, palette.tile_peach),
        CategoryLook::Document => (palette.primary, palette.tile_lavender),
        CategoryLook::Music => (palette.music, palette.tile_pink),
        CategoryLook::Program => (palette.program, palette.tile_mint),
        CategoryLook::Video => (palette.video, palette.tile_violet),
        CategoryLook::Image => (palette.image, palette.tile_sky),
        CategoryLook::Generic => (palette.ink_soft, palette.surface_muted),
    }
}

/// Text and background colors for a status pill.
pub fn status_colors(status: &DownloadStatus) -> (Color32, Color32) {
    let palette = palette();
    match status {
        DownloadStatus::Downloading | DownloadStatus::Connecting => {
            (palette.primary, palette.primary_soft)
        }
        DownloadStatus::Queued | DownloadStatus::WaitingForSpace { .. } => {
            (palette.warning, palette.warning_soft)
        }
        DownloadStatus::Paused => (palette.ink_soft, palette.surface_muted),
        DownloadStatus::Completed => (palette.success, palette.success_soft),
        DownloadStatus::Failed { .. } => (palette.danger, palette.danger_soft),
    }
}

/// Fonts and spacing, set once at start.
pub fn install(ctx: &egui::Context, look: Look) {
    ctx.set_fonts(fonts());
    // egui keeps one style per theme; both get the same fonts and spacing.
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, regular(22.0)),
            (TextStyle::Body, regular(14.0)),
            (TextStyle::Button, regular(14.0)),
            (TextStyle::Small, regular(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
    });
    apply(ctx, look);
}

/// About how tall egui's own buttons and fields are, for their corners.
const EGUI_CONTROL_HEIGHT: f32 = 28.0;

/// Switches the palette and egui's own widget colors and shapes.
pub fn apply(ctx: &egui::Context, look: Look) {
    let mode = look.mode;
    LOOK.store(look.to_bits(), Ordering::Relaxed);
    let palette = palette_of(look);
    let form = form_for(look.colors);
    // Pinning the theme stops egui from swapping in its own style when
    // Windows changes; the app follows the system itself (see `mode_for`).
    let theme = match mode {
        Mode::Light => egui::Theme::Light,
        Mode::Dark => egui::Theme::Dark,
    };
    ctx.set_theme(theme);
    ctx.style_mut_of(theme, |style| {
        let visuals = &mut style.visuals;
        *visuals = match mode {
            Mode::Light => egui::Visuals::light(),
            Mode::Dark => egui::Visuals::dark(),
        };
        visuals.panel_fill = palette.backdrop;
        visuals.window_fill = palette.surface;
        visuals.extreme_bg_color = palette.surface_muted;
        visuals.faint_bg_color = palette.surface_muted;
        visuals.override_text_color = Some(palette.ink);
        visuals.hyperlink_color = palette.primary;
        visuals.selection.bg_fill = palette.primary_soft;
        visuals.selection.stroke = Stroke::new(1.0, palette.primary);
        visuals.window_corner_radius = CornerRadius::same(form.window_radius);
        visuals.window_shadow = shadow_of(form, palette);
        visuals.window_stroke = Stroke::NONE;
        visuals.popup_shadow = shadow_of(form, palette);
        let control = control_radius_of(form, EGUI_CONTROL_HEIGHT);
        visuals.menu_corner_radius = control;
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = control;
        }
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
        visuals.widgets.noninteractive.bg_fill = palette.surface;
        visuals.widgets.inactive.bg_fill = palette.surface_muted;
        visuals.widgets.inactive.weak_bg_fill = palette.surface_muted;
        visuals.widgets.inactive.bg_stroke = control_look_of(form, palette).stroke();
        visuals.widgets.hovered.bg_fill = palette.primary_soft;
        visuals.widgets.hovered.weak_bg_fill = palette.primary_soft;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.primary);
        visuals.widgets.active.bg_fill = palette.primary;
        visuals.widgets.active.weak_bg_fill = palette.primary_soft;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.primary);
        visuals.widgets.open.weak_bg_fill = palette.surface_muted;
    });
}

/// Uses Segoe UI on Windows when available; elsewhere (or if the files are
/// missing) the bundled egui fonts stand in for every weight.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut family_for = |name: &str, file: &str| -> Vec<String> {
        let mut family = fallback.clone();
        if let Ok(bytes) = std::fs::read(system_font_path(file)) {
            fonts
                .font_data
                .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
            family.insert(0, name.to_owned());
        }
        family
    };
    let regular_family = family_for("segoe-regular", "segoeui.ttf");
    let semibold_family = family_for("segoe-semibold", "seguisb.ttf");
    let bold_family = family_for("segoe-bold", "segoeuib.ttf");
    fonts
        .families
        .insert(FontFamily::Proportional, regular_family);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), semibold_family);
    fonts
        .families
        .insert(FontFamily::Name(BOLD.into()), bold_family);
    fonts
}

fn system_font_path(file: &str) -> std::path::PathBuf {
    let windows = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into());
    windows.join("Fonts").join(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_look_survives_its_storage() {
        let mut seen = std::collections::HashSet::new();
        for colors in ColorTheme::ALL {
            for mode in [Mode::Light, Mode::Dark] {
                let look = Look { colors, mode };
                assert_eq!(Look::from_bits(look.to_bits()), look);
                assert!(seen.insert(look.to_bits()), "{look:?} shares its bits");
            }
        }
    }

    /// WCAG AA: 4.5 for body text, 3 for large or bold text like buttons.
    const BODY_TEXT: f32 = 4.5;
    const BOLD_TEXT: f32 = 3.0;

    #[test]
    fn every_look_keeps_its_text_readable() {
        let mut unreadable = Vec::new();
        for colors in ColorTheme::ALL {
            for mode in [Mode::Light, Mode::Dark] {
                let palette = palette_of(Look { colors, mode });
                let mut at = |pair: &str, a: Color32, b: Color32, least: f32| {
                    let ratio = contrast_ratio(a, b);
                    if ratio < least {
                        unreadable
                            .push(format!("{colors:?} {mode:?} {pair}: {ratio:.2} < {least}"));
                    }
                };
                at("ink on surface", palette.ink, palette.surface, 7.0);
                at(
                    "soft ink on surface",
                    palette.ink_soft,
                    palette.surface,
                    BOLD_TEXT,
                );
                at(
                    "text on primary",
                    palette.on_primary,
                    palette.primary,
                    BOLD_TEXT,
                );
                at(
                    "text on hovered primary",
                    palette.on_primary,
                    palette.primary_hover,
                    BOLD_TEXT,
                );
                // The four solid tiles, each with the text `ink_on` picks.
                for block in solid_blocks_of(palette) {
                    at("text on a solid tile", ink_on(block), block, BOLD_TEXT);
                }
                // A light selection reuses this pair, the other way round.
                if let SidebarFill::Contrast { selection } = form_for(colors).sidebar {
                    at("sidebar ink", CONTRAST_INK, palette.sidebar, BODY_TEXT);
                    // An underlined entry's count sits on a badge of the bar's color.
                    if selection == ContrastSelection::Underline {
                        at(
                            "count on the chosen entry's badge",
                            ink_on(palette.primary),
                            palette.primary,
                            BODY_TEXT,
                        );
                    }
                }
            }
        }
        assert!(unreadable.is_empty(), "{unreadable:#?}");
    }

    #[test]
    fn every_theme_is_stored_as_its_place_in_the_list() {
        for (place, colors) in ColorTheme::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(colors as u8), place, "{colors:?}");
        }
    }

    #[test]
    fn downpour_is_the_default_and_the_first_swatch() {
        assert_eq!(ColorTheme::default(), ColorTheme::Downpour);
        assert_eq!(ColorTheme::ALL[0], ColorTheme::Downpour);
    }

    #[test]
    fn downpour_wears_the_icon_colors() {
        for palette in [&DOWNPOUR_LIGHT, &DOWNPOUR_DARK] {
            assert_eq!(palette.primary, ICON_CORAL);
            assert_eq!(palette.sidebar, ICON_SLATE);
        }
        assert_eq!(DOWNPOUR_DARK.accent, SUNNY_YELLOW);
    }

    /// Flat themes set controls apart by fill alone, so the fill must differ
    /// from the page, the cards, and the blocks inset into dialogs.
    #[test]
    fn flat_controls_stand_out_from_everything_they_sit_on() {
        for colors in ColorTheme::ALL {
            let form = form_for(colors);
            let Depth::Flat { .. } = form.depth else {
                continue;
            };
            for mode in [Mode::Light, Mode::Dark] {
                let palette = palette_of(Look { colors, mode });
                let control = control_look_of(form, palette);
                assert_eq!(control.outline, None, "{colors:?} {mode:?}");
                assert_eq!(field_look_of(form, palette), control, "{colors:?} {mode:?}");
                for (place, under) in [
                    ("page and inset blocks", palette.backdrop),
                    ("cards", palette.surface),
                ] {
                    assert_ne!(control.fill, under, "{colors:?} {mode:?} on {place}");
                }
            }
        }
    }

    #[test]
    fn raised_fields_sink_into_the_page() {
        assert_eq!(
            field_look_of(&CREAM, &CREAM_LIGHT),
            ControlLook {
                fill: CREAM_LIGHT.surface_muted,
                outline: None,
            }
        );
    }

    #[test]
    fn an_underlined_entry_reads_in_light_ink_with_a_primary_bar() {
        let palette = &DOWNPOUR_DARK;
        let ink = contrast_nav_ink(palette, ContrastSelection::Underline);
        assert_eq!(
            ink.selection,
            NavSelection::Underline {
                bar: palette.primary,
                ink: CONTRAST_INK,
            }
        );
        assert!(contrast_ratio(CONTRAST_INK, palette.sidebar) >= BODY_TEXT);
    }

    #[test]
    fn outlined_forms_keep_their_hairline_controls() {
        assert_eq!(
            control_look_of(&DASHBOARD, &LIGHT),
            ControlLook {
                fill: LIGHT.surface,
                outline: Some(LIGHT.border),
            }
        );
    }

    #[test]
    fn two_tone_charts_draw_the_second_series_in_the_accent() {
        let main = series_ink_of(&DOWNPOUR, &DOWNPOUR_DARK, Series::Main);
        let second = series_ink_of(&DOWNPOUR, &DOWNPOUR_DARK, Series::Second);
        assert_eq!(main.color, DOWNPOUR_DARK.primary);
        assert_eq!(second.color, DOWNPOUR_DARK.accent);
        assert_ne!(second.lit, second.color);
        // Single-color themes draw every series alike.
        assert_eq!(
            series_ink_of(&DASHBOARD, &LIGHT, Series::Second),
            series_ink_of(&DASHBOARD, &LIGHT, Series::Main)
        );
    }

    #[test]
    fn flat_cards_leave_the_shadow_to_floating_windows() {
        let shadow = shadow_of(&DOWNPOUR, &DOWNPOUR_DARK);
        assert_eq!((shadow.offset, shadow.blur), ([0, 12], 36));
    }

    #[test]
    fn the_first_five_themes_keep_the_dashboard_form() {
        let dashboard = [
            ColorTheme::Lavender,
            ColorTheme::Sunset,
            ColorTheme::Slate,
            ColorTheme::Ocean,
            ColorTheme::Graphite,
        ];
        for colors in dashboard {
            assert_eq!(form_for(colors), &DASHBOARD, "{colors:?}");
        }
        // The look these themes had before forms existed.
        assert_eq!(
            DASHBOARD,
            Form {
                card_radius: 16,
                window_radius: 18,
                controls: ControlShape::Rounded { radius: 10 },
                depth: Depth::Shadow { drop: 6, blur: 24 },
                sidebar: SidebarFill::Plain,
                panels: Placement::Edge,
                tiles: TileStyle::Tinted,
                rows: RowStyle::Lines,
                progress: ProgressStyle::Bar,
                titles: TitleStyle::Standard,
                icon_buttons: IconButtonShape::Square,
                main_button: MainButton::Primary,
                charts: ChartColors::Primary,
            }
        );
    }

    #[test]
    fn pills_round_their_ends_and_rounded_controls_keep_their_radius() {
        assert_eq!(control_radius_of(&CREAM, 36.0), CornerRadius::same(18));
        assert_eq!(control_radius_of(&DASHBOARD, 36.0), CornerRadius::same(10));
        assert_eq!(round_ends(1000.0), CornerRadius::same(u8::MAX), "clamped");
    }

    #[test]
    fn each_depth_casts_its_own_shadow() {
        let shadow = |form: &Form| shadow_of(form, &LIGHT);
        assert_eq!(
            (shadow(&DASHBOARD).offset, shadow(&DASHBOARD).blur),
            ([0, 6], 24)
        );
        assert_eq!((shadow(&CREAM).offset, shadow(&CREAM).blur), ([6, 6], 18));
        assert_eq!((shadow(&EMBER).offset, shadow(&EMBER).blur), ([0, 10], 30));
    }

    #[test]
    fn a_raised_control_is_half_as_deep_as_a_card() {
        let raise = ControlRaise::of(CREAM.depth).unwrap();
        assert_eq!(
            raise,
            ControlRaise {
                distance: 3,
                blur: 9
            }
        );
        assert_eq!(raise.reach(), 7.5);
        assert_eq!(ControlRaise::of(DASHBOARD.depth), None);
    }

    #[test]
    fn an_icon_that_vanishes_on_the_sidebar_gives_way_to_soft_ink() {
        let violet = IconColors::ReadableOn {
            fill: VIOLET_LIGHT.sidebar,
        };
        assert_eq!(
            readable_icon(VIOLET_LIGHT.primary, violet, Color32::WHITE),
            Color32::WHITE
        );
        let near_black = IconColors::ReadableOn {
            fill: CORAL_LIGHT.sidebar,
        };
        assert_eq!(
            readable_icon(CORAL_LIGHT.accent, near_black, Color32::WHITE),
            CORAL_LIGHT.accent
        );
        assert_eq!(
            readable_icon(VIOLET_LIGHT.primary, IconColors::Kept, Color32::WHITE),
            VIOLET_LIGHT.primary
        );
    }

    #[test]
    fn text_on_a_fill_is_light_on_dark_and_dark_on_light() {
        assert_eq!(ink_on(Color32::BLACK), CONTRAST_INK);
        assert_eq!(ink_on(rgb(0x5B, 0x2F, 0xB0)), CONTRAST_INK);
        assert_eq!(ink_on(Color32::WHITE), LIGHT.ink);
        assert_eq!(ink_on(rgb(0xF4, 0xA2, 0x3C)), LIGHT.ink, "amber");
    }

    #[test]
    fn explicit_choices_ignore_the_system() {
        assert_eq!(
            mode_for(Appearance::Light, Some(egui::Theme::Dark)),
            Mode::Light
        );
        assert_eq!(
            mode_for(Appearance::Dark, Some(egui::Theme::Light)),
            Mode::Dark
        );
    }

    #[test]
    fn matching_the_system_defaults_to_light_when_unknown() {
        assert_eq!(
            mode_for(Appearance::MatchSystem, Some(egui::Theme::Dark)),
            Mode::Dark
        );
        assert_eq!(
            mode_for(Appearance::MatchSystem, Some(egui::Theme::Light)),
            Mode::Light
        );
        assert_eq!(mode_for(Appearance::MatchSystem, None), Mode::Light);
    }
}
