//! Solana swaps through Jupiter, against a scripted node and a scripted Jupiter.

use super::*;
use crate::solana::swap::SolanaSwapFee;
use crate::solana::swap::SolanaSwapSettings;
use crate::solana::swap::{self as ssw};

const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
const BUILD: &str = "/swap/v2/build";

fn usdc_account() -> Pubkey {
    saddr::associated_token_address(&owner(), &usdc(), &token_program()).unwrap()
}

/// The fee recipient used by the fee tests, and its (unopened) USDC account.
fn fee_recipient() -> Pubkey {
    key(5)
}

fn fee_account() -> Pubkey {
    saddr::associated_token_address(&fee_recipient(), &usdc(), &token_program()).unwrap()
}

fn account_json(pubkey: &Pubkey, signer: bool, writable: bool) -> String {
    format!(
        r#"{{"pubkey":"{}","isSigner":{signer},"isWritable":{writable}}}"#,
        saddr::to_text(pubkey)
    )
}

fn instruction_json(program: &Pubkey, accounts: &[(Pubkey, bool, bool)], data: &[u8]) -> String {
    let accounts: Vec<String> = accounts
        .iter()
        .map(|(k, signer, writable)| account_json(k, *signer, *writable))
        .collect();
    format!(
        r#"{{"programId":"{}","accounts":[{}],"data":"{}"}}"#,
        saddr::to_text(program),
        accounts.join(","),
        base64::encode(data)
    )
}

/// Jupiter's swap instruction: a `route_v2` whose terms match `amounts` (sold, quoted out) at 1%
/// slippage, laid out as the program expects, delivering to the wallet's account for `out_mint`.
/// `extra` accounts follow the route's own.
fn route_instruction(
    in_mint: &str,
    out_mint: &str,
    amounts: (u64, u64, u64),
    fee_bps: u16,
    extra: &[(Pubkey, bool, bool)],
) -> String {
    let (in_mint, out_mint) = (
        saddr::parse(in_mint).unwrap(),
        saddr::parse(out_mint).unwrap(),
    );
    let account_for =
        |mint: &Pubkey| saddr::associated_token_address(&owner(), mint, &token_program()).unwrap();
    let jupiter = saddr::id(ssw::JUPITER_PROGRAM);
    let mut accounts = vec![
        (owner(), true, true),
        (account_for(&in_mint), false, false),
        (account_for(&out_mint), false, true),
        (in_mint, false, false),
        (out_mint, false, false),
        (token_program(), false, false),
        (token_program(), false, false),
        (jupiter, false, false),
        (key(98), false, false),
        (jupiter, false, false),
    ];
    accounts.extend_from_slice(extra);
    instruction_json(
        &jupiter,
        &accounts,
        &route_data(amounts.0, amounts.1, fee_bps, 9),
    )
}

/// `route_v2` data at 1% slippage, with a route plan of `plan_bytes` opaque bytes.
fn route_data(in_amount: u64, quoted_out: u64, fee_bps: u16, plan_bytes: usize) -> Vec<u8> {
    let mut data = vec![187, 100, 250, 204, 49, 196, 175, 20];
    data.extend_from_slice(&in_amount.to_le_bytes());
    data.extend_from_slice(&quoted_out.to_le_bytes());
    data.extend_from_slice(&100u16.to_le_bytes());
    data.extend_from_slice(&fee_bps.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend(std::iter::repeat_n(1u8, plan_bytes));
    data
}

/// SOL -> USDC with the default amounts and extra accounts on the swap instruction.
fn sol_to_usdc_with(extra: &[(Pubkey, bool, bool)]) -> String {
    let amounts = (100_000_000, 14_000_000, 13_860_000);
    body_with(
        SOL_MINT,
        USDC,
        amounts,
        &route_instruction(SOL_MINT, USDC, amounts, 0, extra),
        "{}",
    )
}

fn body_with(
    in_mint: &str,
    out_mint: &str,
    amounts: (u64, u64, u64),
    swap: &str,
    tables: &str,
) -> String {
    let mut price = vec![3u8];
    price.extend_from_slice(&7_000u64.to_le_bytes());
    let budget = instruction_json(&saddr::id(saddr::COMPUTE_BUDGET_PROGRAM), &[], &price);
    format!(
        r#"{{"inputMint":"{in_mint}","outputMint":"{out_mint}","inAmount":"{}","outAmount":"{}","otherAmountThreshold":"{}","swapMode":"ExactIn","slippageBps":100,"priceImpactPct":"-0.0012","routePlan":[{{"percent":100,"swapInfo":{{"label":"Raydium"}}}}],"computeBudgetInstructions":[{budget}],"setupInstructions":[],"swapInstruction":{swap},"cleanupInstruction":null,"otherInstructions":[],"tipInstruction":null,"addressesByLookupTableAddress":{tables},"blockhashWithMetadata":{{"blockhash":[1],"lastValidBlockHeight":5}}}}"#,
        amounts.0, amounts.1, amounts.2
    )
}

fn plain_body(in_mint: &str, out_mint: &str, amounts: (u64, u64, u64)) -> String {
    body_with(
        in_mint,
        out_mint,
        amounts,
        &route_instruction(in_mint, out_mint, amounts, 0, &[]),
        "{}",
    )
}

/// SOL -> USDC: sells 0.1 SOL, quoted 14 USDC, at least 13.86.
fn sol_to_usdc() -> String {
    plain_body(SOL_MINT, USDC, (100_000_000, 14_000_000, 13_860_000))
}

fn swap_world() -> Shared {
    let w = world();
    {
        let mut state = w.borrow_mut();
        state
            .accounts
            .insert(usdc(), mint_acct(6, &token_program()));
        state
            .accounts
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 0));
        // the swap: the wallet pays 0.1 SOL plus the fee and receives 14 USDC
        state
            .post
            .insert(owner(), system_account(5_000_000_000 - 100_000_000 - 5_000));
        state
            .post
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 14_000_000));
    }
    w
}

