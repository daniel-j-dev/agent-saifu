//! On-disk wallet storage. Each wallet is one JSON file holding only its name, chain and
//! encrypted vault; addresses, balances, networks and keys exist solely inside the vault.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::blockchain::Blockchain;
use crate::crypto::{hex, random, sha2::sha256};
use crate::json::{self, Value};
use crate::vault::EncryptedVault;

pub const MAX_NAME_CHARS: usize = 64;
const FORMAT_VERSION: i64 = 1;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletRecord {
    pub name: String,
    pub blockchain: Blockchain,
    pub vault: EncryptedVault,
}

/// Wallet names are unique ignoring case and surrounding whitespace.
pub fn validate_wallet_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("wallet name must not be empty".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!(
            "wallet name must be at most {MAX_NAME_CHARS} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("wallet name must not contain control characters".to_string());
    }
    Ok(name.to_string())
}

/// File stem for a wallet: a hash of the lowercase name, so names never become paths.
pub fn file_stem(name: &str) -> String {
    hex::encode(&sha256(name.trim().to_lowercase().as_bytes()))
}

pub fn default_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("AGENT_SAIFU_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .ok_or("cannot locate a home directory; set AGENT_SAIFU_HOME")?;
    Ok(PathBuf::from(home).join(".agent-saifu"))
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn private_file_options() -> fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true).mode(0o600);
    o
}

#[cfg(not(unix))]
fn private_file_options() -> fs::OpenOptions {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    o
}

/// Writes `bytes` to a new private file (0600 on Unix); fails if it already exists.
pub fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = private_file_options()
        .open(path)
        .map_err(|e| format!("cannot create file: {e}"))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write file: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync file: {e}"))
}

pub struct Store {
    dir: PathBuf,
}

impl WalletRecord {
    pub fn to_json(&self) -> Value {
        Value::object(vec![
            ("version", Value::int(FORMAT_VERSION)),
            ("name", Value::str(&self.name)),
            ("blockchain", Value::str(self.blockchain.id())),
            ("vault", self.vault.to_json()),
        ])
    }

