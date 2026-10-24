//! Creating, importing, sealing and unsealing Sui wallets.
//!
//! Keys are Ed25519 (Sui's default scheme). Seed phrases derive with SLIP-0010 at
//! `m/44'/784'/0'/0'/0'`, the path every Sui wallet uses. The vault is encrypted exactly like
//! the Ethereum wallets' (MetaMask's scheme) and its plaintext follows the same keyring layout
//! (`HD Key Tree` / `Simple Key Pair`), plus the one network the wallet is bound to.

use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_sdk_types::Address;

use super::network::SuiNetwork;
use crate::crypto::secret::{SecretBytes, SecretString, zeroize, zeroize_string};
use crate::crypto::slip10::{Ed25519Node, SUI_DEFAULT_PATH};
use crate::crypto::{base64, bip39, hex};
use crate::json::{self, Value};
use crate::vault::{self, EncryptedVault};

const HD_TYPE: &str = "HD Key Tree";
const SIMPLE_TYPE: &str = "Simple Key Pair";
/// MetaMask stores the path without the account index; the account index is always 0 here.
const HD_PATH_PREFIX: &str = "m/44'/784'/0'/0'";
const ED25519_FLAG: u8 = 0;
const KEY_LEN: usize = 32;

pub enum Keyring {
    Hd { mnemonic: SecretString },
    Simple { private_key: SecretBytes },
}

pub struct SuiVault {
    pub keyring: Keyring,
    pub network: SuiNetwork,
}

impl std::fmt::Debug for SuiVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SuiVault(<redacted>)")
    }
}

/// Accepts the formats Sui wallets export: a `suiprivkey1...` Bech32 string (what Slush and the
/// Sui CLI show), the CLI keystore's base64 `flag || key`, or 64 hex characters.
/// Only Ed25519 keys are supported.
pub fn parse_private_key(input: &str) -> Result<SecretBytes, String> {
    let text = input.trim();
    if text.len() > 200 {
        return Err("private key is too long".to_string());
    }
    if text.starts_with("suiprivkey") {
        let key = Ed25519PrivateKey::from_suiprivkey(text).map_err(|_| {
            "not a valid Ed25519 suiprivkey (only Ed25519 keys are supported)".to_string()
        })?;
        return key_bytes(&key);
    }
    let digits = hex::strip_prefix(text);
    if digits.len() == 64 {
        let mut bytes = hex::decode_fixed::<KEY_LEN>(digits)
            .map_err(|_| "private key contains invalid hex".to_string())?;
        let secret = SecretBytes::new(bytes.to_vec());
        zeroize(&mut bytes);
        return Ok(secret);
    }
    let key = Ed25519PrivateKey::from_base64(text).map_err(|_| {
        "private key must be a suiprivkey string, base64 (flag + key) or 64 hex characters (Ed25519 only)"
            .to_string()
    })?;
    key_bytes(&key)
}

/// The 32 raw bytes of a key, read back through the SDK's `flag || key` base64 form.
fn key_bytes(key: &Ed25519PrivateKey) -> Result<SecretBytes, String> {
    let mut encoded = key.to_base64();
    let decoded = base64::decode(&encoded);
    zeroize_string(&mut encoded);
    let mut raw = decoded.map_err(|_| "private key is corrupted".to_string())?;
    let result = match raw.split_first() {
        Some((&ED25519_FLAG, rest)) if rest.len() == KEY_LEN => Ok(SecretBytes::new(rest.to_vec())),
        _ => Err("private key must be an Ed25519 key".to_string()),
    };
    zeroize(&mut raw);
    result
}

/// First Sui account (`m/44'/784'/0'/0'/0'`) of a mnemonic.
pub fn derive_account_key(mnemonic: &str) -> Result<SecretBytes, String> {
    let seed = bip39::mnemonic_to_seed(mnemonic, "")?;
    let master = Ed25519Node::from_seed(seed.expose())?;
    let account = master.derive_path(SUI_DEFAULT_PATH)?;
    Ok(SecretBytes::new(account.private_key().to_vec()))
}

fn signer_from(bytes: &[u8]) -> Result<Ed25519PrivateKey, String> {
    let mut array =
        <[u8; KEY_LEN]>::try_from(bytes).map_err(|_| "private key must be 32 bytes".to_string())?;
    let key = Ed25519PrivateKey::new(array);
    zeroize(&mut array);
    Ok(key)
}

impl SuiVault {
    pub fn create_new(network: SuiNetwork) -> Result<SuiVault, String> {
        Ok(SuiVault {
            keyring: Keyring::Hd {
                mnemonic: bip39::generate_mnemonic()?,
            },
            network,
        })
    }