fn swap_fx(body: String) -> Fx {
    swap_fx_in(swap_world(), body, |s| s)
}

fn swap_fx_in(
    w: Shared,
    body: String,
    configure: impl FnOnce(WalletService) -> WalletService,
) -> Fx {
    let f = build_with(w, SendMode::Accept, MAINNET_GENESIS, configure, move |m| {
        m.get(BUILD, Ok(body))
    });
    import(&f);
    f
}

fn fee_fx(body: String, existing_fee_account: bool) -> Fx {
    let w = swap_world();
    if existing_fee_account {
        w.borrow_mut()
            .accounts
            .insert(fee_account(), token_acct(&usdc(), &fee_recipient(), 0));
    }
    let fee = SolanaSwapFee::from_parts(&key_text(5), 70).unwrap();
    swap_fx_in(w, body, move |s| {
        s.with_solana_swap_settings(SolanaSwapSettings::new(fee))
    })
}

fn fee_body() -> String {
    // 13.9 USDC after the 0.7% fee; the program enforces 1% less, 13.761
    let amounts = (100_000_000, 13_900_000, 13_761_000);
    body_with(
        SOL_MINT,
        USDC,
        amounts,
        &route_instruction(SOL_MINT, USDC, amounts, 70, &[(fee_account(), false, true)]),
        "{}",
    )
}

fn params() -> SwapParams {
    SwapParams {
        from_token: "native".to_string(),
        to_token: USDC.to_string(),
        amount: "0.1".to_string(),
        ..SwapParams::default()
    }
}

fn quote_of(f: &Fx, p: &SwapParams) -> Result<Value, String> {
    f.service.solana_swap_quote("w", PW, p)
}

fn swap_of(f: &Fx, p: &SwapParams) -> Result<Value, String> {
    f.service.solana_swap_tokens("w", PW, p)
}

fn build_url_of(f: &Fx) -> String {
    f.mock
        .get_urls
        .borrow()
        .iter()
        .find(|u| u.contains(BUILD))
        .cloned()
        .unwrap()
}

fn programs_of(t: &Transaction) -> Vec<Pubkey> {
    t.message
        .instructions
        .iter()
        .map(|i| t.message.account_keys[usize::from(i.program_id_index)])
        .collect()
}

#[test]
fn a_quote_reports_amounts_fees_and_the_simulated_outcome() {
    let f = swap_fx(sol_to_usdc());
    let v = quote_of(&f, &params()).unwrap();
    assert_eq!(text(&v, &["aggregator"]), "Jupiter");
    assert_eq!(text(&v, &["from", "token"]), "native");
    assert_eq!(text(&v, &["from", "symbol"]), "SOL");
    assert_eq!(text(&v, &["from", "amount"]), "0.1");
    assert_eq!(text(&v, &["to", "token"]), USDC);
    assert_eq!(text(&v, &["to", "expected_amount"]), "14");
    assert_eq!(text(&v, &["to", "minimum_amount"]), "13.86");
    assert_eq!(text(&v, &["to", "recipient"]), owner_text());
    assert_eq!(text(&v, &["price_impact_percent"]), "0.12");
    assert_eq!(items(&v, &["route"])[0].as_str(), Some("Raydium"));
    assert_eq!(
        field(&v, &["requirements", "sufficient_balance"]),
        &Value::Bool(true)
    );
    assert_eq!(
        field(&v, &["verification", "dry_run", "simulated"]),
        &Value::Bool(true)
    );
    assert_eq!(
        text(&v, &["verification", "dry_run", "wallet_spends"]),
        "0.100005"
    );
    assert_eq!(
        text(&v, &["verification", "dry_run", "wallet_receives"]),
        "14"
    );
    assert_eq!(text(&v, &["fees", "network_fee", "currency"]), "SOL");
    assert_eq!(field(&v, &["fees", "service_fee"]), &Value::Null);
    assert_eq!(f.mock.count("sendTransaction"), 0);
    // the request asked Jupiter for exactly this swap, with no key and no fee
    let url = build_url_of(&f);
    assert!(
        url.starts_with("https://api.jup.ag/swap/v2/build?"),
        "{url}"
    );
    assert!(url.contains(&format!("inputMint={SOL_MINT}")), "{url}");
    assert!(url.contains(&format!("outputMint={USDC}")));
    assert!(url.contains("amount=100000000"));
    assert!(url.contains("slippageBps=100"));
    assert!(url.contains(&format!("taker={}", owner_text())));
    assert!(!url.contains("platformFeeBps") && !url.contains("feeAccount"));
}