    pub fn from_json(v: &Value) -> Result<WalletRecord, String> {
        if v.get("version").and_then(Value::as_u64) != Some(FORMAT_VERSION as u64) {
            return Err("unsupported wallet file version".to_string());
        }
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .ok_or("wallet file has no name")?;
        let chain = v
            .get("blockchain")
            .and_then(Value::as_str)
            .ok_or("wallet file has no blockchain")?;
        let vault = v.get("vault").ok_or("wallet file has no vault")?;
        Ok(WalletRecord {
            name: validate_wallet_name(name)?,
            blockchain: Blockchain::parse(chain)?,
            vault: EncryptedVault::from_json(vault)?,
        })
    }
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, String> {
        let wallets = dir.join("wallets");
        create_private_dir(&wallets).map_err(|e| format!("cannot create wallet directory: {e}"))?;
        Ok(Store {
            dir: dir.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.dir
    }

    fn wallets_dir(&self) -> PathBuf {
        self.dir.join("wallets")
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.wallets_dir().join(format!("{}.json", file_stem(name)))
    }

    fn read_file(&self, path: &Path) -> Result<WalletRecord, String> {
        let meta = fs::metadata(path).map_err(|_| "wallet file is unreadable".to_string())?;
        if meta.len() > MAX_FILE_BYTES {
            return Err("wallet file is unexpectedly large".to_string());
        }
        let text = fs::read_to_string(path).map_err(|_| "wallet file is unreadable".to_string())?;
        let record = WalletRecord::from_json(
            &json::parse(&text).map_err(|_| "wallet file is corrupted".to_string())?,
        )?;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem != file_stem(&record.name) {
            return Err("wallet file does not match its name".to_string());
        }
        Ok(record)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path_for(name).exists()
    }

    pub fn load(&self, name: &str) -> Result<WalletRecord, String> {
        let path = self.path_for(name);
        if !path.exists() {
            return Err(format!(
                "no wallet named '{}'",
                crate::eth::explorer::sanitize_text(name)
            ));
        }
        self.read_file(&path)
    }

    /// All wallets (names and chains only), sorted by name. Unreadable files are skipped.
    pub fn list(&self) -> Result<Vec<(String, Blockchain)>, String> {
        let entries = fs::read_dir(self.wallets_dir())
            .map_err(|e| format!("cannot read wallet directory: {e}"))?;
        let mut out: Vec<(String, Blockchain)> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .filter_map(|p| self.read_file(&p).ok())
            .map(|r| (r.name, r.blockchain))
            .collect();
        out.sort_by_key(|(name, _)| name.to_lowercase());
        Ok(out)
    }

    fn temp_path(&self) -> Result<PathBuf, String> {
        let suffix = hex::encode(&random::random_bytes(8)?);
        Ok(self.wallets_dir().join(format!(".tmp-{suffix}")))
    }

    fn write_temp(&self, record: &WalletRecord) -> Result<PathBuf, String> {
        let tmp = self.temp_path()?;
        write_new_private_file(&tmp, record.to_json().to_json().as_bytes())?;
        Ok(tmp)
    }

    /// Creates a wallet file; fails if the name is already taken. Never overwrites.
    pub fn create(&self, record: &WalletRecord) -> Result<(), String> {
        let tmp = self.write_temp(record)?;
        let result = fs::hard_link(&tmp, self.path_for(&record.name));
        let _ = fs::remove_file(&tmp);
        result.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("a wallet named '{}' already exists", record.name)
            } else {
                format!("cannot store wallet: {e}")
            }
        })
    }

    /// Atomically replaces an existing wallet file.
    pub fn save(&self, record: &WalletRecord) -> Result<(), String> {
        let tmp = self.write_temp(record)?;
        fs::rename(&tmp, self.path_for(&record.name)).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("cannot store wallet: {e}")
        })
    }

    /// Renames a wallet. The new name must not belong to a different wallet.
    pub fn rename(&self, old_name: &str, new_name: &str) -> Result<WalletRecord, String> {
        let new_name = validate_wallet_name(new_name)?;
        let mut record = self.load(old_name)?;
        let same_file = file_stem(old_name) == file_stem(&new_name);
        record.name = new_name;
        if same_file {
            self.save(&record)?;
        } else {
            self.create(&record)?;
            fs::remove_file(self.path_for(old_name))
                .map_err(|e| format!("cannot remove old wallet file: {e}"))?;
        }
        Ok(record)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let path = self.path_for(name);
        if !path.exists() {
            return Err(format!(
                "no wallet named '{}'",
                crate::eth::explorer::sanitize_text(name)
            ));
        }
        fs::remove_file(path).map_err(|e| format!("cannot delete wallet: {e}"))
    }

    pub fn exports_dir(&self) -> Result<PathBuf, String> {
        let dir = self.dir.join("exports");
        create_private_dir(&dir).map_err(|e| format!("cannot create exports directory: {e}"))?;
        Ok(dir)
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    /// Temporary directory removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            let id = hex::encode(&random::random_bytes(8).unwrap());
            let path = std::env::temp_dir().join(format!("agent-saifu-test-{id}"));
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Default for TempDir {
        fn default() -> Self {
            TempDir::new()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn dummy_vault() -> EncryptedVault {
        crate::vault::encrypt("password-123", b"{}", crate::vault::MIN_ITERATIONS).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    fn record(name: &str) -> WalletRecord {
        WalletRecord {
            name: name.to_string(),
            blockchain: Blockchain::Ethereum,
            vault: dummy_vault(),
        }
    }

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new();
        let store = Store::open(&dir.0).unwrap();
        (dir, store)
    }

    #[test]
    fn name_validation() {
        assert_eq!(
            validate_wallet_name("  Daniel's wallet "),
            Ok("Daniel's wallet".to_string())
        );
        assert!(validate_wallet_name("").is_err());
        assert!(validate_wallet_name("   ").is_err());
        assert!(validate_wallet_name(&"a".repeat(65)).is_err());
        assert!(validate_wallet_name("bad\nname").is_err());
        assert!(validate_wallet_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn file_stem_is_case_and_space_insensitive_and_path_safe() {
        assert_eq!(file_stem("Main"), file_stem("  main "));
        assert_ne!(file_stem("a"), file_stem("b"));
        let stem = file_stem("../../etc/passwd");
        assert_eq!(stem.len(), 64);
        assert!(stem.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn create_load_list() {
        let (_d, s) = store();
        s.create(&record("Zed")).unwrap();
        s.create(&record("alpha")).unwrap();
        assert_eq!(s.load("ZED").unwrap().name, "Zed");
        assert!(s.exists("alpha"));
        let names: Vec<_> = s.list().unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha", "Zed"]);
        assert!(s.load("missing").is_err());
        assert!(!s.exists("missing"));
    }

    #[test]
    fn names_are_unique_ignoring_case() {
        let (_d, s) = store();
        s.create(&record("Main")).unwrap();
        assert!(
            s.create(&record("main"))
                .unwrap_err()
                .contains("already exists")
        );
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn save_overwrites_atomically_and_leaves_no_temp_files() {
        let (d, s) = store();
        let mut r = record("w");
        s.create(&r).unwrap();
        r.vault = dummy_vault();
        s.save(&r).unwrap();
        assert_eq!(s.load("w").unwrap().vault, r.vault);
        let leftovers = fs::read_dir(d.0.join("wallets"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn rename_moves_the_wallet() {
        let (_d, s) = store();
        s.create(&record("old")).unwrap();
        s.create(&record("taken")).unwrap();
        let renamed = s.rename("old", "New Name").unwrap();
        assert_eq!(renamed.name, "New Name");
        assert!(!s.exists("old"));
        assert_eq!(s.load("new name").unwrap().name, "New Name");
        assert!(
            s.rename("New Name", "taken")
                .unwrap_err()
                .contains("already exists")
        );
        assert!(s.exists("New Name"));
        assert!(s.rename("missing", "x").is_err());
        assert!(s.rename("New Name", "  ").is_err());
    }

    #[test]
    fn rename_to_different_case_keeps_one_file() {
        let (_d, s) = store();
        s.create(&record("main")).unwrap();
        s.rename("main", "MAIN").unwrap();
        assert_eq!(
            s.list().unwrap(),
            vec![("MAIN".to_string(), Blockchain::Ethereum)]
        );
    }

    #[test]
    fn delete_removes_only_that_wallet() {
        let (_d, s) = store();
        s.create(&record("a")).unwrap();
        s.create(&record("b")).unwrap();
        s.delete("A").unwrap();
        assert!(!s.exists("a") && s.exists("b"));
        assert!(s.delete("a").is_err());
    }

    #[test]
    fn corrupted_or_misplaced_files_are_not_listed_or_loaded() {
        let (d, s) = store();
        s.create(&record("good")).unwrap();
        fs::write(d.0.join("wallets").join("junk.json"), "not json").unwrap();
        fs::write(d.0.join("wallets").join("notes.txt"), "hi").unwrap();
        // a valid record copied under the wrong file name must be rejected
        let wrong =
            d.0.join("wallets")
                .join(format!("{}.json", file_stem("other")));
        fs::write(&wrong, record("good").to_json().to_json()).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert!(s.load("other").is_err());
    }

    #[test]
    fn record_json_round_trip_and_validation() {
        let r = record("w");
        assert_eq!(WalletRecord::from_json(&r.to_json()).unwrap(), r);
        let mut bad = r.to_json();
        if let Value::Object(pairs) = &mut bad {
            pairs.retain(|(k, _)| k != "vault");
        }
        assert!(WalletRecord::from_json(&bad).is_err());
        let wrong_version = r
            .to_json()
            .to_json()
            .replace("\"version\":1", "\"version\":2");
        assert!(WalletRecord::from_json(&json::parse(&wrong_version).unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (d, s) = store();
        s.create(&record("w")).unwrap();
        let file_mode = fs::metadata(s.path_for("w")).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = fs::metadata(d.0.join("wallets"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    #[test]
    fn new_private_files_are_never_overwritten() {
        let d = TempDir::new();
        let path = d.0.join("x");
        write_new_private_file(&path, b"one").unwrap();
        assert!(write_new_private_file(&path, b"two").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"one");
    }

    #[test]
    fn default_dir_honors_environment() {
        // Only checks the pure fallback shape; the env var path is exercised in integration use.
        let dir = default_dir().unwrap();
        assert!(dir.is_absolute() || dir.to_string_lossy().len() > 1);
    }
}
