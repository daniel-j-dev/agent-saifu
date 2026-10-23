//! Write-ahead record of the transaction being broadcast, so a lost broadcast response can be
//! reconciled instead of re-paid. One record per (chain, sender); it exists only while a
//! broadcast outcome is unknown.

use std::fs;
use std::path::PathBuf;

use super::address::Address;
use crate::crypto::{hex, random};
use crate::json::{self, Value};
use crate::store::{create_private_dir, write_new_private_file};

const FORMAT_VERSION: i64 = 1;
const MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTx {
    pub chain_id: u64,
    pub from: Address,
    pub nonce: u64,
    pub hash: [u8; 32],
    pub raw: Vec<u8>,
}

impl PendingTx {
    fn to_json(&self) -> Value {
        Value::object(vec![
            ("version", Value::int(FORMAT_VERSION)),
            ("chain_id", Value::Number(self.chain_id.to_string())),
            ("from", Value::String(hex::encode_prefixed(&self.from))),
            ("nonce", Value::Number(self.nonce.to_string())),
            ("hash", Value::String(hex::encode_prefixed(&self.hash))),
            ("raw", Value::String(hex::encode_prefixed(&self.raw))),
        ])
    }

    fn from_json(v: &Value) -> Result<PendingTx, String> {
        if v.get("version").and_then(Value::as_u64) != Some(FORMAT_VERSION as u64) {
            return Err("unsupported pending-transaction file version".to_string());
        }
        let text = |key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("pending-transaction file has no '{key}'"))
        };
        let number = |key: &str| {
            v.get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("pending-transaction file has no '{key}'"))
        };
        Ok(PendingTx {
            chain_id: number("chain_id")?,
            from: hex::decode_fixed(text("from")?)?,
            nonce: number("nonce")?,
            hash: hex::decode_fixed(text("hash")?)?,
            raw: hex::decode(text("raw")?)?,
        })
    }
}

pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    pub fn new(dir: PathBuf) -> Journal {
        Journal { dir }
    }

    fn path_for(&self, chain_id: u64, from: &Address) -> PathBuf {
        self.dir
            .join(format!("{chain_id}-{}.json", hex::encode(from)))
    }

    /// Durably records `tx`, replacing any earlier record for the same chain and sender.
    pub fn record(&self, tx: &PendingTx) -> Result<(), String> {
        create_private_dir(&self.dir)
            .map_err(|e| format!("cannot create pending-transaction directory: {e}"))?;
        let suffix = hex::encode(&random::random_bytes(8)?);
        let tmp = self.dir.join(format!(".tmp-{suffix}"));
        write_new_private_file(&tmp, tx.to_json().to_json().as_bytes())?;
        fs::rename(&tmp, self.path_for(tx.chain_id, &tx.from)).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cannot record pending transaction: {e}")
        })
    }

    pub fn load(&self, chain_id: u64, from: &Address) -> Result<Option<PendingTx>, String> {
        let path = self.path_for(chain_id, from);
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
        PendingTx::from_json(&value).map(Some)
    }

    pub fn clear(&self, chain_id: u64, from: &Address) {
        let _ = fs::remove_file(self.path_for(chain_id, from));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::TempDir;

    fn sample() -> PendingTx {
        PendingTx {
            chain_id: 1,
            from: [7u8; 20],
            nonce: 9,
            hash: [0xab; 32],
            raw: vec![2, 3, 4],
        }
    }

    #[test]
    fn record_load_clear_round_trip() {
        let dir = TempDir::new();
        let journal = Journal::new(dir.0.join("pending"));
        let tx = sample();
        assert_eq!(journal.load(1, &tx.from), Ok(None));
        journal.record(&tx).unwrap();
        assert_eq!(journal.load(1, &tx.from), Ok(Some(tx.clone())));
        assert_eq!(journal.load(10, &tx.from), Ok(None));
        let newer = PendingTx {
            nonce: 10,
            ..tx.clone()
        };
        journal.record(&newer).unwrap();
        assert_eq!(journal.load(1, &tx.from), Ok(Some(newer)));
        journal.clear(1, &tx.from);
        assert_eq!(journal.load(1, &tx.from), Ok(None));
    }

    #[test]
    fn corrupted_records_are_reported() {
        let dir = TempDir::new();
        let journal = Journal::new(dir.0.join("pending"));
        let tx = sample();
        journal.record(&tx).unwrap();
        fs::write(journal.path_for(1, &tx.from), "not json").unwrap();
        assert!(journal.load(1, &tx.from).is_err());
    }
}
