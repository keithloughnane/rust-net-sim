//! Colours shared by the canvas and the side panels.

use eframe::egui::Color32;

pub(crate) const CANVAS_BG: Color32 = Color32::from_rgb(16, 18, 23);
pub(crate) const GRID_DOT: Color32 = Color32::from_rgb(38, 42, 52);
pub(crate) const NODE_BG: Color32 = Color32::from_rgb(30, 34, 43);
pub(crate) const NODE_BG_DIM: Color32 = Color32::from_rgb(22, 25, 31);
pub(crate) const NODE_BORDER: Color32 = Color32::from_rgb(88, 96, 112);
pub(crate) const TEXT: Color32 = Color32::from_rgb(222, 226, 234);
pub(crate) const TEXT_WEAK: Color32 = Color32::from_rgb(120, 128, 142);

/// Distinct hues that read well on a dark background.
const PALETTE: [Color32; 10] = [
    Color32::from_rgb(77, 208, 225),  // cyan
    Color32::from_rgb(255, 167, 38),  // orange
    Color32::from_rgb(171, 136, 255), // violet
    Color32::from_rgb(156, 204, 101), // lime
    Color32::from_rgb(240, 98, 146),  // pink
    Color32::from_rgb(100, 181, 246), // sky
    Color32::from_rgb(255, 213, 79),  // gold
    Color32::from_rgb(255, 138, 101), // coral
    Color32::from_rgb(77, 182, 172),  // teal
    Color32::from_rgb(186, 104, 200), // purple
];

/// FNV-1a: a stable hash, so a name always gets the same colour across runs.
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn pick(s: &str, salt: u64) -> Color32 {
    let index = (hash(s) ^ salt) % PALETTE.len() as u64;
    PALETTE[usize::try_from(index).unwrap_or(0)]
}

/// Colour for a link, chosen by name so the same link looks the same everywhere.
pub(crate) fn link_color(name: &str) -> Color32 {
    pick(name, 0)
}

/// Accent colour for a node kind.
pub(crate) fn kind_color(kind: &str) -> Color32 {
    pick(kind, 0x9E37_79B9)
}
