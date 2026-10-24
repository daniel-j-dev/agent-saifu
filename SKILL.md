---
name: agent-saifu
description: Use the agent-saifu MCP server, an all-in-one crypto wallet for AI agents (Ethereum, EVM layer 2s and Sui). Use it to create or import wallets, check balances, send assets, swap tokens, sign transactions and messages, and connect to dapps over WalletConnect. Read this before calling any wallet tool, and before connecting a dapp.
---

# agent-saifu: crypto wallet for agents

agent-saifu is a Rust MCP server (stdio JSON-RPC) that holds wallets and signs for you. It supports **Ethereum** (plus any EVM layer 2 you add, chosen per call with `chain_id`) and **Sui** (a wallet is bound to one network, so there is no `chain_id`). Solana and Bitcoin are not supported yet.

Full detail (every tool, parameters, security design, worked examples) is in `spec.md`, shipped next to this file. Read it when this summary is not enough.

## Setup (if you only have this file)

If you only have access to this `SKILL.md` and cannot see `spec.md`, the repo, or an executable for agent-saifu, set it up like this:

1. Download the latest agent-saifu release and extract it: https://github.com/daniel-j-dev/agent-saifu/releases
2. Read the extracted `SKILL.md` and `spec.md` files for context.

**That release method must be used for production use.**

If you need to use the codebase instead of the faster compiled release, feel free to clone the repo instead. Agents can still use the full MCP functionality: just read the `SKILL.md` and `spec.md` files.

## Operated or autonomous

How you work with this wallet depends on how you are run. This file uses "your principal" for whoever gives you your instructions.

- **Operated agent.** A person is in the conversation. They give you wallet names, passwords, `wc:` links and instructions, and they trigger actions in dapps. Check with them before spending, and wait for their go-ahead where this file says so.
- **Autonomous agent.** No person is in the loop. Never stop to wait for a human reply to a question, a `wc:` link, an approval or a click. Your principal is your standing mandate (your task, its limits and any spending policy you were configured with). You start and finish everything yourself: you drive the dapp (browser automation, scripts or an API), obtain the `wc:` URI from it, trigger the action, and approve it against your mandate. If the mandate does not cover an action, do not do it; reject it or skip it and log why. Do not invent permission.

Wherever this file says "ask the user" or "when the user says to approve", an operated agent waits for the person, and an autonomous agent decides from its mandate and the review checks.

## Ground rules

- **Real funds.** Wallets usually hold real mainnet money, and sends, swaps and approvals are irreversible. Never spend, swap, send, approve a dapp request or export keys unless your principal told you to (an operated agent: the user said so; an autonomous agent: your mandate covers it). Confirm the recipient, asset and amount first. Sui recipients must be written out in full (`0x` plus 64 hex characters). Autonomous agents should also keep to any amount limits in their mandate.
- **Passwords, seed phrases and private keys.** Every tool that touches a wallet needs `wallet_name` and `password`. The wallet is unlocked for that one call and locked again on return; unlocking one wallet never unlocks another. A wrong password is rate limited. Anyone who gets a seed phrase or private key controls the funds permanently, and a password plus the vault file does the same.
  - **Operated agent:** never store, log or repeat the user's password, seed phrase or private key beyond what the user asked for. Show a seed phrase or key (`view_seed_phrase`, `view_private_key`, `export_wallet`) only when the user asks.
  - **Autonomous agent:** the secrets are yours to handle as your mandate needs; nobody is there to ask. You decide where the password lives and when to read the seed phrase, and you are responsible for keeping every wallet's backup, private key and seed phrase safe and findable (see "Backing up a wallet"). Strong recommendations:
    - Keep the password in a secret store or environment variable, never in source code, prompts, chat transcripts, logs or version control, and never send it to any other service or tool.
    - Do not call `view_seed_phrase` or `view_private_key` unless you need them (for example, a one-time backup), and do not echo the output into logs or model-visible history that persists. Prefer `export_wallet`: it writes an encrypted ZIP, so no plaintext secret has to pass through your output. Use a code-generated random encryption key and store it separately from the ZIP (see "Backing up a wallet").
    - Use one wallet per purpose, funded only with what the task needs, so a leak costs little. Give separate wallets different passwords.
    - Use long, random, unique passwords (not examples like `demo-password-123`), and keep a backup of the seed phrase offline so losing the password or vault is recoverable.
    - Never paste a secret into a dapp, a web form or a message to anyone. Only the `wc_*` tools talk to dapps, and they never reveal secrets.
