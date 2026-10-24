## About

All-in-one crypto wallet for Agents. Built from the ground up in Rust.

## Feature Requirements

- Everything in the Core folder must be written in rust. 
- Functions must be broken up into individual parts that are east to unit test. For example, a function for formatting text; this must be it's own function instead of being implemented within an existing function.
- Functions must be tested thoroughly so we never get unexpected output. 
- Security is #1. Never 
- Limit use of external packages. Build it yourself unless it would be an uunreasonable amount of work.
- If using outside packages, confirm online that they are extremely stable, have no securty reports / vulnerabilities, & have a large amount of downloads on crates.io
- A user's seed phrase(s) must be extremely secure. Another user being able to steal them is unacceptable.
- Features must be easily accessed through the MCP server for agents.
- For each implemented crypto wallet, the password KDF and vault must be encrypted using the same methods that Metamask uses.
- When unlocking one wallet with a password (e.g. ETH), the other wallets must stay locked.
- Public addresses & balances must stay encrypted / not visible until a wallet is unlocked.
- A wallet only unlocks per-action, meaning after a MCP tool action is complete (e.g. viewing the address of a wallet) it must lock again.
- Seed phrases must be 24 words long.


## Current features

The MCP server lives in `core/` (Rust, stdio JSON-RPC, run with `cargo run -q` from `core/`; an MCP client launches the binary). Wallet logic for Ethereum is in `core/src/eth/`, for Sui in `core/src/sui/`. Wallets are stored in `$AGENT_SAIFU_HOME` (default `~/.agent-saifu`).

### MCP tools

Every tool that touches a wallet needs the wallet's password; the wallet is unlocked only for that single call and is locked again when it returns. Unlocking one wallet never unlocks another. Wallet names are unique (case-insensitive).

Two blockchains are supported: **Ethereum** (and EVM chains, selected per call with `chain_id`) and **Sui** (a wallet is bound to one network, so there is no `chain_id`). The tool is chosen by the wallet it is given: a tool shared by both reads the wallet's blockchain and does the right thing. The next table says which tools work with which wallet; the descriptions below say how each behaves (Ethereum unless a "Sui:" note says otherwise).

| Tool | Ethereum | Sui | Notes |
|---|---|---|---|
| `get_info` | yes | yes | |
| `create_wallet` | yes | yes | Sui takes an optional `network` (`mainnet` default, `testnet`, `devnet`) |
| `import_wallet` | yes | yes | Sui: 24-word seed phrase, `suiprivkey1...` key or 64-hex Ed25519 key |
| `list_wallets`, `rename_wallet`, `delete_wallet` | yes | yes | |
| `get_wallet_address` | yes | yes | |
| `get_wallet_balances` | yes | yes | Sui lists every coin type held; no `chain_id` / `token_contracts` |
| `send_asset` | yes | yes | Sui: SUI or any coin type; no `chain_id` / `gas_limit` |
| `sign_contract_transaction` | yes | yes | Sui: signs a complete transaction (`transaction_bytes`); `broadcast=true` is sign-and-execute |
| `sign_personal_message` | yes (EIP-191) | yes | added for the Wallet Standard `signPersonalMessage` |
| `estimate_gas_fee`, `simulate_transaction` | yes | yes | Sui: a transfer, or given `transaction_bytes`; no `value` / `data` |
| `get_transaction` | yes | yes | Sui: `transaction_hash` is the base58 digest |
| `get_transaction_history` | yes | yes | Sui: transactions only (no `token_transfers`) |
| `list_networks` | yes | yes | Sui: the one network the wallet is bound to |
| `get_swap_quote`, `swap_tokens` | yes (OpenOcean) | yes (Cetus, mainnet) | Sui: no `from_chain_id` / `to_chain_id` |
| `view_seed_phrase`, `view_private_key`, `export_wallet` | yes | yes | Sui keys are shown as `suiprivkey1...` (plus hex) |
| `read_smart_contract` | yes | **no** | Sui has no equivalent call-by-signature; Move calls go through `sign_contract_transaction` |
| `add_network`, `update_network`, `delete_network` | yes | **no** | a Sui wallet is bound to its one network |
| `add_token`, `remove_token` | yes | **no** | a Sui wallet lists every coin it holds automatically |
| `revoke_token_approval` | yes | **no** | Sui has no token allowances |
| `wc_*` (WalletConnect) | yes | yes | Sui uses the `sui` WalletConnect namespace (see "WalletConnect (any dapp)"); the Sui signing tools also mirror the Wallet Standard (see "Sui wallets") |

A Sui wallet given to an Ethereum-only tool (or the reverse) is refused with a clear message before anything is decrypted; chain-specific arguments given to the wrong wallet (for example `chain_id` for Sui) are refused too.

