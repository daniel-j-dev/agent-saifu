//! Connecting apps over WalletConnect: pair, approve or reject, and manage sessions.

use dioxus::prelude::*;

use crate::backend::Args;
use crate::fmt::{chain_label, expires_in, password_ok, supports_walletconnect};
use crate::model::{WcProposal, parse_proposals};
use crate::pages::RequestRow;
use crate::state::{ActivityKind, AppState, Sheet};
use crate::widgets::*;

#[component]
pub fn ConnectSheet() -> Element {
    let mut state = use_context::<AppState>();
    let uri = use_signal(String::new);
    let task = use_task();
    let connect = move |_| {
        let link = uri.read().trim().to_string();
        if !link.starts_with("wc:") {
            return state.toast("A WalletConnect link starts with wc:");
        }
        let args = Args::new()
            .text("uri", &link)
            .int("wait_seconds", 20)
            .build();
        run_tool(task, "wc_pair", args, move |answer| {
            let proposals = parse_proposals(answer);
            if let Some(first) = proposals.first() {
                let id = first.id.clone();
                state.proposals.set(proposals);
                state.open(Sheet::Proposal(id));
            }
        });
    };
    let waiting = task.read().answer().is_some();
    let pending = state.proposals.read().clone();
    rsx! {
        SheetFrame { title: "Connect to an app",
            p { class: "sub", "Paste the WalletConnect link (it starts with wc:) from the app you want to use." }
            div { class: "field",
                label { r#for: "f-wc-link", "WalletConnect link" }
                div { class: "input-row",
                    input {
                        id: "f-wc-link",
                        value: "{uri}",
                        placeholder: "wc:…",
                        autocomplete: "off",
                        autocapitalize: "off",
                        spellcheck: "false",
                        oninput: move |e| uri.clone().set(e.value()),
                    }
                    button { class: "pill", onclick: move |_| paste_into(uri, state), "Paste" }
                }
            }
            Notice { "Connecting shares nothing yet, and never lets an app spend on its own. You review and approve every request." }
            TaskStatus { task, label: "Waiting for the app to ask to connect…" }
            if waiting {
                Notice { warn: true, "The app has not sent a connection request yet. Keep its QR code open, then try again." }
            }
            button { class: "primary", disabled: task.read().is_busy(), onclick: connect, "Review connection" }
            if !pending.is_empty() {
                p { class: "eyebrow section", "WAITING TO CONNECT" }
                for proposal in pending.iter().cloned() {
                    {
                        let id = proposal.id.clone();
                        rsx! {
                            LinkRow {
                                icon: "⊞",
                                title: proposal.dapp.title(),
                                subtitle: expires_in(proposal.expires_in),
                                onclick: move |_| state.open(Sheet::Proposal(id.clone())),
                            }
                        }
                    }
                }
            }
            button { class: "text-btn", onclick: move |_| state.open(Sheet::Sessions), "Manage connected apps →" }
        }
    }
}

#[component]
pub fn SessionsSheet() -> Element {
    let state = use_context::<AppState>();
    let sessions = state.sessions.read().clone();
    let proposals = state.proposals.read().clone();
    let can_connect = state
        .current()
        .is_some_and(|w| supports_walletconnect(w.chain));
    rsx! {
        SheetFrame { title: "Connected apps",
            p { class: "sub", "Connections last until the app is closed. Reconnect after restarting Saifu." }
            for proposal in proposals.iter().cloned() {
                {
                    let id = proposal.id.clone();
                    rsx! {
                        LinkRow {
                            icon: "⊞",
                            title: format!("{} wants to connect", proposal.dapp.title()),
                            subtitle: "Tap to review",
                            onclick: move |_| state.open(Sheet::Proposal(id.clone())),
                        }
                    }
                }
            }
            for session in sessions.iter().cloned() {
                {
                    let topic = session.topic.clone();
                    rsx! {
                        LinkRow {
                            icon: "↗",
                            title: session.dapp.title(),
                            subtitle: format!("{} · {}", session.wallet, session.chains.join(", ")),
                            onclick: move |_| state.open(Sheet::Session(topic.clone())),
                        }
                    }
                }
            }
            if sessions.is_empty() && proposals.is_empty() {
                div { class: "empty sub", "No connected apps." }
            }
            if can_connect {
                button { class: "primary", onclick: move |_| state.open(Sheet::Connect), "Connect an app" }
            }
        }
    }
}

#[component]
pub fn SessionSheet(topic: String) -> Element {
    let state = use_context::<AppState>();
    let task = use_task();
    let session = state
        .sessions
        .read()
        .iter()
        .find(|s| s.topic == topic)
        .cloned();
    let Some(session) = session else {
        return rsx! {
            SheetFrame { title: "Connected app",
                div { class: "empty sub", "This connection has ended." }
                button { class: "secondary", onclick: move |_| state.open(Sheet::Sessions), "Back" }
            }
        };
    };
    let requests: Vec<_> = state
        .requests
        .read()
        .iter()
        .filter(|r| r.session_topic == topic)
        .cloned()
        .collect();
    let disconnect = {
        let topic = topic.clone();
        move |_| {
            let args = Args::new().text("session_topic", &topic).build();
            run_tool(task, "wc_disconnect_session", args, move |_| {
                state.toast("Disconnected.");
                state.open(Sheet::Sessions);
            });
        }
    };
    rsx! {
        SheetFrame { title: session.dapp.title(),
            Notice { warn: true, "App identity is self-declared and not verified." }
            div { class: "panel",
                if !session.dapp.url.is_empty() {
                    Detail { label: "Website", value: session.dapp.url.clone() }
                }
                Detail { label: "Wallet", value: session.wallet.clone() }
                Detail { label: "Networks", value: session.chains.join(", ") }
                Detail { label: "Methods", value: session.methods.join(", ") }
                Detail { label: "Confirmed by app", value: if session.confirmed { "yes" } else { "not yet" } }
            }
            for request in requests.iter().cloned() {
                RequestRow { request }
            }
            TaskStatus { task, label: "Disconnecting…" }
            button { class: "danger", disabled: task.read().is_busy(), onclick: disconnect, "Disconnect app" }
            button { class: "secondary", onclick: move |_| state.open(Sheet::Sessions), "Back" }
        }
    }
}

#[component]
pub fn ProposalSheet(id: String) -> Element {
    let state = use_context::<AppState>();
    let proposal: Option<WcProposal> = state.proposals.read().iter().find(|p| p.id == id).cloned();
    let Some(proposal) = proposal else {
        return rsx! {
            SheetFrame { title: "Connection request",
                div { class: "empty sub", "This request is no longer waiting. Ask the app to connect again." }
                button { class: "secondary", onclick: move |_| state.close(), "Close" }
            }
        };
    };
    let wallets: Vec<_> = state
        .wallet_list()
        .into_iter()
        .filter(|w| proposal.works_with(w.chain))
        .collect();
    let default_wallet = state
        .current()
        .filter(|w| proposal.works_with(w.chain))
        .or_else(|| wallets.first().cloned())
        .map(|w| w.name)
        .unwrap_or_default();
    let wallet = use_signal(|| default_wallet.clone());
    let chosen = wallets
        .iter()
        .find(|w| w.name == *wallet.read())
        .cloned()
        .or_else(|| wallets.first().cloned());
    let chain_options: Vec<(String, String)> = proposal
        .all_chains()
        .iter()
        .map(|c| (c.to_string(), format!("{} ({c})", chain_label(*c))))
        .collect();
    let chain = use_signal(|| {
        chain_options
            .first()
            .map(|(v, _)| v.clone())
            .unwrap_or_default()
    });
    let password = use_secret();
    let approve = use_task();
    let reject = use_task();

    let wallet_options: Vec<(String, String)> = wallets
        .iter()
        .map(|w| {
            (
                w.name.clone(),
                format!("{} · {}", w.name, w.chain.display_name()),
            )
        })
        .collect();
    let submit = {
        let proposal = proposal.clone();
        let chosen = chosen.clone();
        move |_| {
            let Some(chosen) = chosen.clone() else {
                return state.toast("No wallet can connect to this app.");
            };
            if !password_ok(&password.read()) {
                return state.toast("Enter the wallet password.");
            }
            let mut args = Args::new()
                .text("proposal_id", &proposal.id)
                .text("wallet_name", &chosen.name)
                .text("password", &password.read());
            if chosen.chain == agent_saifu_core::blockchain::Blockchain::Ethereum {
                let primary: u64 = chain.read().parse().unwrap_or(1);
                let extra: Vec<agent_saifu_core::json::Value> = proposal
                    .required_chains
                    .iter()
                    .filter(|c| **c != primary)
                    .map(|c| agent_saifu_core::json::Value::Number(c.to_string()))
                    .collect();
                args = args.int("chain_id", primary).value(
                    "additional_chain_ids",
                    agent_saifu_core::json::Value::Array(extra),
                );
            }
            let dapp = proposal.dapp.title();
            run_tool(approve, "wc_approve_session", args.build(), move |_| {
                state.log(
                    ActivityKind::Connected,
                    &format!("Connected {dapp}"),
                    "",
                    "Connected",
                );
                state.toast("Connected.");
                wipe(password);
                state.open(Sheet::Sessions);
            });
        }
    };
    let decline = {
        let id = proposal.id.clone();
        move |_| {
            let args = Args::new().text("proposal_id", &id).build();
            run_tool(reject, "wc_reject_session", args, move |_| {
                state.toast("Connection declined.");
                state.close();
            });
        }
    };
    let busy = approve.read().is_busy() || reject.read().is_busy();
    let multi_chain = chosen
        .as_ref()
        .is_some_and(|w| w.chain == agent_saifu_core::blockchain::Blockchain::Ethereum)
        && chain_options.len() > 1;
    rsx! {
        SheetFrame { title: "Connection request",
            div { class: "panel",
                h2 { "{proposal.dapp.title()}" }
                if !proposal.dapp.url.is_empty() {
                    Detail { label: "Website", value: proposal.dapp.url.clone() }
                }
                if !proposal.dapp.description.is_empty() {
                    Detail { label: "About", value: proposal.dapp.description.clone() }
                }
                Detail { label: "Works with", value: if proposal.can_connect_with.is_empty() { "None of your wallets".to_string() } else { proposal.can_connect_with.join(", ") } }
                Detail { label: "Time left", value: expires_in(proposal.expires_in) }
            }
            for warning in proposal.warnings.iter() {
                Notice { warn: true, "⚠  {warning}" }
            }
            if proposal.can_connect && !wallets.is_empty() {
                SelectField { label: "Connect with wallet", value: wallet, options: wallet_options }
                if multi_chain {
                    SelectField { label: "Main network", value: chain, options: chain_options.clone() }
                }
                Notice { "Approving shares this wallet's address with the app. Its funds stay locked: every request still needs your approval." }
                PasswordField { value: password }
                TaskStatus { task: approve, label: "Connecting…" }
                button { class: "primary", disabled: busy, onclick: submit, "Approve connection" }
            } else if proposal.can_connect {
                Notice { warn: true, "You need a compatible wallet to connect to this app." }
            }
            TaskStatus { task: reject, label: "Declining…" }
            button { class: "secondary", disabled: busy, onclick: decline, "Decline" }
        }
    }
}
