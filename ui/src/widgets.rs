//! Building blocks shared by every screen.

use agent_saifu_core::crypto::secret::zeroize_string;
use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::Backend;
use crate::fmt::humanize_key;
use crate::state::AppState;

#[derive(Clone, PartialEq, Debug)]
pub enum Task {
    Idle,
    Busy,
    Done(Value),
    Failed(String),
}

impl Task {
    pub fn is_busy(&self) -> bool {
        matches!(self, Task::Busy)
    }

    pub fn answer(&self) -> Option<&Value> {
        match self {
            Task::Done(v) => Some(v),
            _ => None,
        }
    }
}

/// A task whose answer (which can hold a recovery phrase or key) is wiped when its screen goes.
pub fn use_task() -> Signal<Task> {
    let mut task = use_signal(|| Task::Idle);
    use_drop(move || {
        if let Ok(mut current) = task.try_write()
            && let Task::Done(answer) = &mut *current
        {
            answer.wipe();
        }
    });
    task
}

/// A text signal that is overwritten with zeros when its screen goes away. Use for passwords,
/// recovery phrases and keys.
pub fn use_secret() -> Signal<String> {
    let secret = use_signal(String::new);
    use_drop(move || wipe(secret));
    secret
}

pub fn wipe(mut secret: Signal<String>) {
    if let Ok(mut text) = secret.try_write() {
        zeroize_string(&mut text);
    }
}

/// Runs a wallet tool without blocking the screen. `on_ok` sees the answer before the task
/// switches to `Done`.
pub fn run_tool(
    mut task: Signal<Task>,
    name: &'static str,
    args: Value,
    on_ok: impl FnOnce(&Value) + 'static,
) {
    if task.peek().is_busy() {
        return;
    }
    task.set(Task::Busy);
    let backend = consume_context::<Backend>();
    spawn(async move {
        match backend.call(name, args).await {
            Ok(answer) => {
                on_ok(&answer);
                task.set(Task::Done(answer));
            }
            Err(message) => task.set(Task::Failed(message)),
        }
    });
}

/// Shows progress or the error of a task; nothing otherwise.
#[component]
pub fn TaskStatus(
    task: Signal<Task>,
    #[props(default = "Working…".to_string())] label: String,
) -> Element {
    match &*task.read() {
        Task::Busy => rsx! { Busy { label } },
        Task::Failed(message) => rsx! { Alert { message: message.clone() } },
        _ => rsx! {},
    }
}

#[component]
pub fn Busy(label: String) -> Element {
    rsx! {
        div { class: "busy", role: "status",
            span { class: "spin" }
            "{label}"
        }
    }
}

#[component]
pub fn Alert(message: String) -> Element {
    rsx! { div { class: "notice warn", role: "alert", "{message}" } }
}

#[component]
pub fn Notice(#[props(default)] warn: bool, children: Element) -> Element {
    let class = if warn { "notice warn" } else { "notice" };
    rsx! { div { class: "{class}", {children} } }
}

#[component]
pub fn Detail(label: String, value: String) -> Element {
    rsx! {
        div { class: "details",
            span { "{label}" }
            span { class: "wrap", "{value}" }
        }
    }
}

fn field_id(label: &str) -> String {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("f-{slug}")
}

#[component]
pub fn Field(
    label: String,
    value: Signal<String>,
    #[props(default)] placeholder: String,
    #[props(default = "text".to_string())] kind: String,
    #[props(default)] hint: String,
) -> Element {
    let id = field_id(&label);
    let mode = if kind == "number" { "decimal" } else { "text" };
    rsx! {
        div { class: "field",
            label { r#for: "{id}", "{label}" }
            input {
                id: "{id}",
                r#type: if kind == "number" { "text" } else { "{kind}" },
                inputmode: "{mode}",
                value: "{value}",
                placeholder: "{placeholder}",
                autocomplete: "off",
                autocapitalize: "off",
                spellcheck: "false",
                oninput: move |e| value.set(e.value()),
            }
            if !hint.is_empty() {
                p { class: "sub hint", "{hint}" }
            }
        }
    }
}

#[component]
pub fn PasswordField(
    value: Signal<String>,
    #[props(default = "Wallet password".to_string())] label: String,
) -> Element {
    rsx! { Field { label, value, kind: "password", placeholder: "Enter your password" } }
}

#[component]
pub fn TextArea(
    label: String,
    value: Signal<String>,
    #[props(default)] placeholder: String,
) -> Element {
    let id = field_id(&label);
    rsx! {
        div { class: "field",
            label { r#for: "{id}", "{label}" }
            textarea {
                id: "{id}",
                value: "{value}",
                placeholder: "{placeholder}",
                autocomplete: "off",
                autocapitalize: "off",
                spellcheck: "false",
                oninput: move |e| value.set(e.value()),
            }
        }
    }
}

#[component]
pub fn SelectField(
    label: String,
    value: Signal<String>,
    options: Vec<(String, String)>,
) -> Element {
    let id = field_id(&label);
    rsx! {
        div { class: "field",
            label { r#for: "{id}", "{label}" }
            select {
                id: "{id}",
                value: "{value}",
                onchange: move |e| value.set(e.value()),
                for (v , l) in options.iter() {
                    option { value: "{v}", selected: *v == *value.read(), "{l}" }
                }
            }
        }
    }
}

fn glyph(symbol: &str) -> String {
    match symbol {
        "ETH" => "⟠".to_string(),
        "SUI" => "◒".to_string(),
        "SOL" => "≋".to_string(),
        "BTC" => "₿".to_string(),
        "USDC" | "USDT" | "DAI" => "$".to_string(),
        other => other
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_else(|| "?".to_string()),
    }
}

#[component]
pub fn Coin(symbol: String, #[props(default)] small: bool) -> Element {
    let class = format!(
        "coin {}{}",
        symbol.to_lowercase(),
        if small { " small" } else { "" }
    );
    rsx! { span { class: "{class}", "{glyph(&symbol)}" } }
}

/// Enter in a text field presses the first enabled primary button inside `container`.
pub fn press_primary_on_enter(event: &KeyboardEvent, container: &str) {
    if event.key() != Key::Enter {
        return;
    }
    let script = format!(
        "const a = document.activeElement; \
         if (a && a.tagName === 'INPUT') {{ \
           const b = document.querySelector('{container} button.primary:not(:disabled)'); \
           if (b) b.click(); }}"
    );
    document::eval(&script);
}

/// A bottom sheet (a centred card on wide screens) over the current screen.
#[component]
pub fn SheetFrame(title: String, children: Element) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        div {
            class: "overlay",
            onclick: move |_| state.close(),
            section {
                class: "sheet",
                role: "dialog",
                aria_modal: "true",
                aria_label: "{title}",
                onclick: move |e| e.stop_propagation(),
                onkeydown: move |e| press_primary_on_enter(&e, ".sheet"),
                div { class: "sheet-grip" }
                div { class: "sheet-head",
                    h2 { "{title}" }
                    button {
                        class: "icon-btn",
                        aria_label: "Close",
                        onclick: move |_| state.close(),
                        "✕"
                    }
                }
                {children}
            }
        }
    }
}

#[component]
pub fn Intro(title: String, subtitle: String) -> Element {
    rsx! {
        div { class: "intro",
            h1 { "{title}" }
            p { class: "sub", "{subtitle}" }
        }
    }
}

#[component]
pub fn LinkRow(
    icon: String,
    title: String,
    subtitle: String,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        button { class: "list-link", onclick: move |e| onclick.call(e),
            span { class: "listicon", "{icon}" }
            span { class: "grow",
                strong { "{title}" }
                span { class: "sub", "{subtitle}" }
            }
            b { "›" }
        }
    }
}

