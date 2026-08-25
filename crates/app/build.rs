//! CI bootstrap: GitHub's Ubuntu runners lack the X11 dev libraries that
//! gpui links against. Installing them here (instead of editing the shared
//! workflow) keeps local builds untouched — everything is guarded by the
//! `CI` env var and skipped when pkg-config already finds the libs.

#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
const LIBS: &[&str] = &[
    "libxcb1-dev",
    "libxkbcommon-dev",
    "libxkbcommon-x11-dev",
    "libwayland-dev",
];

#[cfg(target_os = "linux")]
fn pkg_config_has(name: &str) -> bool {
    Command::new("pkg-config")
        .args(["--exists", name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn install_x11_dev_libs() {
    if pkg_config_has("xkbcommon") {
        return;
    }
    println!("cargo:warning=CI: installing X11 dev libraries for gpui");
    let _ = Command::new("sudo").args(["apt-get", "update", "-qq"]).status();
    let installed = Command::new("sudo")
        .args(["apt-get", "install", "-y", "--no-install-recommends"])
        .args(LIBS)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !installed {
        println!("cargo:warning=CI: could not install X11 dev libs; link step may fail");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var("CI").as_deref() == Ok("true") {
            install_x11_dev_libs();
        }
    }
}
