//! Sheets that move value: send, receive, asset details, swap review and swap settings.

use agent_saifu_core::blockchain::Blockchain;
use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::{Args, UNLOCK};
use crate::fmt::{
    MIN_RECEIVE_BUFFER_BPS, chain_symbol, looks_like_amount, parse_chain_id, parse_slippage_bps,
    reduce_by_bps, short_address, trim_amount,
};
use crate::model::{Asset, parse_balances, strings, text};
use crate::picker::{TokenChoice, TokenPicker};
use crate::qr::qr_path;
use crate::state::{ActivityKind, AppState, Sheet, SwapDraft, Tab};
use crate::tx::ReviewView;
use crate::widgets::*;

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Form,
    Review,
    Done,
}

/// `chain|asset`, so one select can carry both parts.
pub fn asset_key(asset: &Asset) -> String {
    format!(
        "{}|{}",
        asset.chain_id.map(|c| c.to_string()).unwrap_or_default(),
        asset.asset
    )
}

#[component]
pub fn SendSheet(initial: Option<String>) -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let snapshot = state.current_snapshot();
    let assets: Vec<Asset> = snapshot
        .as_ref()
        .map(|s| s.assets.clone())
        .unwrap_or_default();
    let chain = wallet.chain;
    let first_chain = if chain == Blockchain::Ethereum {
        Some(1)
    } else {
        None
    };
    // Until the wallet is unlocked the only thing it can send is its native coin.
    let pool: Vec<Asset> = if assets.is_empty() {
        vec![Asset {
            symbol: chain_symbol(chain).to_string(),
            name: chain_symbol(chain).to_string(),
            asset: "native".to_string(),
            balance: String::new(),
            usd: None,
            chain_id: first_chain,
            network: String::new(),
        }]
    } else {
        assets.clone()
    };
    let start = initial
        .as_deref()
        .and_then(|key| pool.iter().find(|a| asset_key(a) == key))
        .unwrap_or(&pool[0])
        .clone();

    let mut chosen = use_signal(|| Some(TokenChoice::of_asset(&start, first_chain)));
    let to = use_signal(String::new);
    let amount = use_signal(String::new);
    let chain_id = use_signal(|| "1".to_string());
    let fee_a = use_signal(String::new);
    let fee_b = use_signal(String::new);
    let password = use_secret();
    let mut step = use_signal(|| Step::Form);
    let simulate = use_task();
    let send = use_task();
    let balance_task = use_task();

    let wallet_name = wallet.name.clone();
    let choice = chosen.read().clone();
    let selected_asset = choice
        .as_ref()
        .map(|c| c.token.clone())
        .unwrap_or_else(|| "native".to_string());
    // Ethereum tokens live on a network: the picked one, else the one typed under Advanced.
    let selected_chain = if chain == Blockchain::Ethereum {
        choice
            .as_ref()
            .and_then(|c| c.chain)
            .or_else(|| parse_chain_id(&chain_id.read()))
    } else {
        None
    };
    let symbol = choice
        .as_ref()
        .map(|c| c.symbol.clone())
        .unwrap_or_else(|| "token".to_string());
    let effective_chain = selected_chain;
    let network_label = snapshot
        .as_ref()
        .and_then(|s| {
            s.assets
                .iter()
                .find(|a| a.chain_id.is_some() && a.chain_id == effective_chain)
        })
        .map(|a| a.network.clone())
        .unwrap_or_else(|| match effective_chain {
            Some(id) => format!("Chain {id}"),
            None => format!("{} network", chain.display_name()),
        });
    let net = match chain {
        Blockchain::Ethereum => "ethereum",
        Blockchain::Sui => "sui",
        Blockchain::Solana => "solana",
        Blockchain::Bitcoin => "",
    }
    .to_string();

    let (fee_a_label, fee_a_hint, fee_b_label) = match chain {
        Blockchain::Ethereum => (
            "Gas limit (optional)",
            "Leave empty to let the wallet estimate it.",
            "",
        ),
        Blockchain::Bitcoin => (
            "Fee rate in sat/vB (optional)",
            "Or set a confirmation target below.",
            "Confirmation target in blocks (optional)",
        ),
        Blockchain::Solana => (
            "Priority fee in SOL (optional)",
            "Total, in whole SOL. Empty means automatic.",
            "",
        ),
        Blockchain::Sui => (
            "Gas budget in SUI (optional)",
            "Empty means automatic from a simulation.",
            "",
        ),
    };

    let build_args = {
        let wallet_name = wallet_name.clone();
        let selected_asset = selected_asset.clone();
        move |for_send: bool| -> Args {
            let asset_value = if selected_asset.is_empty() {
                "native"
            } else {
                selected_asset.as_str()
            };
            let args = Args::new()
                .text("wallet_name", &wallet_name)
                .text("password", &password.read())
                .opt_int("chain_id", effective_chain)
                .text("to", to.read().trim())
                .text("asset", asset_value)
                .text("amount", amount.read().trim());
            let (a, b) = (fee_a.read().clone(), fee_b.read().clone());
            match chain {
                // A gas limit only matters when signing; the simulation estimates its own.
                Blockchain::Ethereum => match a.trim().parse::<u64>() {
                    Ok(limit) if for_send => args.int("gas_limit", limit),
                    _ => args,
                },
                Blockchain::Bitcoin => args
                    .opt("fee_rate", &a)
                    .opt_int("confirmation_target", parse_chain_id(&b)),
                Blockchain::Solana => args.opt("priority_fee", &a),
                Blockchain::Sui => args.opt("gas_budget", &a),
            }
        }
    };

    let known = snapshot.is_some();
    let masked = *state.hidden.read();
    let held = snapshot
        .as_ref()
        .filter(|_| !masked)
        .and_then(|s| s.find(selected_chain, &selected_asset))
        .cloned();
    let show_balance = {
        let wallet_name = wallet_name.clone();
        move |_| {
            if known {
                return state.set_hidden(false);
            }
            if password.read().is_empty() {
                return state.toast("Enter the wallet password below, then show your balance.");
            }
            let args = Args::new()
                .text("wallet_name", &wallet_name)
                .text("password", &password.read())
                .build();
            let name = wallet_name.clone();
            run_tool(balance_task, UNLOCK, args, move |answer| {
                state.store_snapshot(parse_balances(&name, answer));
            });
        }
    };
    let review = {
        let build_args = build_args.clone();
        move |_| {
            if to.read().trim().is_empty() {
                return state.toast("Enter a recipient address.");
            }
            if !looks_like_amount(&amount.read()) {
                return state.toast("Enter an amount greater than zero.");
            }
            if chain == Blockchain::Ethereum && effective_chain.is_none() {
                return state.toast("Enter a numeric chain ID under Advanced options.");
            }
            if selected_asset.is_empty() {
                return state.toast("Choose what to send.");
            }
            if password.read().is_empty() {
                return state.toast("Enter the wallet password to review.");
            }
            run_tool(
                simulate,
                "simulate_transaction",
                build_args(false).build(),
                move |_| step.set(Step::Review),
            );
        }
    };
    let confirm = {
        let symbol = symbol.clone();
        move |_| {
            let (symbol, amount_text) = (symbol.clone(), amount.read().trim().to_string());
            run_tool(send, "send_asset", build_args(true).build(), move |_| {
                state.log(
                    ActivityKind::Sent,
                    &format!("Sent {symbol}"),
                    &format!("− {amount_text} {symbol}"),
                    "Submitted",
                );
                state.toast("Payment submitted.");
                state.refresh();
                step.set(Step::Done);
                wipe(password);
            });
        }
    };

    let simulation = simulate.read().answer().cloned();
    let sent = send.read().answer().cloned();
    let full_to = to.read().trim().to_string();
    let summary_to = short_address(&full_to);
    let simulation_ok = simulation
        .as_ref()
        .is_none_or(|s| crate::model::verdict(s) != Some(false));
    let summary_amount = format!("{} {}", amount.read().trim(), symbol);
    let busy = simulate.read().is_busy() || send.read().is_busy();
    let heading = match *step.read() {
        Step::Form => "Send with confidence.",
        Step::Review => "Review payment",
        Step::Done => "Payment submitted",
    };

    rsx! {
        SheetFrame { title: heading,
            match *step.read() {
                Step::Form => rsx! {
                    p { class: "sub", "From {wallet_name} · {network_label}" }
                    div { class: "field",
                        label { "Asset" }
                        TokenPicker {
                            net: net.clone(),
                            chain_id: effective_chain.or(first_chain),
                            held: pool.clone(),
                            exclude: String::new(),
                            selected: choice.clone(),
                            search: chain != Blockchain::Bitcoin,
                            wide: true,
                            show_network: snapshot.as_ref().is_some_and(|s| s.networks().len() > 1),
                            on_pick: move |picked| chosen.set(Some(picked)),
                        }
                    }
                    Field { label: "Recipient address", value: to, placeholder: "Paste the address" }
                    Field { label: "Amount", value: amount, placeholder: "0.00", kind: "number" }
                    div { class: "balance-line",
                        if let Some(a) = held.clone() {
                            span {
                                "Available: "
                                strong { "{trim_amount(&a.balance, 8)} {a.symbol}" }
                                if a.is_native() {
                                    span { class: "sub", " · keep a little for network fees" }
                                }
                            }
                        } else if known && !masked {
                            span { "Balance of this token is unknown" }
                        } else if known {
                            span { "Balances are hidden" }
                            button { class: "text-btn", onclick: show_balance, "Show" }
                        } else {
                            span { "Balance hidden · enter your password below" }
                            button { class: "text-btn", disabled: balance_task.read().is_busy(), onclick: show_balance, "Show balance" }
                        }
                    }
                    TaskStatus { task: balance_task, label: "Reading your balance…" }
                    details { class: "tech",
                        summary { "Advanced options" }
                        if chain == Blockchain::Ethereum {
                            Field { label: "Network chain ID", value: chain_id, placeholder: "1", hint: "Used for tokens not in your list. 1 is Ethereum mainnet." }
                        }
                        Field { label: fee_a_label, value: fee_a, placeholder: "Automatic", hint: fee_a_hint, kind: "number" }
                        if !fee_b_label.is_empty() {
                            Field { label: fee_b_label, value: fee_b, placeholder: "Automatic", kind: "number" }
                        }
                    }
                    PasswordField { value: password }
                    Notice { "The wallet unlocks only to check this payment. You approve it on the next screen." }
                    TaskStatus { task: simulate, label: "Checking the payment on the network…" }
                    button { class: "primary", disabled: busy, onclick: review, "Review payment →" }
                },
                Step::Review => rsx! {
                    p { class: "step-label", "REVIEW → APPROVE → RESULT" }
                    div { class: "panel",
                        h2 { "{summary_amount}" }
                        Detail { label: "To", value: full_to.clone() }
                        Detail { label: "Wallet", value: wallet_name.clone() }
                        Detail { label: "Network", value: network_label.clone() }
                    }
                    if let Some(result) = simulation.clone() {
                        ReviewView { review: result }
                    }
                    TaskStatus { task: send, label: "Signing and sending…" }
                    button { class: "primary", disabled: busy || !simulation_ok, onclick: confirm, "Approve & send" }
                    button { class: "secondary", disabled: busy, onclick: move |_| step.set(Step::Form), "Edit payment" }
                },
                Step::Done => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { "{summary_amount}" }
                        p { class: "sub gap", "Sent to {summary_to}. The wallet locked again." }
                    }
                    if let Some(result) = sent.clone() {
                        div { class: "panel", JsonView { value: result } }
                    }
                    button {
                        class: "primary",
                        onclick: move |_| {
                            state.close();
                            state.go(Tab::Activity);
                        },
                        "View activity"
                    }
                },
            }
        }
    }
}

