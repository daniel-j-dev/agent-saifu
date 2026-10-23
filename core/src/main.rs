use std::io;
use std::process::ExitCode;
use std::rc::Rc;

use agent_saifu_core::http::UreqClient;
use agent_saifu_core::server;
use agent_saifu_core::service::WalletService;
use agent_saifu_core::store::{self, Store};
use agent_saifu_core::tools::Toolbox;

fn run() -> Result<(), String> {
    let dir = store::default_dir()?;
    let store = Store::open(&dir)?;
    let toolbox = Toolbox::new(WalletService::new(store, Rc::new(UreqClient::new())));
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
