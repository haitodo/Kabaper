use std::sync::atomic::AtomicU32;
use windows::Win32::Foundation::*;
use windows::core::*;

pub const OVERLAY_CLASS_NAME: PCWSTR = w!("KabaperOverlayClass");
pub static SEQ_COUNTER: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Normal,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalDialog {
    None,
    Settings,
    ExcludeList,
    Shortcuts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarButtonId {
    Open,
    Sidebar,
    ViewMode,
    Monitor,
    Split,
    Scale,
    Apply,
    Random,
    UpdateWallpaper,
    Undo,
    SavePng,
    Settings,
    Shortcuts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOverlayButtonId {
    Pin,
    Swap,
    Random,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModernButtonStyle {
    Primary,         // Google Blue, White text, 8px radius
    SecondaryTonal,  // Surface High, Text Primary, subtle border, 8px radius
    SegmentActive,   // Primary Accent border, container fill, 8px radius
    SegmentInactive, // Low container, Text Secondary, 8px radius
    Ghost,           // Low contrast, Text Secondary, 8px radius
}

#[inline]
pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

pub const M3_BG: COLORREF = rgb(15, 17, 19);                 // #0F1113 Deep background
pub const M3_SURFACE_BAR: COLORREF = rgb(26, 29, 32);         // #1A1D20 App bar surface
pub const M3_SURFACE_CARD: COLORREF = rgb(34, 37, 42);        // #22252A Modal / Dialog surface
pub const M3_SURFACE_HIGH: COLORREF = rgb(44, 48, 54);        // #2C3036 Button surface
pub const M3_SURFACE_HIGHEST: COLORREF = rgb(53, 58, 66);     // #353A42 Active control / inputs
pub const M3_OUTLINE: COLORREF = rgb(62, 68, 77);             // #3E444D Distinct outline
pub const M3_OUTLINE_VARIANT: COLORREF = rgb(45, 50, 58);     // #2D323A Subtle border / divider
pub const M3_PRIMARY_BLUE: COLORREF = rgb(11, 87, 208);       // #0B57D0 Google Blue Primary
pub const M3_PRIMARY_TEXT: COLORREF = rgb(255, 255, 255);     // #FFFFFF Primary text on dark/blue
pub const M3_PRIMARY_ACCENT: COLORREF = rgb(168, 199, 250);   // #A8C7FA Google Blue tint
pub const M3_SECONDARY_MINT: COLORREF = rgb(109, 213, 140);   // #6DD58C Google Mint Green (Success/OSD)
pub const M3_CYAN_PINNED: COLORREF = rgb(128, 216, 255);      // #80D8FF Cyan for Pinned state
pub const M3_AMBER_BADGE: COLORREF = rgb(255, 183, 77);       // #FFB74D Amber for limits/warnings
pub const M3_TEXT_PRIMARY: COLORREF = rgb(240, 243, 248);     // #F0F3F8 High-emphasis text
pub const M3_TEXT_SECONDARY: COLORREF = rgb(176, 180, 188);   // #B0B4BC Medium-emphasis text
pub const M3_TEXT_MUTED: COLORREF = rgb(117, 123, 133);       // #757B85 Low-emphasis text
