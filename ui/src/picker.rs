//! The token picker used by the swap and send screens: tokens you hold, plus (when `search` is
//! on) a search by name, symbol or address that works on Ethereum (and its layer 2s), Sui and
//! Solana.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::{Args, Backend, TOKEN_SEARCH};
use crate::fmt::{short_address, trim_amount};
use crate::model::Asset;
use crate::tokens::{Hit, Net, hits_from_value, looks_like_address};
use crate::widgets::{Alert, Busy, Coin};

/// A token chosen for a swap.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenChoice {
    /// `native`, a contract, a mint or a Sui coin type.
    pub token: String,
    pub symbol: String,
    pub name: String,
    pub verified: bool,
    /// The EVM chain it was chosen on (`None` on Sui and Solana).
    pub chain: Option<u64>,
    /// The network's name when known (for telling "ETH on Base" from "ETH on Ethereum").
    pub network: String,
}

impl TokenChoice {
    pub fn of_asset(asset: &Asset, fallback_chain: Option<u64>) -> TokenChoice {
        TokenChoice {
            token: asset.asset.clone(),
            symbol: asset.symbol.clone(),
            name: asset.name.clone(),
            verified: true,
            chain: asset.chain_id.or(fallback_chain),
            network: asset.network.clone(),
        }
    }
}

#[derive(Clone, PartialEq)]
enum Found {
    Idle,
    Loading,
    Done(Vec<Hit>),
    Failed(String),
}

const SEARCH_DELAY: Duration = Duration::from_millis(300);
const MIN_QUERY_CHARS: usize = 2;

/// Held tokens whose symbol, name or address contains `query` (all of them when it is empty).
pub fn matching_held(held: &[Asset], query: &str, exclude: &str) -> Vec<Asset> {
    let q = query.trim().to_lowercase();
    held.iter()
        .filter(|a| a.asset != exclude)
        .filter(|a| {
            q.is_empty()
                || a.symbol.to_lowercase().contains(&q)
                || a.name.to_lowercase().contains(&q)
                || a.asset.to_lowercase() == q
        })
        .cloned()
        .collect()
}

