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

The MCP server lives in `core/` (Rust, stdio JSON-RPC, run with `cargo run -q` from `core/`; an MCP client launches the binary). Wallet logic for Ethereum is in `core/src/eth/`, for Sui in `core/src/sui/`, for Solana in `core/src/solana/`, for Bitcoin in `core/src/bitcoin/`. Wallets are stored in `$AGENT_SAIFU_HOME` (default `~/.agent-saifu`).

### MCP tools

Every tool that touches a wallet needs the wallet's password; the wallet is unlocked only for that single call and is locked again when it returns. Unlocking one wallet never unlocks another. Wallet names are unique (case-insensitive).

Four blockchains are supported: **Ethereum** (and EVM chains, selected per call with `chain_id`), **Sui**, **Solana** and **Bitcoin** (a Sui, Solana or Bitcoin wallet is bound to one network, so there is no `chain_id`). The tool is chosen by the wallet it is given: a tool shared by several reads the wallet's blockchain and does the right thing. The next table says which tools work with which wallet; the descriptions below say how each behaves (Ethereum unless a "Sui:", "Solana:" or "Bitcoin:" note says otherwise).

| Tool | Ethereum | Sui | Solana | Bitcoin | Notes |
|---|---|---|---|---|---|
| `get_info` | yes | yes | yes | yes | |
| `create_wallet` | yes | yes | yes | yes | Sui and Solana take an optional `network` (`mainnet` default, `testnet`, `devnet`). Bitcoin takes an optional `network` too (`mainnet` default, `testnet` = testnet4, `signet`). |
| `import_wallet` | yes | yes | yes | yes | Sui: 24-word seed phrase, `suiprivkey1...` key or 64-hex Ed25519 key. Solana: 24-word seed phrase, the base58 key Phantom shows, a Solana CLI keypair byte list or hex. Bitcoin: 24-word seed phrase, a compressed WIF key or 64-hex key. |
| `list_wallets`, `rename_wallet`, `delete_wallet` | yes | yes | yes | yes | |
| `get_wallet_address` | yes | yes | yes | yes | |
| `get_wallet_balances` | yes | yes | yes | yes | Sui lists every coin type held, Solana SOL and every SPL / Token-2022 token; no `chain_id` / `token_contracts`. Bitcoin: BTC with confirmed and unconfirmed amounts; no `chain_id` / `token_contracts`. |
| `send_asset` | yes | yes | yes | yes | Sui: SUI or any coin type, optional `gas_budget`; Solana: SOL or any token mint, optional `priority_fee`; no `chain_id` / `gas_limit`. Bitcoin: BTC only (`asset` = `native`), optional `fee_rate` / `confirmation_target`; no `chain_id` / `gas_limit`. |
| `sign_contract_transaction` | yes | yes | yes | **no** | Sui and Solana: sign a complete transaction (`transaction_bytes`); `broadcast=true` is sign-and-send. Bitcoin wallets do not sign PSBTs or other externally built transactions; use `send_asset`. |
| `sign_personal_message` | yes (EIP-191) | yes | yes (raw Ed25519) | yes (BIP 322) | added for the Wallet Standard `signPersonalMessage` / Solana `signMessage` |
| `estimate_gas_fee`, `simulate_transaction` | yes | yes | yes | yes | Sui and Solana: a transfer, or given `transaction_bytes`; no `value` / `data`. A transfer takes the same optional `gas_budget` (Sui) / `priority_fee` (Solana) as `send_asset`; `transaction_bytes` carry their own and refuse them. Bitcoin: a payment (`to` + `amount`, plus optional `fee_rate` / `confirmation_target`); `estimate_gas_fee` without a payment lists the current rates; no `value` / `data` / `transaction_bytes`. |
| `get_transaction` | yes | yes | yes | yes | Sui: `transaction_hash` is the base58 digest; Solana: the base58 signature. Bitcoin: `transaction_hash` is the 64-hex-digit transaction id. |
| `get_transaction_history` | yes | yes | yes | yes | Sui and Solana: transactions only (no `token_transfers`). Bitcoin: transactions only. |
| `list_networks` | yes | yes | yes | yes | Sui and Solana: the one network the wallet is bound to. Bitcoin: the one network the wallet is bound to. |
| `get_swap_quote`, `swap_tokens` | yes (OpenOcean) | yes (Cetus, mainnet) | yes (Jupiter, mainnet) | **no** | Sui and Solana: no `from_chain_id` / `to_chain_id`; Solana: no `to_address`. Sui takes an optional `gas_budget` and Solana an optional `priority_fee`. No aggregator serves native Bitcoin. |
| `view_seed_phrase`, `view_private_key`, `export_wallet` | yes | yes | yes | yes | Sui keys are shown as `suiprivkey1...` (plus hex), Solana keys as the base58 keypair Phantom imports (plus hex). Bitcoin keys are shown as compressed WIF (plus hex). |
| `read_smart_contract` | yes | **no** | **no** | **no** | Sui and Solana have no equivalent call-by-signature; Move calls and Solana instructions go through `sign_contract_transaction` |
| `add_network`, `update_network`, `delete_network` | yes | **no** | **no** | **no** | a Sui or Solana wallet is bound to its one network (Solana has no layer 2s to add). A Bitcoin wallet is bound to its one network too. |
| `add_token`, `remove_token` | yes | **no** | **no** | **no** | a Sui or Solana wallet lists every coin or token it holds automatically. Bitcoin has no tokens. |
| `revoke_token_approval` | yes | **no** | **no** | **no** | Sui has no token allowances; Solana delegates are shown by `simulate_transaction` / `wc_review_request` |
| `wc_*` (WalletConnect) | yes | yes | yes | **no** | Sui uses the `sui` WalletConnect namespace and Solana the `solana` namespace (see "WalletConnect (any dapp)"); the Sui signing tools also mirror the Wallet Standard (see "Sui wallets"), and Solana follows Phantom (see "Solana wallets"). Bitcoin wallets are refused by these tools. |

A wallet given to a tool for another blockchain is refused with a clear message before anything is decrypted; chain-specific arguments given to the wrong wallet (for example `chain_id` for Sui or Solana) are refused too.

