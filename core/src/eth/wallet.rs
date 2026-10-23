//! Creating, importing, sealing and unsealing Ethereum wallets.
//!
//! A wallet's secrets live only inside the encrypted vault. The vault plaintext follows
//! MetaMask's keyring layout (`HD Key Tree` / `Simple Key Pair`), plus this wallet's networks and added tokens.

use super::address::{Address, address_from_private_key};
use super::networks::Network;
use super::tokens::CustomToken;
use crate::crypto::bip32::{ETH_DEFAULT_PATH, ExtendedKey};
use crate::crypto::bip39;
use crate::crypto::secp256k1;
use crate::crypto::secret::{SecretBytes, SecretString, zeroize};
use crate::crypto::{hex, random};
use crate::json::{self, Value};
use crate::vault::{self, EncryptedVault};

pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MIN_EXPORT_KEY_CHARS: usize = 12;
const HD_TYPE: &str = "HD Key Tree";
const SIMPLE_TYPE: &str = "Simple Key Pair";
const HD_PATH_PREFIX: &str = "m/44'/60'/0'/0";

pub enum Keyring {
    Hd { mnemonic: SecretString },
    Simple { private_key: SecretBytes },
}

pub struct VaultContents {
    pub keyring: Keyring,
    pub networks: Vec<Network>,
    pub tokens: Vec<CustomToken>,
}

pub fn validate_password(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "password must be at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    Ok(())
}

pub fn validate_export_key(key: &str) -> Result<(), String> {
    if key.chars().count() < MIN_EXPORT_KEY_CHARS {
        return Err(format!(
            "encryption key must be at least {MIN_EXPORT_KEY_CHARS} characters"
        ));
    }
    Ok(())
}

/// Parses a 32-byte private key given as 64 hex digits (optional `0x`).
pub fn parse_private_key(input: &str) -> Result<SecretBytes, String> {
    let trimmed = input.trim();
    let digits = hex::strip_prefix(trimmed);
    if digits.len() != 64 {
        return Err("private key must be 64 hex characters".to_string());
    }
    let mut bytes = hex::decode_fixed::<32>(digits)
        .map_err(|_| "private key contains invalid hex".to_string())?;
    let valid = secp256k1::is_valid_private_key(&bytes);
    let secret = SecretBytes::new(bytes.to_vec());
    zeroize(&mut bytes);
    if !valid {
        return Err("private key is not a valid secp256k1 key".to_string());
    }
    Ok(secret)
}

impl std::fmt::Debug for VaultContents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultContents(<redacted>)")
    }
}

impl VaultContents {
    pub fn create_new() -> Result<VaultContents, String> {
        Ok(VaultContents {
            keyring: Keyring::Hd {
                mnemonic: bip39::generate_mnemonic()?,
            },
            networks: vec![],
            tokens: vec![],
        })
    }

    /// Imports a 24-word seed phrase (checksum-validated).
    pub fn from_mnemonic(phrase: &str) -> Result<VaultContents, String> {
        let mnemonic = bip39::validate_mnemonic(phrase, bip39::REQUIRED_WORD_COUNT)?;
        Ok(VaultContents {
            keyring: Keyring::Hd { mnemonic },
            networks: vec![],
            tokens: vec![],
        })
    }

    pub fn from_private_key(input: &str) -> Result<VaultContents, String> {
        Ok(VaultContents {
            keyring: Keyring::Simple {
                private_key: parse_private_key(input)?,
            },
            networks: vec![],
            tokens: vec![],
        })
    }

    pub fn mnemonic(&self) -> Option<&SecretString> {
        match &self.keyring {
            Keyring::Hd { mnemonic } => Some(mnemonic),
            Keyring::Simple { .. } => None,
        }
    }

    /// The account's private key (first Ethereum account for seed-phrase wallets).
    pub fn private_key(&self) -> Result<SecretBytes, String> {
        match &self.keyring {
            Keyring::Simple { private_key } => Ok(private_key.clone()),
            Keyring::Hd { mnemonic } => derive_account_key(mnemonic.expose()),
        }
    }