- `get_info` - returns "All-in-one crypto wallet for Agents. Built from the ground up in Rust." and "Version 0.0".
- `create_wallet` (blockchain = `ethereum` or `sui`, wallet_name, password, optional network) - new wallet with a 24-word seed phrase in a password-encrypted vault. Multiple wallets are supported. `network` is for Sui only.
- `import_wallet` (wallet_name, password, seed_phrase *or* private_key, optional blockchain / network) - add an existing wallet; `blockchain` defaults to `ethereum`.
- `list_wallets` - names and blockchains only; no password.
- `rename_wallet` (wallet_name, new_wallet_name, password) - added beyond the original list.
- `delete_wallet` (wallet_name, password, confirm=true) - added beyond the original list; irreversible.
- `get_wallet_address` (wallet_name, password)
- `get_wallet_balances` (wallet_name, password, optional chain_id / token_contracts) - each asset with amount, USDC value, and chain ID, across all of the wallet's networks. Checks the native coin, a built-in list of well-known tokens, tokens saved with `add_token`, and any token contracts you pass for a one-off check. `usdc_value` is null when no real market price exists (testnets, unlisted tokens).
- `send_asset` (wallet_name, chain_id, asset = `native` or token contract, to, amount, password) - signs and broadcasts.
- `sign_contract_transaction` (wallet_name, chain_id, password, to, data, optional value / gas_limit / broadcast) - signs a smart-contract call; only broadcasts if `broadcast=true`. Decodes token transfers/approvals and warns about unlimited approvals.
- `sign_personal_message` (wallet_name, password, message, optional message_format = `text` / `hex` / `base64`) - added for Sui's Wallet Standard `signPersonalMessage` and offered for both blockchains: Ethereum signs with EIP-191 `personal_sign`; Sui signs with Sui's personal-message intent. Nothing is sent and no network access is needed. The response shows the message text (or "(binary data)").
- `estimate_gas_fee` (wallet_name, chain_id, password, to, optional value / data, or asset + amount) - gas, fee in native currency and USDC.
- `simulate_transaction` (same parameters as estimate) - added beyond the original list; dry-runs the transaction before signing and reports success or the revert reason, fees, affordability and a decoded description.
- `read_smart_contract` (wallet_name, chain_id, password, contract, function, optional args / outputs) - added beyond the original list; read-only call, nothing is signed.
- `get_transaction` (wallet_name, chain_id, password, transaction_hash) - added beyond the original list; status, confirmations, fee paid.
- `get_transaction_history` (wallet_name, chain_id, password, optional type / limit) - added beyond the original list; uses the network's Blockscout-compatible explorer API.
- `add_network`, `update_network` (wallet_name, network_name, chain_id, rpc_url, currency_symbol, block_explorer_url, password), `delete_network` (wallet_name, chain_id, password) - per-wallet layer 2 networks, stored inside the encrypted vault. Ethereum mainnet is built in and cannot be changed or deleted. The RPC must report the chain ID it is added under.
- `list_networks` (wallet_name, password) - helper for viewing a wallet's networks. Deleting a network also removes the tokens added on it.
- `add_token` (wallet_name, chain_id, contract, password) - added beyond the original list; saves an ERC-20 token to the wallet on that network (any network the wallet has). The contract is checked on-chain and its symbol and decimals are read from it. Added tokens always appear in `get_wallet_balances`, even at zero. Stored in the encrypted vault; max 50 per network.
- `remove_token` (wallet_name, chain_id, contract, password) - added beyond the original list; stops tracking an added token (no funds move). Built-in well-known tokens cannot be removed.
- `get_swap_quote` (wallet_name, password, from_chain_id, from_token, to_chain_id, to_token, amount, optional slippage_bps / to_address) - added by request; previews a swap through OpenOcean without signing or sending anything: how much of the new token you will get (expected and guaranteed minimum), the fees (service fee, estimated network fee), the route, price impact, and whether an approval or more balance is needed.
- `swap_tokens` (same parameters plus optional min_receive) - swaps through OpenOcean and broadcasts. Tokens are a contract address or `native`. If the sold token needs an approval, exactly the sold amount is approved first (a non-zero allowance is reset to zero first, as USDT requires) and confirmed before the swap is sent. `min_receive` is a safety floor: if the fresh quote's guaranteed minimum is lower, nothing is sent.
- `view_seed_phrase`, `view_private_key` (wallet_name, password)
- `export_wallet` (wallet_name, password, encryption_key, optional file_name) - writes a ZIP to `exports/` whose single entry is the seed phrase and private key encrypted with MetaMask's vault scheme under the given key. Never overwrites files.
- `revoke_token_approval` (wallet_name, chain_id, password, token, spender) - useful for revoking smart-contract token approvals (sets the ERC-20 allowance to zero).
- `wc_pair` (uri, optional wait_seconds) - added by request; connects to any dapp over WalletConnect v2, Ethereum or Sui. Give it the `wc:` URI the dapp shows. It waits for the dapp's session proposal and returns what the dapp asks for (chains, methods, whether this wallet can satisfy it). Nothing is shared until the proposal is approved. No wallet is unlocked.
- `wc_list_proposals`, `wc_reject_session` (proposal_id) - waiting connection proposals; decline one.
- `wc_approve_session` (proposal_id, wallet_name, password, chain_id for Ethereum wallets, optional additional_chain_ids) - approves a proposal with a wallet; this is the only step that reveals the address, and only to the dapp. Ethereum: the chains must cover everything the dapp requires and may only include chains the dapp asked for; the wallet needs a network for each. Sui: omit `chain_id` / `additional_chain_ids`; the dapp is connected on the wallet's own network (`sui:mainnet`, `sui:testnet` or `sui:devnet`).
- `wc_list_sessions`, `wc_disconnect_session` (session_topic) - connected dapps (wallet, chains, methods; never addresses); end one.
- `wc_list_requests` - what connected dapps are waiting on, with a summary (decoded token transfers/approvals, message text, typed data). No password; the signer's address is left out.
- `wc_review_request` (request_id, wallet_name, password, chain_id for Ethereum wallets) - the expected costs and outcome. For transactions: a dry run (would it succeed or revert), what is sent or approved in plain language (`expected_outcome`), the network fee (max and expected, in the native coin and USDC), and whether the wallet can afford it. For messages and typed data: exactly what would be signed, with warnings. Signs and sends nothing.
- `wc_approve_request` (request_id, wallet_name, password, chain_id for Ethereum wallets) - signs and answers the dapp. `eth_sendTransaction` is signed and broadcast; `eth_signTransaction` only signs; `personal_sign` and `eth_signTypedData(_v4)` return a signature. Sui methods are listed under "WalletConnect (any dapp)".
- `wc_reject_request` (request_id, optional reason) - tells the dapp the user rejected it. Nothing is signed. No password.

### Swaps (OpenOcean)

- **No API key, account or environment variable.** Swaps use an OpenOcean v4 swap API host (`https://open-api.de1.exchange/v4`, endpoint `/:chain/swap`). The host is the `OPENOCEAN_API_BASE_URL` setting in [core/src/swap_config.rs](core/src/swap_config.rs). The host supplies the transaction data the wallet signs, but the wallet decodes that data and refuses anything that does not match your request (see the safety checks below).
- **Same-chain only.** OpenOcean's API swaps within one chain; its cross-chain swaps exist only in its web widget, and non-EVM chains (Solana, and so on) need a whitelisted key. So `to_chain_id` must equal `from_chain_id` (a different value returns an explanatory error), and there is no `to_blockchain` option: swaps such as ETH to BTC or SUI are not possible through this API. What *is* supported is `to_address`: an optional address on the same chain that receives the bought tokens instead of the wallet itself.
- **Chains:** the 41 chains OpenOcean lists for its swap API (Ethereum, BNB Chain, Base, Polygon, Arbitrum, Optimism, Avalanche, and others). The wallet must have the network (see `add_network`). The wallet only ever approves or sends to OpenOcean's published router contract.
- **Costs to know about:** OpenOcean also applies its own tiered fee on public-API swaps (0.01% stable pairs, 0.1% correlated, 0.25% common, 0.5% exotic tokens).
- **Safety checks before signing:** the quote must be for the requested tokens (decimals included), may not sell more than requested or send extra native currency, must target OpenOcean's router, must meet `min_receive`, and may not have a price impact worse than 5%. The router calldata is also decoded locally (`swap(...)` on OpenOcean's router) and must sell the requested token and the quoted amount, buy the requested token, **deliver it to the wallet (or the requested `to_address`)** and guarantee a minimum return at least the quote's (and your `min_receive`); the quote's guaranteed minimum may also not be looser than the requested slippage (plus 0.5%). So a dishonest or compromised API host cannot redirect the output or hide a bad price; calldata the wallet cannot decode is refused and nothing is sent. The approval is for exactly the sold amount, never unlimited. OpenOcean text (route names, errors) is length-limited and stripped of control characters before it reaches the agent.
- **Rate limit:** 2 requests per second on the free API. **Known issue:** OpenOcean's Cloudflare bot protection challenged the quote and swap endpoints (HTTP 403) when tested from a cloud server, so swaps may fail from some networks; the tools then say so and send nothing.

