//! The Swap tab.

use agent_saifu_core::blockchain::Blockchain;
use dioxus::prelude::*;

use crate::fmt::{chain_symbol, looks_like_amount, supports_swaps, trim_amount};
use crate::model::Asset;
use crate::pages::wallet_gate;
use crate::picker::{TokenChoice, TokenPicker};
use crate::state::{AppState, Sheet, SwapDraft};
use crate::widgets::*;

const USDC_ETHEREUM: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const USDC_SOLANA: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const USDC_SUI: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";

/// What to receive when nothing was picked: another token the wallet holds, or USDC.
pub fn default_receive(
    chain: Blockchain,
    evm_chain: Option<u64>,
    held: &[Asset],
    paying: &str,
) -> Option<TokenChoice> {
    if let Some(other) = held.iter().find(|a| a.asset != paying) {
        return Some(TokenChoice {
            token: other.asset.clone(),
            symbol: other.symbol.clone(),
            name: other.name.clone(),
            verified: true,
            chain: evm_chain,
            network: other.network.clone(),
        });
    }
    let usdc = match chain {
        // The address of USDC is only known for Ethereum mainnet.
        Blockchain::Ethereum if evm_chain == Some(1) => USDC_ETHEREUM,
        Blockchain::Solana => USDC_SOLANA,
        Blockchain::Sui => USDC_SUI,
        _ => return None,
    };
    (usdc != paying).then(|| TokenChoice {
        token: usdc.to_string(),
        symbol: "USDC".to_string(),
        name: "USD Coin".to_string(),
        verified: true,
        chain: evm_chain,
        network: String::new(),
    })
}

/// What the wallet can pay with before it is unlocked: its native coin, balance unknown.
fn native_placeholder(chain: Blockchain) -> Asset {
    Asset {
        symbol: chain_symbol(chain).to_string(),
        name: chain_symbol(chain).to_string(),
        asset: "native".to_string(),
        balance: String::new(),
        usd: None,
        chain_id: (chain == Blockchain::Ethereum).then_some(1),
        network: String::new(),
    }
}

/// Whether a held asset is the token a choice names (contract addresses compare case-blind).
fn same_token(asset: &Asset, choice: &TokenChoice) -> bool {
    asset.asset.eq_ignore_ascii_case(&choice.token)
        && (choice.chain.is_none() || asset.chain_id == choice.chain)
}

fn network_name(chain: Blockchain) -> &'static str {
    match chain {
        Blockchain::Ethereum => "ethereum",
        Blockchain::Sui => "sui",
        Blockchain::Solana => "solana",
        Blockchain::Bitcoin => "",
    }
}

