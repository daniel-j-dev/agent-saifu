//! Creating, importing, sealing and unsealing Solana wallets.
//!
//! Keys are Ed25519. A seed phrase derives its account with SLIP-0010 at `m/44'/501'/0'/0'`,
//! the path Phantom uses for its first account, so a phrase from Phantom gives the same address.
//! The vault is encrypted exactly like the Ethereum and Sui wallets' (MetaMask's scheme) and its
//! plaintext follows the same keyring layout (`HD Key Tree` / `Simple Key Pair`), plus the one
//! cluster the wallet is bound to.

use super::address::{self, Pubkey};
use super::network::SolanaNetwork;
use crate::crypto::secret::{SecretBytes, SecretString, zeroize};
use crate::crypto::slip10::{Ed25519Node, SOLANA_DEFAULT_PATH};
use crate::crypto::{base58, bip39, ed25519, hex};
use crate::json::{self, Value};
use crate::vault::{self, EncryptedVault};

const HD_TYPE: &str = "HD Key Tree";
const SIMPLE_TYPE: &str = "Simple Key Pair";
const SEED_LEN: usize = 32;
const KEYPAIR_LEN: usize = 64;
const MAX_KEY_TEXT: usize = 2000;

pub enum Keyring {
    Hd { mnemonic: SecretString },
    Simple { private_key: SecretBytes },
}

pub struct SolanaVault {
    pub keyring: Keyring,
    pub network: SolanaNetwork,
}

impl std::fmt::Debug for SolanaVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SolanaVault(<redacted>)")
    }
}

/// A 32-byte secret, or a 64-byte keypair whose second half must be the matching public key.
fn secret_from_bytes(bytes: &[u8]) -> Result<SecretBytes, String> {
    match bytes.len() {
        SEED_LEN => Ok(SecretBytes::new(bytes.to_vec())),
        KEYPAIR_LEN => {
            let mut seed = [0u8; SEED_LEN];
            seed.copy_from_slice(&bytes[..SEED_LEN]);
            let matches = ed25519::public_key(&seed)[..] == bytes[SEED_LEN..];
            let secret = SecretBytes::new(seed.to_vec());
            zeroize(&mut seed);
            if matches {
                Ok(secret)
            } else {
                Err("the keypair's public half does not match its private half".to_string())
            }
        }
        _ => Err("a Solana private key is 32 bytes (or a 64-byte keypair)".to_string()),
    }
}

/// The byte list of a Solana CLI keypair file, e.g. `[12,34,...]`.
fn bytes_from_json_array(text: &str) -> Result<Vec<u8>, String> {
    let bad = || "private key is not a valid list of bytes".to_string();
    let mut doc = json::parse(text).map_err(|_| bad())?;
    let result = (|| {
        let items = doc.as_array().ok_or_else(bad)?;
        if items.len() != SEED_LEN && items.len() != KEYPAIR_LEN {
            return Err(bad());
        }
        items
            .iter()
            .map(|item| match item {
                Value::Number(n) => n.parse::<u8>().map_err(|_| bad()),
                _ => Err(bad()),
            })
            .collect::<Result<Vec<u8>, String>>()
    })();
    doc.wipe();
    result
}

/// Accepts the formats Solana wallets export: base58 of the 64-byte keypair (what Phantom and
/// Solflare show), a Solana CLI keypair file's byte list, or hex of the 32-byte secret (or of
/// the 64-byte keypair). Returns the 32-byte secret.
pub fn parse_private_key(input: &str) -> Result<SecretBytes, String> {
    let text = input.trim();
    if text.len() > MAX_KEY_TEXT {
        return Err("private key is too long".to_string());
    }
    let mut bytes = if text.starts_with('[') {
        bytes_from_json_array(text)?
    } else {
        let digits = hex::strip_prefix(text);
        let hex_like = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit());
        if hex_like && (digits.len() == 2 * SEED_LEN || digits.len() == 2 * KEYPAIR_LEN) {
            hex::decode(digits).map_err(|_| "private key contains invalid hex".to_string())?
        } else {
            base58::decode(text).map_err(|_| {
                "private key must be base58 (as Phantom shows it), a list of bytes or hex"
                    .to_string()
            })?
        }
    };
    let result = secret_from_bytes(&bytes);
    zeroize(&mut bytes);
    result
}