#[test]
fn swapping_assembles_signs_and_submits_the_checked_transaction() {
    let f = swap_fx(sol_to_usdc());
    let v = swap_of(&f, &params()).unwrap();
    assert_eq!(text(&v, &["status"]), "confirmed");
    assert_eq!(text(&v, &["wallet"]), "w");
    assert_eq!(text(&v, &["swap", "to", "minimum_amount"]), "13.86");
    let sent = submitted(&f);
    assert_signed_by_wallet(&sent);
    assert_eq!(text(&v, &["transaction_signature"]), sent.id().unwrap());
    // the wallet's own compute budget (sized by the dry run) comes first, then Jupiter's swap
    assert_eq!(
        programs_of(&sent),
        [
            saddr::id(saddr::COMPUTE_BUDGET_PROGRAM),
            saddr::id(saddr::COMPUTE_BUDGET_PROGRAM),
            saddr::id(ssw::JUPITER_PROGRAM)
        ]
    );
    let (limit, price) = tx::compute_budget(&sent.message, &sent.message.account_keys);
    assert_eq!(limit, Some(150 + 300 + 30 + 1_000));
    assert_eq!(price, Some(7_000));
    assert_eq!(sent.message.fee_payer(), &owner());
    assert_eq!(sent.message.recent_blockhash, BLOCKHASH);
    let methods = f.mock.methods();
    let sim = methods
        .iter()
        .position(|m| m == "simulateTransaction")
        .unwrap();
    let send = methods.iter().position(|m| m == "sendTransaction").unwrap();
    assert!(sim < send);
}

#[test]
fn the_priority_fee_is_capped_and_defaults_to_the_networks() {
    let mut price = vec![3u8];
    price.extend_from_slice(&900_000_000u64.to_le_bytes());
    let huge = instruction_json(&saddr::id(saddr::COMPUTE_BUDGET_PROGRAM), &[], &price);
    let body = sol_to_usdc().replace(
        &instruction_json(
            &saddr::id(saddr::COMPUTE_BUDGET_PROGRAM),
            &[],
            &[3, 0x58, 0x1b, 0, 0, 0, 0, 0, 0],
        ),
        &huge,
    );
    let f = swap_fx(body);
    swap_of(&f, &params()).unwrap();
    let sent = submitted(&f);
    let (_, price) = tx::compute_budget(&sent.message, &sent.message.account_keys);
    assert_eq!(price, Some(250_000));

    // without a price from Jupiter, the cluster's recent fee is used
    let none = sol_to_usdc().replace(
        r#""computeBudgetInstructions""#,
        r#""computeBudgetInstructions_""#,
    );
    let f = swap_fx(none);
    swap_of(&f, &params()).unwrap();
    let sent = submitted(&f);
    let (_, price) = tx::compute_budget(&sent.message, &sent.message.account_keys);
    assert_eq!(price, Some(1_000));
}

