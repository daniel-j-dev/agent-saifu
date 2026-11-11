//! Sheets for managing wallets: switch, create, import, rename, remove and unlock.

use dioxus::prelude::*;

use crate::backend::{Args, Backend, UNLOCK};
use crate::fmt::{network_choices, password_ok};
use crate::model::{parse_balances, text};
use crate::state::{AppState, Sheet};
use crate::widgets::*;
use agent_saifu_core::blockchain::Blockchain;

fn refresh_wallets(state: AppState) {
    let backend = consume_context::<Backend>();
    spawn(async move { state.refresh_wallets(&backend).await });
}

#[component]
pub fn WalletsSheet() -> Element {
    let state = use_context::<AppState>();
    let list = state.wallet_list();
    let current = state.current().map(|w| w.name);
    rsx! {
        SheetFrame { title: "Your wallets",
            p { class: "sub", "Separate wallets. Independent protection." }
            for wallet in list.iter().cloned() {
                {
                    let selected = current.as_deref() == Some(wallet.name.as_str());
                    let name = wallet.name.clone();
                    rsx! {
                        button {
                            class: if selected { "wallet-option selected" } else { "wallet-option" },
                            onclick: move |_| {
                                state.select(&name);
                                state.close();
                            },
                            Coin { symbol: crate::fmt::chain_symbol(wallet.chain).to_string() }
                            span { class: "grow",
                                strong { "{wallet.name}" }
                                div { class: "sub", "{wallet.chain.display_name()} · locked" }
                            }
                            if selected { "✓" } else { "›" }
                        }
                    }
                }
            }
            button { class: "primary", onclick: move |_| state.open(Sheet::Create), "+ Create a wallet" }
            button { class: "secondary", onclick: move |_| state.open(Sheet::Import), "Import existing wallet" }
            if !list.is_empty() {
                div { class: "row",
                    button { class: "text-btn grow", onclick: move |_| state.open(Sheet::Rename), "Rename selected" }
                    button { class: "text-btn grow", onclick: move |_| state.open(Sheet::Delete), "Remove selected" }
                }
            }
        }
    }
}

