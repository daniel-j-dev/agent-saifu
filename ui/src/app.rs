//! The app shell: header, page, tab bar, sheets, request popup and toast.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::Backend;
use crate::pages::{ActivityPage, SettingsPage, WalletPage};
use crate::sheets::SheetHost;
use crate::state::{AppState, Sheet, Tab};
use crate::swap::SwapPage;
use crate::tx::{RequestPopup, TransactionScreen};
use crate::widgets::press_primary_on_enter;

const STYLE: &str = include_str!("../assets/main.css");
const POLL_EVERY: Duration = Duration::from_secs(3);

#[component]
pub fn App() -> Element {
    let backend = use_context_provider(Backend::start);
    let state = use_context_provider(AppState::new);

    use_future({
        let backend = backend.clone();
        move || {
            let backend = backend.clone();
            async move { state.refresh_wallets(&backend).await }
        }
    });
    // Connected apps can ask for something at any time, so look for news while the app is open.
    use_future(move || {
        let backend = backend.clone();
        async move {
            loop {
                state.poll_walletconnect(&backend).await;
                tokio::time::sleep(POLL_EVERY).await;
            }
        }
    });

    let screen = state.screen.read().clone();
    rsx! {
        document::Title { "Saifu — Your wallet, your control" }
        style { dangerous_inner_html: STYLE }
        div { class: "desktop-note",
            div { class: "brand",
                "◈ saifu"
                span { "YOUR KEYS. YOUR CALL." }
            }
            p {
                "A little wallet."
                br {}
                "A world of possibility."
            }
            div { class: "note-foot", "SELF-CUSTODY · AGENT READY" }
        }
        div { id: "app",
            RequestPopup {}
            header {
                button { class: "wordmark", aria_label: "Home", onclick: move |_| state.go(Tab::Wallet),
                    "◈ saifu"
                    span { "ウォレット" }
                }
                div { class: "header-actions",
                    button {
                        class: if *state.refreshing.read() { "icon-btn refreshing" } else { "icon-btn" },
                        aria_label: "Refresh balances",
                        title: "Refresh balances",
                        disabled: *state.refreshing.read(),
                        onclick: move |_| state.refresh(),
                        span { class: "refresh-glyph", "↻" }
                    }
                    button { class: "icon-btn", aria_label: "Manage wallets", onclick: move |_| state.open(Sheet::Wallets), "▦" }
                }
            }
            main {
                id: "main",
                onkeydown: move |e| press_primary_on_enter(&e, "#main"),
                if let Some(id) = screen {
                    TransactionScreen { key: "{id}", id }
                } else {
                    match *state.tab.read() {
                        Tab::Wallet | Tab::Agent => rsx! { WalletPage {} },
                        Tab::Swap => rsx! { SwapPage {} },
                        Tab::Activity => rsx! { ActivityPage {} },
                        Tab::Settings => rsx! { SettingsPage {} },
                    }
                }
            }
            TabBar {}
        }
        SheetHost {}
        Toast {}
    }
}

fn tab_icon(tab: Tab) -> &'static str {
    match tab {
        Tab::Wallet => "M3 5h18v15H3z",
        Tab::Swap => "M4 8h15m-4-4 4 4-4 4M20 16H5m4-4-4 4 4 4",
        Tab::Activity => "M4 17V9m5 8V5m6 12v-6m5 6V3",
        Tab::Agent => "m12 3 3 6 6 3-6 3-3 6-3-6-6-3 6-3Z",
        Tab::Settings => {
            "M12 8a4 4 0 100 8 4 4 0 000-8ZM10 2h4l1 3 3 1 3 4v4l-3 4-3 1-1 3h-4l-1-3-3-1-3-4v-4l3-4 3-1Z"
        }
    }
}

#[component]
fn TabBar() -> Element {
    let state = use_context::<AppState>();
    let active = if state.screen.read().is_some() {
        Tab::Activity
    } else {
        *state.tab.read()
    };
    let waiting = state.requests.read().len();
    rsx! {
        nav {
            for (tab , label) in [(Tab::Wallet, "Wallet"), (Tab::Swap, "Swap"), (Tab::Activity, "Activity"), (Tab::Agent, "Agent"), (Tab::Settings, "Settings")] {
                button {
                    key: "{label}",
                    class: if active == tab { "active" } else { "" },
                    aria_current: if active == tab { "page" } else { "false" },
                    aria_disabled: if tab == Tab::Agent { "true" } else { "false" },
                    title: if tab == Tab::Agent { "Coming soon" } else { "" },
                    onclick: move |_| state.go(tab),
                    span { class: "navicon",
                        svg { view_box: "0 0 24 24",
                            if tab == Tab::Wallet {
                                rect {
                                    x: "3",
                                    y: "5",
                                    width: "18",
                                    height: "15",
                                    rx: "4",
                                }
                                path { d: "M16 10h5v5h-5a2.5 2.5 0 010-5Z" }
                            } else if tab == Tab::Settings {
                                circle { cx: "12", cy: "12", r: "4" }
                                path { d: "m10 2 4 0 1 3 3 1 3 4v4l-3 4-3 1-1 3h-4l-1-3-3-1-3-4v-4l3-4 3-1Z" }
                            } else {
                                path { d: "{tab_icon(tab)}" }
                            }
                        }
                        if tab == Tab::Activity && waiting > 0 {
                            span { class: "nav-badge", "{waiting}" }
                        }
                    }
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn Toast() -> Element {
    let state = use_context::<AppState>();
    let message = state.toast.read().as_ref().map(|t| t.1.clone());
    let shown = message.is_some();
    let text = message.unwrap_or_default();
    rsx! {
        div { id: "toast", class: if shown { "show" } else { "" }, role: "status",
            "{text}"
        }
    }
}
