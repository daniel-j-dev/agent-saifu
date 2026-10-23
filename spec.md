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

The MCP server lives in `core/` (Rust, stdio JSON-RPC, run with `cargo run -q` from `core/`; an MCP client launches the binary). Wallet logic for Ethereum is in `core/src/eth/`. Wallets are stored in `$AGENT_SAIFU_HOME` (default `~/.agent-saifu`).

### MCP tools

Every tool that touches a wallet needs the wallet's password; the wallet is unlocked only for that single call and is locked again when it returns. Unlocking one wallet never unlocks another. Wallet names are unique (case-insensitive).

- `get_info` - returns "All-in-one crypto wallet for Agents. Built from the ground up in Rust." and "Version 0.0".
- `create_wallet` (blockchain, wallet_name, password) - new wallet with a 24-word seed phrase in a password-encrypted vault. Multiple wallets are supported.
- `import_wallet` (wallet_name, password, seed_phrase *or* private_key) - add an existing wallet.
- `list_wallets` - names and blockchains only; no password.
- `rename_wallet` (wallet_name, new_wallet_name, password) - added beyond the original list.
- `delete_wallet` (wallet_name, password, confirm=true) - added beyond the original list; irreversible.
- `get_wallet_address` (wallet_name, password)
- `get_wallet_balances` (wallet_name, password, optional chain_id / token_contracts) - each asset with amount, USDC value, and chain ID, across all of the wallet's networks. Checks the native coin, a built-in list of well-known tokens, tokens saved with `add_token`, and any token contracts you pass for a one-off check. `usdc_value` is null when no real market price exists (testnets, unlisted tokens).
- `send_asset` (wallet_name, chain_id, asset = `native` or token contract, to, amount, password) - signs and broadcasts.
- `sign_contract_transaction` (wallet_name, chain_id, password, to, data, optional value / gas_limit / broadcast) - signs a smart-contract call; only broadcasts if `broadcast=true`. Decodes token transfers/approvals and warns about unlimited approvals.
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
- `wc_pair` (uri, optional wait_seconds) - added by request; connects to any dapp over WalletConnect v2. Give it the `wc:` URI the dapp shows. It waits for the dapp's session proposal and returns what the dapp asks for (chains, methods, whether this wallet can satisfy it). Nothing is shared until the proposal is approved. No wallet is unlocked.
- `wc_list_proposals`, `wc_reject_session` (proposal_id) - waiting connection proposals; decline one.
- `wc_approve_session` (proposal_id, wallet_name, chain_id, password, optional additional_chain_ids) - approves a proposal with a wallet; this is the only step that reveals the address, and only to the dapp. The chains must cover everything the dapp requires and may only include chains the dapp asked for; the wallet needs a network for each.
- `wc_list_sessions`, `wc_disconnect_session` (session_topic) - connected dapps (wallet, chains, methods; never addresses); end one.
- `wc_list_requests` - what connected dapps are waiting on, with a summary (decoded token transfers/approvals, message text, typed data). No password; the signer's address is left out.
- `wc_review_request` (request_id, wallet_name, chain_id, password) - the expected costs and outcome. For transactions: a dry run (would it succeed or revert), what is sent or approved in plain language (`expected_outcome`), the network fee (max and expected, in the native coin and USDC), and whether the wallet can afford it. For messages and typed data: exactly what would be signed, with warnings. Signs and sends nothing.
- `wc_approve_request` (request_id, wallet_name, chain_id, password) - signs and answers the dapp. `eth_sendTransaction` is signed and broadcast; `eth_signTransaction` only signs; `personal_sign` and `eth_signTypedData(_v4)` return a signature.
- `wc_reject_request` (request_id, optional reason) - tells the dapp the user rejected it. Nothing is signed. No password.

### Swaps (OpenOcean)

- **No API key, account or environment variable.** Swaps use an OpenOcean v4 swap API host (`https://open-api.de1.exchange/v4`, endpoint `/:chain/swap`). The host is the `OPENOCEAN_API_BASE_URL` setting in [core/src/swap_config.rs](core/src/swap_config.rs). The host supplies the transaction data the wallet signs, but the wallet decodes that data and refuses anything that does not match your request (see the safety checks below).
- **Same-chain only.** OpenOcean's API swaps within one chain; its cross-chain swaps exist only in its web widget, and non-EVM chains (Solana, and so on) need a whitelisted key. So `to_chain_id` must equal `from_chain_id` (a different value returns an explanatory error), and there is no `to_blockchain` option: swaps such as ETH to BTC or SUI are not possible through this API. What *is* supported is `to_address`: an optional address on the same chain that receives the bought tokens instead of the wallet itself.
- **Chains:** the 41 chains OpenOcean lists for its swap API (Ethereum, BNB Chain, Base, Polygon, Arbitrum, Optimism, Avalanche, and others). The wallet must have the network (see `add_network`). The wallet only ever approves or sends to OpenOcean's published router contract.
- **Costs to know about:** OpenOcean also applies its own tiered fee on public-API swaps (0.01% stable pairs, 0.1% correlated, 0.25% common, 0.5% exotic tokens).
- **Safety checks before signing:** the quote must be for the requested tokens (decimals included), may not sell more than requested or send extra native currency, must target OpenOcean's router, must meet `min_receive`, and may not have a price impact worse than 5%. The router calldata is also decoded locally (`swap(...)` on OpenOcean's router) and must sell the requested token and the quoted amount, buy the requested token, **deliver it to the wallet (or the requested `to_address`)** and guarantee a minimum return at least the quote's (and your `min_receive`); the quote's guaranteed minimum may also not be looser than the requested slippage (plus 0.5%). So a dishonest or compromised API host cannot redirect the output or hide a bad price; calldata the wallet cannot decode is refused and nothing is sent. The approval is for exactly the sold amount, never unlimited. OpenOcean text (route names, errors) is length-limited and stripped of control characters before it reaches the agent.
- **Rate limit:** 2 requests per second on the free API. **Known issue:** OpenOcean's Cloudflare bot protection challenged the quote and swap endpoints (HTTP 403) when tested from a cloud server, so swaps may fail from some networks; the tools then say so and send nothing.