Ethereum wallets use the OpenOcean aggregator (this section); Sui wallets use the Cetus aggregator (see "Swaps on Sui (Cetus)"). Chain IDs select Ethereum networks such as Base or Arbitrum; Sui has no chain IDs (a wallet is bound to a network), so Sui tools do not take one.

### Sui wallets

Sui wallets are created and used like Ethereum wallets (same vault encryption, per-call unlocking, password rate limiting, tools), with these specifics. They were built following Sui's documentation on the [Wallet Standard](https://docs.sui.io/onchain-finance/asset-custody/wallets/wallet-standard), [self-custodial wallets](https://docs.sui.io/onchain-finance/asset-custody/wallets/self-custody) and its [security best practices](https://docs.sui.io/develop/security/best-practices) for key management, and use the official [Sui Rust SDK](https://github.com/MystenLabs/sui-rust-sdk).

- **Keys.** Ed25519, Sui's default scheme. A 24-word BIP39 seed phrase derives the key with SLIP-0010 at `m/44'/784'/0'/0'/0'` (the path every Sui wallet uses, so a phrase from Slush or the Sui CLI gives the same address). The address is the BLAKE2b-256 hash of `0x00` plus the public key, computed by the Sui SDK. Both the derivation and the signing formats are checked against an independent implementation (see the tests). Imported keys may be a `suiprivkey1...` Bech32 string, the Sui CLI keystore's base64 `flag || key`, or 64 hex characters; only Ed25519 keys are accepted. `view_private_key` / `export_wallet` give the `suiprivkey1...` form that Sui wallets import, plus hex.
- **Self-custody.** Keys exist only inside the password-encrypted vault (same PBKDF2-SHA256 900,000 iterations + AES-256-GCM format as the Ethereum wallets, with MetaMask's keyring layout), are decrypted for one call and wiped afterwards, and never leave the machine. The wallet file holds only the name, blockchain and vault: the address, the network and the keys are inside the vault.
- **One network per wallet.** Sui's guidance is to keep Testnet and Mainnet keys separate so a test key can never sign a production transaction. A Sui wallet is therefore bound to one network (`mainnet` by default; `testnet` or `devnet` with `network` on `create_wallet` / `import_wallet`), recorded inside the vault. Every call first checks that the endpoint really is that network (mainnet and testnet by their chain identifier) and signing refuses a transaction that names another network. Create separate wallets for separate networks, and separate wallets for unrelated activity (everything on Sui is public).
- **Hardware wallets and multisig are not supported.** Sui's guidance prefers a hardware wallet or multisig for privileged (admin/deployer) keys; this wallet is a hot wallet and is meant for routine operation. Keep recovery material offline: back up the seed phrase with `view_seed_phrase`, never in plaintext or screenshots, and send a small test transfer before moving a large amount.
- **Verify recipients.** A recipient must be written out in full (`0x` + 64 hex characters, the zero address is refused): a mistyped short address would otherwise send coins somewhere unrecoverable.
- **Node access.** Public Sui nodes retired JSON-RPC, so reads, dry runs and submission go through Sui's GraphQL API (`graphql.mainnet.sui.io`, `graphql.testnet.sui.io`, `graphql.devnet.sui.io`) over HTTPS with certificate checking. Transactions are built offline with the Sui SDK's `TransactionBuilder` from data read over GraphQL.
- **Coins and address balances.** A balance is the sum of the coin objects the address owns and its *address balance* (Sui's newer account-style balance; many wallets hold SUI only that way). Both are shown (`coin_objects_raw`, `address_balance_raw`) and both can be spent and can pay gas: gas is paid from coin objects when they cover it, otherwise from the address balance (a transaction with an `expiration` bound to the current epoch and the chain).
- **Gas.** Gas price is the network's reference price. The budget comes from a dry run (computation + a margin + storage - rebate, as Sui's TypeScript SDK does, capped at the protocol's 50 SUI) and the final transaction is dry-run again before signing. A transaction whose dry run fails is never signed.
- **Per-action unlock and double submission.** The wallet is unlocked only for the call. Sui executes a given signed transaction at most once; if the answer to a submission is lost or any error comes back, the transaction's digest (computed locally before submitting) is looked up first; if the node has it, it is reported as executed, and only when the node answered with a validation error and has no such transaction is it called rejected. If it still cannot be confirmed the error returns the digest and says not to send again before checking with `get_transaction`.
- **Dapps.** A Sui dapp can connect over WalletConnect (the `wc_*` tools, see "WalletConnect (any dapp)"); the Wallet Standard tools below are for use without WalletConnect.
- **Wallet Standard mapping.** The Wallet Standard is a browser-extension interface (account discovery, events, signing). The wallet is not a browser extension, so its signing features are offered as MCP tools with the same inputs and outputs: `sui:signTransaction` = `sign_contract_transaction` with `broadcast=false` (returns `bytes` and `signature` exactly as the standard does, plus a decoded description and a dry run); `sui:signAndExecuteTransaction` = the same tool with `broadcast=true`; `sui:signPersonalMessage` = `sign_personal_message`. The legacy `...TransactionBlock` features use the same bytes. `standard:connect` / `standard:events` have no meaning without a browser (the account is `get_wallet_address`; the supported chain is the wallet's network, `sui:mainnet` / `sui:testnet` / `sui:devnet`, shown by `list_networks`). Signing follows Sui's intent scheme: a transaction is signed over `blake2b-256(intent [0,0,0] || bytes)` and a personal message over `blake2b-256(intent [3,0,0] || bcs(message))`, so a message signature can never be replayed as a transaction (tested).
- **What `sign_contract_transaction` checks for Sui.** The bytes must decode as a user transaction whose sender and gas owner is this wallet (sponsored and system transactions are refused), and a transaction bound to another network is refused. The response describes every command (Move calls with package/module/function, transfers, splits, merges, publishes), warns about publishing or upgrading packages, about transferring the gas coin itself, and about a failing dry run, and reports the wallet's balance changes from the dry run. `broadcast=true` is refused unless the dry run succeeds. This wallet cannot decode a Move call into plain language beyond its target; read the description and the balance changes before approving.
- **Prices.** `usdc_value` for SUI comes from CoinGecko, USDC is valued at 1, and the first few other coins from the Cetus aggregator's quote for selling the whole balance; it is `null` on testnet and devnet and wherever no price exists. Coins without on-chain metadata (unknown decimals) are shown in base units and can neither be sent nor swapped.
- **Text from the network** (coin symbols, error messages) is length-limited and stripped of control characters before it reaches the agent.

### Swaps on Sui (Cetus)

Sui swaps go through the [Cetus aggregator](https://cetus-1.gitbook.io/cetus-developer-docs/developer/cetus-aggregator/getting-started) (`https://api-sui.cetus.zone/router_v3`). No API key, account or environment variable is needed.

