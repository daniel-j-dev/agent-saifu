//! Advanced tools: message signing, contract transactions, contract reads, transaction lookup.

use agent_saifu_core::blockchain::Blockchain;
use agent_saifu_core::json::{self, Value};
use dioxus::prelude::*;

use crate::backend::{Args, Backend};
use crate::fmt::parse_chain_id;
use crate::model::text;
use crate::state::{ActivityKind, AppState, Sheet};
use crate::tx::ReviewView;
use crate::widgets::*;

#[component]
pub fn AdvancedSheet() -> Element {
    let state = use_context::<AppState>();
    let chain = state.current().map(|w| w.chain);
    rsx! {
        SheetFrame { title: "Advanced tools",
            LinkRow { icon: "✎", title: "Sign a message", subtitle: "Review the exact text before signing", onclick: move |_| state.open(Sheet::SignMessage) }
            if chain == Some(Blockchain::Bitcoin) {
                Notice { "Bitcoin supports BIP 322 messages and native payments. External transactions and PSBTs are not supported." }
            } else {
                LinkRow { icon: "⌘", title: "Contract transaction", subtitle: "Simulate first, then sign or broadcast", onclick: move |_| state.open(Sheet::Contract) }
            }
            if chain == Some(Blockchain::Ethereum) {
                LinkRow { icon: "◉", title: "Read smart contract", subtitle: "Read-only call, nothing signed", onclick: move |_| state.open(Sheet::ReadContract) }
            }
            LinkRow { icon: "⌕", title: "Find a transaction", subtitle: "Look up status and fees by hash", onclick: move |_| state.open(Sheet::Lookup) }
        }
    }
}