- `get_info` - returns "All-in-one crypto wallet for Agents. Built from the ground up in Rust." and "Version 0.0".
- `create_wallet` (blockchain = `ethereum`, `sui`, `solana` or `bitcoin`, wallet_name, password, optional network) - new wallet with a 24-word seed phrase in a password-encrypted vault. Multiple wallets are supported. `network` is for Sui, Solana and Bitcoin only (Bitcoin: `mainnet`, `testnet` or `signet`).
- `import_wallet` (wallet_name, password, seed_phrase *or* private_key, optional blockchain / network) - add an existing wallet; `blockchain` defaults to `ethereum`.
- `list_wallets` - names and blockchains only; no password.
- `rename_wallet` (wallet_name, new_wallet_name, password) - added beyond the original list.
- `delete_wallet` (wallet_name, password, confirm=true) - added beyond the original list; irreversible.
- `get_wallet_address` (wallet_name, password)
- `get_wallet_balances` (wallet_name, password, optional chain_id / token_contracts) - each asset with amount, USDC value, and chain ID, across all of the wallet's networks. Checks the native coin, a built-in list of well-known tokens, tokens saved with `add_token`, and any token contracts you pass for a one-off check. `usdc_value` is null when no real market price exists (testnets, unlisted tokens).
- `send_asset` (wallet_name, chain_id, asset = `native` or token contract, to, amount, password) - signs and broadcasts. Bitcoin: `asset` is `native`, `amount` is in BTC, and optional `fee_rate` (sat/vB) or `confirmation_target` (blocks) chooses the fee. Solana: optional `priority_fee` (whole SOL, total). Sui: optional `gas_budget` (whole SUI).
- `sign_contract_transaction` (wallet_name, chain_id, password, to, data, optional value / gas_limit / broadcast) - signs a smart-contract call; only broadcasts if `broadcast=true`. Decodes token transfers/approvals and warns about unlimited approvals.
- `sign_personal_message` (wallet_name, password, message, optional message_format = `text` / `hex` / `base64`) - added for Sui's Wallet Standard `signPersonalMessage` and offered for both blockchains: Ethereum signs with EIP-191 `personal_sign`; Sui signs with Sui's personal-message intent. Nothing is sent and no network access is needed. The response shows the message text (or "(binary data)").
- `estimate_gas_fee` (wallet_name, chain_id, password, to, optional value / data, or asset + amount) - gas, fee in native currency and USDC. Bitcoin: `to` + `amount` for the exact fee of a payment, or nothing for the current rates by confirmation target.
- `simulate_transaction` (same parameters as estimate) - added beyond the original list; dry-runs the transaction before signing and reports success or the revert reason, fees, affordability and a decoded description. Bitcoin has no execution to run, so the payment is fully built and signed in memory and checked instead (see "Bitcoin wallets").
- `read_smart_contract` (wallet_name, chain_id, password, contract, function, optional args / outputs) - added beyond the original list; read-only call, nothing is signed.
- `get_transaction` (wallet_name, chain_id, password, transaction_hash) - added beyond the original list; status, confirmations, fee paid.
- `get_transaction_history` (wallet_name, chain_id, password, optional type / limit) - added beyond the original list; uses the network's Blockscout-compatible explorer API.
- `add_network`, `update_network` (wallet_name, network_name, chain_id, rpc_url, currency_symbol, block_explorer_url, password), `delete_network` (wallet_name, chain_id, password) - per-wallet layer 2 networks, stored inside the encrypted vault. Ethereum mainnet is built in and cannot be changed or deleted. The RPC must report the chain ID it is added under.
- `list_networks` (wallet_name, password) - helper for viewing a wallet's networks. Deleting a network also removes the tokens added on it.
- `add_token` (wallet_name, chain_id, contract, password) - added beyond the original list; saves an ERC-20 token to the wallet on that network (any network the wallet has). The contract is checked on-chain and its symbol and decimals are read from it. Added tokens always appear in `get_wallet_balances`, even at zero. Stored in the encrypted vault; max 50 per network.
- `remove_token` (wallet_name, chain_id, contract, password) - added beyond the original list; stops tracking an added token (no funds move). Built-in well-known tokens cannot be removed.
- `get_swap_quote` (wallet_name, password, from_chain_id, from_token, to_chain_id, to_token, amount, optional slippage_bps / to_address) - added by request; previews a swap (OpenOcean on Ethereum, Cetus on Sui, Jupiter on Solana; not available for Bitcoin) without signing or sending anything: how much of the new token you will get (expected and guaranteed minimum), the fees (service fee, estimated network fee), the route, price impact, and whether an approval or more balance is needed.
- `swap_tokens` (same parameters plus optional min_receive) - swaps and broadcasts (Ethereum: OpenOcean; Sui: Cetus; Solana: Jupiter). Tokens are a contract address or `native`. If the sold token needs an approval, exactly the sold amount is approved first (a non-zero allowance is reset to zero first, as USDT requires) and confirmed before the swap is sent. `min_receive` is a safety floor: if the fresh quote's guaranteed minimum is lower, nothing is sent.
- `view_seed_phrase`, `view_private_key` (wallet_name, password)
- `export_wallet` (wallet_name, password, encryption_key, optional file_name) - writes a ZIP to `exports/` whose single entry (`agent-saifu-wallet-export.json`) is the seed phrase and private key encrypted with MetaMask's vault scheme under the given key. Never overwrites files. The ZIP itself has no ZIP password; the entry is a MetaMask-style vault (`{data, iv, salt, keyMetadata}`, PBKDF2-HMAC-SHA256 + AES-256-GCM). **To open it later you need only the ZIP and the `encryption_key` used when exporting.** The wallet's password is needed only to create the export, not to open it, and neither key is stored anywhere, so a lost `encryption_key` cannot be recovered. The key must be at least 12 characters.
- `revoke_token_approval` (wallet_name, chain_id, password, token, spender) - useful for revoking smart-contract token approvals (sets the ERC-20 allowance to zero).
- `wc_pair` (uri, optional wait_seconds) - added by request; connects to any dapp over WalletConnect v2, Ethereum, Sui or Solana (not Bitcoin). Give it the `wc:` URI the dapp shows. It waits for the dapp's session proposal and returns what the dapp asks for (chains, methods, whether this wallet can satisfy it). Nothing is shared until the proposal is approved. No wallet is unlocked.
- `wc_list_proposals`, `wc_reject_session` (proposal_id) - waiting connection proposals; decline one.
- `wc_approve_session` (proposal_id, wallet_name, password, chain_id for Ethereum wallets, optional additional_chain_ids) - approves a proposal with a wallet; this is the only step that reveals the address, and only to the dapp. Ethereum: the chains must cover everything the dapp requires and may only include chains the dapp asked for; the wallet needs a network for each. Sui: omit `chain_id` / `additional_chain_ids`; the dapp is connected on the wallet's own network (`sui:mainnet`, `sui:testnet` or `sui:devnet`). Solana: the same; the dapp is connected on the wallet's own cluster (`solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` for mainnet, `solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1` for devnet, `solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z` for testnet).
- `wc_list_sessions`, `wc_disconnect_session` (session_topic) - connected dapps (wallet, chains, methods; never addresses); end one.
- `wc_list_requests` - what connected dapps are waiting on, with a summary (decoded token transfers/approvals, message text, typed data). No password; the signer's address is left out.
- `wc_review_request` (request_id, wallet_name, password, chain_id for Ethereum wallets) - the expected costs and outcome. For transactions: a dry run (would it succeed or revert), what is sent or approved in plain language (`expected_outcome`), the network fee (max and expected, in the native coin and USDC), and whether the wallet can afford it. For messages and typed data: exactly what would be signed, with warnings. Signs and sends nothing.
- `wc_approve_request` (request_id, wallet_name, password, chain_id for Ethereum wallets) - signs and answers the dapp. `eth_sendTransaction` is signed and broadcast; `eth_signTransaction` only signs; `personal_sign` and `eth_signTypedData(_v4)` return a signature. Sui methods are listed under "WalletConnect (any dapp)", as are the Solana ones.
- `wc_reject_request` (request_id, optional reason) - tells the dapp the user rejected it. Nothing is signed. No password.

### Swaps (OpenOcean)