    pub fn private_key_array(&self) -> Result<SecretKey32, String> {
        let key = self.private_key()?;
        SecretKey32::from_slice(key.expose())
    }

    pub fn address(&self) -> Result<Address, String> {
        address_from_private_key(&self.private_key_array()?.0)
    }

    pub fn to_plaintext(&self) -> SecretBytes {
        let keyring = match &self.keyring {
            Keyring::Hd { mnemonic } => Value::object(vec![
                ("type", Value::str(HD_TYPE)),
                (
                    "data",
                    Value::object(vec![
                        ("mnemonic", Value::str(mnemonic.expose())),
                        ("numberOfAccounts", Value::int(1)),
                        ("hdPath", Value::str(HD_PATH_PREFIX)),
                    ]),
                ),
            ]),
            Keyring::Simple { private_key } => Value::object(vec![
                ("type", Value::str(SIMPLE_TYPE)),
                (
                    "data",
                    Value::Array(vec![Value::String(hex::encode(private_key.expose()))]),
                ),
            ]),
        };
        let mut doc = Value::object(vec![
            ("keyrings", Value::Array(vec![keyring])),
            (
                "networks",
                Value::Array(self.networks.iter().map(Network::to_json).collect()),
            ),
            (
                "tokens",
                Value::Array(self.tokens.iter().map(CustomToken::to_json).collect()),
            ),
        ]);
        let text = doc.to_json();
        doc.wipe();
        SecretBytes::new(text.into_bytes())
    }

    pub fn from_plaintext(bytes: &[u8]) -> Result<VaultContents, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "vault contents are corrupted".to_string())?;
        let mut doc = json::parse(text).map_err(|_| "vault contents are corrupted".to_string())?;
        let result = Self::from_value(&doc);
        doc.wipe();
        result
    }

    fn from_value(doc: &Value) -> Result<VaultContents, String> {
        let keyrings = doc
            .get("keyrings")
            .and_then(Value::as_array)
            .ok_or("vault has no keyrings")?;
        if keyrings.len() != 1 {
            return Err("vault must contain exactly one keyring".to_string());
        }
        let keyring = &keyrings[0];
        let data = keyring.get("data").ok_or("keyring has no data")?;
        let keyring = match keyring.get("type").and_then(Value::as_str) {
            Some(HD_TYPE) => {
                let phrase = data
                    .get("mnemonic")
                    .and_then(Value::as_str)
                    .ok_or("keyring has no mnemonic")?;
                Keyring::Hd {
                    mnemonic: bip39::validate_mnemonic(phrase, bip39::REQUIRED_WORD_COUNT)?,
                }
            }
            Some(SIMPLE_TYPE) => {
                let key = data
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .ok_or("keyring has no private key")?;
                Keyring::Simple {
                    private_key: parse_private_key(key)?,
                }
            }
            _ => return Err("unknown keyring type".to_string()),
        };
        let networks = match doc.get("networks").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .map(Network::from_json)
                .collect::<Result<Vec<_>, _>>()?,
            None => vec![],
        };
        // Vaults created before tokens existed have no "tokens" entry.
        let tokens = match doc.get("tokens").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .map(CustomToken::from_json)
                .collect::<Result<Vec<_>, _>>()?,
            None => vec![],
        };
        Ok(VaultContents {
            keyring,
            networks,
            tokens,
        })
    }

    pub fn seal(&self, password: &str, iterations: u32) -> Result<EncryptedVault, String> {
        let plaintext = self.to_plaintext();
        vault::encrypt(password, plaintext.expose(), iterations)
    }

    pub fn unseal(vault: &EncryptedVault, password: &str) -> Result<VaultContents, String> {
        let plaintext = vault::decrypt(password, vault)?;
        VaultContents::from_plaintext(plaintext.expose())
    }
}

