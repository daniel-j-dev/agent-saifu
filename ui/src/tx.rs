//! The transaction screen and the popup that announces incoming WalletConnect requests.

use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::Args;
use crate::fmt::{expires_in, short_address};
use crate::model::{
    WcRequest, balance_changes, failure_reason, fee_line, outcome_lines, text, verdict, warnings,
};
use crate::state::{ActivityKind, AppState, Tab};
use crate::widgets::*;

/// What a simulation or review says, in plain language first and raw details last.
#[cfg(test)]
mod tests {
    use super::preview;

    #[test]
    fn previews_long_text_only() {
        assert_eq!(preview("short", 10), "short");
        assert_eq!(preview("0123456789ab", 10), "0123456789…");
        assert_eq!(preview("ab  cd", 3), "ab…");
    }
}

#[component]
pub fn ReviewView(review: Value) -> Element {
    let verdict = verdict(&review);
    let mut lines = outcome_lines(&review);
    lines.extend(balance_changes(&review));
    let warnings = warnings(&review);
    let reason = failure_reason(&review);
    let fee = fee_line(&review);
    let note = text(&review, "note");
    rsx! {
        match verdict {
            Some(true) => rsx! {
                div { class: "verdict ok",
                    span { class: "verdict-mark", "✓" }
                    div {
                        strong { "Simulation passed" }
                        span { class: "sub", "A dry run says this would succeed." }
                    }
                }
            },
            Some(false) => rsx! {
                div { class: "verdict bad",
                    span { class: "verdict-mark", "✕" }
                    div {
                        strong { "This would fail" }
                        span { class: "sub", "{reason.clone().unwrap_or_else(|| \"The dry run did not succeed.\".to_string())}" }
                    }
                }
            },
            None => rsx! {},
        }
        if !lines.is_empty() {
            div { class: "panel",
                p { class: "eyebrow", "WHAT HAPPENS" }
                ul { class: "lines",
                    for line in lines.iter() {
                        li { "{line}" }
                    }
                }
            }
        }
        if let Some(fee) = fee {
            div { class: "panel",
                Detail { label: "Network fee (at most)", value: fee }
            }
        }
        for warning in warnings.iter() {
            Notice { warn: true, "⚠  {warning}" }
        }
        if !note.is_empty() {
            Notice { "{note}" }
        }
        details { class: "tech",
            summary { "Technical details" }
            JsonView { value: review.clone() }
        }
    }
}

#[derive(Clone, PartialEq)]
enum Outcome {
    Approved(Value),
    Rejected,
}

