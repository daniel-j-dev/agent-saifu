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
- `view_seed_phrase`, `view_private_key` (wallet_name, password)
- `export_wallet` (wallet_name, password, encryption_key, optional file_name) - writes a ZIP to `exports/` whose single entry is the seed phrase and private key encrypted with MetaMask's vault scheme under the given key. Never overwrites files.
- `revoke_token_approval` (wallet_name, chain_id, password, token, spender) - useful for revoking smart-contract token approvals (sets the ERC-20 allowance to zero).

Only Ethereum wallets can be created and used for now. Chain IDs select networks such as Base or Arbitrum; chains without chain IDs will not require one.

### Security design

- Vault encryption matches MetaMask's `browser-passworder`: PBKDF2-HMAC-SHA256 (900,000 iterations, 32-byte salt) derives an AES-256-GCM key (16-byte IV); the vault is `{data, iv, salt, keyMetadata}` in base64. The vault plaintext uses MetaMask's keyring layout (`HD Key Tree` / `Simple Key Pair`). Verified against vaults produced and read by WebCrypto.
- The wallet file holds only the name, blockchain and vault. Addresses, balances, networks, added tokens and keys exist only inside the encrypted vault. Files are `0600`, directories `0700`; writes are atomic and never overwrite exports.
- Seed phrases are always 24 words (BIP39, checksum-validated); keys derive at `m/44'/60'/0'/0/0`.
- Passwords must be at least 8 characters (export keys 12). Five wrong passwords lock a wallet for 60 seconds.
- Secrets are wiped from memory after each call where possible; request/response buffers are zeroed. Secrets are never logged and are never included in error messages.
- Text coming from third parties (token names, explorer data) is shortened and stripped of control characters before it reaches an agent; RPC URLs must be https (http only for localhost) and may not be literal private-network IPv4 addresses.

### Dependencies

Hashing (SHA-2, Keccak), HMAC, PBKDF2, base64/hex, BIP39/BIP32, RLP, ABI, transaction signing, ZIP and JSON are implemented in this repository and tested against published vectors. Only primitives that are risky to hand-roll use crates, each checked against the RustSec advisory database (all advisories are fixed in the versions locked in `Cargo.lock`) and crates.io download counts:

- `k256` 0.14 (RustCrypto; secp256k1 ECDSA, constant-time) - ~90M downloads
- `aes-gcm` 0.11 (RustCrypto; NCC Group audited) - ~186M downloads
- `getrandom` 0.4 (OS randomness) - ~2.3B downloads
- `ureq` 3.5 with `rustls` (HTTPS for RPC / price / explorer requests, redirects disabled) - ~238M downloads

### External services

Ethereum mainnet RPC `ethereum-rpc.publicnode.com`, CoinGecko public API (prices), and a Blockscout explorer (`eth.blockscout.com`, history). Custom networks supply their own RPC and explorer.

## Example input

Run these from the `core/` folder. Each command starts the server, sends one request and exits; wallets persist on disk between commands. To keep test wallets away from your real ones, first run:

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
