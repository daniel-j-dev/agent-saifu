//! Write-ahead record of the Bitcoin transaction being broadcast, so a lost broadcast answer
//! can be reconciled instead of paid twice. One record per address; it exists only while a
//! broadcast outcome is unknown. The file name is a hash of the wallet's output script, so it
//! shows no address, and the record holds only the public signed transaction (no keys).

use std::fs;
use std::path::PathBuf;

use crate::crypto::sha2::sha256;
use crate::crypto::{hex, random};
use crate::json::{self, Value};
use crate::store::{create_private_dir, write_new_private_file};

const FORMAT_VERSION: i64 = 1;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const KEY_DOMAIN: &[u8] = b"agent-saifu-bitcoin-pending";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBitcoinTx {
    pub network: String,
    pub txid: String,
    pub raw: Vec<u8>,
}

impl PendingBitcoinTx {
    fn to_json(&self) -> Value {
        Value::object(vec![
            ("version", Value::int(FORMAT_VERSION)),
            ("network", Value::str(&self.network)),
            ("txid", Value::str(&self.txid)),
            ("raw", Value::String(hex::encode(&self.raw))),
        ])
    }

    fn from_json(v: &Value) -> Result<PendingBitcoinTx, String> {
        if v.get("version").and_then(Value::as_u64) != Some(FORMAT_VERSION as u64) {
            return Err("unsupported pending-transaction file version".to_string());
        }
        let text = |key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("pending-transaction file has no '{key}'"))
        };
        let txid = text("txid")?;
        if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("pending-transaction file has an invalid transaction id".to_string());
        }
        Ok(PendingBitcoinTx {
            network: text("network")?.to_string(),
            txid: txid.to_ascii_lowercase(),
            raw: hex::decode(text("raw")?)?,
        })
    }
}

pub struct BitcoinJournal {
    dir: PathBuf,
}

/// The record key of a wallet: a hash of its output script, so the file name reveals no address.
pub fn journal_key(script_pubkey: &[u8]) -> String {
    let mut data = KEY_DOMAIN.to_vec();
    data.extend_from_slice(script_pubkey);
    hex::encode(&sha256(&data))
}

impl BitcoinJournal {
    pub fn new(dir: PathBuf) -> BitcoinJournal {
        BitcoinJournal { dir }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("bitcoin-{key}.json"))
    }

    /// Durably records `tx`, replacing any earlier record of the same wallet.
    pub fn record(&self, key: &str, tx: &PendingBitcoinTx) -> Result<(), String> {
        create_private_dir(&self.dir)
            .map_err(|e| format!("cannot create pending-transaction directory: {e}"))?;
        let suffix = hex::encode(&random::random_bytes(8)?);
        let tmp = self.dir.join(format!(".tmp-{suffix}"));
        write_new_private_file(&tmp, tx.to_json().to_json().as_bytes())?;
        fs::rename(&tmp, self.path_for(key)).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cannot record pending transaction: {e}")
        })
    }

    pub fn load(&self, key: &str) -> Result<Option<PendingBitcoinTx>, String> {
        let path = self.path_for(key);
        if !path.exists() {
            return Ok(None);
        }
        let unreadable = || "the pending-transaction record is unreadable".to_string();
        let meta = fs::metadata(&path).map_err(|_| unreadable())?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(unreadable());
        }
        let text = fs::read_to_string(&path).map_err(|_| unreadable())?;
        let value = json::parse(&text).map_err(|_| unreadable())?;
        PendingBitcoinTx::from_json(&value).map(Some)
    }

    pub fn clear(&self, key: &str) {
        let _ = fs::remove_file(self.path_for(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::TempDir;

    fn sample() -> PendingBitcoinTx {
        PendingBitcoinTx {
            network: "mainnet".to_string(),
            txid: "ab".repeat(32),
            raw: vec![2, 3, 4],
        }
    }

    #[test]
    fn keys_are_stable_and_reveal_no_script() {
        let key = journal_key(&[0, 20, 1, 2, 3]);
        assert_eq!(key, journal_key(&[0, 20, 1, 2, 3]));
        assert_ne!(key, journal_key(&[0, 20, 1, 2, 4]));
        assert_eq!(key.len(), 64);
        assert!(!key.contains("0014"));
    }

    #[test]
    fn record_load_clear_round_trip() {
        let dir = TempDir::new();
        let journal = BitcoinJournal::new(dir.0.join("pending"));
        let tx = sample();
        assert_eq!(journal.load("k"), Ok(None));
        journal.record("k", &tx).unwrap();
        assert_eq!(journal.load("k"), Ok(Some(tx.clone())));
        assert_eq!(journal.load("other"), Ok(None));
        let newer = PendingBitcoinTx {
            txid: "cd".repeat(32),
            ..tx
        };
        journal.record("k", &newer).unwrap();
        assert_eq!(journal.load("k"), Ok(Some(newer)));
        journal.clear("k");
        assert_eq!(journal.load("k"), Ok(None));
    }

    #[test]
    fn corrupted_records_are_reported() {
        let dir = TempDir::new();
        let journal = BitcoinJournal::new(dir.0.join("pending"));
        journal.record("k", &sample()).unwrap();
        fs::write(journal.path_for("k"), "not json").unwrap();
        assert!(journal.load("k").is_err());
        fs::write(
            journal.path_for("k"),
            r#"{"version":1,"network":"mainnet","txid":"zz","raw":"00"}"#,
        )
        .unwrap();
        assert!(journal.load("k").is_err());
    }
}