#[component]
pub fn SignMessageSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let message = use_signal(String::new);
    let format = use_signal(|| "text".to_string());
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let submit = move |_| {
        if message.read().trim().is_empty() {
            return state.toast("Enter a message first.");
        }
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .text("message", &message.read())
            .text("message_format", &format.read())
            .build();
        run_tool(task, "sign_personal_message", args, move |_| {
            state.log(ActivityKind::Signed, "Signed a message", "", "Signed");
            wipe(password);
        });
    };
    let answer = task.read().answer().cloned();
    rsx! {
        SheetFrame { title: "Sign a message",
            match answer {
                Some(answer) => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { "Message signed" }
                        p { class: "sub gap", "Nothing was sent to the network." }
                    }
                    div { class: "address", "{text(&answer, \"signature\")}" }
                    div { class: "panel", JsonView { value: answer.clone() } }
                    button { class: "primary", onclick: move |_| copy_text(text(&answer, "signature"), state), "Copy signature" }
                },
                None => rsx! {
                    TextArea { label: "Exact message", value: message, placeholder: "Enter the message to sign" }
                    SelectField {
                        label: "Message format",
                        value: format,
                        options: vec![
                            ("text".to_string(), "Text".to_string()),
                            ("hex".to_string(), "Hex".to_string()),
                            ("base64".to_string(), "Base64".to_string()),
                        ],
                    }
                    Notice { "You are signing exactly this message. Message signing never moves funds." }
                    PasswordField { value: password }
                    TaskStatus { task, label: "Signing…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Sign message" }
                },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Step {
    Form,
    Review,
    Done,
}

#[component]
pub fn ContractSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let chain = wallet.chain;
    let chain_id = use_signal(|| "1".to_string());
    let target = use_signal(String::new);
    let data = use_signal(String::new);
    let value = use_signal(String::new);
    let broadcast = use_signal(|| "sign".to_string());
    let password = use_secret();
    let mut step = use_signal(|| Step::Form);
    let simulate = use_task();
    let sign = use_task();
    let name = wallet.name.clone();
    let evm = chain == Blockchain::Ethereum;

    let args = {
        let name = name.clone();
        move || {
            let base = Args::new()
                .text("wallet_name", &name)
                .text("password", &password.read());
            if evm {
                base.opt_int("chain_id", parse_chain_id(&chain_id.read()))
                    .text("to", target.read().trim())
                    .text("data", data.read().trim())
                    .opt("value", &value.read())
            } else {
                base.text("transaction_bytes", data.read().trim())
            }
        }
    };
    let review = {
        let args = args.clone();
        move |_| {
            if evm && target.read().trim().is_empty() {
                return state.toast("Enter the contract address.");
            }
            if data.read().trim().is_empty() {
                return state.toast(if evm {
                    "Enter the calldata."
                } else {
                    "Enter the transaction bytes."
                });
            }
            if evm && parse_chain_id(&chain_id.read()).is_none() {
                return state.toast("Enter a numeric chain ID.");
            }
            if password.read().is_empty() {
                return state.toast("Enter the wallet password.");
            }
            run_tool(
                simulate,
                "simulate_transaction",
                args().build(),
                move |_| step.set(Step::Review),
            );
        }
    };
    let will_broadcast = *broadcast.read() == "broadcast";
    let confirm = move |_| {
        let built = args().flag("broadcast", will_broadcast).build();
        run_tool(sign, "sign_contract_transaction", built, move |_| {
            state.log(
                if will_broadcast {
                    ActivityKind::Sent
                } else {
                    ActivityKind::Signed
                },
                "Contract transaction",
                "",
                if will_broadcast {
                    "Submitted"
                } else {
                    "Signed"
                },
            );
            step.set(Step::Done);
            wipe(password);
        });
    };
    let simulation = simulate.read().answer().cloned();
    let signed = sign.read().answer().cloned();
    let busy = simulate.read().is_busy() || sign.read().is_busy();
    let simulation_ok = simulation
        .as_ref()
        .is_none_or(|s| crate::model::verdict(s) != Some(false));
    let data_label = if evm {
        "Calldata (hex)"
    } else {
        "Transaction bytes (base64)"
    };
    rsx! {
        SheetFrame { title: "Contract transaction",
            match *step.read() {
                Step::Form => rsx! {
                    if evm {
                        Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
                        Field { label: "Contract address", value: target, placeholder: "0x…" }
                        Field { label: "Value in ETH (optional)", value, placeholder: "0", kind: "number" }
                    }
                    TextArea { label: data_label, value: data, placeholder: "Paste the encoded transaction" }
                    SelectField {
                        label: "Action",
                        value: broadcast,
                        options: vec![
                            ("sign".to_string(), "Sign only".to_string()),
                            ("broadcast".to_string(), "Sign and broadcast".to_string()),
                        ],
                    }
                    PasswordField { value: password }
                    Notice { "The transaction is decoded and simulated first. You sign on the next screen." }
                    TaskStatus { task: simulate, label: "Simulating on the network…" }
                    button { class: "primary", disabled: busy, onclick: review, "Review transaction →" }
                },
                Step::Review => rsx! {
                    if let Some(result) = simulation.clone() {
                        ReviewView { review: result }
                    }
                    TaskStatus { task: sign, label: "Signing…" }
                    button { class: "primary", disabled: busy || !simulation_ok, onclick: confirm,
                        if will_broadcast { "Sign & broadcast" } else { "Sign only" }
                    }
                    button { class: "secondary", disabled: busy, onclick: move |_| step.set(Step::Form), "Edit transaction" }
                },
                Step::Done => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { if will_broadcast { "Broadcast" } else { "Signed" } }
                    }
                    if let Some(result) = signed.clone() {
                        div { class: "panel", JsonView { value: result } }
                    }
                    button { class: "primary", onclick: move |_| state.close(), "Done" }
                },
            }
        }
    }
}

/// Turns `["0x…", 5]` into a list the wallet accepts; `None` if it is not a JSON array.
fn parse_args_list(input: &str) -> Option<Value> {
    let t = input.trim();
    if t.is_empty() {
        return Some(Value::Array(vec![]));
    }
    match json::parse(t).ok()? {
        list @ Value::Array(_) => Some(list),
        _ => None,
    }
}