/// First Solana account (`m/44'/501'/0'/0'`) of a mnemonic.
pub fn derive_account_key(mnemonic: &str) -> Result<SecretBytes, String> {
    let seed = bip39::mnemonic_to_seed(mnemonic, "")?;
    let master = Ed25519Node::from_seed(seed.expose())?;
    let account = master.derive_path(SOLANA_DEFAULT_PATH)?;
    Ok(SecretBytes::new(account.private_key().to_vec()))
}

fn seed_array(bytes: &[u8]) -> Result<[u8; SEED_LEN], String> {
    <[u8; SEED_LEN]>::try_from(bytes).map_err(|_| "private key must be 32 bytes".to_string())
}

impl SolanaVault {
    pub fn create_new(network: SolanaNetwork) -> Result<SolanaVault, String> {
        Ok(SolanaVault {
            keyring: Keyring::Hd {
                mnemonic: bip39::generate_mnemonic()?,
            },
            network,
        })
    }

    /// Imports a 24-word seed phrase (checksum-validated).
    pub fn from_mnemonic(phrase: &str, network: SolanaNetwork) -> Result<SolanaVault, String> {
        let mnemonic = bip39::validate_mnemonic(phrase, bip39::REQUIRED_WORD_COUNT)?;
        Ok(SolanaVault {
            keyring: Keyring::Hd { mnemonic },
            network,
        })
    }

    pub fn from_private_key(input: &str, network: SolanaNetwork) -> Result<SolanaVault, String> {
        Ok(SolanaVault {
            keyring: Keyring::Simple {
                private_key: parse_private_key(input)?,
            },
            network,
        })
    }

    pub fn mnemonic(&self) -> Option<&SecretString> {
        match &self.keyring {
            Keyring::Hd { mnemonic } => Some(mnemonic),
            Keyring::Simple { .. } => None,
        }
    }

    /// The account's 32-byte secret.
    pub fn private_key(&self) -> Result<SecretBytes, String> {
        match &self.keyring {
            Keyring::Simple { private_key } => Ok(private_key.clone()),
            Keyring::Hd { mnemonic } => derive_account_key(mnemonic.expose()),
        }
    }

    pub fn public_key(&self) -> Result<Pubkey, String> {
        let key = self.private_key()?;
        let mut seed = seed_array(key.expose())?;
        let public = ed25519::public_key(&seed);
        zeroize(&mut seed);
        Ok(public)
    }

    pub fn address(&self) -> Result<String, String> {
        Ok(address::to_text(&self.public_key()?))
    }