- **No API key, account or environment variable.** Swaps use an OpenOcean v4 swap API host (`https://open-api.de1.exchange/v4`, endpoint `/:chain/swap`). The host is the `OPENOCEAN_API_BASE_URL` setting in [core/src/swap_config.rs](core/src/swap_config.rs). The host supplies the transaction data the wallet signs, but the wallet decodes that data and refuses anything that does not match your request (see the safety checks below).
- **Same-chain only.** OpenOcean's API swaps within one chain; its cross-chain swaps exist only in its web widget. So `to_chain_id` must equal `from_chain_id` (a different value returns an explanatory error), and there is no `to_blockchain` option: swaps such as ETH to BTC or SUI are not possible through this API. What *is* supported is `to_address`: an optional address on the same chain that receives the bought tokens instead of the wallet itself. Solana is the one non-EVM chain served through this host (see "Swaps on Solana (OpenOcean)").
- **Chains:** the 41 chains OpenOcean lists for its swap API (Ethereum, BNB Chain, Base, Polygon, Arbitrum, Optimism, Avalanche, and others). The wallet must have the network (see `add_network`). The wallet only ever approves or sends to OpenOcean's published router contract.
- **Costs to know about:** OpenOcean also applies its own tiered fee on public-API swaps (0.01% stable pairs, 0.1% correlated, 0.25% common, 0.5% exotic tokens).
- **Safety checks before signing:** the quote must be for the requested tokens (decimals included), may not sell more than requested or send extra native currency, must target OpenOcean's router, must meet `min_receive`, and may not have a price impact worse than 5%. The router calldata is also decoded locally (`swap(...)` on OpenOcean's router) and must sell the requested token and the quoted amount, buy the requested token, **deliver it to the wallet (or the requested `to_address`)** and guarantee a minimum return at least the quote's (and your `min_receive`); the quote's guaranteed minimum may also not be looser than the requested slippage (plus 0.5%). So a dishonest or compromised API host cannot redirect the output or hide a bad price; calldata the wallet cannot decode is refused and nothing is sent. The approval is for exactly the sold amount, never unlimited. OpenOcean text (route names, errors) is length-limited and stripped of control characters before it reaches the agent.
- **Rate limit:** 2 requests per second on the free API. **Known issue:** OpenOcean's Cloudflare bot protection challenged the quote and swap endpoints (HTTP 403) when tested from a cloud server, so swaps may fail from some networks; the tools then say so and send nothing.

Ethereum wallets use the OpenOcean aggregator (this section); Sui wallets use the Cetus aggregator (see "Swaps on Sui (Cetus)"); Solana wallets use the Jupiter aggregator (see "Swaps on Solana (Jupiter)"). Chain IDs select Ethereum networks such as Base or Arbitrum; Sui and Solana have no chain IDs (a wallet is bound to a network), so their tools do not take one.

### Solana wallets

Solana wallets are created and used like Ethereum and Sui wallets (same vault encryption, per-call unlocking, password rate limiting, tools), with these specifics. They were built following Phantom's behaviour and documentation (derivation paths, `signMessage`, `signAndSendTransaction` as the preferred way to send, transaction simulation before approval) and the Solana and Reown (WalletConnect) RPC references. The cryptography uses crates the project already depends on (`ed25519-dalek`); transaction encoding, program-derived addresses and the JSON-RPC client are implemented in this repository, with no Solana SDK.

