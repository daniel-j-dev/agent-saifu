//! WalletConnect for Solana wallets: connecting a dapp, then reviewing and answering its
//! `solana_*` requests. Like the other blockchains, every action unlocks the wallet for that
//! call only.

use super::solana::Session;
use super::*;
use crate::solana::address::{self as saddr};
use crate::solana::tx::Transaction;
use crate::walletconnect::manager::{PendingRequest, SessionTerms};
use crate::walletconnect::namespaces::{self, DappMetadata};
use crate::walletconnect::requests::{self, Action, SolanaKind, SolanaRequest};
use crate::wc_service::names_match;

struct OpenSolana {
    record_name: String,
    vault: SolanaVault,
    request: PendingRequest,
    solana: SolanaRequest,
    dapp: DappMetadata,
    chain: String,
}

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::str(s)).collect())
}

fn kind_name(kind: SolanaKind) -> &'static str {
    match kind {
        SolanaKind::GetAccounts => "accounts",
        SolanaKind::SignMessage => "message",
        SolanaKind::SignTransaction | SolanaKind::SignAndSend | SolanaKind::SignAll => {
            "transaction"
        }
    }
}

impl WalletService {
    /// Connects a dapp that asked for Solana. The wallet's own cluster is the only one granted.
    pub(crate) fn wc_approve_session_solana(
        &self,
        proposal_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<Value, String> {
        let data = {
            let mut wc = self.wc.borrow_mut();
            wc.poll();
            wc.proposal(proposal_id)
                .ok_or("no such proposal (it may have expired); ask the dapp to connect again")?
                .data
                .clone()
        };
        let (record, vault) = self.unlock_solana(wallet, password)?;
        let approval = namespaces::plan_solana_approval(&data, vault.network)?;
        let address = vault.address()?;
        let terms = SessionTerms {
            namespaces: namespaces::solana_settle_namespaces(&approval, &address),
            properties: None,
            chains: vec![],
            sui_chain: None,
            solana_chains: approval.chains.clone(),
            methods: approval.methods.clone(),
        };
        let approved =
            self.wc
                .borrow_mut()
                .approve_session_with(proposal_id, &record.name, terms)?;
        let note = if approved.acknowledged {
            "The dapp confirmed the session."
        } else {
            "The session was sent to the dapp but it has not confirmed yet; check wc_list_sessions."
        };
        Ok(Value::object(vec![
            ("session_topic", Value::String(approved.topic)),
            ("wallet", Value::String(record.name)),
            ("network", Value::str(vault.network.id())),
            ("dapp", data.dapp.to_value()),
            ("chains", strings(&approval.chains)),
            ("methods", strings(&approval.methods)),
            ("confirmed_by_dapp", Value::Bool(approved.acknowledged)),
            ("note", Value::str(note)),
        ]))
    }

    fn wc_open_solana(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<OpenSolana, String> {
        let (request, session) = self.wc_clone_request(request_id)?;
        let Action::Solana(solana) = &request.action else {
            return Err(
                "this is not a Solana request; use a wallet on the blockchain the request is for (an Ethereum wallet also needs its chain_id)"
                    .to_string(),
            );
        };
        let solana = solana.clone();
        if !names_match(wallet, &session.wallet) {
            return Err(format!(
                "this request belongs to a session connected with wallet '{}', not '{}'",
                session.wallet,
                crate::eth::explorer::sanitize_text_max(wallet, 60)
            ));
        }
        let (record, vault) = self.unlock_solana(wallet, password)?;
        let chain = request.solana_chain.clone().unwrap_or_default();
        if !vault.network.matches_chain(&chain) {
            return Err(format!(
                "this request is for {}, but this wallet is on {}",
                crate::eth::explorer::sanitize_text_max(&chain, 50),
                vault.network.chain_name()
            ));
        }
        let owner = vault.address()?;
        if let Some(asked) = solana.pubkey
            && saddr::to_text(&asked) != owner
        {
            return Err(format!(
                "the dapp asked {} to sign, but this wallet's address is {owner}; refusing",
                saddr::to_text(&asked)
            ));
        }
        Ok(OpenSolana {
            record_name: record.name,
            vault,
            request,
            solana,
            dapp: session.dapp,
            chain,
        })
    }

    /// Checks every transaction of a request can be signed by this wallet.
    fn wc_solana_check_all(s: &Session<'_>, solana: &SolanaRequest) -> Result<(), String> {
        for transaction in &solana.transactions {
            let accounts =
                crate::solana::effects::resolve_message_accounts(&s.rpc, &transaction.message)
                    .unwrap_or_else(|_| transaction.message.account_keys.clone());
            Self::solana_check_signable(s, transaction, &accounts)?;
        }
        Ok(())
    }

    pub(crate) fn wc_review_request_solana(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_solana(request_id, wallet, password)?;
        let mut pairs = vec![
            ("request_id", Value::String(request_id.to_string())),
            ("wallet", Value::str(&open.record_name)),
            ("from", Value::String(open.vault.address()?)),
            ("dapp", open.dapp.to_value()),
            ("method", Value::str(&open.request.method)),
            ("kind", Value::str(kind_name(open.solana.kind))),
            ("chain", Value::str(&open.chain)),
            ("network", Value::str(open.vault.network.id())),
            (
                "expires_in_seconds",
                Value::Number(
                    open.request
                        .expires_at
                        .saturating_sub(crate::walletconnect::now_secs())
                        .to_string(),
                ),
            ),
        ];
        match open.solana.kind {
            SolanaKind::GetAccounts => pairs.push((
                "note",
                Value::str("Approving tells the dapp this wallet's address. Nothing is signed."),
            )),
            SolanaKind::SignMessage => {
                Self::solana_check_message_signable(&open.solana.message)?;
                pairs.push(("details", requests::describe_message(&open.solana.message)));
                pairs.push((
                    "note",
                    Value::str(
                        "Approving signs these message bytes off-chain with the wallet's Ed25519 key; nothing is sent to the network.",
                    ),
                ));
            }
            SolanaKind::SignTransaction | SolanaKind::SignAndSend | SolanaKind::SignAll => {
                let s = self.solana_session(&open.vault)?;
                Self::wc_solana_check_all(&s, &open.solana)?;
                let mut all_succeed = true;
                let mut reports = vec![];
                for (index, transaction) in open.solana.transactions.iter().enumerate() {
                    let (described, dry, warnings) = Self::solana_inspect(&s, transaction);
                    let (succeeds, simulation) = match &dry {
                        Ok(effects) => {
                            (effects.succeeded(), Self::solana_effects_json(&s, effects))
                        }
                        Err(e) => (
                            false,
                            Value::object(vec![
                                ("status", Value::str("unavailable")),
                                ("error", Value::str(e)),
                            ]),
                        ),
                    };
                    all_succeed &= succeeds;
                    reports.push(Value::object(vec![
                        ("index", num(index as u64)),
                        ("description", Self::solana_description_json(&described)),
                        (
                            "warnings",
                            Value::Array(warnings.into_iter().map(Value::String).collect()),
                        ),
                        ("simulation_succeeded", Value::Bool(succeeds)),
                        ("simulation", simulation),
                    ]));
                }
                pairs.push((
                    "will_broadcast",
                    Value::Bool(open.solana.kind == SolanaKind::SignAndSend),
                ));
                pairs.push(("simulation_succeeded", Value::Bool(all_succeed)));
                pairs.push(("transactions", Value::Array(reports)));
            }
        }
        Ok(Value::object(pairs))
    }

    pub(crate) fn wc_approve_request_solana(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_solana(request_id, wallet, password)?;
        let (answer, mut report) = match open.solana.kind {
            SolanaKind::GetAccounts => {
                let address = open.vault.address()?;
                (
                    namespaces::solana_accounts_value(&address),
                    Value::object(vec![("address", Value::String(address))]),
                )
            }
            SolanaKind::SignMessage => {
                Self::solana_check_message_signable(&open.solana.message)?;
                let signature =
                    crate::crypto::base58::encode(&open.vault.sign(&open.solana.message)?);
                (
                    Value::object(vec![("signature", Value::str(&signature))]),
                    Value::object(vec![("signature", Value::String(signature))]),
                )
            }
            SolanaKind::SignTransaction | SolanaKind::SignAll => {
                let s = self.solana_session(&open.vault)?;
                Self::wc_solana_check_all(&s, &open.solana)?;
                let mut signed: Vec<Transaction> = vec![];
                let mut signatures: Vec<String> = vec![];
                for transaction in &open.solana.transactions {
                    let mut transaction = transaction.clone();
                    let signature = transaction.sign(&open.vault)?;
                    signatures.push(crate::crypto::base58::encode(&signature));
                    signed.push(transaction);
                }
                let encoded: Vec<String> = signed.iter().map(Transaction::to_base64).collect();
                if open.solana.kind == SolanaKind::SignAll {
                    (
                        Value::object(vec![("transactions", strings(&encoded))]),
                        Value::object(vec![
                            ("signatures", strings(&signatures)),
                            ("transactions", strings(&encoded)),
                        ]),
                    )
                } else {
                    (
                        Value::object(vec![
                            ("signature", Value::str(&signatures[0])),
                            ("transaction", Value::str(&encoded[0])),
                        ]),
                        Value::object(vec![
                            ("signature", Value::String(signatures[0].clone())),
                            ("transaction", Value::String(encoded[0].clone())),
                        ]),
                    )
                }
            }
            SolanaKind::SignAndSend => {
                let s = self.solana_session(&open.vault)?;
                Self::wc_solana_check_all(&s, &open.solana)?;
                let mut transaction = open
                    .solana
                    .transactions
                    .first()
                    .cloned()
                    .ok_or("the request has no transaction")?;
                let (_, dry, _) = Self::solana_inspect(&s, &transaction);
                match &dry {
                    Ok(effects) if effects.succeeded() => {}
                    Ok(effects) => {
                        return Err(format!(
                            "not sending: the dry run of this transaction fails ({})",
                            effects.error.as_deref().unwrap_or("unknown reason")
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "not sending: the dry run could not be completed ({e})"
                        ));
                    }
                }
                // Once submission starts the request is closed whatever happens, so it can never be
                // approved (and submitted) a second time.
                let submitted = match self.solana_submit(&s, &open.vault, &mut transaction) {
                    Ok(submitted) => submitted,
                    Err(e) => {
                        let _ = self.wc.borrow_mut().answer_request(
                            request_id,
                            Err((
                                crate::walletconnect::proto::USER_REJECTED,
                                "The transaction could not be confirmed".to_string(),
                            )),
                        );
                        return Err(format!("{e} (the dapp request has been closed)"));
                    }
                };
                (
                    Value::object(vec![("signature", Value::str(&submitted.signature))]),
                    Self::solana_submission_report(&s, &submitted),
                )
            }
        };
        let notified = self
            .wc
            .borrow_mut()
            .answer_request(request_id, Ok(answer))?;
        if let Value::Object(pairs) = &mut report {
            pairs.insert(0, ("status".into(), Value::str("approved")));
            pairs.insert(
                1,
                ("request_id".into(), Value::String(request_id.to_string())),
            );
            pairs.insert(2, ("method".into(), Value::str(&open.request.method)));
            pairs.insert(3, ("wallet".into(), Value::str(&open.record_name)));
            pairs.insert(4, ("dapp_notified".into(), Value::Bool(notified)));
            if !notified {
                pairs.push((
                    "note".into(),
                    Value::str(
                        "The dapp could not be reached, so it has not received the answer; the request is closed here.",
                    ),
                ));
            }
        }
        Ok(report)
    }
}
