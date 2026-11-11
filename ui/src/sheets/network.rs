//! Networks, tracked tokens and token approvals (Ethereum wallets).

use agent_saifu_core::blockchain::Blockchain;
use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::Args;
use crate::fmt::{parse_chain_id, short_address, trim_amount};
use crate::model::{Asset, text};
use crate::state::{ActivityKind, AppState, Sheet};
use crate::widgets::*;

#[derive(Clone, PartialEq, Debug)]
struct Net {
    name: String,
    chain_id: u64,
    rpc: String,
    symbol: String,
    explorer: String,
    builtin: bool,
}

fn parse_networks(answer: &Value) -> Vec<Net> {
    answer
        .get("networks")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|n| {
                    Some(Net {
                        name: text(n, "name"),
                        chain_id: n.get("chainId")?.as_u64()?,
                        rpc: text(n, "rpcUrl"),
                        symbol: text(n, "currencySymbol"),
                        explorer: text(n, "blockExplorerUrl"),
                        builtin: n.get("builtin").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone, PartialEq)]
enum Mode {
    List,
    Add,
    Edit(u64),
}

fn is_https(url: &str) -> bool {
    url.trim().starts_with("https://") && url.trim().len() > 8
}

#[component]
pub fn NetworksSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    if wallet.chain != Blockchain::Ethereum {
        let name = wallet.chain.display_name();
        return rsx! {
            SheetFrame { title: "Networks",
                Notice { "{name} wallets are bound to one network, chosen when the wallet was created. To use another network, create a separate wallet." }
                button { class: "primary", onclick: move |_| state.open(Sheet::Create), "Create another wallet" }
            }
        };
    }
    rsx! { EvmNetworks { wallet: wallet.name } }
}

fn load_networks(wallet: &str, password: Signal<String>, task: Signal<Task>) {
    let args = Args::new()
        .text("wallet_name", wallet)
        .text("password", &password.read())
        .build();
    run_tool(task, "list_networks", args, |_| {});
}

#[component]
fn EvmNetworks(wallet: String) -> Element {
    let state = use_context::<AppState>();
    let password = use_secret();
    let list_task = use_task();
    let op_task = use_task();
    let mut mode = use_signal(|| Mode::List);
    let name = use_signal(String::new);
    let chain_id = use_signal(String::new);
    let rpc = use_signal(String::new);
    let symbol = use_signal(String::new);
    let explorer = use_signal(String::new);

    let load = {
        let wallet = wallet.clone();
        move |_| {
            if password.read().is_empty() {
                return state.toast("Enter the wallet password.");
            }
            load_networks(&wallet, password, list_task);
        }
    };
    let networks = list_task.read().answer().map(parse_networks);

    let save = {
        let wallet = wallet.clone();
        move |_| {
            let editing = matches!(*mode.read(), Mode::Edit(_));
            let id = parse_chain_id(&chain_id.read());
            let Some(id) = id else {
                return state.toast("Enter a numeric chain ID.");
            };
            if name.read().trim().is_empty() || symbol.read().trim().is_empty() {
                return state.toast("Enter a name and a currency symbol.");
            }
            if !is_https(&rpc.read()) {
                return state.toast("Use an HTTPS RPC URL.");
            }
            if !explorer.read().trim().is_empty() && !is_https(&explorer.read()) {
                return state.toast("Use an HTTPS block explorer URL.");
            }
            let args = Args::new()
                .text("wallet_name", &wallet)
                .text("password", &password.read())
                .text("network_name", name.read().trim())
                .int("chain_id", id)
                .text("rpc_url", rpc.read().trim())
                .text("currency_symbol", symbol.read().trim())
                .text("block_explorer_url", explorer.read().trim())
                .build();
            let tool = if editing {
                "update_network"
            } else {
                "add_network"
            };
            let wallet = wallet.clone();
            run_tool(op_task, tool, args, move |_| {
                mode.set(Mode::List);
                state.toast(if editing {
                    "Network updated."
                } else {
                    "Network added."
                });
                state.unlock_in_background(&wallet, &password.read());
                load_networks(&wallet, password, list_task);
            });
        }
    };

    let busy = list_task.read().is_busy() || op_task.read().is_busy();
    let current_mode = mode.read().clone();
    let title = match current_mode {
        Mode::List => "Networks",
        Mode::Add => "Add an EVM network",
        Mode::Edit(_) => "Edit network",
    };
    rsx! {
        SheetFrame { title,
            match (networks.clone(), current_mode.clone()) {
                (None, _) => rsx! {
                    p { class: "sub", "Networks belong to this wallet and are stored inside its encrypted vault, so the wallet must unlock to list them." }
                    PasswordField { value: password }
                    TaskStatus { task: list_task, label: "Unlocking…" }
                    button { class: "primary", disabled: busy, onclick: load, "Show networks" }
                },
                (Some(nets), Mode::List) => rsx! {
                    p { class: "sub", "Ethereum mainnet is built in and cannot be changed." }
                    for net in nets.iter().cloned() {
                        {
                            let sub = if net.builtin { format!("Built in · Chain ID {}", net.chain_id) } else { format!("Chain ID {} · {}", net.chain_id, net.symbol) };
                            let edit = net.clone();
                            let remove_wallet = wallet.clone();
                            let reload_wallet = wallet.clone();
                            rsx! {
                                div { class: "list-link static",
                                    span { class: "listicon", "◎" }
                                    span { class: "grow",
                                        strong { "{net.name}" }
                                        span { class: "sub", "{sub}" }
                                    }
                                    if !net.builtin {
                                        button {
                                            class: "text-btn",
                                            onclick: move |_| {
                                                name.clone().set(edit.name.clone());
                                                chain_id.clone().set(edit.chain_id.to_string());
                                                rpc.clone().set(edit.rpc.clone());
                                                symbol.clone().set(edit.symbol.clone());
                                                explorer.clone().set(edit.explorer.clone());
                                                mode.set(Mode::Edit(edit.chain_id));
                                            },
                                            "Edit"
                                        }
                                        button {
                                            class: "text-btn danger-text",
                                            disabled: busy,
                                            onclick: move |_| {
                                                let args = Args::new()
                                                    .text("wallet_name", &remove_wallet)
                                                    .text("password", &password.read())
                                                    .int("chain_id", net.chain_id)
                                                    .build();
                                                let reload_wallet = reload_wallet.clone();
                                                run_tool(op_task, "delete_network", args, move |_| {
                                                    state.toast("Network and its tracked tokens removed.");
                                                    state.unlock_in_background(&reload_wallet, &password.read());
                                                    load_networks(&reload_wallet, password, list_task);
                                                });
                                            },
                                            "Remove"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    TaskStatus { task: op_task, label: "Working…" }
                    button {
                        class: "primary",
                        onclick: move |_| {
                            name.clone().set(String::new());
                            chain_id.clone().set(String::new());
                            rpc.clone().set(String::new());
                            symbol.clone().set(String::new());
                            explorer.clone().set(String::new());
                            mode.set(Mode::Add);
                        },
                        "+ Add network"
                    }
                },
                (Some(_), _) => rsx! {
                    Field { label: "Network name", value: name, placeholder: "e.g. Base" }
                    Field { label: "Chain ID", value: chain_id, placeholder: "8453" }
                    Field { label: "RPC URL", value: rpc, placeholder: "https://" }
                    Field { label: "Currency symbol", value: symbol, placeholder: "ETH" }
                    Field { label: "Block explorer URL", value: explorer, placeholder: "https://" }
                    Notice { "The wallet checks that the RPC reports this chain ID before saving." }
                    TaskStatus { task: op_task, label: "Checking the network…" }
                    button { class: "primary", disabled: busy, onclick: save, "Save network" }
                    button { class: "secondary", disabled: busy, onclick: move |_| mode.set(Mode::List), "Back" }
                },
            }
        }
    }
}

#[component]
pub fn TokensSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    if wallet.chain != Blockchain::Ethereum {
        return rsx! {
            SheetFrame { title: "Manage tokens",
                Notice { "Sui and Solana wallets list every coin or token they hold automatically. Bitcoin has no tokens." }
            }
        };
    }
    let chain_id = use_signal(|| "1".to_string());
    let contract = use_signal(String::new);
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let tracked: Vec<Asset> = state
        .current_snapshot()
        .map(|s| s.assets.into_iter().filter(|a| !a.is_native()).collect())
        .unwrap_or_default();

    // Adding or removing a token changes the balances list, so refresh it while the password is at hand.
    let operate = {
        let name = name.clone();
        move |tool: &'static str, chain: u64, token: String, toast: &'static str| {
            let pw = password.read().clone();
            let args = Args::new()
                .text("wallet_name", &name)
                .text("password", &pw)
                .int("chain_id", chain)
                .text("contract", &token)
                .build();
            let name = name.clone();
            run_tool(task, tool, args, move |_| {
                state.toast(toast);
                state.unlock_in_background(&name, &pw);
            });
        }
    };
    let add = {
        let operate = operate.clone();
        move |_| {
            let Some(chain) = parse_chain_id(&chain_id.read()) else {
                return state.toast("Enter a numeric chain ID.");
            };
            if contract.read().trim().is_empty() {
                return state.toast("Enter the token contract address.");
            }
            if password.read().is_empty() {
                return state.toast("Enter the wallet password.");
            }
            operate(
                "add_token",
                chain,
                contract.read().trim().to_string(),
                "Token added.",
            );
        }
    };
    rsx! {
        SheetFrame { title: "Manage tokens",
            p { class: "sub", "The wallet checks the contract on-chain and reads its symbol and decimals itself." }
            Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
            Field { label: "ERC-20 contract address", value: contract, placeholder: "0x…" }
            PasswordField { value: password }
            TaskStatus { task, label: "Checking the token…" }
            button { class: "primary", disabled: task.read().is_busy(), onclick: add, "Add token" }
            if !tracked.is_empty() {
                p { class: "eyebrow section", "TOKENS IN VIEW" }
                for token in tracked.iter().cloned() {
                    {
                        let operate = operate.clone();
                        let remove_chain = token.chain_id.unwrap_or(1);
                        let contract_id = token.asset.clone();
                        rsx! {
                            div { class: "list-link static",
                                Coin { symbol: token.symbol.clone() }
                                span { class: "grow",
                                    strong { "{token.symbol}" }
                                    span { class: "sub", "{trim_amount(&token.balance, 6)} · {short_address(&token.asset)}" }
                                }
                                button {
                                    class: "text-btn danger-text",
                                    disabled: task.read().is_busy(),
                                    onclick: move |_| {
                                        if password.read().is_empty() {
                                            return state.toast("Enter the wallet password.");
                                        }
                                        operate("remove_token", remove_chain, contract_id.clone(), "Token removed from tracking.");
                                    },
                                    "Stop tracking"
                                }
                            }
                        }
                    }
                }
                p { class: "sub", "Stopping tracking moves no funds. Built-in tokens cannot be removed." }
            }
        }
    }
}

#[component]
pub fn ApprovalsSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let chain_id = use_signal(|| "1".to_string());
    let token = use_signal(String::new);
    let spender = use_signal(String::new);
    let password = use_secret();
    let mut confirming = use_signal(|| false);
    let task = use_task();
    let name = wallet.name.clone();
    let review = move |_| {
        if parse_chain_id(&chain_id.read()).is_none() {
            return state.toast("Enter a numeric chain ID.");
        }
        if token.read().trim().is_empty() || spender.read().trim().is_empty() {
            return state.toast("Enter the token and the spender address.");
        }
        confirming.set(true);
    };
    let revoke = move |_| {
        if password.read().is_empty() {
            return state.toast("Enter the wallet password.");
        }
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .opt_int("chain_id", parse_chain_id(&chain_id.read()))
            .text("token", token.read().trim())
            .text("spender", spender.read().trim())
            .flag("broadcast", true)
            .build();
        run_tool(task, "revoke_token_approval", args, move |_| {
            state.log(
                ActivityKind::Signed,
                "Revoked a token approval",
                "",
                "Submitted",
            );
            wipe(password);
        });
    };
    let answer = task.read().answer().cloned();
    rsx! {
        SheetFrame { title: "Token approvals",
            match answer {
                Some(answer) => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { "Approval revoked" }
                    }
                    div { class: "panel", JsonView { value: answer } }
                    button { class: "primary", onclick: move |_| state.close(), "Done" }
                },
                None if *confirming.read() => rsx! {
                    div { class: "panel",
                        h2 { "Set allowance to zero" }
                        Detail { label: "Token", value: token.read().trim().to_string() }
                        Detail { label: "Spender", value: spender.read().trim().to_string() }
                        Detail { label: "Network chain ID", value: chain_id.read().trim().to_string() }
                    }
                    Notice { "This sends a network transaction and costs a small fee. It does nothing if the allowance is already zero." }
                    PasswordField { value: password }
                    TaskStatus { task, label: "Revoking…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: revoke, "Approve & revoke" }
                    button { class: "secondary", disabled: task.read().is_busy(), onclick: move |_| confirming.set(false), "Edit" }
                },
                None => rsx! {
                    Notice { "Revoke an ERC-20 spender's allowance so a contract can no longer move that token." }
                    Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
                    Field { label: "Token contract", value: token, placeholder: "0x…" }
                    Field { label: "Spender address", value: spender, placeholder: "0x…" }
                    button { class: "primary", onclick: review, "Review revocation →" }
                },
            }
        }
    }
}