- **Keys.** Ed25519. A 24-word BIP39 seed phrase derives the key with SLIP-0010 at `m/44'/501'/0'/0'`, the path Phantom uses for its first account, so a phrase from Phantom gives the same address (checked against Phantom's published address for the standard test phrase, and independently re-derived in Python for a 24-word phrase; the signing and transaction layouts were checked against an independent implementation and against a live node). Imported private keys may be the base58 keypair Phantom and Solflare show (64 bytes: the secret, then the public key, which must match), a Solana CLI keypair file's list of bytes, or hex of the 32-byte secret. `view_private_key` shows the base58 keypair (plus hex), which Phantom imports. Accounts of phrases that used the older `m/44'/501'/0'` path are not covered (Phantom only finds them when there is on-chain activity).
- **Self-custody.** The vault is the same PBKDF2-SHA256 900,000 iterations + AES-256-GCM format with MetaMask's keyring layout; the cluster is stored inside the vault. The wallet file holds only the name, blockchain and vault.
- **One network per wallet.** Like a Sui wallet, a Solana wallet is bound to one cluster (`mainnet` by default, `devnet` or `testnet`, recorded inside the vault; `mainnet-beta` is accepted as a name for mainnet). Every call first checks that the RPC endpoint really serves that cluster (by its genesis hash), and a transaction whose blockhash the cluster does not recognise is refused, so a devnet key never signs a mainnet transaction. Create separate wallets for separate clusters. Solana has no layer 2s to add, so `add_network` and friends do not apply.
- **Node access.** Reads, simulation and submission use Solana's JSON-RPC over HTTPS with certificate checking. The endpoints are the public Solana Foundation nodes, set in [core/src/solana_config.rs](core/src/solana_config.rs); replace them with your own provider's URL for heavier use (they are rate limited). A provider URL may contain a secret, so tools only ever show its host.
- **Balances.** SOL plus every SPL Token and Token-2022 token the address holds (a token's balance adds up all of its accounts). `usdc_value` comes from CoinGecko on mainnet (SOL, USDC at 1, and the first 30 tokens CoinGecko prices); it is `null` on devnet and testnet. Tokens are named from a short built-in list (USDC, USDT, PYUSD, wSOL, mSOL, JitoSOL, JUP, BONK, RAY, WIF) and otherwise shown by a shortened mint address and flagged as unrecognised, because Solana wallets collect spam airdrops. Names and prices are for display only; amounts that are sent always use the decimals the token's mint reports.
- **Sending.** `send_asset` sends SOL or any token by mint address. A token send uses `TransferChecked` (the token program verifies the mint and decimals), picks the wallet's account holding enough of the token, derives the recipient's associated token account and opens it (`CreateIdempotent`, paid by the sender, about 0.00204 SOL) when it does not exist. Recipients that are program addresses (including the system program, the burn address) and the wallet itself are refused. The transaction is dry-run first, given a compute-unit limit from the dry run plus headroom and a priority fee, signed, submitted with the node's own preflight check, and the wallet waits for `confirmed`. A transaction that would fail is never signed.
- **Priority fee.** By default the priority fee follows recent network demand (never more than 0.00005 SOL). `priority_fee` (a text amount in whole SOL, e.g. `"0.0001"`) sets the total priority fee instead, on top of the base fee: it is spread over the transaction's compute-unit limit as the price per unit (rounded down, so the total never exceeds what was asked for), and `"0"` sends with none. It is accepted by `send_asset`, `estimate_gas_fee`, `simulate_transaction`, `get_swap_quote` and `swap_tokens`; the wallet must be able to afford it on top of the transfer, and a value above 0.01 SOL is refused as a likely mistake (a swap's default is Jupiter's suggestion or the cluster's recent fee, never above 250,000 micro-lamports per unit). `send_asset`, `estimate_gas_fee` and `simulate_transaction` report `priority_fee_sol`. It is not accepted with `transaction_bytes` (including `sign_contract_transaction`), which carry their own compute-budget instructions, and is refused for non-Solana wallets.
- **Lost answers.** If the answer to a submission is lost, the transaction's signature (known locally) is looked up first: if the cluster has it, it is reported with its status; if the node answered with a rejection and has no such transaction, it is reported as rejected and nothing was spent; otherwise the error returns the signature and says to check `get_transaction` before building another transaction. Submitting the same signed transaction again is harmless, but a transaction expires after about a minute.
- **Per-action unlock.** The wallet is unlocked only for the call, like every other tool.
- **What `sign_contract_transaction` checks for Solana.** The transaction (legacy or version 0, base64 or base58; at most 1,232 bytes) must name this wallet as one of its signers and its blockhash must be valid on the wallet's cluster (a durable-nonce transaction is accepted). Address lookup tables are read from the node so every account can be named. The response decodes every instruction in plain language (system and token transfers, token approvals with a warning when unlimited, authority changes, account closing, assignments, memos, compute budget; any other program is shown as unrecognised with a warning), reports when the wallet is not the fee payer or other signatures are missing, and shows the wallet's balance changes from a dry run (SOL and every token, the network fee included; new spending approvals or ownership changes of the wallet's token accounts are warned about). By default it only signs, filling in the wallet's signature and leaving other signers' slots as they are; `broadcast=true` is sign-and-send and is refused unless the dry run succeeds and no other signature is missing.
- **Messages.** `sign_personal_message` signs the raw message bytes with the Ed25519 key, as Phantom's `signMessage` does (base58 and base64 signatures). Bytes that parse as a Solana transaction or message are refused, because signing them would sign a transaction.
- **Simulation.** `simulate_transaction` and `estimate_gas_fee` accept a transfer or `transaction_bytes`. They work on a wallet that cannot afford the transfer (the shortfall is reported as the dry run's error). Tool-built transfers are simulated again after the compute limit and priority fee are added, so the reported balance changes include the final network fee. The balance changes come from comparing every writable account before and after the simulated transaction, so they are right whatever programs are called.
- **Lookups.** `get_transaction` takes the base58 signature and reports status, slot, fee, the wallet's SOL and token changes, confirmation level and an explorer link. `get_transaction_history` lists the signatures that touched the address (status, time, memo; memos are untrusted text).
- **Text from the network** (error messages, memos, logs) is length-limited and stripped of control characters before it reaches the agent.

### Swaps on Solana (Jupiter)

Solana swaps use Jupiter's Swap API v2 (`JUPITER_API_BASE_URL` in [core/src/swap_config.rs](core/src/swap_config.rs), default `https://api.jup.ag/swap/v2`) through its **keyless** access: no API key, account or environment variable, at about one request every two seconds (a `429` is reported as a rate-limit message to retry shortly). OpenOcean is not used for Solana (its host stopped supporting the chain). The `/build` endpoint (Jupiter's "Router" path, Metis routing) answers with raw swap instructions rather than a transaction, and charges no Jupiter swap fee; the wallet assembles the transaction itself.

- **Mainnet only, same-chain.** `from_token` / `to_token` are `native` (SOL) or a token mint address; wrapped SOL is not accepted as a token (Jupiter addresses SOL by that mint, and wraps and unwraps it inside the swap). There is no `to_address` and no cross-chain option; the bought tokens always arrive in the wallet. `slippage_bps` (default 100, 5 to 1000) and `min_receive` work as on Ethereum. Swaps are exact-input: the amount is what is sold.
- **The wallet builds the transaction.** Jupiter's setup, swap and cleanup instructions are placed after a compute-unit limit sized by a dry run of the swap (the dry run's usage plus headroom) and a priority fee (Jupiter's suggestion, or the cluster's recent fee, never above 250,000 micro-lamports per unit). The blockhash comes from the wallet's own node. Jupiter's address lookup tables are used to fit the route, but the wallet reads those tables on chain and refuses the swap unless they hold exactly the accounts Jupiter described. Routes differ a lot in size: when the assembled transaction would not fit Solana's 1,232-byte limit (more often for a wallet that has no token accounts yet), the swap is requested again with `maxAccounts` 40 and then 28 so that Jupiter picks a simpler route, and refused with an explanation if none fits.
- **Nothing from Jupiter is trusted.** Before signing: the quote must match the request (same tokens, exact input, no more sold than requested, minimum no looser than the slippage plus 0.5%, `min_receive`, price impact at most 5%); the swap instruction itself is decoded, as the router calldata is on Ethereum: it must be a call to Jupiter's aggregator program (`JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4`) in the `route_v2` or `shared_accounts_route_v2` form, whose terms sit at a fixed offset (older forms, any other instruction, and a second call to the aggregator are refused), selling exactly the quoted amount, delivering to the wallet's own token account for the bought token, charging exactly the configured platform fee and keeping no positive slippage. The minimum the program enforces on-chain (the instruction's quoted output less its slippage, rounded down) is what the slippage limit and `min_receive` are checked against, and what `get_swap_quote` reports as `minimum_amount`, so they still hold if the price moves after the simulation. The wallet must be the fee payer and the only missing signer; no instruction may approve a delegate, change an authority or assign the wallet's account; the blockhash must be valid; and a simulation of the exact instructions must show that the wallet spends no more than requested (for SOL, plus network fees and the deposit for any token account it opens), receives at least the quoted minimum, and loses nothing of any other token (every writable account is compared, including accounts loaded from lookup tables). Any new spending approval or ownership change in the simulation stops the swap.
- **Live verification.** On mainnet (2026-11-09) the keyless endpoint returned SOL to USDC, USDC to SOL and USDC to JUP swaps; each assembled transaction was dry-run by a node against real accounts, resolved against the real lookup tables and passed every check, and with a fee configured the simulated post-state showed the fee arriving in the recipient's token account (USDC, wrapped SOL and JUP respectively). No funded swap was sent.

### Sui wallets

Sui wallets are created and used like Ethereum wallets (same vault encryption, per-call unlocking, password rate limiting, tools), with these specifics. They were built following Sui's documentation on the [Wallet Standard](https://docs.sui.io/onchain-finance/asset-custody/wallets/wallet-standard), [self-custodial wallets](https://docs.sui.io/onchain-finance/asset-custody/wallets/self-custody) and its [security best practices](https://docs.sui.io/develop/security/best-practices) for key management, and use the official [Sui Rust SDK](https://github.com/MystenLabs/sui-rust-sdk).

- **Keys.** Ed25519, Sui's default scheme. A 24-word BIP39 seed phrase derives the key with SLIP-0010 at `m/44'/784'/0'/0'/0'` (the path every Sui wallet uses, so a phrase from Slush or the Sui CLI gives the same address). The address is the BLAKE2b-256 hash of `0x00` plus the public key, computed by the Sui SDK. Both the derivation and the signing formats are checked against an independent implementation (see the tests). Imported keys may be a `suiprivkey1...` Bech32 string, the Sui CLI keystore's base64 `flag || key`, or 64 hex characters; only Ed25519 keys are accepted. `view_private_key` / `export_wallet` give the `suiprivkey1...` form that Sui wallets import, plus hex.
- **Self-custody.** Keys exist only inside the password-encrypted vault (same PBKDF2-SHA256 900,000 iterations + AES-256-GCM format as the Ethereum wallets, with MetaMask's keyring layout), are decrypted for one call and wiped afterwards, and never leave the machine. The wallet file holds only the name, blockchain and vault: the address, the network and the keys are inside the vault.
- **One network per wallet.** Sui's guidance is to keep Testnet and Mainnet keys separate so a test key can never sign a production transaction. A Sui wallet is therefore bound to one network (`mainnet` by default; `testnet` or `devnet` with `network` on `create_wallet` / `import_wallet`), recorded inside the vault. Every call first checks that the endpoint really is that network (mainnet and testnet by their chain identifier) and signing refuses a transaction that names another network. Create separate wallets for separate networks, and separate wallets for unrelated activity (everything on Sui is public).
- **Hardware wallets and multisig are not supported.** Sui's guidance prefers a hardware wallet or multisig for privileged (admin/deployer) keys; this wallet is a hot wallet and is meant for routine operation. Keep recovery material offline: back up the seed phrase with `view_seed_phrase`, never in plaintext or screenshots, and send a small test transfer before moving a large amount.
- **Verify recipients.** A recipient must be written out in full (`0x` + 64 hex characters, the zero address is refused): a mistyped short address would otherwise send coins somewhere unrecoverable.
- **Node access.** Public Sui nodes retired JSON-RPC, so reads, dry runs and submission go through Sui's GraphQL API (`graphql.mainnet.sui.io`, `graphql.testnet.sui.io`, `graphql.devnet.sui.io`) over HTTPS with certificate checking. Transactions are built offline with the Sui SDK's `TransactionBuilder` from data read over GraphQL.
- **Coins and address balances.** A balance is the sum of the coin objects the address owns and its *address balance* (Sui's newer account-style balance; many wallets hold SUI only that way). Both are shown (`coin_objects_raw`, `address_balance_raw`) and both can be spent and can pay gas: gas is paid from coin objects when they cover it, otherwise from the address balance (a transaction with an `expiration` bound to the current epoch and the chain).
- **Gas.** Gas price is the network's reference price. By default the budget comes from a dry run (computation + a margin + storage - rebate, as Sui's TypeScript SDK does, capped at the protocol's 50 SUI) and the final transaction is dry-run again before signing. `gas_budget` (a text amount in whole SUI, e.g. `"0.05"`) sets the budget instead: it is the most the transaction may spend on gas (only what is used is charged), at least 2,000 gas units at the reference price and at most 50 SUI, and the wallet must hold it. It is accepted by `send_asset`, `estimate_gas_fee`, `simulate_transaction`, `get_swap_quote` and `swap_tokens`, and `send_asset` reports `gas_budget_mist`. A budget too low for the transaction makes the final dry run fail, so nothing is signed (the error says what a dry run suggests). It is not accepted with `transaction_bytes` (including `sign_contract_transaction`), which already carry their own budget, and is refused for non-Sui wallets. A transaction whose dry run fails is never signed.
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

### Bitcoin wallets

Bitcoin wallets are created and used like the other chains' wallets (same vault encryption, per-call unlocking, password rate limiting, tools), with these specifics. They follow Bitcoin Core's wallet behaviour and the relevant BIPs (32, 39, 84, 125, 141, 143, 173, 322, 350) wherever that matters for safety. The cryptography uses `k256` (already a dependency for Ethereum); everything else (RIPEMD-160, Bech32/Bech32m, Base58Check, transaction encoding, BIP 143 signature hashes, coin selection, the Esplora client) is implemented in this repository, with no Bitcoin library.

- **One address.** A Bitcoin wallet has exactly one address, native SegWit (P2WPKH, `bc1q...`), the type Bitcoin Core creates by default. A 24-word BIP 39 seed phrase derives the key with BIP 32 at the BIP 84 path `m/84'/0'/0'/0/0` (`m/84'/1'/0'/0/0` on the test networks, SLIP-44 coin type 1), the first receiving address of the first account in Sparrow, Electrum (BIP 39 mode), Ledger, Trezor and other wallets, so a phrase from them gives the same address (checked against BIP 84's published test vector for the standard test phrase, and independently re-derived in Python for a 24-word phrase). Change goes back to the same address; there are no extra addresses, account discovery or coin control. Imported private keys may be a compressed WIF key (`K...` / `L...`, `c...` on test networks) or 64 hex digits; an uncompressed WIF key is refused (a native SegWit address cannot use one) and so is a WIF key of another network. `view_private_key` shows the WIF key (plus hex).
- **Self-custody and vault.** The vault is the same PBKDF2-SHA256 900,000 iterations + AES-256-GCM format with MetaMask's keyring layout used by every other wallet (Bitcoin Core's own wallet encryption is AES-256-CBC with a weaker key derivation, so this project's vault was kept). The network is stored inside the vault; the wallet file holds only the name, blockchain and vault, so the address is not visible until unlock. The same attempt limiter (five wrong passwords lock a wallet for 60 seconds) covers Bitcoin wallets, and unlocking one wallet never unlocks another.
- **One network per wallet.** `mainnet` (default), `testnet` (testnet4, BIP 94; the old testnet3 is retired) or `signet`, recorded inside the vault. An address of another network is refused as a recipient (the `bc` / `tb` prefix and the legacy version bytes are checked), a WIF key of another network is refused on import, and every call first checks that the API really follows that chain by the hash of its genesis block, so a testnet key never signs for mainnet.
- **Recipients.** P2PKH (`1...`), P2SH (`3...`), P2WPKH (`bc1q...`), P2WSH (`bc1q...`, 62 characters) and P2TR (`bc1p...`) addresses are accepted, with the BIP 173 and BIP 350 rules (Bech32 for version 0, Bech32m for version 1, mixed case and wrong checksums refused). SegWit versions above 1 are refused because funds sent to a witness program nothing can spend yet might be unrecoverable. Sending to the wallet's own address is refused.
- **Node access.** Balances, unspent coins, fee estimates, transactions and broadcasting use an Esplora-compatible HTTP API over HTTPS with certificate checking. The endpoints are `blockstream.info` (mainnet) and `mempool.space` (testnet4 and signet), set in [core/src/bitcoin_config.rs](core/src/bitcoin_config.rs); replace them with your own Esplora's URL (https, or http for localhost only) for privacy or heavier use. The server is untrusted: its answers are parsed strictly and bounded, the wallet builds and signs the transaction itself, and a lie about a coin's value only produces a signature the network rejects (a SegWit signature commits to the value it spends). The server learns the wallet's address and IP address, like any light wallet.
- **Balances.** `balance` is the address's confirmed balance plus its unconfirmed net change (shown separately as `confirmed_balance` and `unconfirmed_change`). `usdc_value` comes from CoinGecko on mainnet and is `null` on test networks.
- **Sending.** `send_asset` follows Bitcoin Core's wallet rules: the payment is refused if it is below the dust threshold for the recipient's script (294 satoshis for P2WPKH, 330 for P2WSH and P2TR, 546 for P2PKH, 540 for P2SH; computed as Core does at 3 sat/vB); coins that cost more to spend than they are worth at the chosen fee rate are left alone; change below the dust threshold is added to the fee instead of creating a dust output; the fee may not exceed Core's default `-maxtxfee` of 0.1 BTC; and a transaction above the standard weight of 400,000 is refused. Coin selection takes the smallest single coin that covers the payment, otherwise the largest coins until it is covered, then drops any that were not needed. The transaction is version 2, signals replaceability (BIP 125, sequence `0xfffffffd`, Core's default), and sets `nLockTime` to the current block height (Core's anti-fee-sniping measure). Only confirmed coins and the wallet's own unconfirmed change (the parent transaction must be funded entirely by this wallet) are spent; unconfirmed payments from others are not, and the error says how much is waiting. The recipient and amount are exactly what was asked for.
- **Fees.** The default is the Esplora estimate for confirming within 6 blocks (Core's default target); `confirmation_target` (1 to 1008) picks another, and `fee_rate` (1 to 1000 sat/vB, up to three decimals) sets one explicitly (not together). Rates are integers in thousandths of a satoshi per virtual byte, the fee is rounded up, and it is charged on the transaction's virtual size sized with the largest possible signature, so the real rate is never below the chosen one. A fee estimate below 1 sat/vB (the long-standing minimum relay fee) is raised to it, an estimate above 1,000 sat/vB from the server is refused rather than followed, and when the fee comes from an estimate it may not exceed the amount sent (give `fee_rate` to proceed anyway). Fees of 5% or more of the amount are flagged.
- **Signatures and final checks.** Inputs are native SegWit, signed with BIP 143 `SIGHASH_ALL`, deterministic (RFC 6979) and low-S as relay policy requires. Every signature is verified against the public key before it is used (as Core does), and the finished transaction is checked against the plan: exactly the requested payment to exactly the requested script, change only to the wallet's own script, no duplicate or unsigned input, the fee equal to the plan, within limits and at least 1 sat/vB, and no larger than estimated. The signing code reproduces the BIP 143 and BIP 322 published test vectors.
- **Lost answers.** Before every broadcast the signed transaction (id and raw bytes, no keys) is saved in `$AGENT_SAIFU_HOME/pending/` under a name that is a hash of the wallet's script. If the answer is lost, the transaction id is looked up: if the node has it, it is reported as accepted; otherwise the error returns the transaction id and says not to send again. The next send from that wallet first settles the record: if the node has the transaction nothing new is sent and the agent is told; if not, the identical signed transaction is submitted again (the same inputs, so it can never pay twice) and reported; if the node cannot be reached nothing is sent; and a transaction the network rejects is discarded.
- **Simulation and estimates.** Bitcoin has no execution to dry-run. `simulate_transaction` builds the payment, signs it in memory, verifies it and reports `would_succeed`, the fee and rate, the change, the wallet's balance change and the transaction id the real send would produce, without sending anything; a payment the wallet cannot make (not enough spendable coins, dust, fee limits) is reported as `would_succeed: false` with the reason. `estimate_gas_fee` reports the fee of a payment, or without one the current rates by confirmation target and the cost of a typical 141-virtual-byte payment.
- **Messages.** `sign_personal_message` signs with BIP 322 ("simple", for P2WPKH): the message is committed to by a virtual transaction that spends an output that does not exist, so the signature can never be replayed as a payment. The response has the signature with BIP 322's `smp` prefix and also without it (for verifiers written before the prefix existed). Bitcoin Core's own `signmessage` supports only legacy P2PKH addresses and cannot check these.
- **Lookups.** `get_transaction` takes the 64-hex-digit transaction id and reports status, confirmations, block, fee, size, whether the wallet received or sent, its balance change and the outputs. `get_transaction_history` lists the address's transactions (unconfirmed first, then confirmed, newest first) with the wallet's balance change for each.
- **Not supported.** Swaps (no aggregator serves native Bitcoin), WalletConnect, PSBTs and other externally built transactions (`sign_contract_transaction`), several addresses, coin control, fee bumping and hardware wallets. The address is P2WPKH only; Taproot and legacy addresses can be paid but not held.
- **Text from the network** (addresses, errors) is length-limited and stripped of control characters before it reaches the agent.

### WalletConnect (any dapp)

The wallet speaks the WalletConnect v2 Sign protocol as the wallet side, so any dapp that shows a WalletConnect QR code or `wc:` link can connect: Uniswap, OpenSea, Aave, and so on. The flow for an agent:

1. `wc_pair` with the dapp's `wc:` URI, then look at the proposal it returns (`can_connect_with` says whether an Ethereum wallet, a Sui wallet, a Solana wallet or several can satisfy it).
2. `wc_approve_session` (proposal_id, **wallet_name**, **password**, and **chain_id** for Ethereum wallets) to connect, or `wc_reject_session`.
3. When the dapp asks for something, `wc_list_requests` shows it. `wc_review_request` shows the expected costs and outcome. `wc_approve_request` (request_id, **wallet_name**, **password**, and **chain_id** for Ethereum wallets) signs / sends and answers the dapp; `wc_reject_request` declines.

- **Setup: a project ID is needed.** WalletConnect's relay requires a free project ID. Create one at https://dashboard.walletconnect.com/ and set the `WALLETCONNECT_PROJECT_ID` environment variable, or put it in `WALLETCONNECT_PROJECT_ID` in [core/src/walletconnect_config.rs](core/src/walletconnect_config.rs) (the variable wins). Without it every `wc_*` tool says what is missing and the rest of the wallet is unaffected.
- **Wallet, chain and password on every approval.** `wc_approve_session`, `wc_review_request` and `wc_approve_request` all require the wallet name and password, and Ethereum wallets also the chain ID (a Sui or Solana wallet must omit it: it is bound to one network, and giving one is an error). The wallet must be the one the dapp's session was connected with, and the chain must be the one the request is for; otherwise the call fails before anything is unlocked or signed and the request stays open. Passwords are checked (and rate limited) like every other tool.
- **Per-action unlock.** The wallet is unlocked only inside `wc_approve_session`, `wc_review_request` and `wc_approve_request`, and locked again when the call returns. A session holds only WalletConnect keys, never wallet secrets or the address.
- **Supported methods:** `eth_sendTransaction`, `eth_signTransaction`, `personal_sign`, `eth_signTypedData` and `eth_signTypedData_v4` (EIP-712). `eth_sign` is never granted: it signs an arbitrary hash and cannot be reviewed, so a dapp that requires it cannot be connected. Contract creation is refused. The namespaces are `eip155` (Ethereum), `sui` and `solana`; a dapp that requires another namespace (e.g. Cosmos) cannot be connected, and a dapp that requires more than one of `eip155`, `sui` and `solana` cannot be connected either, because a session belongs to one wallet on one blockchain.
- **Solana over WalletConnect.** Implemented to Reown's Solana RPC reference (namespace `solana`, chains `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` mainnet, `solana:EtWTRABZaYq6iMfeYKouRu166VU2xqa1` devnet and `solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z` testnet, plus the legacy identifiers some dapps still send for mainnet and devnet, which are answered in the form the dapp used), as the wallet side only.
  - *Connecting.* A Solana wallet approves a proposal with `wc_approve_session` (no `chain_id`). The session is granted exactly one cluster, the wallet's own, so a dapp that *requires* another cluster, or `eip155` or `sui`, is refused, and one that only lists the wallet's cluster as optional is accepted. The settle message carries the account `solana:<chain>:<address>` and the methods the dapp asked for that the wallet supports. As with the other blockchains, the agent never sees the address in the proposal, the session or the request list; it is revealed only to the dapp.
  - *Methods* (all others are refused with "Unauthorized method" before the agent sees them): `solana_getAccounts` and `solana_requestAccounts` (answered with `[{pubkey}]`; normal requests needing `wc_approve_request`, because a session stores no address), `solana_signMessage` (params `{message: base58, pubkey}`; answered `{signature}` in base58), `solana_signTransaction` (params `{transaction: base64}`; answered `{signature, transaction}`, signed only), `solana_signAndSendTransaction` (same params; signed **and submitted**; answered `{signature}`) and `solana_signAllTransactions` (up to 10 transactions; answered `{transactions}`, signed only, and none is signed unless every one can be). The deprecated `{feePayer, recentBlockhash, instructions}` form of `solana_signTransaction` is accepted too.
  - *Checks.* The request's chain must match the session and the wallet's cluster, and `pubkey` (for messages) must be the wallet's address, or it is refused before anything is signed. A transaction goes through the same checks as `sign_contract_transaction` (the wallet is a signer, the blockhash is valid on the cluster), and the review shows every instruction, warnings and the dry run's balance changes. `solana_signAndSendTransaction` is refused unless the dry run succeeds and no other signature is missing, and the request stays open. A message that is also a valid transaction is refused (Phantom's rule). Sizes are limited (1,232 bytes per transaction, 64 KiB of message).
  - *Mixing blockchains.* A Solana session answers only `solana_*` requests, a Sui session only `sui_*` ones and an Ethereum session only `eth_*` / `personal_sign` ones. Using a wallet of another blockchain for a request fails before anything is unlocked or signed.
  - *Not covered.* Events (`accountsChanged` etc.) are never sent, a Solana wallet cannot switch clusters within a session, and Solana Pay / wallet-standard features outside the WalletConnect methods above are not offered. Tested against a simulated relay and Solana node, not against a live dapp.
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
- Seed phrases are always 24 words (BIP39, checksum-validated); Ethereum keys derive at `m/44'/60'/0'/0/0`, Sui keys (Ed25519, SLIP-0010) at `m/44'/784'/0'/0'/0'`, Solana keys (Ed25519, SLIP-0010) at `m/44'/501'/0'/0'`, Bitcoin keys (secp256k1, BIP 84, native SegWit) at `m/84'/0'/0'/0/0` (`m/84'/1'/0'/0/0` on test networks).
- Passwords must be at least 8 characters (export keys 12). Five wrong passwords lock a wallet for 60 seconds (the same limiter covers Ethereum, Sui, Solana and Bitcoin wallets). A tool for one blockchain given a wallet of another is refused before anything is decrypted.
- Secrets are wiped from memory after each call where possible; request/response buffers are zeroed. Secrets are never logged and are never included in error messages.
- Text coming from third parties (token names, explorer data) is shortened and stripped of control characters before it reaches an agent; RPC URLs must be https (http only for localhost) and may not be literal private-network IPv4 addresses.
- No double payments on a lost broadcast response. Before every broadcast the signed transaction (hash, nonce, raw bytes; no keys) is saved in `$AGENT_SAIFU_HOME/pending/` and removed once the node answers. Bitcoin payments are recorded the same way (see "Bitcoin wallets"). If the response is lost (timeout, network error), the record stays and the error says the outcome is unknown and not to retry. The next send from that wallet on that network first looks the transaction up: if the node has it, nothing is sent and the agent is told it was already accepted; if not, the identical signed bytes are resubmitted (same nonce, so never a second payment) and reported; if the node can't be reached, nothing is sent until it can be checked.

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
- Solana support adds **no new crates**: Ed25519 uses `ed25519-dalek` (already used for relay authentication), and base58, SLIP-0010, the transaction format (legacy and version 0 messages), program-derived addresses, instruction decoding, the JSON-RPC client and the OpenOcean integration are implemented in this repository. Everything was checked against independent computations (a Python implementation of the derivation, Ed25519 signing and the message layout; Phantom's published address for a standard phrase; an associated token account address listed by a mainnet node) and, for signatures, against a live node that verified a wallet-signed transaction.
- Bitcoin support adds **no new crates**: secp256k1 signing and verification use `k256` (already used for Ethereum), and RIPEMD-160, Bech32 / Bech32m, Base58Check, BIP 32 / BIP 84 derivation, transaction encoding, BIP 143 signature hashes, BIP 322 message signing, coin selection and the Esplora client are implemented in this repository, with the HTTPS transport shared with the other chains. Everything was checked against published vectors (BIP 173 / 350 address vectors, BIP 84's derivation vector, BIP 143's signature hash, BIP 322's message hashes, transaction ids and signatures, including signatures produced by other implementations, and RIPEMD-160's reference vectors), an independent Python derivation of a 24-word phrase, and against the live signet and mainnet Esplora APIs (reads, fee estimates, and a wallet-signed transaction that a real signet node decoded and rejected only for spending a coin that does not exist).
- Audit result (RustSec advisory database checked on 2026-10-24 against every package in `Cargo.lock`): no advisory applies to the locked versions of the new Sui dependencies (`ed25519-dalek` 2.2.0 and `curve25519-dalek` 4.1.3 are past RUSTSEC-2022-0093 and RUSTSEC-2024-0344). The audit also found two timing advisories published the day before, RUSTSEC-2026-0385 (`ctutils`) and RUSTSEC-2026-0387 (`crypto-bigint`), in crates that `k256` already pulled in for Ethereum signing; `Cargo.lock` now pins their patched versions (`ctutils` 0.4.5, `crypto-bigint` 0.7.6). One informational notice remains: `ring` 0.17.14 is flagged "unmaintained" (RUSTSEC-2025-0007) through `rustls`, which was already the case.

### External services

Ethereum mainnet RPC `ethereum-rpc.publicnode.com`, CoinGecko public API (prices), a Blockscout explorer (`eth.blockscout.com`, history), and an OpenOcean v4 API host (`open-api.de1.exchange`, swaps; no key). The WalletConnect relay (`relay.walletconnect.org`, needs a free project ID). Custom networks supply their own RPC and explorer. For Sui: the network's GraphQL endpoint (`graphql.mainnet.sui.io`, `graphql.testnet.sui.io` or `graphql.devnet.sui.io`; reads, dry runs, submission) and the Cetus aggregator (`api-sui.cetus.zone`, routes for swaps and indicative prices; no key); SUI prices also come from CoinGecko. For Solana: the cluster's JSON-RPC endpoint (`api.mainnet-beta.solana.com`, `api.devnet.solana.com` or `api.testnet.solana.com`, configurable in `core/src/solana_config.rs`; reads, simulation, submission), the Jupiter Swap API (`api.jup.ag/swap/v2/build`, swaps; keyless, no key), `explorer.solana.com` links, and CoinGecko for SOL and token prices. For Bitcoin: an Esplora-compatible API (`blockstream.info/api` for mainnet, `mempool.space/testnet4/api` and `mempool.space/signet/api`, configurable in `core/src/bitcoin_config.rs`; reads, fee estimates, broadcasting), `mempool.space` explorer links, and CoinGecko for BTC prices.

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

### WalletConnect (Ethereum, layer 2s, Sui and Solana)

Sessions live in the server process, so run these in one process (piped lines, or an MCP client that keeps the server running). Set the project ID first (see the WalletConnect section above) and use a wallet that has a network for the chain the dapp wants:

```bash
export WALLETCONNECT_PROJECT_ID=<your project id>
```

Connect: copy the `wc:` URI from the dapp, then pair, check the proposal and approve it with the wallet name, chain ID and password (a Sui or Solana wallet leaves out `chain_id`: `"arguments":{"proposal_id":"<id>","wallet_name":"DemoSui","password":"demo-password-123"}`; use the same shape for `wc_review_request` and `wc_approve_request`, where Sui requests are `sui_signTransaction`, `sui_signAndExecuteTransaction`, `sui_signPersonalMessage` and `sui_getAccounts`):

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

Cap the gas the transaction may spend with `gas_budget` (whole SUI; only the gas actually used is charged):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"Sui Demo","password":"demo-password-123","asset":"native","amount":"0.1","to":"0x0000000000000000000000000000000000000000000000000000000000000abc","gas_budget":"0.05"}}}' | cargo run -q
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

### Solana wallets

Solana wallets use the same tools; there is no `chain_id` (the wallet is bound to the cluster it was created for). Create one (mainnet by default; add `"network":"devnet"` for a test wallet):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"solana","wallet_name":"Sol Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"solana","wallet_name":"Sol Devnet","password":"demo-password-123","network":"devnet"}}}' | cargo run -q
```

Import a seed phrase (24 words, derived at `m/44'/501'/0'/0'` like Phantom; the well-known public test phrase below must never be used for real funds) or the base58 private key Phantom or Solflare shows:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"blockchain":"solana","wallet_name":"Sol Imported","password":"import-password-1","seed_phrase":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"blockchain":"solana","wallet_name":"Sol Key","password":"import-password-2","private_key":"<base58 private key>"}}}' | cargo run -q
```

SOL and every token the address holds, with USD values on mainnet:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123"}}}' | cargo run -q
```