/// First Ethereum account (`m/44'/60'/0'/0/0`) of a mnemonic.
pub fn derive_account_key(mnemonic: &str) -> Result<SecretBytes, String> {
    let seed = bip39::mnemonic_to_seed(mnemonic, "")?;
    let master = ExtendedKey::from_seed(seed.expose())?;
    let account = master.derive_path(ETH_DEFAULT_PATH)?;
    Ok(SecretBytes::new(account.private_key().to_vec()))
}

/// A 32-byte key that is wiped on drop.
pub struct SecretKey32(pub [u8; 32]);

impl SecretKey32 {
    pub fn from_slice(bytes: &[u8]) -> Result<SecretKey32, String> {
        <[u8; 32]>::try_from(bytes)
            .map(SecretKey32)
            .map_err(|_| "private key must be 32 bytes".to_string())
    }
}

impl Drop for SecretKey32 {
    fn drop(&mut self) {
        zeroize(&mut self.0);
    }
}

/// Random 32-byte value; used by tests and by callers needing fresh key material.
pub fn random_key_material() -> Result<SecretKey32, String> {
    Ok(SecretKey32(random::random_array::<32>()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::address::to_checksum;

    const ITER: u32 = vault::MIN_ITERATIONS;

    fn abandon_art() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    #[test]
    fn password_and_export_key_policies() {
        assert!(validate_password("1234567").is_err());
        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("密码密码密码密码").is_ok());
        assert!(validate_export_key("short-key").is_err());
        assert!(validate_export_key("a-long-enough-key").is_ok());
    }

    #[test]
    fn created_wallets_are_24_words_and_unique() {
        let a = VaultContents::create_new().unwrap();
        let b = VaultContents::create_new().unwrap();
        assert_eq!(a.mnemonic().unwrap().expose().split(' ').count(), 24);
        assert_ne!(a.mnemonic(), b.mnemonic());
        assert_ne!(a.address().unwrap(), b.address().unwrap());
    }

    #[test]
    fn imports_seed_phrase_and_derives_the_standard_address() {
        let w = VaultContents::from_mnemonic(&abandon_art()).unwrap();
        assert_eq!(w.address().unwrap().len(), 20);
        let again = VaultContents::from_mnemonic(&abandon_art().to_uppercase()).unwrap();
        assert_eq!(w.address().unwrap(), again.address().unwrap());
    }

    #[test]
    fn known_derivation_vectors() {
        let cases = [
            (
                "test test test test test test test test test test test junk",
                "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            ),
            (
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "0x9858EfFD232B4033E47d90003D41EC34EcaEda94",
                "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727",
            ),
        ];
        for (phrase, address, key) in cases {
            let private = derive_account_key(phrase).unwrap();
            assert_eq!(hex::encode(private.expose()), key);
            let arr = SecretKey32::from_slice(private.expose()).unwrap();
            assert_eq!(
                to_checksum(&address_from_private_key(&arr.0).unwrap()),
                address
            );
        }
    }

    #[test]
    fn rejects_bad_seed_phrases() {
        assert!(VaultContents::from_mnemonic("").is_err());
        assert!(VaultContents::from_mnemonic(&"abandon ".repeat(24)).is_err());
        let twelve = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        assert!(
            VaultContents::from_mnemonic(twelve)
                .unwrap_err()
                .contains("24 words")
        );
    }

    #[test]
    fn imports_private_keys() {
        let key = format!("0x{}", "00".repeat(31) + "01");
        let w = VaultContents::from_private_key(&key).unwrap();
        assert_eq!(
            to_checksum(&w.address().unwrap()),
            "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        );
        assert!(w.mnemonic().is_none());
        assert!(VaultContents::from_private_key(&key[2..]).is_ok());
        assert!(VaultContents::from_private_key(&format!("  {key}\n")).is_ok());
    }

    #[test]
    fn rejects_bad_private_keys() {
        for bad in [
            "",
            "0x1234",
            &"00".repeat(32),
            &"ff".repeat(32),
            &"zz".repeat(32),
            &"1".repeat(65),
        ] {
            assert!(parse_private_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn plaintext_round_trip_hd_with_networks() {
        let mut w = VaultContents::from_mnemonic(&abandon_art()).unwrap();
        w.networks.push(
            Network::new(
                "Base",
                8453,
                "https://mainnet.base.org",
                "ETH",
                "https://basescan.org",
            )
            .unwrap(),
        );
        let plain = w.to_plaintext();
        let text = std::str::from_utf8(plain.expose()).unwrap();
        assert!(text.contains("HD Key Tree") && text.contains("numberOfAccounts"));
        let back = VaultContents::from_plaintext(plain.expose()).unwrap();
        assert_eq!(back.networks, w.networks);
        assert_eq!(back.address().unwrap(), w.address().unwrap());
        assert_eq!(back.mnemonic(), w.mnemonic());
    }

    #[test]
    fn tokens_are_stored_in_the_vault_and_old_vaults_still_load() {
        let mut w = VaultContents::from_mnemonic(&abandon_art()).unwrap();
        w.tokens.push(CustomToken {
            chain_id: 8453,
            address: [0xab; 20],
            symbol: "TKN".to_string(),
            decimals: 6,
        });
        let plain = w.to_plaintext();
        assert!(
            std::str::from_utf8(plain.expose())
                .unwrap()
                .contains("\"tokens\"")
        );
        let back = VaultContents::from_plaintext(plain.expose()).unwrap();
        assert_eq!(back.tokens, w.tokens);

        let old = VaultContents::from_mnemonic(&abandon_art())
            .unwrap()
            .to_plaintext();
        let without_tokens = std::str::from_utf8(old.expose())
            .unwrap()
            .replace(",\"tokens\":[]", "");
        assert!(!without_tokens.contains("tokens"));
        assert!(
            VaultContents::from_plaintext(without_tokens.as_bytes())
                .unwrap()
                .tokens
                .is_empty()
        );

        let corrupt = std::str::from_utf8(old.expose())
            .unwrap()
            .replace("\"tokens\":[]", "\"tokens\":[{}]");
        assert!(VaultContents::from_plaintext(corrupt.as_bytes()).is_err());
    }

    #[test]
    fn plaintext_round_trip_simple_key() {
        let w = VaultContents::from_private_key(&"11".repeat(32)).unwrap();
        let plain = w.to_plaintext();
        assert!(
            std::str::from_utf8(plain.expose())
                .unwrap()
                .contains("Simple Key Pair")
        );
        let back = VaultContents::from_plaintext(plain.expose()).unwrap();
        assert_eq!(back.private_key().unwrap(), w.private_key().unwrap());
    }

    #[test]
    fn corrupted_plaintext_is_rejected() {
        for bad in [
            "",
            "{",
            r#"{}"#,
            r#"{"keyrings":[]}"#,
            r#"{"keyrings":[{"type":"Other","data":{}}]}"#,
            r#"{"keyrings":[{"type":"HD Key Tree","data":{"mnemonic":"abandon"}}]}"#,
            r#"{"keyrings":[{"type":"Simple Key Pair","data":["zz"]}]}"#,
        ] {
            assert!(
                VaultContents::from_plaintext(bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
        assert!(VaultContents::from_plaintext(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn seal_and_unseal() {
        let w = VaultContents::from_mnemonic(&abandon_art()).unwrap();
        let sealed = w.seal("correct-password", ITER).unwrap();
        let opened = VaultContents::unseal(&sealed, "correct-password").unwrap();
        assert_eq!(opened.address().unwrap(), w.address().unwrap());
        assert_eq!(
            VaultContents::unseal(&sealed, "wrong-password").err(),
            Some(vault::WRONG_PASSWORD.to_string())
        );
        let json = sealed.to_json().to_json();
        assert!(!json.contains("abandon") && !json.contains("mnemonic"));
    }

    #[test]
    fn secret_key32_checks_length() {
        assert!(SecretKey32::from_slice(&[0u8; 31]).is_err());
        assert!(SecretKey32::from_slice(&[0u8; 32]).is_ok());
        assert_ne!(
            random_key_material().unwrap().0,
            random_key_material().unwrap().0
        );
    }
}
