//! Recovery material and backups. Each action asks for the password on its own.

use dioxus::prelude::*;

use crate::backend::Args;
use crate::model::text;
use crate::state::{AppState, Sheet};
use crate::widgets::*;

#[component]
pub fn SecuritySheet() -> Element {
    let state = use_context::<AppState>();
    rsx! {
        SheetFrame { title: "Recovery & backup",
            Notice { "Each sensitive action asks for your wallet password. Never share recovery material with anyone, including an agent." }
            LinkRow { icon: "▦", title: "View recovery phrase", subtitle: "24 words · requires your password", onclick: move |_| state.open(Sheet::SeedPhrase) }
            LinkRow { icon: "⌁", title: "View private key", subtitle: "Requires your password", onclick: move |_| state.open(Sheet::PrivateKey) }
            LinkRow { icon: "↓", title: "Export encrypted backup", subtitle: "Encrypted vault inside a ZIP file", onclick: move |_| state.open(Sheet::Export) }
        }
    }
}

/// Shows the recovery phrase or private key after the password; wipes it when closed.
#[component]
fn SecretView(phrase: bool) -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let tool = if phrase {
        "view_seed_phrase"
    } else {
        "view_private_key"
    };
    let reveal = move |_| {
        if password.read().is_empty() {
            return state.toast("Enter the wallet password.");
        }
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .build();
        run_tool(task, tool, args, move |_| wipe(password));
    };
    let answer = task.read().answer().cloned();
    let title = if phrase {
        "Your recovery phrase"
    } else {
        "Your private key"
    };
    rsx! {
        SheetFrame { title,
            match answer {
                Some(answer) if phrase => {
                    let words: Vec<String> = text(&answer, "seed_phrase").split_whitespace().map(str::to_string).collect();
                    let joined = words.join(" ");
                    rsx! {
                        Notice { warn: true, "Anyone with these words controls the wallet. Write them down and keep them offline." }
                        div { class: "seedgrid",
                            for (index , word) in words.iter().enumerate() {
                                span {
                                    small { "{index + 1}" }
                                    "{word}"
                                }
                            }
                        }
                        button { class: "secondary", onclick: move |_| copy_text(joined.clone(), state), "Copy phrase" }
                        button { class: "primary", onclick: move |_| state.close(), "Hide recovery phrase" }
                    }
                }
                Some(answer) => {
                    let shown = ["private_key", "private_key_wif", "key"].iter().map(|k| text(&answer, k)).find(|s| !s.is_empty()).unwrap_or_default();
                    rsx! {
                        Notice { warn: true, "Anyone with this key controls the wallet. Never share it." }
                        if !shown.is_empty() {
                            div { class: "address", "{shown}" }
                        }
                        div { class: "panel", JsonView { value: answer.clone() } }
                        button { class: "primary", onclick: move |_| state.close(), "Hide private key" }
                    }
                }
                None => rsx! {
                    p { class: "sub", "{wallet.name}" }
                    Notice { "Make sure nobody can see your screen." }
                    PasswordField { value: password }
                    TaskStatus { task, label: "Unlocking…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: reveal,
                        if phrase { "Show recovery phrase" } else { "Show private key" }
                    }
                },
            }
        }
    }
}

#[component]
pub fn SeedPhraseSheet() -> Element {
    rsx! { SecretView { phrase: true } }
}

#[component]
pub fn PrivateKeySheet() -> Element {
    rsx! { SecretView { phrase: false } }
}

#[component]
pub fn ExportSheet() -> Element {
    let state = use_context::<AppState>();
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let key = use_secret();
    let confirm = use_secret();
    let file = use_signal(String::new);
    let password = use_secret();
    let task = use_task();
    let name = wallet.name.clone();
    let submit = move |_| {
        if key.read().chars().count() < 12 || *key.read() != *confirm.read() {
            return state.toast("Use matching export keys of at least 12 characters.");
        }
        let args = Args::new()
            .text("wallet_name", &name)
            .text("password", &password.read())
            .text("encryption_key", &key.read())
            .opt("file_name", &file.read())
            .build();
        run_tool(task, "export_wallet", args, move |_| {
            wipe(key);
            wipe(confirm);
            wipe(password);
        });
    };
    let answer = task.read().answer().cloned();
    rsx! {
        SheetFrame { title: "A backup you control.",
            match answer {
                Some(answer) => rsx! {
                    div { class: "empty",
                        div { class: "success-symbol", "✓" }
                        h2 { "Backup written" }
                    }
                    div { class: "panel", JsonView { value: answer } }
                    Notice { warn: true, "You need this ZIP and your export key to restore. The export key cannot be recovered." }
                    button { class: "primary", onclick: move |_| state.close(), "Done" }
                },
                None => rsx! {
                    p { class: "sub", "The ZIP holds your recovery phrase and key, encrypted with a key only you know. You need the ZIP and that key to restore it." }
                    Field { label: "Export encryption key", value: key, kind: "password", placeholder: "At least 12 characters" }
                    Field { label: "Confirm encryption key", value: confirm, kind: "password", placeholder: "Repeat export key" }
                    Field { label: "File name (optional)", value: file, placeholder: "my-backup.zip" }
                    PasswordField { value: password }
                    Notice { warn: true, "Your export key cannot be recovered. Existing files are never overwritten." }
                    TaskStatus { task, label: "Encrypting backup…" }
                    button { class: "primary", disabled: task.read().is_busy(), onclick: submit, "Export backup" }
                },
            }
        }
    }
}