Estimate and dry-run a transfer, then send it (spends real funds if the wallet is funded; `asset` is `native` for SOL or a token mint address such as USDC's `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`; sending a token opens the recipient's token account if they have none):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"simulate_transaction","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","asset":"native","amount":"0.1","to":"<recipient address>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","asset":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","amount":"1.5","to":"<recipient address>"}}}' | cargo run -q
```

Pay a higher priority fee (whole SOL in total, on top of the base fee) to get included faster when the network is busy:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","asset":"native","amount":"0.1","to":"<recipient address>","priority_fee":"0.0001"}}}' | cargo run -q
```

Sign a transaction built elsewhere (a serialized transaction in base64, as web3.js produces it; add `"broadcast":true` to also send it) and a message (Phantom's `signMessage`):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_contract_transaction","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","transaction_bytes":"<base64 transaction>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_personal_message","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","message":"Sign in to example.app"}}}' | cargo run -q
```

Look up a transaction by signature, and list recent activity:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","transaction_hash":"<signature>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction_history","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","limit":5}}}' | cargo run -q
```

Swap through Jupiter (mainnet wallets only; no `from_chain_id` / `to_chain_id`; no API key). Preview first, then swap with the quote's `minimum_amount` as `min_receive`:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_swap_quote","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","from_token":"native","to_token":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","amount":"0.1"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"swap_tokens","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123","from_token":"native","to_token":"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v","amount":"0.1","min_receive":"<minimum_amount from the quote>"}}}' | cargo run -q
```

The key in the format Phantom imports (base58 keypair, plus hex) and the cluster the wallet is bound to:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"view_private_key","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_networks","arguments":{"wallet_name":"Sol Demo","password":"demo-password-123"}}}' | cargo run -q
```

### Bitcoin wallets

Bitcoin wallets use the same tools; there is no `chain_id` (the wallet is bound to the network it was created for). Create one (mainnet by default; add `"network":"signet"` or `"testnet"` for a test wallet):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"bitcoin","wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_wallet","arguments":{"blockchain":"bitcoin","wallet_name":"BTC Signet","password":"demo-password-123","network":"signet"}}}' | cargo run -q
```

Import a seed phrase (24 words, derived at `m/84'/0'/0'/0/0` like Sparrow, Ledger and Trezor; the well-known public test phrase below must never be used for real funds) or a WIF private key:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"blockchain":"bitcoin","wallet_name":"BTC Imported","password":"import-password-1","seed_phrase":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"import_wallet","arguments":{"blockchain":"bitcoin","wallet_name":"BTC Key","password":"import-password-2","private_key":"<WIF private key>"}}}' | cargo run -q
```

The address (one native SegWit address per wallet) and the balance, confirmed and unconfirmed, with a USD value on mainnet:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_address","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_wallet_balances","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
```