#[component]
pub fn ReadContractSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let chain_id = use_signal(|| "1".to_string());
    let contract = use_signal(String::new);
    let function = use_signal(String::new);
    let args_text = use_signal(String::new);
    let outputs = use_signal(String::new);
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let submit = move |_| {
        let Some(chain) = parse_chain_id(&chain_id.read()) else {
            return state.toast("Enter a numeric chain ID.");
        };
        if contract.read().trim().is_empty() || function.read().trim().is_empty() {
            return state.toast("Enter the contract and the function.");
        }
        let Some(list) = parse_args_list(&args_text.read()) else {
            return state.toast("Arguments must be a JSON list, e.g. [\"0x…\"].");
        };
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .int("chain_id", chain)
            .text("contract", contract.read().trim())
            .text("function", function.read().trim())
            .value("args", list)
            .opt("outputs", &outputs.read())
            .build();
        run_tool(task, "read_smart_contract", args, move |_| wipe(password));
    };
    let answer = task.read().answer().cloned();
    rsx! {
        SheetFrame { title: "Read smart contract",
            Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
            Field { label: "Contract address", value: contract, placeholder: "0x…" }
            Field { label: "Function signature", value: function, placeholder: "balanceOf(address)" }
            Field { label: "Arguments (JSON list)", value: args_text, placeholder: "[\"0x…\"]" }
            Field { label: "Output types (optional)", value: outputs, placeholder: "uint256" }
            PasswordField { value: password }
            TaskStatus { task, label: "Reading the contract…" }
            if let Some(answer) = answer {
                div { class: "panel", JsonView { value: answer } }
            }
            button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Read contract" }
        }
    }
}

#[component]
pub fn LookupSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let hash = use_signal(String::new);
    let chain_id = use_signal(|| "1".to_string());
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let evm = wallet.chain == Blockchain::Ethereum;
    let submit = move |_| {
        if hash.read().trim().is_empty() {
            return state.toast("Enter a transaction identifier.");
        }
        let chain = if evm {
            match parse_chain_id(&chain_id.read()) {
                Some(c) => Some(c),
                None => return state.toast("Enter a numeric chain ID."),
            }
        } else {
            None
        };
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .opt_int("chain_id", chain)
            .text("transaction_hash", hash.read().trim())
            .build();
        run_tool(task, "get_transaction", args, move |_| wipe(password));
    };
    let answer = task.read().answer().cloned();
    rsx! {
        SheetFrame { title: "Find a transaction",
            Field { label: "Transaction hash or signature", value: hash, placeholder: "Paste the transaction identifier" }
            if evm {
                Field { label: "Network chain ID", value: chain_id, placeholder: "1" }
            }
            PasswordField { value: password }
            TaskStatus { task, label: "Looking it up…" }
            if let Some(answer) = answer {
                div { class: "panel", JsonView { value: answer } }
            }
            button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Look up transaction" }
        }
    }
}

#[component]
pub fn AboutSheet() -> Element {
    let info = use_resource(|| async move {
        consume_context::<Backend>()
            .call("get_info", Args::new().build())
            .await
    });
    let version = match &*info.read() {
        Some(Ok(v)) => text(v, "version"),
        _ => "Version 0.0".to_string(),
    };
    rsx! {
        SheetFrame { title: "Small wallet. Big freedom.",
            div { class: "empty",
                div { class: "agent-mark", "◈" }
                h2 { "agent-saifu" }
                p { class: "sub gap",
                    "All-in-one crypto wallet for agents."
                    br {}
                    "Built from the ground up in Rust."
                }
                span { class: "badge", "{version.to_uppercase()}" }
            }
            Notice { "This app runs the same wallet engine as the agent-saifu MCP server, so every security rule of the core applies here too." }
        }
    }
}