- **How it works.** The aggregator's API only finds the best route. The wallet builds the transaction itself the way the aggregator's SDK does, in one transaction: open a swap context holding the input coin (`router::new_swap_context_v3`, which records the quoted output, the allowed slippage and the fee), call each pool (`cetus::swap`), and confirm (`router::confirm_swap_v3`), which makes the aggregator's contract abort the whole transaction on-chain if the output is below the minimum. The output coin is sent to the wallet (or `to_address`). The transaction is dry-run before it is signed, and a dry run that shows less than the minimum stops the swap.
- **Mainnet only, Cetus pools only.** Cetus's own testnet support is limited, so swaps need a mainnet wallet. The aggregator can also route through many other exchanges, but each needs its own transaction code that must be verified individually; only routes made entirely of Cetus's pools (provider `CETUS`) are requested and accepted. Routes may have several hops and split across pools. Cross-chain swaps are not offered.
- **Safety checks before signing.** The route must sell exactly the requested coin and amount and end in the requested coin, use only Cetus pools, never spend a coin before it has been produced, add up to its quoted output, and not be priced more than 5% away from the market (`deviation_ratio`). The route's packages are checked on-chain: every hop must call exactly the newest published version of Cetus's router package and the transaction opens the newest version of the aggregator package (both resolved from their original package IDs, which are pinned in the code); every pool, the global config and the partner object must be Cetus objects (types defined by Cetus's CLMM package) and each pool must hold the coins the route claims in the stated order. So a dishonest or compromised API cannot point the wallet at other code or other pools. The contract enforces the minimum output (`slippage_bps`, default 100 = 1%, 5 to 1000) on-chain, and `min_receive` is an additional floor on the quote's guaranteed minimum. Because the aggregator supplies both the price and the numbers that guard it, the quote is also checked against an independent price where one exists: for SUI against USDC it must be within 5% of CoinGecko's SUI price. For any other pair (or if CoinGecko is unreachable) no independent price exists, so `swap_tokens` then **requires `min_receive`** (the least you would accept, from a price you trust) and `get_swap_quote` says so in `price_check`.
- **Costs to know about.** The pools charge their own trading fee inside the price (shown per hop as `fee_rate`); the network fee is paid in SUI.
- **Known limits.** The aggregator's public API has a rate limit; tiny amounts are often refused by the price-deviation check; Sui coins without metadata cannot be swapped; a quote moves with the market, so `swap_tokens` fetches a fresh one.

### WalletConnect (any dapp)

The wallet speaks the WalletConnect v2 Sign protocol as the wallet side, so any dapp that shows a WalletConnect QR code or `wc:` link can connect: Uniswap, OpenSea, Aave, and so on. The flow for an agent:

1. `wc_pair` with the dapp's `wc:` URI, then look at the proposal it returns (`can_connect_with` says whether an Ethereum wallet, a Sui wallet or both can satisfy it).
2. `wc_approve_session` (proposal_id, **wallet_name**, **password**, and **chain_id** for Ethereum wallets) to connect, or `wc_reject_session`.
3. When the dapp asks for something, `wc_list_requests` shows it. `wc_review_request` shows the expected costs and outcome. `wc_approve_request` (request_id, **wallet_name**, **password**, and **chain_id** for Ethereum wallets) signs / sends and answers the dapp; `wc_reject_request` declines.

- **Setup: a project ID is needed.** WalletConnect's relay requires a free project ID. Create one at https://dashboard.walletconnect.com/ and set the `WALLETCONNECT_PROJECT_ID` environment variable, or put it in `WALLETCONNECT_PROJECT_ID` in [core/src/walletconnect_config.rs](core/src/walletconnect_config.rs) (the variable wins). Without it every `wc_*` tool says what is missing and the rest of the wallet is unaffected.
- **Wallet, chain and password on every approval.** `wc_approve_session`, `wc_review_request` and `wc_approve_request` all require the wallet name and password, and Ethereum wallets also the chain ID (a Sui wallet must omit it: it is bound to one network, and giving one is an error). The wallet must be the one the dapp's session was connected with, and the chain must be the one the request is for; otherwise the call fails before anything is unlocked or signed and the request stays open. Passwords are checked (and rate limited) like every other tool.
- **Per-action unlock.** The wallet is unlocked only inside `wc_approve_session`, `wc_review_request` and `wc_approve_request`, and locked again when the call returns. A session holds only WalletConnect keys, never wallet secrets or the address.
- **Supported methods:** `eth_sendTransaction`, `eth_signTransaction`, `personal_sign`, `eth_signTypedData` and `eth_signTypedData_v4` (EIP-712). `eth_sign` is never granted: it signs an arbitrary hash and cannot be reviewed, so a dapp that requires it cannot be connected. Contract creation is refused. The namespaces are `eip155` (Ethereum) and `sui`; a dapp that requires another namespace (e.g. Solana) cannot be connected, and a dapp that requires both `eip155` and `sui` cannot be connected either, because a session belongs to one wallet on one blockchain.
- **Sui over WalletConnect.** Implemented to Reown's Sui RPC reference (namespace `sui`, chains `sui:mainnet` / `sui:testnet` / `sui:devnet`), as the wallet side only.
  - *Connecting.* A Sui wallet approves a proposal with `wc_approve_session` (no `chain_id`). The session is granted exactly one chain, the wallet's own network, so a dapp that *requires* another Sui network, or that requires `eip155`, is refused, and one that only lists the wallet's network as optional is accepted. The settle message carries the account `sui:<network>:<address>`, the granted methods (always including `sui_getAccounts`, which Reown requires of Sui connections), and `sessionProperties.sui_getAccounts`, a JSON string of `[{"pubkey": <base64 of 0x00 flag byte + 32-byte Ed25519 key>, "address": <0x...>}]`, so a dapp can learn the account without asking. As with Ethereum, the agent never sees the address in the proposal, the session or the request list; it is revealed only to the dapp.
  - *Methods* (all others are refused with "Unauthorized method" before the agent sees them): `sui_getAccounts` (answered with the same `[{pubkey, address}]` list; it is a normal request needing `wc_approve_request`, because a session stores no address), `sui_signTransaction` (params `{transaction: base64 BCS TransactionData, address}`; answered `{signature, transactionBytes}`, signed only), `sui_signAndExecuteTransaction` (same params; signed **and submitted**; answered `{digest}`) and `sui_signPersonalMessage` (params `{message, address}`; answered `{signature}`, using Sui's personal-message intent).
  - *SDK-serialized transactions.* Dapps built on the Sui TypeScript SDK (Cetus, for one) send `transaction` not as BCS bytes but as the SDK's serialized JSON (`{"version": 2, "sender", "inputs", "commands", …}`), which has no gas payment and may leave objects unresolved. The wallet accepts this form (version 2; pure inputs, owned/shared/unresolved objects, and the MoveCall, TransferObjects, SplitCoins, MergeCoins, MakeMoveVec and Publish commands). It looks up unresolved objects on the network (shared objects are taken as mutable), picks its own gas coins and price, sets the budget from a dry run (trying smaller trial budgets so the gas reservation cannot starve the amount the transaction spends), and signs the result; the dapp's gas data and expiration are ignored. The `sender` must be this wallet, a sponsored transaction (gas owner not the sender), package upgrades, receiving objects and untyped pure values are refused, and a transaction whose dry run fails is not signed. Address-balance withdrawals (`FundsWithdrawal`, as Cetus swaps use) are accepted only from the sender's own balance. Shared objects the dapp leaves unresolved are taken as mutable except the system Clock and Random objects, which are read-only, and a `mutable` flag from the dapp is honoured. The returned `transactionBytes` are the wallet's final transaction, not the dapp's draft. Tested live against Cetus (a swap signed with `sui_signTransaction` and submitted by the dapp). The gas coin may be spent by the transaction when it uses it, so all of the wallet's SUI coins are offered as gas payment in that case.
  - *Checks.* The request's chain must equal the session's chain, and its `address` must equal the wallet's address, or it is refused before anything is signed. A transaction goes through the same checks as `sign_contract_transaction`: its sender and gas owner must be this wallet, a transaction bound to another network is refused, and the review shows every command, warnings and the dry run's balance changes. `sui_signAndExecuteTransaction` is refused unless the dry run succeeds, and the request stays open. Sizes are limited like the signing tools (128 KiB of transaction bytes, 64 KiB of message).
  - *Personal messages.* Reown specifies `message` as plain text, so it is taken as UTF-8 text, shown in the review, and signed as those bytes. A dapp that sends a base64 or hex encoding would be signed as that literal text; the review shows exactly what is signed.
  - *Mixing blockchains.* A Sui session answers only `sui_*` requests and an Ethereum session only `eth_*` / `personal_sign` ones. Using an Ethereum wallet for a Sui request (or the reverse) fails before anything is unlocked or signed.
  - *Not covered.* Events (`accountsChanged` etc.) are never sent, and a Sui wallet cannot switch networks within a session. Not tested against a live dapp; the flows are tested against a simulated relay and Sui node.
- **Transactions.** (Ethereum) The wallet picks the gas limit, fees and nonce itself; the dapp's own `gas`, fee and `nonce` fields are ignored (and the review says so). A transaction that would revert (gas estimation fails) or that the wallet cannot afford is not signed or sent, and the request stays open so it can be rejected. The request's `from` must be this wallet's address and any `chainId` in the transaction must match the request, or it is refused. Token transfers, approvals (with an unlimited-allowance warning) and `setApprovalForAll` are decoded in plain language; other contract calls show the target and function selector and are flagged as not decodable.
- **Messages and typed data.** `personal_sign` shows the message as text when it is readable and warns when it is binary (e.g. a bare hash). Typed data shows the domain and message, warns about permits, marketplace orders and near-unlimited amounts, and is refused when the domain's `chainId` differs from the request's chain. EIP-712 hashing is implemented in this repository and checked against the EIP's published vectors.
- **Answering twice is impossible.** A request is closed the moment it is approved, even if the dapp could not be reached afterwards, so an already-sent transaction can never be approved a second time. For Sui this holds too: if a `sui_signAndExecuteTransaction` submission fails or cannot be confirmed, the request is closed and the dapp is told it was not confirmed; the error returns the digest to look up with `get_transaction`. Requests expire after about 5 minutes (or the dapp's own expiry); proposals after 5 minutes; sessions after 7 days.
- **Sessions are in memory only.** Sessions last while the MCP server process runs. After a restart the dapp must connect again. Nothing about a session is written to disk, which keeps addresses out of any unencrypted file.
- **Dapp identity is self-declared.** The name and URL a dapp reports are not verified (WalletConnect's Verify API is not used); every proposal says so. Text from dapps is length-limited and stripped of control characters before it reaches the agent.
- **Privacy and transport.** All messages are end-to-end encrypted (X25519 key agreement, HKDF-SHA256, ChaCha20-Poly1305) as the protocol specifies; the relay sees only encrypted payloads and topics. The relay connection is `wss://` with certificate checking, authenticated with a fresh Ed25519 client key (JWT) per connection. A background thread keeps the relay connection so requests arrive while the agent is idle; they are picked up on the next `wc_*` call (call `wc_list_requests` to check).
- **Limits.** Not implemented: WalletConnect Auth / `wc_sessionAuthenticate` (sign-in only, no session), session updates by the wallet, and chain-switching helper methods (`wallet_switchEthereumChain`, `wallet_addEthereumChain`). Use `add_network` to give the wallet a network, then connect with that chain.

### Security design

- Vault encryption matches MetaMask's `browser-passworder`: PBKDF2-HMAC-SHA256 (900,000 iterations, 32-byte salt) derives an AES-256-GCM key (16-byte IV); the vault is `{data, iv, salt, keyMetadata}` in base64. The vault plaintext uses MetaMask's keyring layout (`HD Key Tree` / `Simple Key Pair`). Verified against vaults produced and read by WebCrypto.
- The wallet file holds only the name, blockchain and vault. Addresses, balances, networks, added tokens and keys exist only inside the encrypted vault. Files are `0600`, directories `0700`; writes are atomic and never overwrite exports.
- Seed phrases are always 24 words (BIP39, checksum-validated); Ethereum keys derive at `m/44'/60'/0'/0/0`, Sui keys (Ed25519, SLIP-0010) at `m/44'/784'/0'/0'/0'`.
- Passwords must be at least 8 characters (export keys 12). Five wrong passwords lock a wallet for 60 seconds (the same limiter covers Ethereum and Sui wallets). A tool for one blockchain given a wallet of the other is refused before anything is decrypted.
- Secrets are wiped from memory after each call where possible; request/response buffers are zeroed. Secrets are never logged and are never included in error messages.
- Text coming from third parties (token names, explorer data) is shortened and stripped of control characters before it reaches an agent; RPC URLs must be https (http only for localhost) and may not be literal private-network IPv4 addresses.
- No double payments on a lost broadcast response. Before every broadcast the signed transaction (hash, nonce, raw bytes; no keys) is saved in `$AGENT_SAIFU_HOME/pending/` and removed once the node answers. If the response is lost (timeout, network error), the record stays and the error says the outcome is unknown and not to retry. The next send from that wallet on that network first looks the transaction up: if the node has it, nothing is sent and the agent is told it was already accepted; if not, the identical signed bytes are resubmitted (same nonce, so never a second payment) and reported; if the node can't be reached, nothing is sent until it can be checked.

### Dependencies

Hashing (SHA-2, Keccak), HMAC, PBKDF2, HKDF, base64/base58/hex, BIP39/BIP32, SLIP-0010, RLP, ABI, EIP-191 and EIP-712 signing, transaction signing, the WalletConnect protocol logic, ZIP and JSON are implemented in this repository and tested against published vectors. Only primitives that are risky to hand-roll use crates, each checked against the RustSec advisory database (all advisories are fixed in the versions locked in `Cargo.lock`, apart from the informational `ring` notice below) and crates.io download counts:

- `k256` 0.14 (RustCrypto; secp256k1 ECDSA, constant-time) - ~90M downloads
- `aes-gcm` 0.11 (RustCrypto; NCC Group audited) - ~186M downloads
- `getrandom` 0.4 (OS randomness) - ~2.3B downloads
- `ureq` 3.5 with `rustls` (HTTPS for RPC / price / explorer requests, redirects disabled) - ~238M downloads
- `chacha20poly1305` 0.11 (RustCrypto; WalletConnect message encryption) - ~100M downloads
- `x25519-dalek` 3.0 (WalletConnect key agreement) and `ed25519-dalek` 3.0 (relay authentication), both dalek-cryptography - ~87M and ~248M downloads. The RustSec advisories for `ed25519-dalek` (RUSTSEC-2022-0093, fixed in 2.0) and `curve25519-dalek` (RUSTSEC-2024-0344, fixed in 4.1.3) are fixed in the locked versions.
- `tungstenite` 0.30 (WebSocket framing for the relay, handshake only, no TLS of its own) - ~360M downloads; its advisory RUSTSEC-2023-0065 is fixed since 0.20.1. TLS is the same `rustls` (ring) already used by `ureq`, with `webpki-roots` for certificates.

Sui support uses the official Sui Rust SDK (https://github.com/MystenLabs/sui-rust-sdk, Mysten Labs, Apache-2.0), as requested, instead of hand-rolling Sui's transaction encoding. Only SDK crates that are needed are used, with default features switched off where possible (no gRPC/tonic stack, no zkLogin, passkey or multisig code):

- `sui-sdk-types` 0.4 (Sui transaction/address/type definitions, BCS encoding, digests; ~359k downloads), `sui-crypto` 0.4 (Ed25519 keys, `suiprivkey` Bech32, transaction and personal-message signing in Sui's intent scheme; ~122k downloads) and `sui-transaction-builder` 0.4 (offline programmable-transaction builder, `intents` feature off; ~116k downloads). These have far fewer downloads than the other crates above because Sui's Rust SDK is young and specialised, but they are the SDK Mysten Labs publishes for this purpose; their signing and addressing output was also cross-checked against independent implementations (see the tests). Their dependencies are widely used RustCrypto/serde crates (`ed25519-dalek` 2.2 ~249M downloads, `curve25519-dalek` 4.1.3, `blake2` ~188M, `sha2`, `bcs` ~9.8M, `bech32` ~64M, `bs58` ~168M, `serde_with` ~412M, `roaring` ~59M, `bnum` ~21M).
- SLIP-0010 (Ed25519 key derivation), the MetaMask-style Sui vault, GraphQL access and the Cetus aggregator integration are implemented in this repository.
- Audit result (RustSec advisory database checked on 2026-10-24 against every package in `Cargo.lock`): no advisory applies to the locked versions of the new Sui dependencies (`ed25519-dalek` 2.2.0 and `curve25519-dalek` 4.1.3 are past RUSTSEC-2022-0093 and RUSTSEC-2024-0344). The audit also found two timing advisories published the day before, RUSTSEC-2026-0385 (`ctutils`) and RUSTSEC-2026-0387 (`crypto-bigint`), in crates that `k256` already pulled in for Ethereum signing; `Cargo.lock` now pins their patched versions (`ctutils` 0.4.5, `crypto-bigint` 0.7.6). One informational notice remains: `ring` 0.17.14 is flagged "unmaintained" (RUSTSEC-2025-0007) through `rustls`, which was already the case.

### External services

Ethereum mainnet RPC `ethereum-rpc.publicnode.com`, CoinGecko public API (prices), a Blockscout explorer (`eth.blockscout.com`, history), and an OpenOcean v4 API host (`open-api.de1.exchange`, swaps; no key). The WalletConnect relay (`relay.walletconnect.org`, needs a free project ID). Custom networks supply their own RPC and explorer. For Sui: the network's GraphQL endpoint (`graphql.mainnet.sui.io`, `graphql.testnet.sui.io` or `graphql.devnet.sui.io`; reads, dry runs, submission) and the Cetus aggregator (`api-sui.cetus.zone`, routes for swaps and indicative prices; no key); SUI prices also come from CoinGecko.

## Usage Examples

Note: The examples below are for development and use `cargo run -q`. To use the built executable instead, run it directly and type one JSON request per line, or pipe a request into it for a one-shot command: `echo '{...}' | ./executable`. MCP clients normally launch the executable themselves.

For development, run these from the `core/` folder. Each command starts the server, sends one request and exits; wallets persist on disk between commands. To keep test wallets away from your real ones, first run:

```bash
export AGENT_SAIFU_HOME=/tmp/agent-saifu-demo
```

Creating, unlocking and exporting wallets runs 900,000 PBKDF2 iterations, so those commands take a moment. Passwords are at least 8 characters. Responses are JSON-RPC; tool errors come back as a result with `"isError":true`.

### Server and metadata

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_info"}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' | cargo run -q
```

List every tool with its input schema:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | cargo run -q
```

### Ethereum Wallets

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"ethereum","wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"ethereum","wallet_name":"Second","password":"another-password-456"}}}' | cargo run -q
```

Names and blockchains only; no password:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_wallets"}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_address","arguments":{"wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

Import from a seed phrase (24 words; this is a well-known public test phrase, never use it for real funds):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"wallet_name":"Imported seed","password":"import-password-1","seed_phrase":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"}}}' | cargo run -q
```

Import from a private key (also a public test key):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"wallet_name":"Imported key","password":"import-password-2","private_key":"0x0000000000000000000000000000000000000000000000000000000000000001"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"rename_wallet","arguments":{"wallet_name":"Second","new_wallet_name":"Renamed","password":"another-password-456"}}}' | cargo run -q
```

Deleting is irreversible and needs `confirm`:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"delete_wallet","arguments":{"wallet_name":"Renamed","password":"another-password-456","confirm":true}}}' | cargo run -q
```

A wrong password returns an error (five failures lock the wallet for 60 seconds):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_address","arguments":{"wallet_name":"Demo","password":"wrong-password"}}}' | cargo run -q
```

### Secrets and export

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"view_seed_phrase","arguments":{"wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"view_private_key","arguments":{"wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

Writes `$AGENT_SAIFU_HOME/exports/demo-backup.zip` (existing files are never overwritten):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"export_wallet","arguments":{"wallet_name":"Demo","password":"demo-password-123","encryption_key":"my-export-encryption-key","file_name":"demo-backup.zip"}}}' | cargo run -q
```

### Balances, fees and reading the chain

These use live Ethereum mainnet (chain ID 1). A new wallet is empty, so its balances are zero. The "Imported key" wallet below has a public address that others may have funded.

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Imported key","password":"import-password-2","chain_id":1}}}' | cargo run -q
```

Include extra tokens (here the USDC contract) and check all of a wallet's networks by omitting `chain_id`:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Imported key","password":"import-password-2","chain_id":1,"token_contracts":["0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"]}}}' | cargo run -q
```

Read a smart contract without signing (USDC `symbol()` and a holder's `balanceOf`):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_smart_contract","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"contract":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","function":"symbol()","outputs":"string"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_smart_contract","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"contract":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","function":"balanceOf(address)","args":["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"],"outputs":"uint256"}}}' | cargo run -q
```

Estimate the gas fee for a native transfer (works on an empty wallet; the response notes that the value was ignored), then simulate an ERC-20 transfer of 1 USDC (an empty wallet reports `"success":false` with the revert reason, which is the point of simulating first):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"estimate_gas_fee","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"to":"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","asset":"native","amount":"0.01"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"simulate_transaction","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"to":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","asset":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","amount":"1"}}}' | cargo run -q
```

Look up one transaction and the wallet's history (history needs a Blockscout-compatible explorer):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"transaction_hash":"0xf3ba2c945345f1adce1b6d307350cb3e252dd72fb423d603faedc7fb16e77d67"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction_history","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"type":"transactions","limit":5}}}' | cargo run -q
```

### Sending and signing (spends real funds if the wallet is funded)

These only succeed on a funded wallet. On an empty wallet they return an "insufficient funds" error, which is a safe way to check the validation. Use a testnet network (see below) when trying them for real.

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"asset":"native","to":"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","amount":"0.001"}}}' | cargo run -q
```

Sign a contract call without broadcasting (omit `broadcast`, or set it to `false`); this one is an ERC-20 `approve` that the response flags as unlimited:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_contract_transaction","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"to":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","data":"0x095ea7b3000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","broadcast":false}}}' | cargo run -q
```

Revoke a token approval:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"revoke_token_approval","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","spender":"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","broadcast":false}}}' | cargo run -q
```

### Tokens

Add a token by contract address on a network the wallet has (chain ID and password are required). This one is Chainlink's LINK on Ethereum mainnet; it will then appear in balances even at zero:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add_token","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"contract":"0x514910771AF9Ca656af840dff83E8264EcF986CA"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"remove_token","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":1,"contract":"0x514910771AF9Ca656af840dff83E8264EcF986CA"}}}' | cargo run -q
```

### Swaps

Same-chain swaps through OpenOcean: no API key or setup needed. `0xA0b8...eB48` is USDC on Ethereum. The wallet needs the token (and a little ETH for gas).

Preview a swap of 25 USDC to ETH. Nothing is signed or sent:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_swap_quote","arguments":{"wallet_name":"Demo","password":"demo-password-123","from_chain_id":1,"from_token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","to_chain_id":1,"to_token":"native","amount":"25"}}}' | cargo run -q
```

Preview with a custom slippage (0.5%) and a different receiving address:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_swap_quote","arguments":{"wallet_name":"Demo","password":"demo-password-123","from_chain_id":1,"from_token":"native","to_chain_id":1,"to_token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","amount":"0.01","slippage_bps":50,"to_address":"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"}}}' | cargo run -q
```

Do the swap (spends real funds). `min_receive` is the `minimum_amount` from the quote, in whole units of the token you receive:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"swap_tokens","arguments":{"wallet_name":"Demo","password":"demo-password-123","from_chain_id":1,"from_token":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","to_chain_id":1,"to_token":"native","amount":"25","slippage_bps":50,"min_receive":"0.0102"}}}' | cargo run -q
```

Cross-chain swaps are not offered by OpenOcean's API; this returns an explanatory error:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_swap_quote","arguments":{"wallet_name":"Demo","password":"demo-password-123","from_chain_id":1,"from_token":"native","to_chain_id":8453,"to_token":"native","amount":"0.01"}}}' | cargo run -q
```

### Networks (layer 2s)

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_networks","arguments":{"wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

The RPC must report the chain ID you add it under (Base is 8453):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add_network","arguments":{"wallet_name":"Demo","password":"demo-password-123","network_name":"Base","chain_id":8453,"rpc_url":"https://mainnet.base.org","currency_symbol":"ETH","block_explorer_url":"https://base.blockscout.com"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"update_network","arguments":{"wallet_name":"Demo","password":"demo-password-123","network_name":"Base Mainnet","chain_id":8453,"rpc_url":"https://mainnet.base.org","currency_symbol":"ETH","block_explorer_url":"https://base.blockscout.com"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"delete_network","arguments":{"wallet_name":"Demo","password":"demo-password-123","chain_id":8453}}}' | cargo run -q
```

### WalletConnect (Ethereum, layer 2s and Sui)

Sessions live in the server process, so run these in one process (piped lines, or an MCP client that keeps the server running). Set the project ID first (see the WalletConnect section above) and use a wallet that has a network for the chain the dapp wants:

```bash
export WALLETCONNECT_PROJECT_ID=<your project id>
```

Connect: copy the `wc:` URI from the dapp, then pair, check the proposal and approve it with the wallet name, chain ID and password (a Sui wallet leaves out `chain_id`: `"arguments":{"proposal_id":"<id>","wallet_name":"DemoSui","password":"demo-password-123"}`; use the same shape for `wc_review_request` and `wc_approve_request`, where Sui requests are `sui_signTransaction`, `sui_signAndExecuteTransaction`, `sui_signPersonalMessage` and `sui_getAccounts`):

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_pair","arguments":{"uri":"wc:<topic>@2?relay-protocol=irn&symKey=<key>&expiryTimestamp=<ts>"}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wc_approve_session","arguments":{"proposal_id":"<proposal_id from step 1>","wallet_name":"Demo","chain_id":1,"password":"demo-password-123"}}}' \
  | cargo run -q
```

See what a connected dapp is asking for, review its costs and outcome, then approve (signs, and broadcasts for `eth_sendTransaction`) or reject:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_list_requests"}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_review_request","arguments":{"request_id":"<request_id>","wallet_name":"Demo","chain_id":1,"password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_approve_request","arguments":{"request_id":"<request_id>","wallet_name":"Demo","chain_id":1,"password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_reject_request","arguments":{"request_id":"<request_id>"}}}' | cargo run -q
```

Disconnect a dapp (use `wc_list_sessions` for the topic):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wc_disconnect_session","arguments":{"session_topic":"<session_topic>"}}}' | cargo run -q
```

### Sui wallets

Sui wallets use the same tools; there is no `chain_id` (the wallet is bound to the network it was created for). Create one (mainnet by default; add `"network":"testnet"` for a test wallet):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"sui","wallet_name":"Sui Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"sui","wallet_name":"Sui Testnet","password":"demo-password-123","network":"testnet"}}}' | cargo run -q
```

Import a seed phrase (the well-known public test phrase; never use it for real funds) or a `suiprivkey1...` key as shown by Slush or the Sui CLI:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"blockchain":"sui","wallet_name":"Sui Imported","password":"import-password-1","seed_phrase":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_address","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123"}}}' | cargo run -q
```

Every coin the address holds (coin objects and address balance), with USD values on mainnet:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123"}}}' | cargo run -q
```

Estimate and dry-run a transfer, then send it (spends real funds if the wallet is funded; the recipient is written out in full; `asset` is `native` for SUI or a coin type such as `0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC`):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"simulate_transaction","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","asset":"native","amount":"0.1","to":"0x0000000000000000000000000000000000000000000000000000000000000abc"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","asset":"native","amount":"0.1","to":"0x0000000000000000000000000000000000000000000000000000000000000abc"}}}' | cargo run -q
```

Sign a transaction built elsewhere (base64 BCS `TransactionData`; this is Wallet Standard `sui:signTransaction`; add `"broadcast":true` for `sui:signAndExecuteTransaction`) and a personal message (`sui:signPersonalMessage`):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_contract_transaction","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","transaction_bytes":"<base64 transaction bytes>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_personal_message","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","message":"Sign in to example.app"}}}' | cargo run -q
```

Look up a transaction by digest, and list recent activity:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","transaction_hash":"<digest>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction_history","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","limit":5}}}' | cargo run -q
```

Swap through the Cetus aggregator (mainnet wallets only; no `from_chain_id` / `to_chain_id`). Preview first, then swap with the quote's `minimum_amount` as `min_receive`:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_swap_quote","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","from_token":"native","to_token":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC","amount":"1"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"swap_tokens","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","from_token":"native","to_token":"0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC","amount":"1","slippage_bps":50,"min_receive":"0.95"}}}' | cargo run -q
```

The key in the format Sui wallets import (`suiprivkey1...`) and the network the wallet is bound to:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"view_private_key","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_networks","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123"}}}' | cargo run -q
```

### Several requests in one session

Piping multiple lines runs them in one server process (one JSON message per line; notifications get no reply):

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_info"}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_wallets"}}' \
  | cargo run -q
```