Current fee rates (no payment given), then the exact fee of a payment, then a full dry run that builds and signs the payment in memory without sending it (`fee_rate` in sat/vB or `confirmation_target` in blocks are optional; the default targets 6 blocks):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"estimate_gas_fee","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"estimate_gas_fee","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","to":"<recipient address>","amount":"0.001","confirmation_target":2}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"simulate_transaction","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","to":"<recipient address>","amount":"0.001"}}}' | cargo run -q
```

Send BTC (spends real funds if the wallet is funded; `asset` is `native`, the amount is in BTC):

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_asset","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","asset":"native","to":"<recipient address>","amount":"0.001","fee_rate":"5"}}}' | cargo run -q
```

Sign a message with BIP 322, look up a transaction by its id, and list recent activity:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"sign_personal_message","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","message":"I own this address"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","transaction_hash":"<64-hex transaction id>"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_transaction_history","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123","limit":5}}}' | cargo run -q
```

The key as a WIF string Bitcoin Core, Sparrow and Electrum import (plus hex), and the network the wallet is bound to:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"view_private_key","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
```

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_networks","arguments":{"wallet_name":"BTC Demo","password":"demo-password-123"}}}' | cargo run -q
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

Lessons from using WalletConnect with Ethereum, Sui and Solana dapps (Sui was tested live against Cetus; Solana was tested against a simulated relay and node only; the session, polling and expiry rules apply to all chains).