    /// An Ed25519 signature over `message`.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], String> {
        let key = self.private_key()?;
        let mut seed = seed_array(key.expose())?;
        let signature = ed25519::sign(&seed, message);
        zeroize(&mut seed);
        Ok(signature)
    }

    /// The key as base58 of the 64-byte keypair (secret then public key): the format Phantom and
    /// Solflare show and import.
    pub fn keypair_base58(&self) -> Result<SecretString, String> {
        let key = self.private_key()?;
        let mut keypair = key.expose().to_vec();
        keypair.extend_from_slice(&self.public_key()?);
        let text = base58::encode(&keypair);
        zeroize(&mut keypair);
        Ok(SecretString::new(text))
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
                        ("hdPath", Value::str(SOLANA_DEFAULT_PATH)),
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
            ("solanaNetwork", Value::str(self.network.id())),
        ]);
        let text = doc.to_json();
        doc.wipe();
        SecretBytes::new(text.into_bytes())
    }

    pub fn from_plaintext(bytes: &[u8]) -> Result<SolanaVault, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "vault contents are corrupted".to_string())?;
        let mut doc = json::parse(text).map_err(|_| "vault contents are corrupted".to_string())?;
        let result = Self::from_value(&doc);
        doc.wipe();
        result
    }

    fn from_value(doc: &Value) -> Result<SolanaVault, String> {
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
                if data.get("hdPath").and_then(Value::as_str) != Some(SOLANA_DEFAULT_PATH) {
                    return Err("vault uses an unexpected derivation path".to_string());
                }
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
        let network = doc
            .get("solanaNetwork")
            .and_then(Value::as_str)
            .ok_or("vault has no network")
            .and_then(|n| SolanaNetwork::parse(n).map_err(|_| "vault has an unknown network"))?;
        Ok(SolanaVault { keyring, network })
    }

    pub fn seal(&self, password: &str, iterations: u32) -> Result<EncryptedVault, String> {
        let plaintext = self.to_plaintext();
        vault::encrypt(password, plaintext.expose(), iterations)
    }

    pub fn unseal(vault: &EncryptedVault, password: &str) -> Result<SolanaVault, String> {
        let plaintext = vault::decrypt(password, vault)?;
        SolanaVault::from_plaintext(plaintext.expose())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::MIN_ITERATIONS;

    // Phantom's address for this well-known test phrase at m/44'/501'/0'/0'; the 24-word values
    // below were also derived independently (PBKDF2 + SLIP-0010 + RFC 8032 in Python).
    const ABOUT_PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const ABOUT_ADDRESS: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
    const ART_ADDRESS: &str = "3Cy3YNTFywCmxoxt8n7UH6hg6dLo5uACowX3CFceaSnx";
    const ART_SECRET: &str = "7c139e1a603ca04f5f7cff194e1bb6f6d1b9098470ea90695ab628488a9f921b";

    fn abandon_art() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    fn art_vault() -> SolanaVault {
        SolanaVault::from_mnemonic(&abandon_art(), SolanaNetwork::Mainnet).unwrap()
    }

    #[test]
    fn derives_phantoms_address_from_a_known_phrase() {
        let key = derive_account_key(ABOUT_PHRASE).unwrap();
        let vault = SolanaVault {
            keyring: Keyring::Simple { private_key: key },
            network: SolanaNetwork::Mainnet,
        };
        assert_eq!(vault.address().unwrap(), ABOUT_ADDRESS);
    }

    #[test]
    fn derives_the_24_word_vector() {
        let vault = art_vault();
        assert_eq!(vault.address().unwrap(), ART_ADDRESS);
        assert_eq!(
            hex::encode(vault.private_key().unwrap().expose()),
            ART_SECRET
        );
    }

    #[test]
    fn created_wallets_are_24_words_and_unique() {
        let a = SolanaVault::create_new(SolanaNetwork::Mainnet).unwrap();
        let b = SolanaVault::create_new(SolanaNetwork::Mainnet).unwrap();
        let (ma, mb) = (a.mnemonic().unwrap(), b.mnemonic().unwrap());
        assert_eq!(ma.expose().split_whitespace().count(), 24);
        assert_ne!(ma.expose(), mb.expose());
        assert_ne!(a.address().unwrap(), b.address().unwrap());
    }

    #[test]
    fn imports_only_24_word_checksummed_phrases() {
        assert!(SolanaVault::from_mnemonic(&abandon_art(), SolanaNetwork::Mainnet).is_ok());
        assert!(SolanaVault::from_mnemonic(ABOUT_PHRASE, SolanaNetwork::Mainnet).is_err());
        let bad = format!("{}abandon", "abandon ".repeat(23));
        assert!(SolanaVault::from_mnemonic(&bad, SolanaNetwork::Mainnet).is_err());
    }

    #[test]
    fn private_key_formats_all_give_the_same_key() {
        let vault = art_vault();
        let raw = vault.private_key().unwrap();
        let keypair = vault.keypair_base58().unwrap();
        let mut keypair_bytes = raw.expose().to_vec();
        keypair_bytes.extend_from_slice(&vault.public_key().unwrap());
        let byte_list = format!(
            "[{}]",
            keypair_bytes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let seed_list = format!(
            "[ {} ]",
            raw.expose()
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        for text in [
            keypair.expose().to_string(),
            base58::encode(raw.expose()),
            hex::encode(raw.expose()),
            format!("0x{}", hex::encode(raw.expose())),
            hex::encode(&keypair_bytes),
            format!("  {byte_list}\n"),
            seed_list,
        ] {
            let imported = SolanaVault::from_private_key(&text, SolanaNetwork::Mainnet).unwrap();
            assert_eq!(
                imported.private_key().unwrap().expose(),
                raw.expose(),
                "{text}"
            );
            assert_eq!(imported.address().unwrap(), ART_ADDRESS);
        }
    }

    #[test]
    fn exported_keypair_is_base58_of_secret_then_public_key() {
        let vault = art_vault();
        let text = vault.keypair_base58().unwrap();
        let bytes = base58::decode(text.expose()).unwrap();
        assert_eq!(bytes.len(), 64);
        assert_eq!(hex::encode(&bytes[..32]), ART_SECRET);
        assert_eq!(base58::encode(&bytes[32..]), ART_ADDRESS);
    }

    #[test]
    fn rejects_bad_private_keys() {
        let mismatched = {
            let mut pair = vec![1u8; 32];
            pair.extend_from_slice(&[2u8; 32]);
            base58::encode(&pair)
        };
        let too_many = format!("[{}]", vec!["1"; 40].join(","));
        let out_of_range = format!("[{}]", vec!["300"; 32].join(","));
        for bad in [
            "",
            "0x1234",
            &"zz".repeat(32),
            "not a key",
            &"a".repeat(3000),
            &mismatched,
            &too_many,
            &out_of_range,
            "[1,2,3]",
            "[\"a\"]",
            &base58::encode(&[1u8; 31]),
            &base58::encode(&[1u8; 65]),
        ] {
            let err = SolanaVault::from_private_key(bad, SolanaNetwork::Mainnet).unwrap_err();
            assert!(!err.contains("zzzz"), "errors must not echo the input");
        }
    }

    #[test]
    fn plaintext_round_trip_for_both_keyrings() {
        let hd = SolanaVault::from_mnemonic(&abandon_art(), SolanaNetwork::Devnet).unwrap();
        let back = SolanaVault::from_plaintext(hd.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, SolanaNetwork::Devnet);
        assert_eq!(back.mnemonic().unwrap().expose(), abandon_art());
        assert_eq!(back.address().unwrap(), ART_ADDRESS);

        let simple =
            SolanaVault::from_private_key(&"11".repeat(32), SolanaNetwork::Testnet).unwrap();
        let back = SolanaVault::from_plaintext(simple.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, SolanaNetwork::Testnet);
        assert!(back.mnemonic().is_none());
        assert_eq!(back.address().unwrap(), simple.address().unwrap());
    }

    #[test]
    fn plaintext_uses_metamasks_keyring_layout() {
        let text = String::from_utf8(art_vault().to_plaintext().expose().to_vec()).unwrap();
        assert!(text.contains("\"HD Key Tree\"") && text.contains("m/44'/501'/0'/0'"));
        let simple =
            SolanaVault::from_private_key(&"11".repeat(32), SolanaNetwork::Mainnet).unwrap();
        let text = String::from_utf8(simple.to_plaintext().expose().to_vec()).unwrap();
        assert!(text.contains("\"Simple Key Pair\""));
    }

    #[test]
    fn corrupted_plaintext_is_rejected() {
        let good = art_vault().to_plaintext();
        let text = String::from_utf8(good.expose().to_vec()).unwrap();
        for bad in [
            "not json".to_string(),
            "{}".to_string(),
            r#"{"keyrings":[],"solanaNetwork":"mainnet"}"#.to_string(),
            text.replace("mainnet", "nowhere"),
            text.replace("m/44'/501'/0'/0'", "m/44'/60'/0'/0"),
            text.replace("HD Key Tree", "Mystery"),
            text.replace(",\"solanaNetwork\":\"mainnet\"", ""),
        ] {
            assert!(
                SolanaVault::from_plaintext(bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
        assert!(SolanaVault::from_plaintext(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn seal_and_unseal() {
        let vault = art_vault();
        let sealed = vault.seal("correct horse", MIN_ITERATIONS).unwrap();
        assert!(!sealed.data.contains("abandon"));
        let opened = SolanaVault::unseal(&sealed, "correct horse").unwrap();
        assert_eq!(opened.address().unwrap(), ART_ADDRESS);
        assert_eq!(
            SolanaVault::unseal(&sealed, "wrong").unwrap_err(),
            vault::WRONG_PASSWORD
        );
    }

    #[test]
    fn signatures_verify_for_the_wallets_address() {
        let vault = art_vault();
        let signature = vault.sign(b"hello").unwrap();
        let public = vault.public_key().unwrap();
        assert!(ed25519::verify(&public, b"hello", &signature));
        assert!(!ed25519::verify(&public, b"hellO", &signature));
    }

    #[test]
    fn debug_output_is_redacted() {
        assert_eq!(format!("{:?}", art_vault()), "SolanaVault(<redacted>)");
    }
}