#[component]
pub fn TokenPicker(
    /// `ethereum`, `sui` or `solana`.
    net: String,
    chain_id: Option<u64>,
    held: Vec<Asset>,
    /// A token that is not offered (the other side of a swap).
    exclude: String,
    selected: Option<TokenChoice>,
    on_pick: EventHandler<TokenChoice>,
    /// Whether to search the network's token lists too. Off, only `held` is offered.
    #[props(default = true)]
    search: bool,
    /// Name the network on the button; only useful when the wallet has several.
    #[props(default)]
    show_network: bool,
    /// The button spans the whole row (used when nothing is shown to its left).
    #[props(default)]
    wide: bool,
    /// Shown to the left of the button.
    children: Element,
) -> Element {
    let mut open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut found = use_signal(|| Found::Idle);
    let mut seq = use_signal(|| 0u64);

    let search_net = Net::parse(&net, chain_id).filter(|_| search);
    let typed = query.read().clone();
    let trimmed = typed.trim().to_string();
    let held_matches = matching_held(&held, &typed, &exclude);
    let as_address = search_net.is_some_and(|n| looks_like_address(n, &trimmed));
    let label = selected
        .as_ref()
        .map(|s| {
            if s.network.is_empty() || !show_network {
                s.symbol.clone()
            } else {
                format!("{} · {}", s.symbol, s.network)
            }
        })
        .unwrap_or_else(|| "Select token".to_string());
    let icon = selected
        .as_ref()
        .map(|s| s.symbol.clone())
        .unwrap_or_else(|| "?".to_string());

    let on_input = {
        let net = net.clone();
        move |event: FormEvent| {
            let text = event.value();
            query.set(text.clone());
            let id = *seq.peek() + 1;
            seq.set(id);
            if !search || text.trim().chars().count() < MIN_QUERY_CHARS {
                found.set(Found::Idle);
                return;
            }
            found.set(Found::Loading);
            let net = net.clone();
            let backend = consume_context::<Backend>();
            spawn(async move {
                tokio::time::sleep(SEARCH_DELAY).await;
                if *seq.peek() != id {
                    return;
                }
                let args = Args::new()
                    .text("network", &net)
                    .opt_int("chain_id", chain_id)
                    .text("query", text.trim())
                    .build();
                let result = backend.call(TOKEN_SEARCH, args).await;
                if *seq.peek() != id {
                    return;
                }
                found.set(match result {
                    Ok(answer) => Found::Done(hits_from_value(&answer)),
                    Err(message) => Found::Failed(message),
                });
            });
        }
    };

    let mut pick = move |choice: TokenChoice| {
        on_pick.call(choice);
        open.set(false);
        query.set(String::new());
        found.set(Found::Idle);
        let next = *seq.peek() + 1;
        seq.set(next);
    };
    let is_open = *open.read();
    let held_ids: Vec<String> = held.iter().map(|a| a.asset.to_lowercase()).collect();
    let results = match &*found.read() {
        Found::Done(hits) => hits
            .iter()
            .filter(|h| h.address != exclude && !held_ids.contains(&h.address.to_lowercase()))
            .cloned()
            .collect::<Vec<_>>(),
        _ => vec![],
    };
    let state = found.read().clone();

    rsx! {
        div { class: if wide { "picker-block wide" } else { "picker-block" },
            div { class: "row between pad-y",
                {children}
                button {
                    class: "token-select picker-trigger",
                    aria_haspopup: "listbox",
                    aria_expanded: "{is_open}",
                    onclick: move |_| open.toggle(),
                    Coin { symbol: icon, small: true }
                    span { class: "picker-label", "{label}" }
                    span { class: "picker-caret", "⌄" }
                }
            }
            if is_open {
                div {
                    class: "picker",
                    onmounted: move |e| {
                        spawn(async move {
                            let _ = e.data().scroll_to(ScrollBehavior::Smooth).await;
                        });
                    },
                    input {
                        class: "picker-search",
                        r#type: "search",
                        placeholder: if search { "Search by name, symbol or address" } else { "Filter your tokens" },
                        aria_label: "Search tokens",
                        value: "{query}",
                        autocomplete: "off",
                        autocapitalize: "off",
                        spellcheck: "false",
                        onmounted: move |e| {
                            spawn(async move {
                                let _ = e.data().set_focus(true).await;
                            });
                        },
                        oninput: on_input,
                    }
                    div { class: "picker-list", role: "listbox",
                        if as_address {
                            {
                                let token = trimmed.clone();
                                let chain = chain_id;
                                rsx! {
                                    TokenRow {
                                        symbol: "?".to_string(),
                                        title: "Use this address".to_string(),
                                        sub: short_address(&token),
                                        verified: false,
                                        onpick: move |_| {
                                            pick(TokenChoice {
                                                token: token.clone(),
                                                symbol: "token".to_string(),
                                                name: String::new(),
                                                verified: false,
                                                chain,
                                                network: String::new(),
                                            })
                                        },
                                    }
                                }
                            }
                        }
                        if !held_matches.is_empty() {
                            if search {
                                div { class: "picker-head", "YOUR TOKENS" }
                            }
                            for asset in held_matches.iter().cloned() {
                                {
                                    let choice = TokenChoice::of_asset(&asset, chain_id);
                                    let sub = held_line(&asset);
                                    rsx! {
                                        TokenRow {
                                            symbol: asset.symbol.clone(),
                                            title: asset.symbol.clone(),
                                            sub,
                                            verified: false,
                                            onpick: move |_| pick(choice.clone()),
                                        }
                                    }
                                }
                            }
                        }
                        match state {
                            Found::Loading => rsx! { Busy { label: "Searching…" } },
                            Found::Failed(message) => rsx! { Alert { message } },
                            Found::Done(_) if results.is_empty() && !as_address => rsx! {
                                div { class: "picker-empty", "No tokens found for “{trimmed}”. You can paste the token address instead." }
                            },
                            Found::Idle if held_matches.is_empty() && search => rsx! {
                                div { class: "picker-empty", "Type a name like USDC, or paste a token address." }
                            },
                            Found::Idle if held_matches.is_empty() => rsx! {
                                div { class: "picker-empty", "No matching token. Unlock to see what you hold." }
                            },
                            _ => rsx! {},
                        }
                        if !results.is_empty() {
                            div { class: "picker-head", "SEARCH RESULTS" }
                            for hit in results.iter().cloned() {
                                {
                                    let choice = TokenChoice {
                                        token: hit.address.clone(),
                                        symbol: hit.symbol.clone(),
                                        name: hit.name.clone(),
                                        verified: hit.verified,
                                        chain: chain_id,
                                        network: String::new(),
                                    };
                                    let mut sub = hit.name.clone();
                                    if hit.address != "native" {
                                        sub = format!("{sub} · {}", short_address(&hit.address));
                                    }
                                    if !hit.note.is_empty() {
                                        sub = format!("{sub} · {}", hit.note);
                                    }
                                    rsx! {
                                        TokenRow {
                                            symbol: hit.symbol.clone(),
                                            title: hit.symbol.clone(),
                                            sub,
                                            verified: hit.verified,
                                            onpick: move |_| pick(choice.clone()),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `2.45 held · Ethereum Mainnet`; just the network when the balance is not known.
fn held_line(asset: &Asset) -> String {
    let amount = if asset.balance.is_empty() {
        String::new()
    } else {
        format!("{} held", trim_amount(&asset.balance, 6))
    };
    match (amount.is_empty(), asset.network.is_empty()) {
        (false, false) => format!("{amount} · {}", asset.network),
        (false, true) => amount,
        (true, false) => asset.network.clone(),
        (true, true) => String::new(),
    }
}

#[component]
fn TokenRow(
    symbol: String,
    title: String,
    sub: String,
    verified: bool,
    onpick: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        button { class: "picker-row", role: "option", onclick: move |e| onpick.call(e),
            Coin { symbol }
            span { class: "grow",
                strong {
                    "{title}"
                    if verified {
                        span { class: "tick", title: "From a verified list", " ✓" }
                    }
                }
                span { class: "sub", "{sub}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(symbol: &str, name: &str, id: &str) -> Asset {
        Asset {
            symbol: symbol.to_string(),
            name: name.to_string(),
            asset: id.to_string(),
            balance: "1".to_string(),
            usd: None,
            chain_id: Some(1),
            network: String::new(),
        }
    }

    #[test]
    fn describes_a_holding() {
        let mut a = asset("ETH", "Ether", "native");
        assert_eq!(held_line(&a), "1 held");
        a.network = "Base".to_string();
        assert_eq!(held_line(&a), "1 held · Base");
        a.balance = String::new();
        assert_eq!(held_line(&a), "Base");
        a.network = String::new();
        assert_eq!(held_line(&a), "");
    }

    #[test]
    fn a_choice_remembers_where_it_came_from() {
        let mut a = asset("ETH", "Ether", "native");
        a.chain_id = None;
        a.network = "Ethereum Mainnet".to_string();
        let choice = TokenChoice::of_asset(&a, Some(8453));
        assert_eq!(choice.chain, Some(8453));
        assert_eq!(choice.network, "Ethereum Mainnet");
        a.chain_id = Some(1);
        assert_eq!(TokenChoice::of_asset(&a, Some(8453)).chain, Some(1));
    }

    #[test]
    fn held_tokens_are_filtered_by_name_symbol_or_address() {
        let held = vec![
            asset("ETH", "Ether", "native"),
            asset("USDC", "USD Coin", "0xA0b8"),
            asset("DAI", "Dai", "0x6B17"),
        ];
        let symbols = |q: &str, ex: &str| {
            matching_held(&held, q, ex)
                .into_iter()
                .map(|a| a.symbol)
                .collect::<Vec<_>>()
        };
        assert_eq!(symbols("", ""), vec!["ETH", "USDC", "DAI"]);
        assert_eq!(symbols("", "native"), vec!["USDC", "DAI"]);
        assert_eq!(symbols("usd", ""), vec!["USDC"]);
        assert_eq!(symbols("coin", ""), vec!["USDC"]);
        assert_eq!(symbols("0x6b17", ""), vec!["DAI"]);
        assert!(symbols("zzz", "").is_empty());
    }
}
