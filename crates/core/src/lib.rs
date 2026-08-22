//! Dragon Agent core: everything shared by the CLI and the desktop app.
//!
//! One config, one memory, one session store - two faces.

pub mod agent;
pub mod config;
pub mod memory;
pub mod presets;
pub mod provider;
pub mod session;

pub const NAME: &str = "Dragon Agent";
pub const AUTHOR: &str = "mamad720220";
pub const TELEGRAM: &str = "@mamad720220";
pub const HOMEPAGE: &str = "https://github.com/mamad7202202/dragon-agent";

/// Kept in sync with the workspace version via Cargo env passthrough in bins;
/// core itself carries the same value at compile time of its own package.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
