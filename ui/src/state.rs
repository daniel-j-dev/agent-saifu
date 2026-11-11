//! App-wide state shared by every screen through a Dioxus context.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_saifu_core::json::Value;
use dioxus::prelude::*;

use crate::backend::{Args, Backend, FORGET, REFRESH, UNLOCK};
use crate::model::{
    Asset, Snapshot, WalletInfo, WcProposal, WcRequest, WcSession, parse_balances, parse_proposals,
    parse_requests, parse_sessions, parse_wallets,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Wallet,
    Swap,
    Activity,
    Agent,
    Settings,
}

/// A swap the user is about to review.
#[derive(Clone, PartialEq, Debug)]
pub struct SwapDraft {
    pub from_token: String,
    pub from_symbol: String,
    pub to_token: String,
    pub to_symbol: String,
    pub amount: String,
    pub chain_id: Option<u64>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Sheet {
    Wallets,
    Create,
    Import,
    Rename,
    Delete,
    Unlock,
    /// Optionally starts with this asset (`native` or a contract) selected.
    Send(Option<String>),
    Receive,
    Asset(Asset),
    SwapReview(SwapDraft),
    Slippage,
    Security,
    SeedPhrase,
    PrivateKey,
    Export,
    Advanced,
    SignMessage,
    Contract,
    ReadContract,
    Lookup,
    Networks,
    Tokens,
    Approvals,
    Connect,
    Sessions,
    Session(String),
    Proposal(String),
    About,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActivityKind {
    Sent,
    Received,
    Swapped,
    Signed,
    Connected,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ActivityItem {
    pub kind: ActivityKind,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub at: u64,
}

#[derive(Clone, Copy)]
pub struct AppState {
    pub tab: Signal<Tab>,
    /// The transaction screen for this pending request, when open.
    pub screen: Signal<Option<String>>,
    pub sheet: Signal<Option<Sheet>>,
    pub wallets: Signal<Option<Result<Vec<WalletInfo>, String>>>,
    pub selected: Signal<Option<String>>,
    /// What each unlocked wallet showed last, kept in memory until the app closes. Holds only
    /// public facts (address, balances); the wallet's keys stay locked.
    pub snapshots: Signal<Vec<Snapshot>>,
    /// Masks balances on screen without forgetting them.
    pub hidden: Signal<bool>,
    pub refreshing: Signal<bool>,
    pub requests: Signal<Vec<WcRequest>>,
    pub sessions: Signal<Vec<WcSession>>,
    pub proposals: Signal<Vec<WcProposal>>,
    pub popup: Signal<Option<String>>,
    pub seen: Signal<Vec<String>>,
    pub toast: Signal<Option<(u64, String)>>,
    pub activity: Signal<Vec<ActivityItem>>,
    pub history: Signal<Option<(String, Value)>>,
    pub slippage_bps: Signal<u64>,
}

static TOAST_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `Just now`, `5 min ago`, `2 h ago`, `3 d ago`.
pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..=59 => "Just now".to_string(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

impl AppState {
    pub fn new() -> AppState {
        AppState {
            tab: Signal::new(Tab::Wallet),
            screen: Signal::new(None),
            sheet: Signal::new(None),
            wallets: Signal::new(None),
            selected: Signal::new(None),
            snapshots: Signal::new(vec![]),
            hidden: Signal::new(false),
            refreshing: Signal::new(false),
            requests: Signal::new(vec![]),
            sessions: Signal::new(vec![]),
            proposals: Signal::new(vec![]),
            popup: Signal::new(None),
            seen: Signal::new(vec![]),
            toast: Signal::new(None),
            activity: Signal::new(vec![]),
            history: Signal::new(None),
            slippage_bps: Signal::new(100),
        }
    }

    pub fn open(mut self, sheet: Sheet) {
        self.sheet.set(Some(sheet));
    }

    pub fn close(mut self) {
        self.sheet.set(None);
    }

    pub fn go(mut self, tab: Tab) {
        // The agent page is not built yet, so its tab does nothing.
        if tab == Tab::Agent {
            return;
        }
        self.screen.set(None);
        self.tab.set(tab);
    }

    pub fn open_request(mut self, id: &str) {
        self.popup.set(None);
        self.sheet.set(None);
        self.screen.set(Some(id.to_string()));
    }

    pub fn wallet_list(&self) -> Vec<WalletInfo> {
        match &*self.wallets.read() {
            Some(Ok(list)) => list.clone(),
            _ => vec![],
        }
    }

    /// The wallet the app is working with; the first one until the user picks another.
    pub fn current(&self) -> Option<WalletInfo> {
        let list = self.wallet_list();
        let chosen = self.selected.read().clone();
        chosen
            .and_then(|n| list.iter().find(|w| w.name == n).cloned())
            .or_else(|| list.first().cloned())
    }

    /// What the current wallet last showed, even while balances are masked.
    pub fn current_snapshot(&self) -> Option<Snapshot> {
        let wallet = self.current()?;
        self.snapshots
            .read()
            .iter()
            .find(|s| s.wallet == wallet.name)
            .cloned()
    }

    /// The same, or `None` while balances are masked.
    pub fn visible_snapshot(&self) -> Option<Snapshot> {
        if *self.hidden.read() {
            return None;
        }
        self.current_snapshot()
    }

    pub fn store_snapshot(mut self, snapshot: Snapshot) {
        let mut all = self.snapshots.write();
        match all.iter_mut().find(|s| s.wallet == snapshot.wallet) {
            Some(slot) => *slot = snapshot,
            None => all.push(snapshot),
        }
    }

    pub fn set_hidden(mut self, hidden: bool) {
        self.hidden.set(hidden);
    }

    /// Picks a wallet and, when it was unlocked before, refreshes it without a password.
    pub fn select(mut self, name: &str) {
        self.selected.set(Some(name.to_string()));
        self.history.set(None);
        if self.snapshots.peek().iter().any(|s| s.wallet == name) {
            self.refresh();
        }
    }

    /// Reads the current wallet's balances again. Needs no password once the wallet has been
    /// unlocked in this session; a wallet that never was is sent to the unlock sheet instead.
    pub fn refresh(mut self) {
        let Some(wallet) = self.current() else { return };
        if self.current_snapshot().is_none() {
            self.open(Sheet::Unlock);
            return;
        }
        if *self.refreshing.peek() {
            return;
        }
        self.refreshing.set(true);
        let backend = consume_context::<Backend>();
        spawn(async move {
            let args = Args::new().text("wallet_name", &wallet.name).build();
            match backend.call(REFRESH, args).await {
                Ok(answer) => self.store_snapshot(parse_balances(&wallet.name, &answer)),
                Err(message) => self.toast(&format!("Could not refresh: {message}")),
            }
            self.refreshing.set(false);
        });
    }

    /// Unlocks a wallet with its password (the one time it is needed) and keeps its public side.
    pub fn unlock_in_background(self, wallet: &str, password: &str) {
        let backend = consume_context::<Backend>();
        let (wallet, password) = (wallet.to_string(), password.to_string());
        spawn(async move {
            let args = Args::new()
                .text("wallet_name", &wallet)
                .text("password", &password)
                .build();
            if let Ok(answer) = backend.call(UNLOCK, args).await {
                self.store_snapshot(parse_balances(&wallet, &answer));
            }
        });
    }

    /// Drops what the app remembers of a wallet (after it is renamed or removed).
    pub fn forget(mut self, name: &str) {
        self.snapshots.write().retain(|s| s.wallet != name);
        let backend = consume_context::<Backend>();
        let args = Args::new().text("wallet_name", name).build();
        spawn(async move {
            let _ = backend.call(FORGET, args).await;
        });
    }

    pub fn toast(mut self, message: &str) {
        let id = TOAST_SEQ.fetch_add(1, Ordering::Relaxed);
        self.toast.set(Some((id, message.to_string())));
        dioxus::core::spawn_forever(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if self.toast.peek().as_ref().map(|t| t.0) == Some(id) {
                self.toast.set(None);
            }
        });
    }

    pub fn log(mut self, kind: ActivityKind, title: &str, detail: &str, status: &str) {
        self.activity.write().insert(
            0,
            ActivityItem {
                kind,
                title: title.to_string(),
                detail: detail.to_string(),
                status: status.to_string(),
                at: now_secs(),
            },
        );
    }

    pub fn request(&self, id: &str) -> Option<WcRequest> {
        self.requests.read().iter().find(|r| r.id == id).cloned()
    }

    pub fn session_for(&self, request: &WcRequest) -> Option<WcSession> {
        self.sessions
            .read()
            .iter()
            .find(|s| s.topic == request.session_topic)
            .cloned()
    }

    pub async fn refresh_wallets(mut self, backend: &Backend) {
        let result = backend
            .call("list_wallets", Args::new().build())
            .await
            .map(|v| parse_wallets(&v));
        self.wallets.set(Some(result));
    }

    /// Unlocks nothing: lists what connected apps are waiting on and raises the popup for news.
    pub async fn poll_walletconnect(mut self, backend: &Backend) {
        if let Ok(v) = backend.call("wc_list_sessions", Args::new().build()).await {
            self.sessions.set(parse_sessions(&v));
        }
        if let Ok(v) = backend.call("wc_list_proposals", Args::new().build()).await {
            self.proposals.set(parse_proposals(&v));
        }
        if let Ok(v) = backend.call("wc_list_requests", Args::new().build()).await {
            let requests = parse_requests(&v);
            let open = self.screen.peek().clone();
            let newest = requests
                .iter()
                .rev()
                .find(|r| !self.seen.peek().contains(&r.id) && open.as_deref() != Some(&r.id))
                .map(|r| r.id.clone());
            for r in &requests {
                if !self.seen.peek().contains(&r.id) {
                    self.seen.write().push(r.id.clone());
                }
            }
            if let Some(id) = newest {
                self.popup.set(Some(id));
            }
            let shown = self.popup.peek().clone();
            if let Some(shown) = shown
                && !requests.iter().any(|r| r.id == shown)
            {
                self.popup.set(None);
            }
            self.requests.set(requests);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_elapsed_time() {
        assert_eq!(ago(100, 100), "Just now");
        assert_eq!(ago(100, 130), "Just now");
        assert_eq!(ago(100, 400), "5 min ago");
        assert_eq!(ago(0, 7300), "2 h ago");
        assert_eq!(ago(0, 200_000), "2 d ago");
        assert_eq!(ago(500, 100), "Just now");
    }
}