#[component]
pub fn ReceiveSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let password = use_secret();
    let task = use_task();
    let cached = state.current_snapshot().map(|s| s.address);
    let fetched = task.read().answer().map(|v| text(v, "address"));
    let address = cached.or(fetched);
    let name = wallet.name.clone();
    let reveal = move |_| {
        if password.read().is_empty() {
            return state.toast("Enter the wallet password.");
        }
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .build();
        run_tool(task, "get_wallet_address", args, move |_| wipe(password));
    };
    let symbol = chain_symbol(wallet.chain);
    let chain_name = wallet.chain.display_name();
    let evm = wallet.chain == Blockchain::Ethereum;
    rsx! {
        SheetFrame { title: "Receive {symbol}",
            match address {
                Some(address) => rsx! { AddressCard { address, chain_name: chain_name.to_string(), evm } },
                None => rsx! {
                    p { class: "sub", "Your address stays encrypted until you unlock the wallet." }
                    PasswordField { value: password }
                    TaskStatus { task, label: "Unlocking…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: reveal, "Show my address" }
                },
            }
        }
    }
}

#[component]
fn AddressCard(address: String, chain_name: String, evm: bool) -> Element {
    let state = use_context::<AppState>();
    let qr = qr_path(&address);
    let copy_value = address.clone();
    rsx! {
        div { class: "empty",
            div { class: "success-symbol", "↙" }
            h2 { "Your receiving address" }
            p { class: "sub gap", "{chain_name}" }
        }
        if let Some((width, path)) = qr {
            div { class: "qr", role: "img", aria_label: "QR code of your address",
                svg { view_box: "-2 -2 {width + 4} {width + 4}", shape_rendering: "crispEdges",
                    rect {
                        x: "-2",
                        y: "-2",
                        width: "{width + 4}",
                        height: "{width + 4}",
                        fill: "#fff",
                    }
                    path { d: "{path}", fill: "#18392e" }
                }
            }
        }
        div { class: "address", "{address}" }
        Notice { warn: true,
            if evm {
                "Send only Ethereum-compatible assets to this address. It works on every EVM network the wallet has."
            } else {
                "Send only {chain_name} assets to this address."
            }
        }
        button { class: "primary", onclick: move |_| copy_text(copy_value.clone(), state), "Copy address" }
    }
}

