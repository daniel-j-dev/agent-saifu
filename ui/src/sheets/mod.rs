//! Every sheet (bottom sheet / card) the app can show over a page.

mod connect;
mod money;
mod network;
mod secrets;
mod tools;
mod wallets;

use dioxus::prelude::*;

use crate::state::{AppState, Sheet};

#[component]
pub fn SheetHost() -> Element {
    let state = use_context::<AppState>();
    let Some(sheet) = state.sheet.read().clone() else {
        return rsx! {};
    };
    // Each variant gets its own key so switching sheets never reuses another sheet's inputs.
    let key = format!("{sheet:?}");
    rsx! {
        div { key: "{key}",
            match sheet {
                Sheet::Wallets => rsx! { wallets::WalletsSheet {} },
                Sheet::Create => rsx! { wallets::CreateSheet { import: false } },
                Sheet::Import => rsx! { wallets::CreateSheet { import: true } },
                Sheet::Rename => rsx! { wallets::RenameSheet {} },
                Sheet::Delete => rsx! { wallets::DeleteSheet {} },
                Sheet::Unlock => rsx! { wallets::UnlockSheet {} },
                Sheet::Send(asset) => rsx! { money::SendSheet { initial: asset } },
                Sheet::Receive => rsx! { money::ReceiveSheet {} },
                Sheet::Asset(asset) => rsx! { money::AssetSheet { asset } },
                Sheet::SwapReview(draft) => rsx! { money::SwapReviewSheet { draft } },
                Sheet::Slippage => rsx! { money::SlippageSheet {} },
                Sheet::Security => rsx! { secrets::SecuritySheet {} },
                Sheet::SeedPhrase => rsx! { secrets::SeedPhraseSheet {} },
                Sheet::PrivateKey => rsx! { secrets::PrivateKeySheet {} },
                Sheet::Export => rsx! { secrets::ExportSheet {} },
                Sheet::Advanced => rsx! { tools::AdvancedSheet {} },
                Sheet::SignMessage => rsx! { tools::SignMessageSheet {} },
                Sheet::Contract => rsx! { tools::ContractSheet {} },
                Sheet::ReadContract => rsx! { tools::ReadContractSheet {} },
                Sheet::Lookup => rsx! { tools::LookupSheet {} },
                Sheet::About => rsx! { tools::AboutSheet {} },
                Sheet::Networks => rsx! { network::NetworksSheet {} },
                Sheet::Tokens => rsx! { network::TokensSheet {} },
                Sheet::Approvals => rsx! { network::ApprovalsSheet {} },
                Sheet::Connect => rsx! { connect::ConnectSheet {} },
                Sheet::Sessions => rsx! { connect::SessionsSheet {} },
                Sheet::Session(topic) => rsx! { connect::SessionSheet { topic } },
                Sheet::Proposal(id) => rsx! { connect::ProposalSheet { id } },
            }
        }
    }
}