#[test]
fn an_explicit_priority_fee_replaces_the_one_from_jupiter_or_the_network() {
    let f = swap_fx(sol_to_usdc());
    let p = SwapParams {
        priority_fee: Some("0.0002".to_string()),
        ..params()
    };
    swap_of(&f, &p).unwrap();
    let sent = submitted(&f);
    assert_eq!(
        tx::priority_fee_lamports(&sent.message, &sent.message.account_keys),
        200_000
    );

    let f = swap_fx(sol_to_usdc());
    let none = SwapParams {
        priority_fee: Some("0".to_string()),
        ..params()
    };
    swap_of(&f, &none).unwrap();
    let sent = submitted(&f);
    let (_, price) = tx::compute_budget(&sent.message, &sent.message.account_keys);
    assert_eq!(price, None);

    let f = swap_fx(sol_to_usdc());
    let bad = SwapParams {
        priority_fee: Some("5".to_string()),
        ..params()
    };
    assert!(swap_of(&f, &bad).unwrap_err().contains("0.01 SOL"));
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn selling_a_token_for_sol_works_the_same_way() {
    let body = plain_body(USDC, SOL_MINT, (14_000_000, 100_000_000, 99_000_000));
    let w = swap_world();
    {
        let mut state = w.borrow_mut();
        state
            .accounts
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 20_000_000));
        state
            .post
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 6_000_000));
        state
            .post
            .insert(owner(), system_account(5_000_000_000 + 100_000_000 - 5_000));
    }
    let f = swap_fx_in(w, body, |s| s);
    let p = SwapParams {
        from_token: USDC.to_string(),
        to_token: "native".to_string(),
        amount: "14".to_string(),
        ..SwapParams::default()
    };
    let v = swap_of(&f, &p).unwrap();
    assert_eq!(text(&v, &["swap", "from", "symbol"]), "USDC");
    assert_eq!(text(&v, &["swap", "to", "symbol"]), "SOL");
    assert_eq!(text(&v, &["swap", "to", "token"]), "native");
    assert_eq!(f.world.borrow().sent.len(), 1);
    let url = build_url_of(&f);
    assert!(url.contains(&format!("inputMint={USDC}")), "{url}");
    assert!(url.contains(&format!("outputMint={SOL_MINT}")));
    assert!(url.contains("amount=14000000"));
}

fn refused_with(f: &Fx, expect: &str) {
    let err = swap_of(f, &params()).unwrap_err();
    assert!(err.contains(expect), "{err}");
    assert!(f.world.borrow().sent.is_empty(), "nothing may be sent");
    assert_eq!(f.mock.count("sendTransaction"), 0);
}

#[test]
fn a_swap_that_would_overspend_or_underdeliver_is_never_signed() {
    // the transaction takes 1 SOL, not 0.1
    let f = swap_fx(sol_to_usdc());
    f.world
        .borrow_mut()
        .post
        .insert(owner(), system_account(4_000_000_000));
    refused_with(&f, "more SOL than requested");

    // the wallet gets 10 USDC, below the quoted minimum of 13.86
    let f = swap_fx(sol_to_usdc());
    f.world
        .borrow_mut()
        .post
        .insert(usdc_account(), token_acct(&usdc(), &owner(), 10_000_000));
    refused_with(&f, "less than the quote guarantees");

    // the transaction also drains another token account of the wallet
    let w = swap_world();
    {
        let mut state = w.borrow_mut();
        state
            .accounts
            .insert(key(70), mint_acct(9, &token_program()));
        state
            .accounts
            .insert(key(61), token_acct(&key(70), &owner(), 9_000));
        state
            .post
            .insert(key(61), token_acct(&key(70), &owner(), 0));
    }
    let body = sol_to_usdc_with(&[(key(61), false, true)]);
    let f = swap_fx_in(w, body, |s| s);
    refused_with(&f, "did not ask to sell");
}

