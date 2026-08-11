pub const BASE: u32 = 0x0B0F14;
pub const SURFACE: u32 = 0x111821;
pub const SURFACE_RAISED: u32 = 0x18212C;
pub const BORDER: u32 = 0x263241;

pub const TEXT: u32 = 0xF3F7FB;
pub const TEXT_MUTED: u32 = 0x91A0B3;

pub const PRIMARY: u32 = 0x22D3EE;
pub const PRIMARY_HOVER: u32 = 0x67E8F9;
pub const SUCCESS: u32 = 0x34D399;
pub const WARNING: u32 = 0xF59E0B;
pub const METER_WARNING: u32 = 0xFBBF24;
pub const DANGER: u32 = 0xF87171;

pub const FONT_UI: &str = "Inter";
pub const FONT_VALUES: &str = "JetBrains Mono";

// Compatibility aliases keep component styling readable while all colors resolve
// to the constrained Orion palette above.
pub const BASE_RAISED: u32 = SURFACE;
pub const SURFACE_2: u32 = SURFACE_RAISED;
pub const BORDER_STRONG: u32 = PRIMARY_HOVER;
pub const ACCENT: u32 = PRIMARY;
pub const TEXT_FAINT: u32 = TEXT_MUTED;
pub const GREEN: u32 = SUCCESS;
pub const YELLOW: u32 = METER_WARNING;
pub const RED: u32 = DANGER;
pub const AMBER: u32 = WARNING;
pub const ROUTE_OFF: u32 = SURFACE_RAISED;
pub const ROUTE_OFF_HOVER: u32 = BORDER;