- **Pick tools by wallet type.** Most tools work for both blockchains. Ethereum-only tools: `read_smart_contract`, `add_network`, `update_network`, `delete_network`, `add_token`, `remove_token`, `revoke_token_approval`. Giving `chain_id` to a Sui wallet, or a Sui wallet to an Ethereum-only tool, is refused.
- **Look before you act.** Use `simulate_transaction` and `estimate_gas_fee` for sends, and `get_swap_quote` before `swap_tokens`. Always check balances with `get_wallet_balances` after a transaction and report the result honestly, including failures.
- **Report what you did:** the transaction hash or digest, amounts, and anything left over (for example SUI or ETH held back for gas). An operated agent tells the user; an autonomous agent writes it to its log or output.

## Common tasks

| Task | Tools |
|---|---|
| See wallets | `list_wallets` (no password) |
| Create or import | `create_wallet` (`blockchain` = `ethereum` or `sui`, optional Sui `network`), `import_wallet` |
| Address and balances | `get_wallet_address`, `get_wallet_balances` |
| Send | `send_asset` (`asset` = `native`, an ERC-20 address, or a Sui coin type like `0x2::sui::SUI`) |
| Swap | `get_swap_quote`, then `swap_tokens` (OpenOcean on Ethereum, Cetus on Sui mainnet) |
| Sign | `sign_contract_transaction`, `sign_personal_message` |
| History and status | `get_transaction`, `get_transaction_history` |
| Backup | `view_seed_phrase`, `view_private_key`, `export_wallet` (an encrypted ZIP in `~/.agent-saifu/exports/`; use a strong generated `encryption_key`, see "Backing up a wallet"; to open it later you need the ZIP and that key, and nothing else) |
| EVM layer 2s (Ethereum wallets) | `add_network`, `list_networks`, `add_token` |

Notes that save time:

- Sui swap tokens must be `native` or a full coin type, not a symbol.
- Swaps are same-chain only. `min_receive` is a safety floor; on Sui pairs other than SUI/USDC it is required.
- Sending a wallet's entire SUI or ETH balance needs a gas reserve, so a few dust units will remain. On Sui, one send can spend at most about the larger of the coin-object balance and the address balance, so a very large send may need to be split into two.
- Wallets are stored in `$AGENT_SAIFU_HOME` (default `~/.agent-saifu`).

## Backing up a wallet (`export_wallet`)

`export_wallet` takes an `encryption_key` (at least 12 characters) and writes an encrypted ZIP. The ZIP has no password of its own, so **the key is the only thing protecting the seed phrase and private key inside.** Anyone with the ZIP and the key controls the wallet, and the key is stored nowhere, so if it is lost the backup is unrecoverable.

- **If the user gave you a key** (operated agent), use exactly that key. Do not replace or alter it.
- **Otherwise, generate a strong key with code.** Never invent one yourself, and never use a word, a phrase, the wallet's password or an example from this file. Use a cryptographically secure random source, for example:
  - `python3 -c "import secrets; print(secrets.token_urlsafe(32))"`
  - `openssl rand -base64 32`
  - Node: `node -e "console.log(require('crypto').randomBytes(32).toString('base64url'))"`

  Either gives about 256 bits of randomness and is far over the 12-character minimum. Generate it once, use it in the call, and do not retry with a weaker key.
- **Keep the key safe and separate from the ZIP.**
  - Operated agent: show the key to the user once and tell them to save it in a password manager, because they need it to open the backup. Tell them where the ZIP is.
  - Autonomous agent: put the key in your secret store (not next to the ZIP, not in logs, prompts, transcripts or version control), and record where the ZIP and the key are.
- To open the backup later you need only the ZIP and the key, not the wallet password. Never reuse the key for anything else, and use a fresh key for each export.

**Autonomous agents own their wallets' recovery material.** Unless your principal instructs otherwise, keeping up with the backup, private key, seed phrase and passwords of every wallet you create, import or operate is your job, and no one else will do it for you. If you lose them, the funds may be gone for good. For each wallet:

