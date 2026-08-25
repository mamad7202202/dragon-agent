//! Dragon Agent — desktop app, rebuilt on GPUI (Zed's GPU-accelerated UI framework).
//!
//! No terminal pre-flight, no console window: the update check runs async and
//! surfaces as an in-app chip. All agent work lives on a worker thread behind
//! channels (see `bridge.rs`).

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod bridge;
mod input;
mod theme;
mod views;

use anyhow::Result;
use app::DragonApp;
use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    prelude::*, size,
};

use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    // A broken config file must never prevent the window from opening.
    let cfg = dragon_core::config::Config::load().unwrap_or_default();
    let memory = Arc::new(Mutex::new(dragon_core::memory::MemoryStore::open()?));
    let graph = Arc::new(Mutex::new(dragon_core::memory::graph::GraphStore::open()?));

    Application::new().run(move |cx: &mut App| {
        input::init_keybindings(cx);
        app::init_keybindings(cx);

        let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Dragon Agent".into()),
                appears_transparent: false,
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            window_min_size: Some(size(px(900.), px(600.))),
            ..Default::default()
        };

        cx.open_window(options, move |_, cx| {
            cx.new(|cx| DragonApp::new(cfg, memory, graph, cx))
        })
        .expect("failed to open the dragon window");

        cx.activate(true);
    });

    Ok(())
}