#[test]
fn simulation_warnings_and_failures_stop_the_swap() {
    // a spending approval would be granted on the wallet's USDC account
    let f = swap_fx(sol_to_usdc());
    f.world.borrow_mut().post.insert(
        usdc_account(),
        Acct {
            lamports: 2_039_280,
            owner: token_program(),
            data: token_account_bytes(&usdc(), &owner(), 14_000_000, Some(&key(66))),
        },
    );
    refused_with(&f, "allowed to spend");

    let f = swap_fx(sol_to_usdc());
    f.world.borrow_mut().sim_error = Some(r#""InsufficientFundsForRent""#.to_string());
    refused_with(&f, "would fail");
}

#[test]
fn instructions_that_need_other_signers_or_grant_rights_are_refused() {
    // a second signer who cannot sign
    let two_signers = sol_to_usdc_with(&[(key(8), true, false)]);
    let f = swap_fx(two_signers);
    refused_with(&f, "someone other than this wallet");

    // an unlimited approval over the wallet's USDC account, alongside the swap
    let mut data = vec![4u8];
    data.extend_from_slice(&u64::MAX.to_le_bytes());
    let approve = instruction_json(
        &token_program(),
        &[
            (usdc_account(), false, true),
            (key(9), false, false),
            (owner(), true, false),
        ],
        &data,
    );
    let body = sol_to_usdc().replace(
        r#""otherInstructions":[]"#,
        &format!(r#""otherInstructions":[{approve}]"#),
    );
    let f = swap_fx(body);
    refused_with(&f, "unsafe");

    // a swap instruction that is not Jupiter's verifiable route
    let body = body_with(
        SOL_MINT,
        USDC,
        (100_000_000, 14_000_000, 13_860_000),
        &approve,
        "{}",
    );
    let f = swap_fx(body);
    refused_with(&f, "other than Jupiter's aggregator");

    // a blockhash the cluster does not accept
    let f = swap_fx(sol_to_usdc());
    f.world.borrow_mut().blockhash_valid = Some(false);
    refused_with(&f, "blockhash is not valid");
}

#[test]
fn quotes_that_disagree_with_the_request_are_refused() {
    for (name, body) in [
        (
            "sells more",
            plain_body(SOL_MINT, USDC, (200_000_000, 14_000_000, 13_860_000)),
        ),
        (
            "other output token",
            plain_body(
                SOL_MINT,
                &key_text(9),
                (100_000_000, 14_000_000, 13_860_000),
            ),
        ),
        (
            "other input token",
            plain_body(&key_text(9), USDC, (100_000_000, 14_000_000, 13_860_000)),
        ),
        (
            "loose minimum",
            plain_body(SOL_MINT, USDC, (100_000_000, 14_000_000, 5_000_000)),
        ),
        ("exact output", sol_to_usdc().replace("ExactIn", "ExactOut")),
    ] {
        let f = swap_fx(body);
        let err = swap_of(&f, &params()).unwrap_err();
        assert!(
            err.contains("refusing") || err.contains("exact-input"),
            "{name}: {err}"
        );
        assert!(f.world.borrow().sent.is_empty(), "{name}");
    }
}

#[test]
fn min_receive_is_a_floor_on_the_guaranteed_minimum() {
    let f = swap_fx(sol_to_usdc());
    let mut p = params();
    p.min_receive = Some("13.87".to_string());
    let err = swap_of(&f, &p).unwrap_err();
    assert!(err.contains("min_receive"), "{err}");
    assert!(f.world.borrow().sent.is_empty());
    p.min_receive = Some("13.86".to_string());
    assert!(swap_of(&f, &p).is_ok());
}

#[test]
fn min_receive_binds_the_minimum_the_program_enforces() {
    // Jupiter reports a 13.86 USDC minimum, but its instruction is quoted at 13.95, so the
    // program would accept as little as 13.8105 once the transaction lands.
    let body = body_with(
        SOL_MINT,
        USDC,
        (100_000_000, 14_000_000, 13_860_000),
        &route_instruction(SOL_MINT, USDC, (100_000_000, 13_950_000, 0), 0, &[]),
        "{}",
    );
    let f = swap_fx(body.clone());
    let v = quote_of(&f, &params()).unwrap();
    assert_eq!(text(&v, &["to", "minimum_amount"]), "13.8105");
    let f = swap_fx(body);
    let mut p = params();
    p.min_receive = Some("13.86".to_string());
    let err = swap_of(&f, &p).unwrap_err();
    assert!(
        err.contains("min_receive") && err.contains("13810500"),
        "{err}"
    );
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn jupiter_errors_are_explained() {
    let f = swap_fx(r#"{"error":"Could not find any route"}"#.to_string());
    let err = quote_of(&f, &params()).unwrap_err();
    assert!(err.contains("Could not find any route"), "{err}");
    for (failure, expect) in [
        (
            "HTTP status 403 (blocked by Cloudflare bot protection)",
            "Jupiter refused",
        ),
        ("HTTP status 429: Too Many Requests", "keyless limit"),
        ("HTTP status 500: oops", "Jupiter API error"),
    ] {
        let f = build_with(
            swap_world(),
            SendMode::Accept,
            MAINNET_GENESIS,
            |s| s,
            |m| m.get(BUILD, Err(failure.to_string())),
        );
        import(&f);
        let err = swap_of(&f, &params()).unwrap_err();
        assert!(err.contains(expect), "{err}");
        assert!(f.world.borrow().sent.is_empty());
    }
}

#[test]
fn swap_parameters_are_validated() {
    let f = swap_fx(sol_to_usdc());
    type Change = Box<dyn Fn(&mut SwapParams)>;
    let cases: Vec<(Change, &str)> = vec![
        (Box::new(|p| p.from_chain_id = 1), "Ethereum parameters"),
        (Box::new(|p| p.to_chain_id = 1), "Ethereum parameters"),
        (
            Box::new(|p| p.to_address = Some(key_text(2))),
            "'to_address'",
        ),
        (
            Box::new(|p| p.to_token = "native".to_string()),
            "same token",
        ),
        (
            Box::new(|p| p.from_token = SOL_MINT.to_string()),
            "use \"native\"",
        ),
        (Box::new(|p| p.amount = "0".to_string()), "'amount'"),
        (Box::new(|p| p.amount = "x".to_string()), "'amount'"),
        (Box::new(|p| p.slippage_bps = Some(1)), "slippage_bps"),
        (
            Box::new(|p| p.to_token = "nonsense".to_string()),
            "'to_token'",
        ),
        (Box::new(|p| p.to_token = key_text(99)), "does not exist"),
        (
            Box::new(|p| p.min_receive = Some("0".to_string())),
            "'min_receive'",
        ),
    ];
    for (change, expect) in cases {
        let mut p = params();
        change(&mut p);
        let err = swap_of(&f, &p).unwrap_err();
        assert!(err.contains(expect), "{err}");
    }
    assert!(f.mock.get_urls.borrow().iter().all(|u| !u.contains(BUILD)));
}

fn oversized_body() -> String {
    let amounts = (100_000_000, 14_000_000, 13_860_000);
    sol_to_usdc_with(&[(key(31), false, false)]).replace(
        &base64::encode(&route_data(amounts.0, amounts.1, 0, 9)),
        &base64::encode(&route_data(amounts.0, amounts.1, 0, 1_300)),
    )
}

#[test]
fn a_route_that_does_not_fit_is_asked_again_with_fewer_accounts() {
    let f = build_with(
        swap_world(),
        SendMode::Accept,
        MAINNET_GENESIS,
        |s| s,
        |m| {
            m.get("maxAccounts=40", Ok(sol_to_usdc()))
                .get(BUILD, Ok(oversized_body()))
        },
    );
    import(&f);
    let v = swap_of(&f, &params()).unwrap();
    assert_eq!(text(&v, &["status"]), "confirmed");
    let urls: Vec<String> = f
        .mock
        .get_urls
        .borrow()
        .iter()
        .filter(|u| u.contains(BUILD))
        .cloned()
        .collect();
    assert_eq!(urls.len(), 2, "{urls:?}");
    assert!(!urls[0].contains("maxAccounts") && urls[1].contains("maxAccounts=40"));
    assert!(submitted(&f).to_bytes().len() <= tx::MAX_TX_BYTES);
}

#[test]
fn a_route_that_never_fits_is_refused() {
    let f = swap_fx(oversized_body());
    refused_with(&f, "too large to fit in one Solana transaction");
    let tries = f
        .mock
        .get_urls
        .borrow()
        .iter()
        .filter(|u| u.contains(BUILD))
        .count();
    assert_eq!(tries, 3);
}

#[test]
fn swaps_need_enough_balance_but_quotes_still_work() {
    let f = swap_fx(sol_to_usdc());
    let mut p = params();
    p.amount = "9".to_string();
    let err = swap_of(&f, &p).unwrap_err();
    assert!(err.contains("insufficient SOL"), "{err}");
    // a quote reports the shortfall instead of failing
    let body = plain_body(
        SOL_MINT,
        USDC,
        (9_000_000_000, 1_260_000_000, 1_247_400_000),
    );
    let f = swap_fx(body);
    let v = quote_of(&f, &p).unwrap();
    assert_eq!(
        field(&v, &["requirements", "sufficient_balance"]),
        &Value::Bool(false)
    );
    assert_eq!(
        field(&v, &["verification", "dry_run", "simulated"]),
        &Value::Bool(false)
    );
    assert!(
        field(&v, &["warnings"])
            .to_json()
            .contains("insufficient SOL")
    );
    assert_eq!(f.mock.count("simulateTransaction"), 0);
}

#[test]
fn swaps_work_only_on_mainnet() {
    let f = build_with(world(), SendMode::Accept, DEVNET_GENESIS, |s| s, |m| m);
    f.service
        .create_wallet_on("solana", "dev", PW, Some("devnet"))
        .unwrap();
    let err = f
        .service
        .solana_swap_quote("dev", PW, &params())
        .unwrap_err();
    assert!(err.contains("only available on Solana mainnet"), "{err}");
}

// ---- the swap fee --------------------------------------------------------------------------------

#[test]
fn the_fee_is_requested_from_jupiter_and_its_account_is_opened() {
    let f = fee_fx(fee_body(), false);
    let v = quote_of(&f, &params()).unwrap();
    assert_eq!(text(&v, &["fees", "service_fee", "percent"]), "0.7");
    assert_eq!(text(&v, &["fees", "service_fee", "bps"]), "70");
    assert_eq!(text(&v, &["fees", "service_fee", "symbol"]), "USDC");
    // 0.7% of the gross amount, which is what arrives plus the fee
    assert_eq!(
        text(&v, &["fees", "service_fee", "estimated_amount"]),
        "0.097985"
    );
    assert_eq!(text(&v, &["fees", "service_fee", "recipient"]), key_text(5));
    assert_eq!(
        text(&v, &["fees", "service_fee", "recipient_token_account"]),
        saddr::to_text(&fee_account())
    );
    assert_eq!(
        field(
            &v,
            &["fees", "service_fee", "opens_recipient_token_account"]
        ),
        &Value::Bool(true)
    );
    let url = build_url_of(&f);
    assert!(url.contains("platformFeeBps=70"), "{url}");
    assert!(
        url.contains(&format!("feeAccount={}", saddr::to_text(&fee_account()))),
        "{url}"
    );

    // swapping opens the recipient's token account first (paid by the wallet), then swaps
    swap_of(&f, &params()).unwrap();
    let sent = submitted(&f);
    assert_signed_by_wallet(&sent);
    let programs = programs_of(&sent);
    assert_eq!(programs.len(), 4);
    assert_eq!(programs[2], saddr::id(saddr::ASSOCIATED_TOKEN_PROGRAM));
    assert_eq!(programs[3], saddr::id(ssw::JUPITER_PROGRAM));
    let created: Vec<_> = ops_of(&sent)
        .into_iter()
        .filter_map(|op| match op {
            Op::CreateAssociatedToken {
                payer,
                associated,
                owner,
                mint,
                idempotent,
            } => Some((payer, associated, owner, mint, idempotent)),
            _ => None,
        })
        .collect();
    assert_eq!(
        created,
        [(owner(), fee_account(), fee_recipient(), usdc(), true)]
    );
}

#[test]
fn an_existing_fee_account_is_used_as_it_is() {
    let f = fee_fx(fee_body(), true);
    let v = quote_of(&f, &params()).unwrap();
    assert_eq!(
        field(
            &v,
            &["fees", "service_fee", "opens_recipient_token_account"]
        ),
        &Value::Bool(false)
    );
    swap_of(&f, &params()).unwrap();
    let sent = submitted(&f);
    assert!(
        !programs_of(&sent).contains(&saddr::id(saddr::ASSOCIATED_TOKEN_PROGRAM)),
        "no account is opened"
    );
    assert!(build_url_of(&f).contains("platformFeeBps=70"));
}

#[test]
fn a_swap_that_would_not_pay_the_fee_is_refused() {
    // Jupiter's swap instruction charges no fee
    let f = fee_fx(sol_to_usdc(), false);
    refused_with(&f, "platform fee of 0 bps, not the configured 70 bps");

    // it charges the fee but does not include the fee account
    let amounts = (100_000_000, 13_900_000, 13_761_000);
    let body = body_with(
        SOL_MINT,
        USDC,
        amounts,
        &route_instruction(SOL_MINT, USDC, amounts, 70, &[]),
        "{}",
    );
    let f = fee_fx(body, false);
    refused_with(&f, "fee would be lost");
}

#[test]
fn a_fee_account_address_held_by_something_else_stops_the_swap() {
    let w = swap_world();
    w.borrow_mut()
        .accounts
        .insert(fee_account(), system_account(1));
    let fee = SolanaSwapFee::from_parts(&key_text(5), 70).unwrap();
    let f = swap_fx_in(w, fee_body(), move |s| {
        s.with_solana_swap_settings(SolanaSwapSettings::new(fee))
    });
    refused_with(&f, "not a token account");
    assert!(f.mock.get_urls.borrow().iter().all(|u| !u.contains(BUILD)));
}

#[test]
fn the_fee_is_paid_in_the_token_bought_even_when_that_is_sol() {
    // selling USDC for SOL: the fee goes to the recipient's wrapped SOL account
    let wsol = saddr::id(saddr::WRAPPED_SOL_MINT);
    let wsol_fee_account =
        saddr::associated_token_address(&key(5), &wsol, &token_program()).unwrap();
    let amounts = (14_000_000, 100_000_000, 99_000_000);
    let body = body_with(
        USDC,
        SOL_MINT,
        amounts,
        &route_instruction(
            USDC,
            SOL_MINT,
            amounts,
            70,
            &[(wsol_fee_account, false, true)],
        ),
        "{}",
    );
    let w = swap_world();
    {
        let mut state = w.borrow_mut();
        state.accounts.insert(wsol, mint_acct(9, &token_program()));
        state
            .accounts
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 20_000_000));
        state
            .post
            .insert(usdc_account(), token_acct(&usdc(), &owner(), 6_000_000));
        state
            .post
            .insert(owner(), system_account(5_000_000_000 + 100_000_000 - 5_000));
    }
    let fee = SolanaSwapFee::from_parts(&key_text(5), 70).unwrap();
    let f = swap_fx_in(w, body, move |s| {
        s.with_solana_swap_settings(SolanaSwapSettings::new(fee))
    });
    let p = SwapParams {
        from_token: USDC.to_string(),
        to_token: "native".to_string(),
        amount: "14".to_string(),
        ..SwapParams::default()
    };
    let v = swap_of(&f, &p).unwrap();
    assert_eq!(text(&v, &["swap", "fees", "service_fee", "symbol"]), "SOL");
    assert_eq!(
        text(
            &v,
            &["swap", "fees", "service_fee", "recipient_token_account"]
        ),
        saddr::to_text(&wsol_fee_account)
    );
    assert!(
        build_url_of(&f).contains(&format!("feeAccount={}", saddr::to_text(&wsol_fee_account)))
    );
}

// ---- address lookup tables -----------------------------------------------------------------------

fn lookup_table(addresses: &[Pubkey]) -> Acct {
    let mut data = vec![0u8; 56];
    for a in addresses {
        data.extend_from_slice(a);
    }
    Acct {
        lamports: 1,
        owner: saddr::id(saddr::ADDRESS_LOOKUP_TABLE_PROGRAM),
        data,
    }
}

fn table_body() -> String {
    body_with(
        SOL_MINT,
        USDC,
        (100_000_000, 14_000_000, 13_860_000),
        &route_instruction(
            SOL_MINT,
            USDC,
            (100_000_000, 14_000_000, 13_860_000),
            0,
            &[],
        ),
        &format!(
            r#"{{"{}":["{}","{}"]}}"#,
            key_text(60),
            key_text(31),
            saddr::to_text(&usdc_account())
        ),
    )
}

#[test]
fn accounts_are_loaded_from_lookup_tables_that_match_the_chain() {
    let w = swap_world();
    w.borrow_mut()
        .accounts
        .insert(key(60), lookup_table(&[key(31), usdc_account()]));
    let f = swap_fx_in(w, table_body(), |s| s);
    swap_of(&f, &params()).unwrap();
    let sent = submitted(&f);
    assert_signed_by_wallet(&sent);
    assert_eq!(sent.message.lookups.len(), 1);
    assert_eq!(sent.message.lookups[0].table, key(60));
    // the USDC account is loaded through the table, not listed in the message
    assert!(!sent.message.account_keys.contains(&usdc_account()));
}

#[test]
fn lookup_tables_that_differ_from_the_chain_are_refused() {
    // on chain the table holds other addresses than Jupiter claims
    let w = swap_world();
    w.borrow_mut()
        .accounts
        .insert(key(60), lookup_table(&[key(31), key(32)]));
    let f = swap_fx_in(w, table_body(), |s| s);
    refused_with(&f, "lookup tables do not match");

    // a table that does not exist
    let f = swap_fx(table_body());
    let err = swap_of(&f, &params()).unwrap_err();
    assert!(err.contains("lookup table"), "{err}");
    assert!(f.world.borrow().sent.is_empty());
}

#[test]
fn the_tools_route_solana_swaps() {
    let (f, tb) = tool_fx_with_swap(sol_to_usdc());
    let creds = format!(r#""wallet_name":"s","password":"{PW}""#);
    let (err, _) = call(
        &tb,
        "import_wallet",
        &format!(
            r#"{{"blockchain":"solana","wallet_name":"s","password":"{PW}","private_key":"{}"}}"#,
            key_hex()
        ),
    );
    assert!(!err);
    let args = format!(r#"{{{creds},"from_token":"native","to_token":"{USDC}","amount":"0.1"}}"#);
    let (err, v) = call(&tb, "get_swap_quote", &args);
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["to", "expected_amount"]), "14");
    let (err, v) = call(&tb, "swap_tokens", &args);
    assert!(!err, "{}", v.to_json());
    assert_eq!(text(&v, &["status"]), "confirmed");
    assert_eq!(f.world.borrow().sent.len(), 1);
    // Ethereum-style parameters are refused
    let (err, v) = call(
        &tb,
        "swap_tokens",
        &format!(
            r#"{{{creds},"from_chain_id":1,"from_token":"native","to_token":"{USDC}","amount":"0.1"}}"#
        ),
    );
    assert!(err && v.to_json().contains("Ethereum parameters"));
}

fn tool_fx_with_swap(body: String) -> (Fx, Toolbox) {
    let dir = TempDir::new();
    let w = swap_world();
    let mock = Rc::new(node(&w, SendMode::Accept, MAINNET_GENESIS).get(BUILD, Ok(body)));
    let http: Rc<dyn HttpClient> = mock.clone();
    let service = WalletService::new(Store::open(&dir.0).unwrap(), http)
        .with_iterations(vault::MIN_ITERATIONS)
        .with_receipt_wait(Duration::ZERO, 3);
    let fx = Fx {
        _dir: dir,
        mock,
        world: w,
        service: WalletService::new(
            Store::open(&TempDir::new().0).unwrap(),
            Rc::new(SolMock::new()),
        ),
    };
    (fx, Toolbox::new(service))
}