- Back it up right after creating or importing it (and whenever it changes hands): export it with a generated key, as above.
- Keep a record you can find again: the wallet name, blockchain and address, the password's location in your secret store, the ZIP's path, and the export key's location. Never put the secrets themselves in that record.
- Keep a copy of the ZIP somewhere other than the machine the wallet lives on, and keep the key in a different place from the ZIP.
- Check from time to time that the records are still current, especially after you create, import, rename or delete a wallet, or change its password.
- Do not delete a wallet (`delete_wallet`, irreversible) until you have a verified backup, unless your principal told you to.

An operated agent does not take this over: it helps the user back up when asked, and the user keeps their own recovery material.

## Connecting to dapps with WalletConnect

Any dapp that shows a WalletConnect QR code or `wc:` link (Uniswap, Cetus, Aave and so on) can connect to an Ethereum or Sui wallet.

**The MCP server must keep running for the whole time you use a dapp.** WalletConnect sessions and the requests dapps send live only in the memory of the running `agent-saifu-core` process. If the server exits or restarts, the session is gone, the dapp must be paired again, and you will never see its transaction requests. Start the server once, keep the same process and connection open, and send every later tool call to it. Do not launch a fresh process per call for WalletConnect work.

The flow:

1. Get a `wc:` URI. Operated: the user gives it to you. Autonomous: you get it yourself from the dapp's connect dialog (the WalletConnect QR code or "copy link"), then call `wc_pair` with it and read the proposal (`can_connect_with` says whether an Ethereum wallet, a Sui wallet or both can satisfy it).
2. `wc_approve_session` with `proposal_id`, `wallet_name`, `password` (plus `chain_id` for Ethereum wallets; omit it for Sui), or `wc_reject_session`. Autonomous: only connect to the dapp your task calls for.
3. Trigger the action in the dapp. Operated: ask the user to do it (or do it yourself if you can drive the dapp). Autonomous: do it yourself; never wait for a person. Then poll `wc_list_requests` until the request appears (every few seconds).
4. `wc_review_request` (wallet, password, `chain_id` for Ethereum) shows a dry run, the plain-language outcome, balance changes, fees and warnings. Read it. Operated: tell the user what it will do.
5. Approve with `wc_approve_request`, or decline with `wc_reject_request`.
   - Operated: only when the user says to approve.
   - Autonomous: approve only if the review passes your own checks: the dry run succeeds, there are no warnings you cannot explain, and the outcome and balance changes match the action you started and stay within your mandate's limits. Otherwise reject, and log why. Do not approve a request you did not trigger.

Things to know:

- **A `wc:` link is single use and short lived.** It expires within minutes and cannot be reused after a restart. If you restart the server, get a new link (operated: ask the user; autonomous: generate a new one from the dapp) and pair again straight away.
- **Requests expire after about five minutes.** Review and approve promptly. Operated: once the user has asked you to. Autonomous: right after the review passes.
- **Signing is not always broadcasting.** On Sui, dapps such as Cetus use `sui_signTransaction`: the wallet only returns the signature and the dapp submits the transaction. Check that the approval says `dapp_notified: true`, then confirm with `get_wallet_balances` or `get_transaction`. On Ethereum, `eth_sendTransaction` is broadcast by the wallet.
- **Refused requests never show up in `wc_list_requests`.** Unsupported chains, methods or formats are answered to the dapp with an error, and the server writes `walletconnect: refused a dapp request (...)` with the reason to stderr. If the list stays empty, check stderr.
- **A stale connection means restarting.** A long-running server can lose its relay connection (`NotParticipant`, or `dapp_notified: false` on approval). Restart the server, get a new link (see above), and pair again.
- **Not supported:** `eth_sign`, contract creation, WalletConnect Auth sign-in, Sui sponsored transactions, and dapps that need a namespace other than `eip155` or `sui` (or both at once).
- WalletConnect needs a project ID. One is built in; the `WALLETCONNECT_PROJECT_ID` environment variable overrides it.

## If something goes wrong

- Read the error: the tools explain what is missing or refused, and a transaction that fails its dry run is never signed.
- A failed or lost submission: look up the transaction with `get_transaction` before sending again. Never resend blindly.
- If you are confused about a tool or argument, read `spec.md`.
