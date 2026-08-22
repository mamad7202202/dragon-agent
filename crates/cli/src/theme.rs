//! Dragon Agent visual language: one ember palette, used everywhere.

use ratatui::style::Color;

// The fire gradient (hot -> warm)
pub const EMBER: Color = Color::Rgb(255, 99, 71); // tomato - primary accent
pub const FLAME: Color = Color::Rgb(255, 152, 74); // orange
pub const GOLD: Color = Color::Rgb(255, 205, 112); // amber highlight

// Structure colors
pub const NIGHT: Color = Color::Rgb(16, 15, 18); // app background
pub const SMOKE: Color = Color::Rgb(30, 28, 33); // panel background
pub const ASH: Color = Color::Rgb(124, 122, 129); // muted text / hints
pub const BONE: Color = Color::Rgb(229, 226, 219); // primary text
pub const SCALE: Color = Color::Rgb(66, 62, 70); // borders

// Semantic accents
pub const JADE: Color = Color::Rgb(105, 210, 150); // success / tool ok
pub const SKY: Color = Color::Rgb(108, 170, 245); // user label
pub const VIOLET: Color = Color::Rgb(172, 140, 250); // tool activity
pub const BLOOD: Color = Color::Rgb(240, 84, 84); // errors