#[component]
pub fn TransactionScreen(id: String) -> Element {
    let mut state = use_context::<AppState>();
    // The wallet drops a request from its list once it is answered or expires; keep our copy so
    // the screen can still show the result.
    let held: Signal<Option<WcRequest>> = use_signal({
        let id = id.clone();
        move || state.request(&id)
    });
    let password = use_secret();
    let review = use_task();
    let approve = use_task();
    let reject = use_task();
    let mut outcome = use_signal(|| None::<Outcome>);

    let Some(request) = held.read().clone() else {
        return rsx! {
            BackBar {}
            div { class: "empty",
                div { class: "agent-mark", "⇄" }
                h2 { "This request is gone." }
                p { class: "sub gap", "It was answered, rejected or it expired." }
                button { class: "primary", onclick: move |_| state.screen.set(None), "Back to activity" }
            }
        };
    };

    let session = state.session_for(&request);
    let wallet_name = session
        .as_ref()
        .map(|s| s.wallet.clone())
        .unwrap_or_default();
    let still_pending = state.request(&request.id).is_some();
    let reviewed = review.read().answer().cloned();
    let can_approve = reviewed.as_ref().is_some_and(|r| verdict(r) != Some(false)) && still_pending;
    let simulate_label = if request.is_transaction() {
        "Simulate transaction"
    } else {
        "Review what you sign"
    };
    let busy = review.read().is_busy() || approve.read().is_busy() || reject.read().is_busy();

    let args = {
        let request = request.clone();
        let wallet_name = wallet_name.clone();
        move || {
            Args::new()
                .text("request_id", &request.id)
                .text("wallet_name", &wallet_name)
                .opt_int("chain_id", request.chain_id)
                .text("password", &password.read())
                .build()
        }
    };
    let run_review = {
        let args = args.clone();
        let wallet_name = wallet_name.clone();
        move |_| {
            if wallet_name.is_empty() {
                return state.toast("The connection for this request is gone.");
            }
            if password.read().is_empty() {
                return state.toast("Enter the wallet password to simulate.");
            }
            run_tool(review, "wc_review_request", args(), |_| {});
        }
    };
    let run_approve = {
        let request = request.clone();
        move |_| {
            let request = request.clone();
            let title = request.title();
            let kind = if request.is_transaction() {
                ActivityKind::Sent
            } else {
                ActivityKind::Signed
            };
            let dapp = request.dapp.title();
            let amount = request.native_amount().unwrap_or_default();
            run_tool(approve, "wc_approve_request", args(), move |answer| {
                state.log(kind, &format!("{title} · {dapp}"), &amount, "Approved");
                state.toast("Approved and sent back to the app.");
                // An approved transaction changes the balances.
                state.refresh();
                outcome.set(Some(Outcome::Approved(answer.clone())));
                wipe(password);
            });
        }
    };
    let run_reject = {
        let request = request.clone();
        move |_| {
            let title = request.title();
            let dapp = request.dapp.title();
            let args = Args::new().text("request_id", &request.id).build();
            run_tool(reject, "wc_reject_request", args, move |_| {
                state.log(
                    ActivityKind::Signed,
                    &format!("{title} · {dapp}"),
                    "",
                    "Rejected",
                );
                state.toast("Request rejected.");
                outcome.set(Some(Outcome::Rejected));
                wipe(password);
            });
        }
    };

    let expiry = if still_pending {
        expires_in(request.expires_in)
    } else {
        "closed".to_string()
    };
    let message = text(&request.details, "text");
    let headline = match (request.native_amount(), request.kind.as_str()) {
        (Some(amount), _) => amount,
        (None, "message") if !message.is_empty() => format!("“{}”", preview(&message, 70)),
        _ => request.title(),
    };
    let show_title = headline != request.title();
    let target = text(&request.details, "to");
    let hero_sub = if target.is_empty() {
        format!("{} · {}", request.dapp.title(), request.chain)
    } else {
        format!("to {} · {}", short_address(&target), request.chain)
    };
    let host = request.dapp.url.clone();
    rsx! {
        BackBar {}
        section { class: "balance-card tx-hero",
            div { class: "row between",
                span { class: "eyebrow", "{request.dapp.title()} asks to" }
                span { class: "badge", "{expiry}" }
            }
            if show_title {
                div { class: "balance small-balance", "{request.title()}" }
            }
            div { class: "tx-amount", "{headline}" }
            div { class: "change", "{hero_sub}" }
            div { class: "card-bottom",
                span { "♧ Wallet · {wallet_name}" }
                span { "{host}" }
            }
        }
        Notice { warn: true, "App identity is self-declared and not verified. Check the website before approving." }

        match outcome.read().clone() {
            Some(Outcome::Approved(answer)) => rsx! {
                div { class: "empty",
                    div { class: "success-symbol", "✓" }
                    h2 { "Approved" }
                    p { class: "sub gap", "The answer was sent back to {request.dapp.title()}. The wallet locked again." }
                }
                div { class: "panel", JsonView { value: answer } }
                button { class: "primary", onclick: move |_| state.screen.set(None), "Done" }
            },
            Some(Outcome::Rejected) => rsx! {
                div { class: "empty",
                    div { class: "success-symbol", "✕" }
                    h2 { "Rejected" }
                    p { class: "sub gap", "{request.dapp.title()} was told you declined. Nothing was signed." }
                }
                button { class: "primary", onclick: move |_| state.screen.set(None), "Done" }
            },
            None => rsx! {
                if let Some(result) = reviewed.clone() {
                    ReviewView { review: result }
                } else {
                    div { class: "panel",
                        p { class: "eyebrow", "{simulate_label}" }
                        p { class: "sub", "Your password unlocks {wallet_name} for this check only. It locks again right after." }
                        PasswordField { value: password }
                        TaskStatus { task: review, label: "Simulating on the network…" }
                        button { class: "primary", disabled: busy || !still_pending, onclick: run_review, "{simulate_label}" }
                    }
                }
                if reviewed.is_some() {
                    TaskStatus { task: approve, label: "Signing…" }
                    button { class: "primary", disabled: busy || !can_approve, onclick: run_approve, "Approve & sign" }
                }
                TaskStatus { task: reject, label: "Telling the app…" }
                button { class: "danger", disabled: busy || !still_pending, onclick: run_reject, "Reject" }
                if !request.details_is_empty() {
                    details { class: "tech", open: !request.is_transaction(),
                        summary { "What the app asked for" }
                        JsonView { value: request.details.clone() }
                        Detail { label: "Method", value: request.method.clone() }
                        Detail { label: "Network", value: request.chain.clone() }
                    }
                }
            },
        }
    }
}

impl WcRequest {
    fn details_is_empty(&self) -> bool {
        match &self.details {
            Value::Object(pairs) => pairs.is_empty(),
            Value::Null => true,
            _ => false,
        }
    }
}

/// The first `max` characters, with an ellipsis when cut.
fn preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

#[component]
fn BackBar() -> Element {
    let mut state = use_context::<AppState>();
    rsx! {
        button {
            class: "back",
            onclick: move |_| {
                state.screen.set(None);
                state.tab.set(Tab::Activity);
            },
            "‹ Activity"
        }
    }
}

/// The banner at the top of the screen for a request that just arrived.
#[component]
pub fn RequestPopup() -> Element {
    let mut state = use_context::<AppState>();
    let Some(id) = state.popup.read().clone() else {
        return rsx! {};
    };
    let Some(request) = state.request(&id) else {
        return rsx! {};
    };
    let amount = request
        .native_amount()
        .unwrap_or_else(|| request.chain.clone());
    let open_id = id.clone();
    rsx! {
        PopupTimer { key: "{id}", id: id.clone() }
        div { class: "popup", role: "alert",
            button { class: "popup-body", onclick: move |_| state.open_request(&open_id),
                span { class: "agent-mark", "⇄" }
                span { class: "grow",
                    strong { "{request.title()} · {request.dapp.title()}" }
                    span { class: "sub", "{amount} · tap to review" }
                }
                b { "›" }
            }
            button {
                class: "icon-btn popup-x",
                aria_label: "Dismiss",
                onclick: move |_| state.popup.set(None),
                "✕"
            }
        }
    }
}

#[component]
fn PopupTimer(id: String) -> Element {
    let mut state = use_context::<AppState>();
    use_future(move || {
        let id = id.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if state.popup.peek().as_deref() == Some(id.as_str()) {
                state.popup.set(None);
            }
        }
    });
    rsx! {}
}
