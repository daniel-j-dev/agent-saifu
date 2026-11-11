//! The tab pages: Wallet, Activity and Settings (Swap lives in `swap.rs`).

use agent_saifu_core::blockchain::Blockchain;
use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::Args;
use crate::fmt::{
    chain_symbol, expires_in, parse_chain_id, short_address, supports_walletconnect, trim_amount,
    usd,
};
use crate::model::{Asset, WcRequest, text};
use crate::state::{ActivityKind, AppState, Sheet, Tab, ago, now_secs};
use crate::widgets::*;

/// Shown when no wallet exists yet or the wallet list is still loading.
pub(crate) fn wallet_gate(state: AppState, title: &str, subtitle: &str) -> Option<Element> {
    let message = match &*state.wallets.read() {
        None => {
            return Some(rsx! {
                Intro { title: title.to_string(), subtitle: subtitle.to_string() }
                Busy { label: "Opening your vault…" }
            });
        }
        Some(Err(e)) => {
            let message = e.clone();
            return Some(rsx! {
                Intro { title: title.to_string(), subtitle: subtitle.to_string() }
                Alert { message }
            });
        }
        Some(Ok(list)) if list.is_empty() => "Create or import a wallet to get started.",
        Some(Ok(_)) => return None,
    };
    Some(rsx! {
        div { class: "empty",
            div { class: "agent-mark", "◈" }
            h2 { "Welcome to Saifu." }
            p { class: "sub gap", "{message}" }
            button { class: "primary", onclick: move |_| state.open(Sheet::Create), "Create a wallet" }
            button { class: "secondary", onclick: move |_| state.open(Sheet::Import), "Import an existing wallet" }
        }
    })
}

#[component]
pub fn WalletPage() -> Element {
    let state = use_context::<AppState>();
    if let Some(gate) = wallet_gate(state, "Your wallet.", "A space of your own.") {
        return gate;
    }
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let known = state.current_snapshot().is_some();
    let snapshot = state.visible_snapshot();
    let unlocked = snapshot.is_some();
    let balance = match &snapshot {
        Some(s) => usd(s.total_usd()),
        None => "••••••".to_string(),
    };
    let change = match (&snapshot, known) {
        (Some(s), _) => short_address(&s.address),
        (None, true) => "Balances hidden".to_string(),
        (None, false) => "Balances are private".to_string(),
    };
    let problem = snapshot.as_ref().and_then(|s| s.error.clone());
    let networks = snapshot.as_ref().map(|s| s.networks().len()).unwrap_or(0);
    let assets: Vec<Asset> = snapshot
        .as_ref()
        .map(|s| s.assets.clone())
        .unwrap_or_default();
    let count = assets.len();
    let symbol = chain_symbol(wallet.chain);
    let chain_name = wallet.chain.display_name();
    let is_eth = wallet.chain == Blockchain::Ethereum;

    rsx! {
        div { class: "row between intro",
            div {
                p { class: "eyebrow", "A SPACE OF YOUR OWN" }
                h1 { class: "tight", "Your wallet." }
            }
            button { class: "pill", onclick: move |_| state.open(Sheet::Wallets),
                Coin { symbol: symbol.to_string(), small: true }
                "{chain_name} ⌄"
            }
        }
        section { class: "balance-card",
            div { class: "row between",
                span { class: "eyebrow", "{wallet.name}" }
                button {
                    class: "icon-btn",
                    aria_label: if unlocked { "Hide balances" } else { "Show balances" },
                    onclick: move |_| {
                        if unlocked {
                            state.set_hidden(true);
                        } else if known {
                            state.set_hidden(false);
                        } else {
                            state.open(Sheet::Unlock);
                        }
                    },
                    if unlocked { "◉" } else { "⊘" }
                }
            }
            div { class: "balance", "{balance}" }
            div { class: "change", "{change}" }
            div { class: "card-bottom",
                span {
                    if unlocked {
                        "♧ Tap ◉ to hide · ↻ to refresh"
                    } else if known {
                        "♧ Hidden · tap ⊘ to show"
                    } else {
                        "♧ Locked · authenticate to view"
                    }
                }
                span { "{chain_name}" }
            }
        }
        div { class: "actions",
            ActionButton { label: "Send", icon: "M6 18 18 6M6 6h12v12", onclick: move |_| state.open(Sheet::Send(None)) }
            ActionButton { label: "Receive", icon: "M18 6 6 18M6 6v12h12", onclick: move |_| state.open(Sheet::Receive) }
            ActionButton { label: "Swap", icon: "M4 8h15m-4-4 4 4-4 4M20 16H5m4-4-4 4 4 4", onclick: move |_| state.go(Tab::Swap) }
            ActionButton {
                label: "Connect",
                icon: "M4 4h16v16H4zM8 12h8m-4-4v8",
                onclick: move |_| {
                    if supports_walletconnect(wallet.chain) {
                        state.open(Sheet::Connect);
                    } else {
                        state.toast("WalletConnect is not available for Bitcoin wallets.");
                    }
                },
            }
        }
        div { class: "section-head",
            h3 {
                "Your assets "
                if unlocked {
                    span { class: "sub", "{count}" }
                }
            }
            button { class: "text-btn", onclick: move |_| state.open(Sheet::Networks), "Networks ⌄" }
        }
        if unlocked {
            if let Some(problem) = problem {
                Alert { message: format!("Balances could not be loaded: {problem} Tap ↻ to try again.") }
            }
            div {
                for asset in assets.iter().cloned() {
                    AssetRow { asset, show_network: networks > 1 }
                }
            }
            if is_eth {
                button { class: "text-btn", onclick: move |_| state.open(Sheet::Tokens), "+ Manage tokens" }
            }
        } else if known {
            Notice { "Balances are hidden. Showing them again needs no password." }
            button { class: "primary", onclick: move |_| state.set_hidden(false), "Show balances" }
        } else {
            Notice { "Your address and balances stay encrypted until you unlock this wallet. Enter your password once; the wallet's keys lock again right after." }
            button { class: "primary", onclick: move |_| state.open(Sheet::Unlock), "Unlock to view {symbol} balance" }
        }
        p { class: "demo-note", "SELF-CUSTODY, SIMPLIFIED" }
    }
}