/// Renders any wallet answer as labelled rows, for details that have no dedicated layout.
#[component]
pub fn JsonView(value: Value) -> Element {
    rsx! { div { class: "json", {rows(&value)} } }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "—".to_string(),
        Value::Bool(true) => "yes".to_string(),
        Value::Bool(false) => "no".to_string(),
        Value::Number(n) => n.clone(),
        Value::String(s) if s.is_empty() => "—".to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn rows(value: &Value) -> Element {
    match value {
        Value::Object(pairs) => rsx! {
            for (key , v) in pairs.iter() {
                {node(Some(key), v)}
            }
        },
        Value::Array(items) => rsx! {
            for v in items.iter() {
                {node(None, v)}
            }
        },
        other => rsx! { div { class: "kv", span { class: "v", "{scalar(other)}" } } },
    }
}

fn node(key: Option<&str>, value: &Value) -> Element {
    let label = key.map(humanize_key).unwrap_or_default();
    match value {
        Value::Object(_) | Value::Array(_) => {
            let plain_list = matches!(value, Value::Array(items) if items.iter().all(|i| !matches!(i, Value::Object(_) | Value::Array(_))));
            if plain_list {
                let joined = match value {
                    Value::Array(items) => items.iter().map(scalar).collect::<Vec<_>>().join(", "),
                    _ => String::new(),
                };
                let joined = if joined.is_empty() {
                    "—".to_string()
                } else {
                    joined
                };
                return rsx! {
                    div { class: "kv",
                        span { class: "k", "{label}" }
                        span { class: "v", "{joined}" }
                    }
                };
            }
            rsx! {
                div { class: "kv-group",
                    if key.is_some() {
                        div { class: "k-head", "{label}" }
                    }
                    div { class: "kv-children", {rows(value)} }
                }
            }
        }
        other => rsx! {
            div { class: "kv",
                if key.is_some() {
                    span { class: "k", "{label}" }
                }
                span { class: "v", "{scalar(other)}" }
            }
        },
    }
}

/// Copies text to the clipboard (falls back to the old selection trick on webviews that deny
/// the clipboard API) and reports the result in a toast.
pub fn copy_text(text: String, state: AppState) {
    spawn(async move {
        let script = r#"
            const text = await dioxus.recv();
            try { await navigator.clipboard.writeText(text); return true; } catch (e) {}
            try {
                const box = document.createElement('textarea');
                box.value = text; box.style.position = 'fixed'; box.style.opacity = '0';
                document.body.appendChild(box); box.select();
                const ok = document.execCommand('copy');
                document.body.removeChild(box);
                return ok;
            } catch (e) { return false; }
        "#;
        let eval = document::eval(script);
        let _ = eval.send(text);
        let copied = matches!(eval.join::<bool>().await, Ok(true));
        state.toast(if copied {
            "Copied"
        } else {
            "Copy is unavailable here. Select the text instead."
        });
    });
}

/// Reads the clipboard into `target`.
pub fn paste_into(mut target: Signal<String>, state: AppState) {
    spawn(async move {
        let script =
            "try { return await navigator.clipboard.readText(); } catch (e) { return ''; }";
        match document::eval(script).join::<String>().await {
            Ok(text) if !text.is_empty() => target.set(text),
            _ => state.toast("Paste is unavailable here. Long-press the field to paste."),
        }
    });
}