fn chain_options() -> Vec<(String, String)> {
    ["ethereum", "sui", "solana", "bitcoin"]
        .iter()
        .map(|id| {
            (
                id.to_string(),
                Blockchain::parse(id)
                    .map(|c| c.display_name().to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[component]
pub fn CreateSheet(import: bool) -> Element {
    let state = use_context::<AppState>();
    let name = use_signal(String::new);
    let chain = use_signal(|| "ethereum".to_string());
    let network = use_signal(|| "mainnet".to_string());
    let method = use_signal(|| "seed_phrase".to_string());
    let secret = use_secret();
    let password = use_secret();
    let confirm = use_secret();
    let task = use_task();

    let parsed = Blockchain::parse(&chain.read()).unwrap_or(Blockchain::Ethereum);
    let networks: Vec<(String, String)> = network_choices(parsed)
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    let valid_network = networks.iter().any(|(v, _)| *v == *network.read());
    let network_value = if valid_network {
        network.read().clone()
    } else {
        "mainnet".to_string()
    };

    let has_networks = !networks.is_empty();
    let submit = {
        let network_value = network_value.clone();
        move |_| {
            let wallet_name = name.read().trim().to_string();
            if wallet_name.is_empty() {
                return state.toast("Give your wallet a name.");
            }
            if state
                .wallet_list()
                .iter()
                .any(|w| w.name.to_lowercase() == wallet_name.to_lowercase())
            {
                return state.toast("Wallet names must be unique.");
            }
            if !password_ok(&password.read()) || *password.read() != *confirm.read() {
                return state.toast("Use matching passwords of at least 8 characters.");
            }
            if import && secret.read().trim().is_empty() {
                return state.toast("Enter the recovery phrase or private key to import.");
            }
            let mut args = Args::new()
                .text("blockchain", &chain.read())
                .text("wallet_name", &wallet_name)
                .text("password", &password.read());
            if has_networks {
                args = args.text("network", &network_value);
            }
            if import {
                args = args.text(&method.read(), secret.read().trim());
            }
            let tool = if import {
                "import_wallet"
            } else {
                "create_wallet"
            };
            let pw = password.read().clone();
            run_tool(task, tool, args.build(), move |_| {
                state.select(&wallet_name);
                // The password was just typed, so keep the new wallet's address in view.
                state.unlock_in_background(&wallet_name, &pw);
                refresh_wallets(state);
                wipe(secret);
                wipe(password);
                wipe(confirm);
            });
        }
    };

    let done = task.read().answer().cloned();
    let title = if import {
        "Welcome back."
    } else {
        "A new beginning."
    };
    rsx! {
        SheetFrame { title,
            match done {
                Some(answer) => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { "{text(&answer, \"name\")}" }
                        p { class: "sub gap", if import { "Imported and encrypted with your password." } else { "Created. Its recovery phrase is the only way back if you lose this device." } }
                    }
                    if !import {
                        Notice { warn: true, "Back up your 24-word recovery phrase now and keep it offline." }
                        button { class: "primary", onclick: move |_| state.open(Sheet::SeedPhrase), "Show recovery phrase" }
                        button { class: "secondary", onclick: move |_| state.close(), "Do it later" }
                    } else {
                        button { class: "primary", onclick: move |_| state.close(), "Go to wallet" }
                    }
                },
                None => rsx! {
                    p { class: "sub",
                        if import { "Bring an existing wallet into Saifu." } else { "Create a wallet for the network you call home." }
                    }
                    Field { label: "Wallet name", value: name, placeholder: "e.g. Everyday wallet" }
                    SelectField { label: "Blockchain", value: chain, options: chain_options() }
                    if has_networks {
                        SelectField { label: "Network", value: network, options: networks.clone() }
                    }
                    if import {
                        SelectField {
                            label: "Import method",
                            value: method,
                            options: vec![
                                ("seed_phrase".to_string(), "24-word recovery phrase".to_string()),
                                ("private_key".to_string(), "Private key".to_string()),
                            ],
                        }
                        div { class: "field",
                            label { r#for: "import-secret", "Recovery phrase or private key" }
                            textarea {
                                id: "import-secret",
                                value: "{secret}",
                                autocomplete: "off",
                                autocapitalize: "off",
                                spellcheck: "false",
                                oninput: move |e| secret.clone().set(e.value()),
                            }
                        }
                    }
                    Field { label: "Password", value: password, kind: "password", placeholder: "At least 8 characters" }
                    Field { label: "Confirm password", value: confirm, kind: "password", placeholder: "Repeat password" }
                    Notice { "This password encrypts the wallet on this device. It cannot be recovered, so keep it safe." }
                    TaskStatus { task, label: "Encrypting your wallet — this takes a few seconds…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: submit,
                        if import { "Import wallet" } else { "Create wallet" }
                    }
                },
            }
        }
    }
}

#[component]
pub fn RenameSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let new_name = use_signal(|| wallet.name.clone());
    let password = use_secret();
    let task = use_task();
    let old = wallet.name.clone();
    let submit = move |_| {
        let name = new_name.read().trim().to_string();
        if name.is_empty() || name == old {
            return state.toast("Enter a different name.");
        }
        if state
            .wallet_list()
            .iter()
            .any(|w| w.name.to_lowercase() == name.to_lowercase() && w.name != old)
        {
            return state.toast("Wallet names must be unique.");
        }
        let args = Args::new()
            .text("wallet_name", &old)
            .text("new_wallet_name", &name)
            .text("password", &password.read())
            .build();
        let (old, pw) = (old.clone(), password.read().clone());
        run_tool(task, "rename_wallet", args, move |_| {
            state.forget(&old);
            state.select(&name);
            state.unlock_in_background(&name, &pw);
            refresh_wallets(state);
            wipe(password);
            state.close();
        });
    };
    rsx! {
        SheetFrame { title: "A name that feels right.",
            Field { label: "Wallet name", value: new_name }
            PasswordField { value: password }
            TaskStatus { task, label: "Renaming…" }
            button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Save name" }
        }
    }
}

#[component]
pub fn DeleteSheet() -> Element {
    let mut state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let typed = use_signal(String::new);
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let submit = {
        let name = name.clone();
        move |_| {
            if *typed.read() != name {
                return state.toast("Type the wallet name exactly.");
            }
            let args = Args::new()
                .text("wallet_name", &name)
                .text("password", &password.read())
                .flag("confirm", true)
                .build();
            let name = name.clone();
            run_tool(task, "delete_wallet", args, move |_| {
                state.selected.set(None);
                state.forget(&name);
                refresh_wallets(state);
                wipe(password);
                state.close();
            });
        }
    };
    rsx! {
        SheetFrame { title: "Remove this wallet?",
            Notice { warn: true,
                "Removing {name} deletes it from this device for good. Make sure its recovery phrase is backed up first."
            }
            Field { label: "Type the wallet name to confirm", value: typed, placeholder: "{name}" }
            PasswordField { value: password }
            TaskStatus { task, label: "Removing…" }
            button { class: "danger", disabled: task.read().is_busy(), onclick: submit, "Remove wallet" }
        }
    }
}

/// Unlocks the current wallet once and keeps its address and balances on screen.
#[component]
pub fn UnlockSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let submit = {
        let name = name.clone();
        move |_| {
            if password.read().is_empty() {
                return state.toast("Enter the wallet password.");
            }
            let args = Args::new()
                .text("wallet_name", &name)
                .text("password", &password.read())
                .build();
            let name = name.clone();
            run_tool(task, UNLOCK, args, move |answer| {
                let snapshot = parse_balances(&name, answer);
                if let Some(problem) = &snapshot.error {
                    state.toast(&format!(
                        "Unlocked, but balances could not be loaded: {problem}"
                    ));
                }
                state.store_snapshot(snapshot);
                state.set_hidden(false);
                wipe(password);
                state.close();
            });
        }
    };
    rsx! {
        SheetFrame { title: "Approve this action",
            p { class: "sub", "View balances · {name}" }
            Notice { "Only this wallet is unlocked, and only for this step. Its keys lock again right after. The app then remembers the wallet's public address until you close it, so you can refresh balances without the password." }
            PasswordField { value: password }
            TaskStatus { task, label: "Unlocking and reading balances…" }
            button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Approve view balances" }
        }
    }
}