Only Ethereum wallets can be created and used for now. Chain IDs select networks such as Base or Arbitrum; chains without chain IDs will not require one.

### WalletConnect (any dapp)

The wallet speaks the WalletConnect v2 Sign protocol as the wallet side, so any dapp that shows a WalletConnect QR code or `wc:` link can connect: Uniswap, OpenSea, Aave, and so on. The flow for an agent:

1. `wc_pair` with the dapp's `wc:` URI, then look at the proposal it returns.
2. `wc_approve_session` (proposal_id, **wallet_name**, **chain_id**, **password**) to connect, or `wc_reject_session`.
3. When the dapp asks for something, `wc_list_requests` shows it. `wc_review_request` shows the expected costs and outcome. `wc_approve_request` (request_id, **wallet_name**, **chain_id**, **password**) signs / sends and answers the dapp; `wc_reject_request` declines.

- **Setup: a project ID is needed.** WalletConnect's relay requires a free project ID. Create one at https://dashboard.walletconnect.com/ and set the `WALLETCONNECT_PROJECT_ID` environment variable, or put it in `WALLETCONNECT_PROJECT_ID` in [core/src/walletconnect_config.rs](core/src/walletconnect_config.rs) (the variable wins). Without it every `wc_*` tool says what is missing and the rest of the wallet is unaffected.
- **Wallet, chain and password on every approval.** `wc_approve_session`, `wc_review_request` and `wc_approve_request` all require the wallet name, chain ID and password. The wallet must be the one the dapp's session was connected with, and the chain must be the one the request is for; otherwise the call fails before anything is unlocked or signed and the request stays open. Passwords are checked (and rate limited) like every other tool.
- **Per-action unlock.** The wallet is unlocked only inside `wc_approve_session`, `wc_review_request` and `wc_approve_request`, and locked again when the call returns. A session holds only WalletConnect keys, never wallet secrets or the address.
- **Supported methods:** `eth_sendTransaction`, `eth_signTransaction`, `personal_sign`, `eth_signTypedData` and `eth_signTypedData_v4` (EIP-712). `eth_sign` is never granted: it signs an arbitrary hash and cannot be reviewed, so a dapp that requires it cannot be connected. Contract creation is refused. EVM chains only (`eip155`); a dapp that requires another namespace (e.g. Solana) cannot be connected.
- **Transactions.** The wallet picks the gas limit, fees and nonce itself; the dapp's own `gas`, fee and `nonce` fields are ignored (and the review says so). A transaction that would revert (gas estimation fails) or that the wallet cannot afford is not signed or sent, and the request stays open so it can be rejected. The request's `from` must be this wallet's address and any `chainId` in the transaction must match the request, or it is refused. Token transfers, approvals (with an unlimited-allowance warning) and `setApprovalForAll` are decoded in plain language; other contract calls show the target and function selector and are flagged as not decodable.
- **Messages and typed data.** `personal_sign` shows the message as text when it is readable and warns when it is binary (e.g. a bare hash). Typed data shows the domain and message, warns about permits, marketplace orders and near-unlimited amounts, and is refused when the domain's `chainId` differs from the request's chain. EIP-712 hashing is implemented in this repository and checked against the EIP's published vectors.
- **Answering twice is impossible.** A request is closed the moment it is approved, even if the dapp could not be reached afterwards, so an already-sent transaction can never be approved a second time. Requests expire after about 5 minutes (or the dapp's own expiry); proposals after 5 minutes; sessions after 7 days.
- **Sessions are in memory only.** Sessions last while the MCP server process runs. After a restart the dapp must connect again. Nothing about a session is written to disk, which keeps addresses out of any unencrypted file.
- **Dapp identity is self-declared.** The name and URL a dapp reports are not verified (WalletConnect's Verify API is not used); every proposal says so. Text from dapps is length-limited and stripped of control characters before it reaches the agent.
- **Privacy and transport.** All messages are end-to-end encrypted (X25519 key agreement, HKDF-SHA256, ChaCha20-Poly1305) as the protocol specifies; the relay sees only encrypted payloads and topics. The relay connection is `wss://` with certificate checking, authenticated with a fresh Ed25519 client key (JWT) per connection. A background thread keeps the relay connection so requests arrive while the agent is idle; they are picked up on the next `wc_*` call (call `wc_list_requests` to check).
- **Limits.** Not implemented: WalletConnect Auth / `wc_sessionAuthenticate` (sign-in only, no session), session updates by the wallet, and chain-switching helper methods (`wallet_switchEthereumChain`, `wallet_addEthereumChain`). Use `add_network` to give the wallet a network, then connect with that chain.

### Security design

- Vault encryption matches MetaMask's `browser-passworder`: PBKDF2-HMAC-SHA256 (900,000 iterations, 32-byte salt) derives an AES-256-GCM key (16-byte IV); the vault is `{data, iv, salt, keyMetadata}` in base64. The vault plaintext uses MetaMask's keyring layout (`HD Key Tree` / `Simple Key Pair`). Verified against vaults produced and read by WebCrypto.
- The wallet file holds only the name, blockchain and vault. Addresses, balances, networks, added tokens and keys exist only inside the encrypted vault. Files are `0600`, directories `0700`; writes are atomic and never overwrite exports.
- Seed phrases are always 24 words (BIP39, checksum-validated); keys derive at `m/44'/60'/0'/0/0`.
- Passwords must be at least 8 characters (export keys 12). Five wrong passwords lock a wallet for 60 seconds.
- Secrets are wiped from memory after each call where possible; request/response buffers are zeroed. Secrets are never logged and are never included in error messages.
- Text coming from third parties (token names, explorer data) is shortened and stripped of control characters before it reaches an agent; RPC URLs must be https (http only for localhost) and may not be literal private-network IPv4 addresses.
- No double payments on a lost broadcast response. Before every broadcast the signed transaction (hash, nonce, raw bytes; no keys) is saved in `$AGENT_SAIFU_HOME/pending/` and removed once the node answers. If the response is lost (timeout, network error), the record stays and the error says the outcome is unknown and not to retry. The next send from that wallet on that network first looks the transaction up: if the node has it, nothing is sent and the agent is told it was already accepted; if not, the identical signed bytes are resubmitted (same nonce, so never a second payment) and reported; if the node can't be reached, nothing is sent until it can be checked.

### Dependencies

Hashing (SHA-2, Keccak), HMAC, PBKDF2, HKDF, base64/base58/hex, BIP39/BIP32, RLP, ABI, EIP-191 and EIP-712 signing, transaction signing, the WalletConnect protocol logic, ZIP and JSON are implemented in this repository and tested against published vectors. Only primitives that are risky to hand-roll use crates, each checked against the RustSec advisory database (all advisories are fixed in the versions locked in `Cargo.lock`) and crates.io download counts:

- `k256` 0.14 (RustCrypto; secp256k1 ECDSA, constant-time) - ~90M downloads
- `aes-gcm` 0.11 (RustCrypto; NCC Group audited) - ~186M downloads
- `getrandom` 0.4 (OS randomness) - ~2.3B downloads
- `ureq` 3.5 with `rustls` (HTTPS for RPC / price / explorer requests, redirects disabled) - ~238M downloads
- `chacha20poly1305` 0.11 (RustCrypto; WalletConnect message encryption) - ~100M downloads
- `x25519-dalek` 3.0 (WalletConnect key agreement) and `ed25519-dalek` 3.0 (relay authentication), both dalek-cryptography - ~87M and ~248M downloads. The RustSec advisories for `ed25519-dalek` (RUSTSEC-2022-0093, fixed in 2.0) and `curve25519-dalek` (RUSTSEC-2024-0344, fixed in 4.1.3) are fixed in the locked versions.
- `tungstenite` 0.30 (WebSocket framing for the relay, handshake only, no TLS of its own) - ~360M downloads; its advisory RUSTSEC-2023-0065 is fixed since 0.20.1. TLS is the same `rustls` (ring) already used by `ureq`, with `webpki-roots` for certificates.

### External services

Ethereum mainnet RPC `ethereum-rpc.publicnode.com`, CoinGecko public API (prices), a Blockscout explorer (`eth.blockscout.com`, history), and an OpenOcean v4 API host (`open-api.de1.exchange`, swaps; no key). The WalletConnect relay (`relay.walletconnect.org`, needs a free project ID). Custom networks supply their own RPC and explorer.

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

### Wallets

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

### WalletConnect

Sessions live in the server process, so run these in one process (piped lines, or an MCP client that keeps the server running). Set the project ID first (see the WalletConnect section above) and use a wallet that has a network for the chain the dapp wants:

```bash
export WALLETCONNECT_PROJECT_ID=<your project id>
```

Connect: copy the `wc:` URI from the dapp, then pair, check the proposal and approve it with the wallet name, chain ID and password:

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

## Planned Features

None at the moment.