#[component]
fn ActionButton(label: String, icon: String, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button { class: "action", onclick: move |e| onclick.call(e),
            i {
                svg {
                    view_box: "0 0 24 24",
                    width: "23",
                    height: "23",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.6",
                    path { d: "{icon}" }
                }
            }
            "{label}"
        }
    }
}

#[component]
fn AssetRow(asset: Asset, show_network: bool) -> Element {
    let state = use_context::<AppState>();
    let value = asset.usd.map(usd).unwrap_or_else(|| "—".to_string());
    let sub = if show_network && !asset.network.is_empty() {
        format!(
            "{} {} · {}",
            trim_amount(&asset.balance, 8),
            asset.symbol,
            asset.network
        )
    } else {
        format!("{} {}", trim_amount(&asset.balance, 8), asset.symbol)
    };
    let picked = asset.clone();
    rsx! {
        button { class: "asset", onclick: move |_| state.open(Sheet::Asset(picked.clone())),
            Coin { symbol: asset.symbol.clone() }
            span { class: "grow",
                strong { "{asset.name}" }
                span { class: "sub", "{sub}" }
            }
            span { class: "right", strong { "{value}" } }
        }
    }
}

#[component]
pub fn ActivityPage() -> Element {
    let state = use_context::<AppState>();
    if let Some(gate) = wallet_gate(
        state,
        "Every move, in view.",
        "A clear record of your wallet activity.",
    ) {
        return gate;
    }
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let mut filter = use_signal(|| "All".to_string());
    let requests = state.requests.read().clone();
    let now = now_secs();
    let log: Vec<_> = state
        .activity
        .read()
        .iter()
        .filter(|a| match filter.read().as_str() {
            "Sent" => a.kind == ActivityKind::Sent,
            "Received" => a.kind == ActivityKind::Received,
            "Swapped" => a.kind == ActivityKind::Swapped,
            _ => true,
        })
        .cloned()
        .collect();
    let wallet_name = wallet.name.clone();
    rsx! {
        Intro { title: "Every move, in view.", subtitle: "A clear record of your wallet activity." }
        button { class: "pill", onclick: move |_| state.open(Sheet::Wallets), "{wallet_name} ⌄" }

        if !requests.is_empty() {
            div { class: "section-head",
                p { class: "eyebrow", "WAITING FOR YOU" }
            }
            for request in requests.iter().cloned() {
                RequestRow { request }
            }
        }

        div { class: "segmented",
            for name in ["All", "Sent", "Received", "Swapped"] {
                button {
                    class: if *filter.read() == name { "active" } else { "" },
                    onclick: move |_| filter.set(name.to_string()),
                    "{name}"
                }
            }
        }
        div { class: "section-head",
            p { class: "eyebrow", "THIS SESSION" }
            button { class: "text-btn", onclick: move |_| state.open(Sheet::Lookup), "Find transaction ↗" }
        }
        if log.is_empty() {
            div { class: "empty sub", "Nothing yet. Payments, swaps and signatures you approve appear here." }
        }
        for item in log.iter() {
            div { class: "asset static",
                span { class: "coin plain",
                    match item.kind {
                        ActivityKind::Received => "↙",
                        ActivityKind::Sent => "↗",
                        ActivityKind::Swapped => "⇄",
                        ActivityKind::Signed => "✎",
                        ActivityKind::Connected => "⊞",
                    }
                }
                span { class: "grow",
                    strong { "{item.title}" }
                    span { class: "sub", "{ago(item.at, now)}" }
                }
                span { class: "right",
                    strong { class: "small", "{item.detail}" }
                    span { class: "sub positive", "{item.status}" }
                }
            }
        }
        HistoryPanel {}
    }
}