#[component]
pub fn AssetSheet(asset: Asset) -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let key = asset_key(&asset);
    let can_swap = wallet.chain != Blockchain::Bitcoin;
    let title = asset.name.clone();
    let balance = trim_amount(&asset.balance, 8);
    rsx! {
        SheetFrame { title,
            div { class: "empty",
                Coin { symbol: asset.symbol.clone() }
                div { class: "balance dark",
                    "{balance} "
                    small { "{asset.symbol}" }
                }
                if !asset.network.is_empty() {
                    p { class: "sub", "{asset.network}" }
                }
            }
            button { class: "primary", onclick: move |_| state.open(Sheet::Send(Some(key.clone()))), "Send {asset.symbol}" }
            button { class: "secondary", onclick: move |_| state.open(Sheet::Receive), "Receive" }
            if can_swap {
                button {
                    class: "secondary",
                    onclick: move |_| {
                        state.close();
                        state.go(Tab::Swap);
                    },
                    "Swap"
                }
            }
        }
    }
}

fn quote_part(quote: &Value, side: &str, key: &str) -> String {
    quote.get(side).map(|s| text(s, key)).unwrap_or_default()
}

fn fee_text(quote: &Value, which: &str) -> Option<String> {
    let fee = quote.get("fees")?.get(which)?;
    let amount = ["amount", "estimated_amount"]
        .iter()
        .map(|k| text(fee, k))
        .find(|s| !s.is_empty())?;
    let unit = ["symbol", "currency"]
        .iter()
        .map(|k| text(fee, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let percent = text(fee, "percent");
    let suffix = if percent.is_empty() {
        String::new()
    } else {
        format!(" ({percent}%)")
    };
    Some(format!("{} {unit}{suffix}", trim_amount(&amount, 8)))
}

#[component]
pub fn SwapReviewSheet(draft: SwapDraft) -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let password = use_secret();
    let quote_task = use_task();
    let swap_task = use_task();
    let mut done = use_signal(|| false);
    let slippage = *state.slippage_bps.read();
    let name = wallet.name.clone();

    let base = {
        let draft = draft.clone();
        let name = name.clone();
        move || {
            Args::new()
                .text("wallet_name", &name)
                .text("password", &password.read())
                .opt_int("from_chain_id", draft.chain_id)
                .text("from_token", &draft.from_token)
                .opt_int("to_chain_id", draft.chain_id)
                .text("to_token", &draft.to_token)
                .text("amount", &draft.amount)
                .int("slippage_bps", slippage)
        }
    };
    let get_quote = {
        let base = base.clone();
        move |_| {
            if password.read().is_empty() {
                return state.toast("Enter the wallet password to get a quote.");
            }
            run_tool(quote_task, "get_swap_quote", base().build(), |_| {});
        }
    };
    let quote = quote_task.read().answer().cloned();
    let minimum = quote
        .as_ref()
        .map(|q| quote_part(q, "to", "minimum_amount"))
        .unwrap_or_default();
    // The quote moves a little before the swap is sent, so the floor sits slightly under the
    // minimum shown. Without one (a minimum that cannot be read) no floor is sent.
    let floor = reduce_by_bps(&minimum, MIN_RECEIVE_BUFFER_BPS).unwrap_or_default();
    let confirm = {
        let draft = draft.clone();
        let floor = floor.clone();
        move |_| {
            let draft = draft.clone();
            let args = base().opt("min_receive", &floor).build();
            run_tool(swap_task, "swap_tokens", args, move |_| {
                state.log(
                    ActivityKind::Swapped,
                    &format!("Swapped {} → {}", draft.from_symbol, draft.to_symbol),
                    &format!("− {} {}", draft.amount, draft.from_symbol),
                    "Submitted",
                );
                state.toast("Swap submitted.");
                state.refresh();
                done.set(true);
                wipe(password);
            });
        }
    };
    let finished = swap_task.read().answer().cloned();
    let busy = quote_task.read().is_busy() || swap_task.read().is_busy();
    let title = if *done.read() {
        "Swap submitted"
    } else {
        "Swap tokens"
    };
    let slippage_text = format!("{}%", slippage as f64 / 100.0);

    rsx! {
        SheetFrame { title,
            if *done.read() {
                div { class: "empty",
                    div { class: "success-symbol", "✓" }
                    h2 { "{draft.amount} {draft.from_symbol} → {draft.to_symbol}" }
                    p { class: "sub gap", "The wallet locked again." }
                }
                if let Some(result) = finished {
                    div { class: "panel", JsonView { value: result } }
                }
                button {
                    class: "primary",
                    onclick: move |_| {
                        state.close();
                        state.go(Tab::Activity);
                    },
                    "View activity"
                }
            } else if let Some(quote) = quote {
                {
                    let expected = trim_amount(&quote_part(&quote, "to", "expected_amount"), 8);
                    let min_shown = trim_amount(&quote_part(&quote, "to", "minimum_amount"), 8);
                    let route = quote
                        .get("route")
                        .and_then(Value::as_array)
                        .map(|r| r.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" → "))
                        .unwrap_or_default();
                    let impact = text(&quote, "price_impact_percent");
                    // What the wallet itself resolved, not what a token list claimed.
                    let receive_token = {
                        let symbol = quote_part(&quote, "to", "symbol");
                        let token = quote_part(&quote, "to", "token");
                        let symbol = if symbol.is_empty() { draft.to_symbol.clone() } else { symbol };
                        if token == "native" || token.is_empty() {
                            symbol
                        } else {
                            format!("{symbol} · {}", short_address(&token))
                        }
                    };
                    let requirements = quote.get("requirements");
                    let approval = requirements.and_then(|r| r.get("approval_needed")).and_then(Value::as_bool) == Some(true);
                    let enough = requirements.and_then(|r| r.get("sufficient_balance")).and_then(Value::as_bool);
                    let mut warns = strings(&quote, "warnings");
                    if approval {
                        warns.push("An approval for exactly the amount you sell will be sent first.".to_string());
                    }
                    rsx! {
                        p { class: "step-label", "REVIEW → APPROVE → RESULT" }
                        div { class: "panel",
                            h2 { "{draft.amount} {draft.from_symbol} → {expected} {draft.to_symbol}" }
                            Detail { label: "Receive token", value: receive_token }
                            Detail { label: "Minimum received", value: format!("{min_shown} {}", draft.to_symbol) }
                            if !floor.is_empty() {
                                Detail { label: "Refused if it falls below", value: format!("{} {}", trim_amount(&floor, 8), draft.to_symbol) }
                            }
                            Detail { label: "Slippage", value: slippage_text.clone() }
                            if !route.is_empty() {
                                Detail { label: "Route", value: route }
                            }
                            if !impact.is_empty() {
                                Detail { label: "Price impact", value: format!("{impact}%") }
                            }
                            if let Some(fee) = fee_text(&quote, "service_fee") {
                                Detail { label: "Service fee", value: fee }
                            }
                            if let Some(fee) = fee_text(&quote, "network_fee") {
                                Detail { label: "Estimated network fee", value: fee }
                            }
                        }
                        for w in warns.iter() {
                            Notice { warn: true, "⚠  {w}" }
                        }
                        Notice { "Nothing is signed until you approve. The wallet fetches a fresh quote and refuses the swap if it would deliver less than the floor shown above (the minimum minus a 0.5% allowance for price movement)." }
                        TaskStatus { task: swap_task, label: "Swapping — this can take a minute…" }
                        button { class: "primary", disabled: busy || enough == Some(false), onclick: confirm, "Approve & swap" }
                        button { class: "secondary", disabled: busy, onclick: move |_| state.close(), "Cancel" }
                    }
                }
            } else {
                div { class: "panel",
                    h2 { "{draft.amount} {draft.from_symbol} → {draft.to_symbol}" }
                    Detail { label: "Wallet", value: name.clone() }
                    Detail { label: "Slippage", value: slippage_text.clone() }
                }
                p { class: "sub", "A quote needs your wallet unlocked for a moment. Nothing is signed yet." }
                PasswordField { value: password }
                TaskStatus { task: quote_task, label: "Finding the best route…" }
                button { class: "primary", disabled: busy, onclick: get_quote, "Get quote →" }
            }
        }
    }
}

#[component]
pub fn SlippageSheet() -> Element {
    let mut state = use_context::<AppState>();
    let current = *state.slippage_bps.read();
    let value = use_signal(|| format!("{}", current as f64 / 100.0));
    rsx! {
        SheetFrame { title: "Swap preferences",
            Field { label: "Slippage tolerance (%)", value, placeholder: "1", kind: "number" }
            p { class: "sub", "Allowed range: 0.05% to 10%. Your minimum received is shown before approval." }
            button {
                class: "primary",
                onclick: move |_| match parse_slippage_bps(&value.read()) {
                    Some(bps) => {
                        state.slippage_bps.set(bps);
                        state.close();
                    }
                    None => state.toast("Choose a tolerance from 0.05% to 10% (two decimals at most)."),
                },
                "Save preferences"
            }
        }
    }
}
