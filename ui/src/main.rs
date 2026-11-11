//! Dioxus app for agent-saifu (desktop, Android, iOS). The wallet itself lives in `../core`.

// The demo wallet bypasses the real one, so some of its plumbing is unused there.
#![cfg_attr(feature = "demo", allow(dead_code))]

mod app;
mod backend;
#[cfg(feature = "demo")]
mod demo;
mod fmt;
mod model;
mod pages;
mod picker;
mod qr;
mod sheets;
mod state;
mod swap;
mod tokens;
mod tx;
mod widgets;

fn main() {
    #[cfg(feature = "desktop")]
    {
        use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
        let window = WindowBuilder::new()
            .with_title("Saifu")
            .with_inner_size(LogicalSize::new(520.0, 900.0))
            .with_min_inner_size(LogicalSize::new(340.0, 560.0));
        let config = Config::new().with_window(window).with_menu(None);
        dioxus::LaunchBuilder::new()
            .with_cfg(config)
            .launch(app::App);
    }
    #[cfg(not(feature = "desktop"))]
    dioxus::launch(app::App);
}
