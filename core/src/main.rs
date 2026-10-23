use std::io;
use std::process::ExitCode;
use std::rc::Rc;

use agent_saifu_core::eth::swap::SwapSettings;
use agent_saifu_core::http::UreqClient;
use agent_saifu_core::server;
use agent_saifu_core::service::WalletService;
use agent_saifu_core::store::{self, Store};
use agent_saifu_core::tools::Toolbox;

fn run() -> Result<(), String> {
    let dir = store::default_dir()?;
    let store = Store::open(&dir)?;
    let swap_settings = SwapSettings::from_config()?;
    if swap_settings.fee.is_none() {
        eprintln!(
            "agent-saifu: no swap fee recipient is set (core/src/swap_config.rs), so swaps will not charge a fee"
        );
    }
    let service =
        WalletService::new(store, Rc::new(UreqClient::new())).with_swap_settings(swap_settings);
    let toolbox = Toolbox::new(service);
    let stdin = io::stdin();
    let stdout = io::stdout();
    server::serve(stdin.lock(), stdout.lock(), &toolbox).map_err(|e| format!("I/O error: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("agent-saifu: {message}");
            ExitCode::FAILURE
        }
    }
}