To pretty-print a response, append `| jq` (or `| python3 -m json.tool`) to any command. Tool output is itself JSON inside `result.content[0].text`, so `| jq -r '.result.content[0].text | fromjson'` shows just the tool's result.

## Tips for Agents

Lessons from using WalletConnect with Ethereum and Sui dapps (Sui was tested live against Cetus; the session, polling and expiry rules apply to both chains).

- **Keep the server running for the whole dapp session.** WalletConnect sessions and pending requests live only in the memory of the running `agent-saifu-core` process. Dapp requests arrive while it runs. If the process exits or restarts, the session is gone and the dapp must pair again with a fresh `wc:` URI. Start the server once, leave it open, and keep sending tool calls to the same process (for example through a fifo or an MCP client that holds the stdio connection open). Do not run `cargo run` per call for WalletConnect.
- **A `wc:` URI is single use and short lived.** It expires within minutes and cannot be reused after the server restarts. Ask the user for a new link after any restart, and pair and approve the session right away.
- **Poll, then review, then approve.** After the user triggers an action in the dapp, call `wc_list_requests` until the request appears, run `wc_review_request` (balance changes, simulation result, warnings), and only then `wc_approve_request`. Requests expire after about five minutes.
- **Refused requests never appear in the list.** A request with an unsupported chain, method or format is answered to the dapp with an error, and the server writes `walletconnect: refused a dapp request (...)` with the reason to stderr. Capture stderr when debugging an empty list.
- **Signing is not broadcasting.** On Sui, dapps such as Cetus use `sui_signTransaction`, so the wallet only returns the signature and the dapp submits the transaction. Check `dapp_notified: true` in the approval result and then confirm with `get_wallet_balances` or `get_transaction`. On Ethereum, `eth_sendTransaction` is broadcast by the wallet.
- **If a connection goes stale, restart.** A long-running server can lose its relay connection (`NotParticipant`, or `dapp_notified: false` on approval). Restart the server and pair again with a new link.
- **Every approval spends real funds.** Review each request, and approve only on the user's explicit instruction.

## Planned Features

None at the moment.