- **Keep the server running for the whole dapp session.** WalletConnect sessions and pending requests live only in the memory of the running `agent-saifu-core` process. Dapp requests arrive while it runs. If the process exits or restarts, the session is gone and the dapp must pair again with a fresh `wc:` URI. Start the server once, leave it open, and keep sending tool calls to the same process (for example through a fifo or an MCP client that holds the stdio connection open). Do not run `cargo run` per call for WalletConnect.
- **A `wc:` URI is single use and short lived.** It expires within minutes and cannot be reused after the server restarts. Ask the user for a new link after any restart, and pair and approve the session right away.
- **Poll, then review, then approve.** After the user triggers an action in the dapp, call `wc_list_requests` until the request appears, run `wc_review_request` (balance changes, simulation result, warnings), and only then `wc_approve_request`. Requests expire after about five minutes.
- **Refused requests never appear in the list.** A request with an unsupported chain, method or format is answered to the dapp with an error, and the server writes `walletconnect: refused a dapp request (...)` with the reason to stderr. Capture stderr when debugging an empty list.
- **Signing is not broadcasting.** On Sui, dapps such as Cetus use `sui_signTransaction`, so the wallet only returns the signature and the dapp submits the transaction. Check `dapp_notified: true` in the approval result and then confirm with `get_wallet_balances` or `get_transaction`. On Ethereum, `eth_sendTransaction` is broadcast by the wallet.
- **If a connection goes stale, restart.** A long-running server can lose its relay connection (`NotParticipant`, or `dapp_notified: false` on approval). Restart the server and pair again with a new link.
- **Every approval spends real funds.** Review each request, and approve only on the user's explicit instruction.

## Planned Features

None at the moment. Bitcoin deliberately starts simple (one address per wallet, no coin control); more addresses, UTXO management, PSBT signing and fee bumping (RBF, CPFP) can follow.