#[component]
pub fn RequestRow(request: WcRequest) -> Element {
    let state = use_context::<AppState>();
    let id = request.id.clone();
    let amount = request
        .native_amount()
        .unwrap_or_else(|| request.chain.clone());
    rsx! {
        button { class: "asset request", onclick: move |_| state.open_request(&id),
            span { class: "coin plain pulse", "⇄" }
            span { class: "grow",
                strong { "{request.title()}" }
                span { class: "sub", "{request.dapp.title()} · {expires_in(request.expires_in)}" }
            }
            span { class: "right",
                strong { class: "small", "{amount}" }
                span { class: "sub warn-text", "Review ›" }
            }
        }
    }
}

/// Loads the wallet's on-chain history; asks for the password because the wallet must unlock to
/// know its address.
#[component]
fn HistoryPanel() -> Element {
    let mut state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let password = use_secret();
    let chain_id = use_signal(|| "1".to_string());
    let task = use_task();
    let mut open_item = use_signal(|| None::<usize>);
    let history = state
        .history
        .read()
        .clone()
        .filter(|(w, _)| *w == wallet.name);
    let is_eth = wallet.chain == Blockchain::Ethereum;
    let name = wallet.name.clone();
    let load = move |_| {
        let chain = if is_eth {
            match parse_chain_id(&chain_id.read()) {
                Some(c) => Some(c),
                None => return state.toast("Enter a numeric chain ID, e.g. 1."),
            }
        } else {
            None
        };
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .opt_int("chain_id", chain)
            .int("limit", 20)
            .build();
        let name = name.clone();
        run_tool(task, "get_transaction_history", args, move |answer| {
            state.history.set(Some((name, answer.clone())));
            wipe(password);
        });
    };
    let items: Vec<Value> = history
        .as_ref()
        .and_then(|(_, v)| v.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_history = history.is_some();
    rsx! {
        div { class: "section-head",
            p { class: "eyebrow", "ON-CHAIN HISTORY" }
            if has_history {
                button { class: "text-btn", onclick: move |_| state.history.set(None), "Hide" }
            }
        }
        if !has_history {
            div { class: "panel",
                p { class: "sub", "Loading history unlocks the wallet for this one request." }
                if is_eth {
                    Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
                }
                PasswordField { value: password }
                TaskStatus { task, label: "Fetching history…" }
                button { class: "secondary", disabled: task.read().is_busy(), onclick: load, "Load history" }
            }
        } else if items.is_empty() {
            div { class: "empty sub", "No transactions found." }
        }
        for (index , item) in items.iter().enumerate() {
            {
                let id = ["transaction_hash", "hash", "digest", "signature", "txid"]
                    .iter()
                    .map(|k| text(item, k))
                    .find(|s| !s.is_empty())
                    .unwrap_or_else(|| "Transaction".to_string());
                let when = ["timestamp", "block_time_unix", "block_time", "time"]
                    .iter()
                    .map(|k| match item.get(k) {
                        Some(Value::Number(n)) => n.clone(),
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    })
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
                let status = text(item, "status");
                let expanded = *open_item.read() == Some(index);
                let detail = item.clone();
                rsx! {
                    button {
                        class: "asset",
                        onclick: move |_| open_item.set(if expanded { None } else { Some(index) }),
                        span { class: "coin plain", "⇄" }
                        span { class: "grow",
                            strong { "{short_address(&id)}" }
                            span { class: "sub", "{when}" }
                        }
                        span { class: "right", span { class: "sub positive", "{status}" } }
                    }
                    if expanded {
                        div { class: "panel", JsonView { value: detail } }
                    }
                }
            }
        }
    }
}

#[component]
pub fn SettingsPage() -> Element {
    let state = use_context::<AppState>();
    if let Some(gate) = wallet_gate(
        state,
        "Make it yours.",
        "A few thoughtful controls. Nothing in your way.",
    ) {
        return gate;
    }
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let connections = state.sessions.read().len();
    let is_eth = wallet.chain == Blockchain::Ethereum;
    let symbol = chain_symbol(wallet.chain);
    let chain_name = wallet.chain.display_name();
    let connection_text = format!(
        "{connections} active connection{}",
        if connections == 1 { "" } else { "s" }
    );
    rsx! {
        Intro { title: "Make it yours.", subtitle: "A few thoughtful controls. Nothing in your way." }
        div { class: "panel soft",
            div { class: "row",
                Coin { symbol: symbol.to_string() }
                div { class: "grow",
                    h3 { "{wallet.name}" }
                    p { class: "sub", "{chain_name} · vault locked" }
                }
            }
        }
        p { class: "eyebrow section", "WALLET & SECURITY" }
        LinkRow { icon: "▦", title: "Manage wallets", subtitle: "Create, import, rename, or remove", onclick: move |_| state.open(Sheet::Wallets) }
        LinkRow { icon: "♧", title: "Recovery & backup", subtitle: "Recovery phrase, private key, encrypted export", onclick: move |_| state.open(Sheet::Security) }
        LinkRow { icon: "◎", title: "Networks & tokens", subtitle: "Manage supported networks and assets", onclick: move |_| state.open(Sheet::Networks) }
        LinkRow { icon: "↗", title: "Connected apps", subtitle: connection_text, onclick: move |_| state.open(Sheet::Sessions) }
        if is_eth {
            LinkRow { icon: "⊘", title: "Token approvals", subtitle: "Review and revoke allowances", onclick: move |_| state.open(Sheet::Approvals) }
        }
        p { class: "eyebrow section", "TOOLS" }
        LinkRow { icon: "⌘", title: "Advanced tools", subtitle: "Sign messages and review transactions", onclick: move |_| state.open(Sheet::Advanced) }
        LinkRow { icon: "◈", title: "About Saifu", subtitle: "Built for you. Ready for agents.", onclick: move |_| state.open(Sheet::About) }
        p { class: "demo-note", "AGENT-SAIFU · VERSION 0.0" }
    }
}
