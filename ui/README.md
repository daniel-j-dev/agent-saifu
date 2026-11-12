# agent-saifu UI

A [Dioxus](https://dioxuslabs.com) 0.7 app for desktop, Android and iOS. It is a separate crate: the wallet lives in [`../core`](../core) and keeps working on its own through MCP. Nothing in `core/` depends on this folder.

The design follows [`../saifu-app-design`](../saifu-app-design): one vertical column that is the same on phones and desktops, with Wallet, Swap, Activity, Agent and Settings tabs. The **Agent** tab is shown but does nothing yet.

## How it talks to the wallet

The app links `agent-saifu-core` as a library and drives the same `Toolbox` the MCP server exposes, by tool name and JSON ([`src/backend.rs`](src/backend.rs)). The toolbox is not `Send`, so it lives on one worker thread and screens call it asynchronously. This means every security rule of the core holds in the app: a wallet is unlocked only for one call, passwords are checked and rate limited by the core, and addresses and balances stay hidden until the user enters the password. Passwords and recovery material the app holds on screen are overwritten with zeros when the screen goes away.

**One password, then refresh freely.** Unlocking a wallet once keeps its public address (and, for Ethereum, its networks and tokens) in memory until the app closes, using core's `WalletService::public_view`. The ↻ button next to the wallet switcher, switching back to an unlocked wallet, and finishing a send, swap or approved request all re-read balances with no password, on every screen at once. No key is ever kept: signing, sending, secrets and history still ask for the password. The ◉ button only masks balances.

**Swap safety floor.** The review screen shows the quote's minimum received and, below it, the lower floor the swap is actually refused under (the minimum minus 0.5%). The wallet re-quotes when it swaps, so a floor equal to the old quote would refuse swaps over tiny price moves; the slippage setting still bounds the price.

**Token search.** The pay and receive tokens on Swap and the asset on Send are one searchable dropdown (pay lists only what you hold; receive and Send search by name, symbol or address). The search is fed by OpenOcean's token lists (Ethereum and every layer 2 it serves), Jupiter (Solana) and OpenOcean plus DexScreener markets (Sui), with your own tokens first ([`src/tokens.rs`](src/tokens.rs), [`src/picker.rs`](src/picker.rs)). Results are third-party data: addresses are validated, verified lists are marked, and the review screen shows the token the wallet itself resolved. `cargo test live -- --ignored` checks the real services.

Wallets are stored in `$AGENT_SAIFU_HOME` (default `~/.agent-saifu`, the same place the MCP server uses on desktop). On Android and iOS the app's private storage is used.

## What you can do

| Area | Where |
|---|---|
| Create, import, switch, rename, remove wallets | Wallet pill / ▦ button, Settings → Manage wallets |
| Unlock to see address and balances | Wallet tab (◉ / ⊘) |
| Send (with review and simulation first), receive (QR), asset details | Wallet tab |
| Swap with token search, route, fees and minimum received shown before approval, slippage setting | Swap tab |
| Refresh balances on every screen without the password | ↻ in the header |
| Connect an app by WalletConnect link, approve or decline, disconnect | Wallet tab → Connect, Settings → Connected apps |
| Incoming WalletConnect requests: popup at the top, listed in Activity, full transaction screen with password-gated simulation, approve or reject | Popup / Activity tab |
| Session activity log, on-chain history, transaction lookup | Activity tab |
| Recovery phrase, private key, encrypted export | Settings → Recovery & backup |
| Networks, tokens, token approvals (Ethereum wallets) | Settings |
| Sign a message, contract transaction (simulate then sign or broadcast), read a contract | Settings → Advanced tools |

Amounts are typed as text and passed to the core as strings; the core parses them exactly. The app only uses floating point for USD display.

## Run it

The Rust version is pinned to `1.99.1` in [`Cargo.toml`](Cargo.toml) (`rust-version`) and [`rust-toolchain.toml`](rust-toolchain.toml), the same as the core. Dioxus 0.7 needs Rust 1.83 or newer.

Desktop (Linux also needs the WebKitGTK development packages, e.g. `libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libayatana-appindicator3-dev`):

```bash
cd ui
cargo run                       # desktop window
cargo run --features demo       # canned data, no keys, no network (for looking at the screens)
```

Use a throwaway wallet folder while testing: `AGENT_SAIFU_HOME=/tmp/saifu-ui cargo run`.

### Testing inside a container (devcontainer / podman, Debian)

A container has no display, so run a virtual one and watch it in your browser through noVNC.

Install the packages once:

```bash
sudo apt-get install -y novnc websockify x11vnc openbox xvfb \
  libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev libayatana-appindicator3-dev
```

Start the virtual display and the viewer:

```bash
Xvfb :99 -screen 0 1280x900x24 &
DISPLAY=:99 openbox &
x11vnc -display :99 -forever -shared -nopw -localhost -rfbport 5900 &
websockify --web /usr/share/novnc 6080 localhost:5900 &
```

Forward port `6080` (VS Code: Ports tab), open `http://localhost:6080/vnc.html` and click Connect. Then run the app on that display:

```bash
cd ui
DISPLAY=:99 WEBKIT_DISABLE_DMABUF_RENDERER=1 cargo run
```

Add `--features demo` to look at the screens with canned data, or `AGENT_SAIFU_HOME=/tmp/saifu-ui` to keep test wallets away from real ones.

Android and iOS use the [Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started/) (`cargo install dioxus-cli`) with the platform SDKs installed:

```bash
dx serve --platform android --no-default-features --features mobile
dx serve --platform ios --no-default-features --features mobile
dx bundle --release --platform android --no-default-features --features mobile
```

The mobile builds have not been run on a device in this repository's CI yet.

## Build it

AppImage
```bash
dx bundle --desktop --release --package-types appimage
```


## WalletConnect

Pairing needs a WalletConnect project ID, exactly as for the MCP server: set `WALLETCONNECT_PROJECT_ID` or edit [`../core/src/walletconnect_config.rs`](../core/src/walletconnect_config.rs). The app checks for new requests every few seconds while it is open. Sessions live only while the app is running.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

Pure logic (formatting, parsing the wallet's JSON, QR encoding, argument building) is unit tested; the screens themselves were exercised by hand against the real core, a live node and the WalletConnect test dapp.

## Layout

- `src/backend.rs`: worker thread and tool calls; `src/demo.rs`: canned data for the `demo` feature
- `src/model.rs`: typed views of the wallet's JSON; `src/fmt.rs`: text helpers
- `src/state.rs`: shared state, sheets, polling of WalletConnect
- `src/pages.rs`: the four tab pages; `src/tx.rs`: transaction screen and popup
- `src/sheets/`: every bottom sheet (wallets, money, secrets, tools, networks, connect)
- `assets/main.css`: styles ported from the design