#[component]
pub fn SwapPage() -> Element {
    let state = use_context::<AppState>();
    if let Some(gate) = wallet_gate(
        state,
        "A fresh exchange.",
        "Same network. New possibilities.",
    ) {
        return gate;
    }
    let Some(wallet) = state.current() else {
        return rsx! {};
    };
    let mut from_pick = use_signal(|| None::<TokenChoice>);
    let mut amount = use_signal(|| "0.1".to_string());
    let mut picked = use_signal(|| None::<TokenChoice>);

    if !supports_swaps(wallet.chain) {
        return rsx! {
            Intro { title: "Swap", subtitle: "Move between assets, without the complexity." }
            div { class: "empty",
                div { class: "agent-mark", "⇄" }
                h2 { "Bitcoin stays Bitcoin." }
                p { class: "sub gap", "Native Bitcoin swaps are not supported. Choose an Ethereum, Sui, or Solana wallet to swap on its network." }
                button { class: "primary", onclick: move |_| state.open(Sheet::Wallets), "Choose another wallet" }
            }
        };
    }

    let known = state.current_snapshot().is_some();
    let snapshot = state.visible_snapshot();
    let unlocked = snapshot.is_some();
    let assets: Vec<Asset> = snapshot
        .as_ref()
        .map(|s| s.assets.clone())
        .unwrap_or_default();
    // Until the wallet is unlocked the only thing it can pay with is its native coin.
    let pay_pool: Vec<Asset> = if assets.is_empty() {
        vec![native_placeholder(wallet.chain)]
    } else {
        assets.clone()
    };
    let first_chain = if wallet.chain == Blockchain::Ethereum {
        Some(1)
    } else {
        None
    };
    let from = from_pick
        .read()
        .clone()
        .filter(|c| pay_pool.iter().any(|a| same_token(a, c)))
        .unwrap_or_else(|| TokenChoice::of_asset(&pay_pool[0], first_chain));
    let (from_chain, from_asset) = (from.chain, from.token.clone());
    let from_symbol = from.symbol.clone();
    // Tokens on the chain being paid from; other networks' tokens cannot be received here.
    let held_here: Vec<Asset> = assets
        .iter()
        .filter(|a| a.chain_id == from_chain)
        .cloned()
        .collect();
    let evm_chain = if wallet.chain == Blockchain::Ethereum {
        from_chain.or(Some(1))
    } else {
        None
    };

    let receive = picked
        .read()
        .clone()
        .filter(|c| c.chain == evm_chain && c.token != from_asset)
        .or_else(|| default_receive(wallet.chain, evm_chain, &held_here, &from_asset));
    let held_amount = |chain: Option<u64>, token: &str| {
        snapshot
            .as_ref()
            .and_then(|s| s.find(chain, token))
            .cloned()
    };
    let from_held = held_amount(from_chain, &from_asset);
    let to_held = receive
        .as_ref()
        .and_then(|r| held_amount(r.chain, &r.token));

    let review = {
        let (from_asset, from_symbol, receive) =
            (from_asset.clone(), from_symbol.clone(), receive.clone());
        move |_| {
            let text_amount = amount.read().trim().to_string();
            if !looks_like_amount(&text_amount) {
                return state.toast("Enter an amount greater than zero.");
            }
            let Some(receive) = receive.clone() else {
                return state.toast("Choose the token you want to receive.");
            };
            if receive.token == from_asset {
                return state.toast("Choose two different assets.");
            }
            state.open(Sheet::SwapReview(SwapDraft {
                from_token: from_asset.clone(),
                from_symbol: from_symbol.clone(),
                to_token: receive.token,
                to_symbol: receive.symbol,
                amount: text_amount,
                chain_id: from_chain,
            }));
        }
    };
    let reverse = {
        let (from, receive, pool) = (from.clone(), receive.clone(), pay_pool.clone());
        move |_| {
            let Some(receive) = receive.clone() else {
                return;
            };
            let Some(held) = pool
                .iter()
                .find(|a| same_token(a, &receive) && !a.balance.is_empty())
            else {
                return state.toast("You can only pay with a token you hold.");
            };
            from_pick.set(Some(TokenChoice::of_asset(held, evm_chain)));
            picked.set(Some(from.clone()));
        }
    };

    let several_networks = snapshot.as_ref().is_some_and(|s| s.networks().len() > 1);
    let slippage = *state.slippage_bps.read() as f64 / 100.0;
    let wallet_name = wallet.name.clone();
    let net = network_name(wallet.chain).to_string();
    rsx! {
        Intro { title: "A fresh exchange.", subtitle: "Same network. New possibilities." }
        div { class: "row between gap-b",
            button { class: "pill", onclick: move |_| state.open(Sheet::Wallets),
                span { class: "dot" }
                "{wallet_name} ⌄"
            }
            button { class: "icon-btn", aria_label: "Swap settings", onclick: move |_| state.open(Sheet::Slippage), "⚙" }
        }
        div { class: "swapbox",
            p { class: "sub", "You pay" }
            TokenPicker {
                net: net.clone(),
                chain_id: evm_chain,
                held: pay_pool,
                exclude: receive.as_ref().map(|r| r.token.clone()).unwrap_or_default(),
                selected: Some(from.clone()),
                search: false,
                show_network: several_networks,
                on_pick: move |choice| from_pick.set(Some(choice)),
                input {
                    class: "amount",
                    aria_label: "Amount to swap",
                    inputmode: "decimal",
                    value: "{amount}",
                    autocomplete: "off",
                    oninput: move |e| amount.set(e.value()),
                }
            }
            BalanceLine { held: from_held, unlocked, known, native_hint: true }
        }
        button { class: "swap-arrow", aria_label: "Reverse swap", onclick: reverse, "⇅" }
        div { class: "swapbox",
            p { class: "sub", "You receive, estimated" }
            TokenPicker {
                net,
                chain_id: evm_chain,
                held: held_here,
                exclude: from_asset.clone(),
                selected: receive.clone(),
                show_network: several_networks,
                on_pick: move |choice| picked.set(Some(choice)),
                span { class: "quote-note", if receive.is_some() { "Shown when you review" } else { "Pick a token" } }
            }
            BalanceLine { held: to_held, unlocked, known, native_hint: false }
            span { class: "sub", "Slippage tolerance {slippage}%" }
        }
        Notice { "♧  You’ll see the route, fees and minimum received before approving. Swaps stay on the same network." }
        button { class: "primary", onclick: review, "Review swap " span { class: "arrow", "→" } }
    }
}

/// "Balance: 2.45 ETH", or how to see it when it is not on screen.
#[component]
fn BalanceLine(held: Option<Asset>, unlocked: bool, known: bool, native_hint: bool) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        if let Some(held) = held {
            div { class: "balance-line",
                span {
                    "Balance: "
                    strong { "{trim_amount(&held.balance, 8)} {held.symbol}" }
                }
                if native_hint && held.is_native() {
                    span { class: "sub", "keep a little for fees" }
                }
            }
        } else if unlocked {
            div { class: "balance-line", span { "No balance found for this token" } }
        } else if known {
            div { class: "balance-line",
                span { "Balances are hidden" }
                button { class: "text-btn", onclick: move |_| state.set_hidden(false), "Show" }
            }
        } else {
            div { class: "balance-line",
                span { "Balance hidden" }
                button { class: "text-btn", onclick: move |_| state.open(Sheet::Unlock), "Unlock to see it" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(symbol: &str, id: &str) -> Asset {
        Asset {
            symbol: symbol.to_string(),
            name: symbol.to_string(),
            asset: id.to_string(),
            balance: "1".to_string(),
            usd: None,
            chain_id: None,
            network: String::new(),
        }
    }

    #[test]
    fn receives_another_held_token_by_default() {
        let held = vec![asset("SOL", "native"), asset("USDC", USDC_SOLANA)];
        let pick = default_receive(Blockchain::Solana, None, &held, "native").unwrap();
        assert_eq!(pick.symbol, "USDC");
        let back = default_receive(Blockchain::Solana, None, &held, USDC_SOLANA).unwrap();
        assert_eq!(back.token, "native");
    }

    #[test]
    fn falls_back_to_usdc_only_where_its_address_is_known() {
        let only_native = vec![asset("SUI", "native")];
        let sui = default_receive(Blockchain::Sui, None, &only_native, "native").unwrap();
        assert!(sui.token.ends_with("::usdc::USDC"));
        assert!(default_receive(Blockchain::Ethereum, Some(1), &[], "native").is_some());
        assert!(default_receive(Blockchain::Ethereum, Some(8453), &[], "native").is_none());
        assert!(default_receive(Blockchain::Solana, None, &[], USDC_SOLANA).is_none());
        assert!(default_receive(Blockchain::Bitcoin, None, &[], "native").is_none());
    }
}