    /// Imports a 24-word seed phrase (checksum-validated).
    pub fn from_mnemonic(phrase: &str, network: SuiNetwork) -> Result<SuiVault, String> {
        let mnemonic = bip39::validate_mnemonic(phrase, bip39::REQUIRED_WORD_COUNT)?;
        Ok(SuiVault {
            keyring: Keyring::Hd { mnemonic },
            network,
        })
    }

    pub fn from_private_key(input: &str, network: SuiNetwork) -> Result<SuiVault, String> {
        Ok(SuiVault {
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

    /// The account's 32-byte private key.
    pub fn private_key(&self) -> Result<SecretBytes, String> {
        match &self.keyring {
            Keyring::Simple { private_key } => Ok(private_key.clone()),
            Keyring::Hd { mnemonic } => derive_account_key(mnemonic.expose()),
        }
    }

    /// The signing key; it wipes itself when dropped.
    pub fn signer(&self) -> Result<Ed25519PrivateKey, String> {
        signer_from(self.private_key()?.expose())
    }

    pub fn address(&self) -> Result<Address, String> {
        Ok(self.signer()?.public_key().derive_address())
    }

    /// The key in the `suiprivkey1...` format that Sui wallets and the CLI import.
    pub fn suiprivkey(&self) -> Result<SecretString, String> {
        self.signer()?
            .to_suiprivkey()
            .map(SecretString::new)
            .map_err(|_| "cannot encode the private key".to_string())
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
            ("suiNetwork", Value::str(self.network.id())),
        ]);
        let text = doc.to_json();
        doc.wipe();
        SecretBytes::new(text.into_bytes())
    }

    pub fn from_plaintext(bytes: &[u8]) -> Result<SuiVault, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "vault contents are corrupted".to_string())?;
        let mut doc = json::parse(text).map_err(|_| "vault contents are corrupted".to_string())?;
        let result = Self::from_value(&doc);
        doc.wipe();
        result
    }

    fn from_value(doc: &Value) -> Result<SuiVault, String> {
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
                if data.get("hdPath").and_then(Value::as_str) != Some(HD_PATH_PREFIX) {
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
            .get("suiNetwork")
            .and_then(Value::as_str)
            .ok_or("vault has no network")
            .and_then(|n| SuiNetwork::parse(n).map_err(|_| "vault has an unknown network"))?;
        Ok(SuiVault { keyring, network })
    }

    pub fn seal(&self, password: &str, iterations: u32) -> Result<EncryptedVault, String> {
        let plaintext = self.to_plaintext();
        vault::encrypt(password, plaintext.expose(), iterations)
    }

    pub fn unseal(vault: &EncryptedVault, password: &str) -> Result<SuiVault, String> {
        let plaintext = vault::decrypt(password, vault)?;
        SuiVault::from_plaintext(plaintext.expose())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::MIN_ITERATIONS;
    use sui_crypto::SuiSigner;

    // A 12-word phrase from Sui's SDK tests; the address was also derived independently with
    // Python (PBKDF2 + SLIP-0010 + Ed25519 + BLAKE2b-256).
    const SDK_PHRASE: &str = "result crisp session latin must fruit genuine question prevent start coconut brave speak student dismiss";
    const SDK_ADDRESS: &str = "0x936accb491f0facaac668baaedcf4d0cfc6da1120b66f77fa6a43af718669973";

    fn abandon_art() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    #[test]
    fn derives_the_published_sui_address_from_a_known_phrase() {
        let key = derive_account_key(SDK_PHRASE).unwrap();
        let signer = signer_from(key.expose()).unwrap();
        assert_eq!(
            signer.public_key().derive_address().to_string(),
            SDK_ADDRESS
        );
    }

    #[test]
    fn created_wallets_are_24_words_and_unique() {
        let a = SuiVault::create_new(SuiNetwork::Mainnet).unwrap();
        let b = SuiVault::create_new(SuiNetwork::Mainnet).unwrap();
        let (ma, mb) = (a.mnemonic().unwrap(), b.mnemonic().unwrap());
        assert_eq!(ma.expose().split_whitespace().count(), 24);
        assert_ne!(ma.expose(), mb.expose());
        assert_ne!(a.address().unwrap(), b.address().unwrap());
    }

    #[test]
    fn imports_only_24_word_checksummed_phrases() {
        assert!(SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).is_ok());
        assert!(SuiVault::from_mnemonic(SDK_PHRASE, SuiNetwork::Mainnet).is_err());
        let bad = format!("{}abandon", "abandon ".repeat(23));
        assert!(SuiVault::from_mnemonic(&bad, SuiNetwork::Mainnet).is_err());
    }

    #[test]
    fn private_key_formats_all_give_the_same_key() {
        let vault = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).unwrap();
        let raw = vault.private_key().unwrap();
        let bech32 = vault.suiprivkey().unwrap();
        assert!(bech32.expose().starts_with("suiprivkey1"));
        let hex_key = hex::encode(raw.expose());
        let mut flagged = vec![0u8];
        flagged.extend_from_slice(raw.expose());
        let base64_key = base64::encode(&flagged);
        for text in [
            bech32.expose().to_string(),
            hex_key.clone(),
            format!("0x{hex_key}"),
            format!("  {base64_key}\n"),
        ] {
            let imported = SuiVault::from_private_key(&text, SuiNetwork::Mainnet).unwrap();
            assert_eq!(
                imported.private_key().unwrap().expose(),
                raw.expose(),
                "{text}"
            );
            assert_eq!(imported.address().unwrap(), vault.address().unwrap());
        }
    }

    #[test]
    fn rejects_bad_private_keys() {
        let secp_flag = {
            let mut v = vec![1u8];
            v.extend_from_slice(&[7u8; 32]);
            base64::encode(&v)
        };
        for bad in [
            "",
            "0x1234",
            &"zz".repeat(32),
            "suiprivkey1qqqqqqqq",
            &secp_flag,
            &"a".repeat(300),
            "not a key",
        ] {
            let err = SuiVault::from_private_key(bad, SuiNetwork::Mainnet).unwrap_err();
            assert!(!err.contains("zzzz"), "errors must not echo the input");
        }
    }

    #[test]
    fn plaintext_round_trip_for_both_keyrings() {
        let hd = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Testnet).unwrap();
        let back = SuiVault::from_plaintext(hd.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, SuiNetwork::Testnet);
        assert_eq!(back.mnemonic().unwrap().expose(), abandon_art());
        assert_eq!(back.address().unwrap(), hd.address().unwrap());

        let simple = SuiVault::from_private_key(&"11".repeat(32), SuiNetwork::Devnet).unwrap();
        let back = SuiVault::from_plaintext(simple.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, SuiNetwork::Devnet);
        assert!(back.mnemonic().is_none());
        assert_eq!(back.address().unwrap(), simple.address().unwrap());
    }

    #[test]
    fn plaintext_uses_metamasks_keyring_layout() {
        let hd = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).unwrap();
        let text = String::from_utf8(hd.to_plaintext().expose().to_vec()).unwrap();
        assert!(text.contains("\"HD Key Tree\"") && text.contains("m/44'/784'/0'/0'"));
        let simple = SuiVault::from_private_key(&"11".repeat(32), SuiNetwork::Mainnet).unwrap();
        let text = String::from_utf8(simple.to_plaintext().expose().to_vec()).unwrap();
        assert!(text.contains("\"Simple Key Pair\""));
    }

    #[test]
    fn corrupted_plaintext_is_rejected() {
        let good = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet)
            .unwrap()
            .to_plaintext();
        let text = String::from_utf8(good.expose().to_vec()).unwrap();
        for bad in [
            "not json".to_string(),
            "{}".to_string(),
            r#"{"keyrings":[],"suiNetwork":"mainnet"}"#.to_string(),
            text.replace("mainnet", "nowhere"),
            text.replace("m/44'/784'/0'/0'", "m/44'/60'/0'/0"),
            text.replace("HD Key Tree", "Mystery"),
            text.replace(",\"suiNetwork\":\"mainnet\"", ""),
        ] {
            assert!(SuiVault::from_plaintext(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(SuiVault::from_plaintext(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn seal_and_unseal() {
        let vault = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).unwrap();
        let sealed = vault.seal("correct horse", MIN_ITERATIONS).unwrap();
        assert!(!sealed.data.contains("abandon"));
        let opened = SuiVault::unseal(&sealed, "correct horse").unwrap();
        assert_eq!(opened.address().unwrap(), vault.address().unwrap());
        assert_eq!(
            SuiVault::unseal(&sealed, "wrong").unwrap_err(),
            vault::WRONG_PASSWORD
        );
    }

    #[test]
    fn signatures_verify_for_the_wallets_address() {
        use sui_crypto::SuiVerifier;
        use sui_crypto::ed25519::Ed25519VerifyingKey;
        let vault = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).unwrap();
        let signer = vault.signer().unwrap();
        let message = sui_sdk_types::PersonalMessage(b"hello".as_slice().into());
        let signature = signer.sign_personal_message(&message).unwrap();
        let verifier = Ed25519VerifyingKey::new(&signer.public_key()).unwrap();
        verifier
            .verify_personal_message(&message, &signature)
            .unwrap();
        let other = sui_sdk_types::PersonalMessage(b"hellO".as_slice().into());
        assert!(
            verifier
                .verify_personal_message(&other, &signature)
                .is_err()
        );
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let vault = SuiVault::from_mnemonic(&abandon_art(), SuiNetwork::Mainnet).unwrap();
        assert!(!format!("{vault:?}").contains("abandon"));
    }
}
