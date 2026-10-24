//! WalletConnect for Sui wallets: connecting a dapp, then reviewing and answering its
//! `sui_*` requests. Like the Ethereum side, every action unlocks the wallet for that call only.

use super::sui::Session;
use super::*;
use crate::sui::tx;
use crate::sui::wallet::SuiVault;
use crate::walletconnect::manager::{PendingRequest, SessionTerms};
use crate::walletconnect::namespaces;
use crate::walletconnect::requests::{self, Action, SuiKind, SuiRequest};
use crate::wc_service::names_match;

struct OpenSui {
    record_name: String,
    vault: SuiVault,
    request: PendingRequest,
    sui: SuiRequest,
    dapp: crate::walletconnect::namespaces::DappMetadata,
    chain: &'static str,
}

fn public_key_bytes(vault: &SuiVault) -> Result<[u8; 32], String> {
    let key = vault.signer()?;
    key.public_key()
        .as_bytes()
        .try_into()
        .map_err(|_| "unexpected public key length".to_string())
}

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::str(s)).collect())
}

impl WalletService {
    /// Connects a dapp that asked for Sui. The wallet's own network is the only chain granted.
    pub(crate) fn wc_approve_session_sui(
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
        let (record, vault) = self.unlock_sui(wallet, password)?;
        let chain = vault.network.chain_name();
        let approval = namespaces::plan_sui_approval(&data, chain)?;
        let address = vault.address()?.to_string();
        let accounts = namespaces::sui_accounts_value(&public_key_bytes(&vault)?, &address);
        let terms = SessionTerms {
            namespaces: namespaces::sui_settle_namespaces(&approval, &address),
            properties: Some(Value::object(vec![(
                "sui_getAccounts",
                Value::String(accounts.to_json()),
            )])),
            chains: vec![],
            sui_chain: Some(approval.chain.clone()),
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
            ("dapp", data.dapp.to_value()),
            ("chains", strings(&[approval.chain])),
            ("methods", strings(&approval.methods)),
            ("confirmed_by_dapp", Value::Bool(approved.acknowledged)),
            ("note", Value::str(note)),
        ]))
    }

    fn wc_open_sui(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<OpenSui, String> {
        let (request, session) = self.wc_clone_request(request_id)?;
        let Action::Sui(sui) = &request.action else {
            return Err(
                "this is an Ethereum request; use an Ethereum wallet and give its chain_id"
                    .to_string(),
            );
        };
        let sui = sui.clone();
        if !names_match(wallet, &session.wallet) {
            return Err(format!(
                "this request belongs to a session connected with wallet '{}', not '{}'",
                session.wallet,
                crate::eth::explorer::sanitize_text_max(wallet, 60)
            ));
        }
        let (record, vault) = self.unlock_sui(wallet, password)?;
        let chain = vault.network.chain_name();
        if request.sui_chain.as_deref() != Some(chain) {
            return Err(format!(
                "this request is for {}, but this wallet is on {chain}",
                request.sui_chain.as_deref().unwrap_or("another chain")
            ));
        }
        let owner = vault.address()?;
        if let Some(asked) = sui.address
            && asked != owner
        {
            return Err(format!(
                "the dapp asked {asked} to sign, but this wallet's address is {owner}; refusing"
            ));
        }
        Ok(OpenSui {
            record_name: record.name,
            vault,
            request,
            sui,
            dapp: session.dapp,
            chain,
        })
    }

    /// The transaction to sign. A dapp's serialized JSON form has its objects resolved from the
    /// network and is given gas here; the gas budget comes from a dry run.
    fn wc_sui_transaction(
        s: &Session<'_>,
        sui: &SuiRequest,
    ) -> Result<sui_sdk_types::Transaction, String> {
        if let Some(transaction) = &sui.transaction {
            return Ok((**transaction).clone());
        }
        let draft = sui
            .draft
            .as_deref()
            .ok_or_else(|| "the request has no transaction".to_string())?;
        if draft.sender != s.owner {
            return Err(
                "the transaction's sender is not this wallet; refusing to sign it".to_string(),
            );
        }
        let (inputs, commands) = draft.resolve(|id| {
            s.gql
                .object(id)?
                .ok_or_else(|| format!("the transaction uses object {id}, which does not exist"))
        })?;
        let epoch = s.gql.epoch()?;
        let env = tx::GasEnv {
            price: epoch.reference_gas_price,
            epoch: epoch.epoch,
            chain: s
                .chain
                .parse::<sui_sdk_types::Digest>()
                .map_err(|_| "the Sui node reported an invalid chain identifier".to_string())?,
            nonce: tx::fresh_nonce()?,
        };
        let holdings = Self::sui_holdings(s, crate::sui::coins::SUI_COIN_TYPE)?;
        // A transaction that uses the gas coin may spend any of it, so every coin is offered.
        let also_needed = if draft.uses_gas_coin() { u64::MAX } else { 0 };
        let build = |budget: u64| -> Result<sui_sdk_types::Transaction, String> {
            let gas = tx::select_gas(&holdings, budget, also_needed)?;
            Ok(tx::wrap_commands(
                s.owner,
                &env,
                budget,
                &gas,
                inputs.clone(),
                commands.clone(),
            ))
        };
        // Reserving a huge budget can leave too little in the gas coin for what the transaction
        // spends, so smaller trial budgets are tried too.
        let ceiling = tx::max_budget(&holdings);
        let mut failure = "not enough SUI to pay for gas".to_string();
        for trial in [1_000_000_000u64, 100_000_000, 20_000_000] {
            let trial = trial.min(ceiling);
            if trial < env.price.saturating_mul(2000) {
                continue;
            }
            let dry = s.gql.simulate(&tx::encode(&build(trial)?)?)?;
            if dry.success {
                let budget = tx::budget_from_simulation(&dry.gas, env.price).min(ceiling);
                return build(budget);
            }
            failure = format!(
                "the transaction would fail: {}",
                dry.error.as_deref().unwrap_or("unknown reason")
            );
        }
        Err(failure)
    }

    pub(crate) fn wc_review_request_sui(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_sui(request_id, wallet, password)?;
        let kind = match open.sui.kind {
            SuiKind::GetAccounts => "accounts",
            SuiKind::SignPersonalMessage => "message",
            SuiKind::SignTransaction | SuiKind::SignAndExecute => "transaction",
        };
        let mut pairs = vec![
            ("request_id", Value::String(request_id.to_string())),
            ("wallet", Value::str(&open.record_name)),
            ("from", Value::String(open.vault.address()?.to_string())),
            ("dapp", open.dapp.to_value()),
            ("method", Value::str(&open.request.method)),
            ("kind", Value::str(kind)),
            ("chain", Value::str(open.chain)),
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
        match open.sui.kind {
            SuiKind::GetAccounts => pairs.push((
                "note",
                Value::str(
                    "Approving tells the dapp this wallet's address and public key. Nothing is signed.",
                ),
            )),
            SuiKind::SignPersonalMessage => {
                pairs.push(("details", requests::describe_message(&open.sui.message)));
                pairs.push((
                    "note",
                    Value::str(
                        "Approving signs this message off-chain (taken as UTF-8 text) with Sui's personal-message intent; nothing is sent to the network.",
                    ),
                ));
            }
            SuiKind::SignTransaction | SuiKind::SignAndExecute => {
                let s = self.sui_session(&open.vault)?;
                let transaction = Self::wc_sui_transaction(&s, &open.sui)?;
                let transaction = &transaction;
                tx::check_signable(transaction, &s.owner)?;
                Self::sui_check_chain(transaction, &s)?;
                let (summary, mut warnings) = tx::describe(transaction, &s.owner);
                let dry = s.gql.simulate(&tx::encode(transaction)?);
                let (succeeds, report) = match &dry {
                    Ok(effects) => {
                        if !effects.success {
                            warnings.push(format!(
                                "a dry run of this transaction fails: {}",
                                effects.error.as_deref().unwrap_or("unknown reason")
                            ));
                        }
                        (effects.success, Self::sui_effects_json(&s, effects))
                    }
                    Err(e) => {
                        warnings.push(format!("the dry run could not be completed: {e}"));
                        (
                            false,
                            Value::object(vec![
                                ("status", Value::str("unavailable")),
                                ("error", Value::str(e)),
                            ]),
                        )
                    }
                };
                let broadcast = open.sui.kind == SuiKind::SignAndExecute;
                pairs.push(("will_broadcast", Value::Bool(broadcast)));
                pairs.push(("simulation_succeeded", Value::Bool(succeeds)));
                pairs.push((
                    "transaction_digest",
                    Value::String(tx::digest_of(transaction)),
                ));
                pairs.push(("description", summary));
                pairs.push((
                    "warnings",
                    Value::Array(warnings.into_iter().map(Value::String).collect()),
                ));
                pairs.push(("simulation", report));
            }
        }
        Ok(Value::object(pairs))
    }

    pub(crate) fn wc_approve_request_sui(
        &self,
        request_id: u64,
        wallet: &str,
        password: &str,
    ) -> Result<Value, String> {
        let open = self.wc_open_sui(request_id, wallet, password)?;
        let (answer, mut report) = match open.sui.kind {
            SuiKind::GetAccounts => {
                let address = open.vault.address()?.to_string();
                let accounts =
                    namespaces::sui_accounts_value(&public_key_bytes(&open.vault)?, &address);
                (
                    accounts,
                    Value::object(vec![("address", Value::String(address))]),
                )
            }
            SuiKind::SignPersonalMessage => {
                let signature = {
                    let key = open.vault.signer()?;
                    tx::sign_personal_message(&key, &open.sui.message)?
                };
                (
                    Value::object(vec![("signature", Value::str(&signature))]),
                    Value::object(vec![("signature", Value::String(signature))]),
                )
            }
            SuiKind::SignTransaction | SuiKind::SignAndExecute => {
                let s: Session<'_> = self.sui_session(&open.vault)?;
                let transaction = Self::wc_sui_transaction(&s, &open.sui)?;
                let transaction = &transaction;
                tx::check_signable(transaction, &s.owner)?;
                Self::sui_check_chain(transaction, &s)?;
                if open.sui.kind == SuiKind::SignTransaction {
                    let signature = {
                        let key = open.vault.signer()?;
                        tx::sign_transaction(&key, transaction)?
                    };
                    let bytes = tx::encode(transaction)?;
                    (
                        Value::object(vec![
                            ("signature", Value::str(&signature)),
                            ("transactionBytes", Value::str(&bytes)),
                        ]),
                        Value::object(vec![
                            ("signature", Value::String(signature)),
                            ("transaction_bytes", Value::String(bytes)),
                            (
                                "transaction_digest",
                                Value::String(tx::digest_of(transaction)),
                            ),
                        ]),
                    )
                } else {
                    let dry = s.gql.simulate(&tx::encode(transaction)?);
                    match &dry {
                        Ok(effects) if effects.success => {}
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
                    let submitted = match Self::sui_submit(&s, &open.vault, transaction) {
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
                        Value::object(vec![("digest", Value::str(&submitted.digest))]),
                        Self::sui_submission_report(&s, &submitted),
                    )
                }
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
