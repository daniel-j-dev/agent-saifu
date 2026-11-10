//! Creating, importing, sealing and unsealing Bitcoin wallets.
//!
//! A wallet has exactly one address: native SegWit (P2WPKH, `bc1q...`), the type Bitcoin Core
//! creates by default. A seed phrase derives its key with BIP 32 at the BIP 84 path
//! `m/84'/0'/0'/0/0` (`m/84'/1'/0'/0/0` on the test networks), the first receiving address of
//! the first account in Sparrow, Electrum's BIP 39 mode, Ledger, Trezor and most other wallets,
//! so a phrase from those wallets gives the same address. The vault is encrypted exactly like
//! the Ethereum, Sui and Solana wallets' (MetaMask's scheme) and its plaintext follows the same
//! keyring layout (`HD Key Tree` / `Simple Key Pair`), plus the one network the wallet is
//! bound to.

use super::address::Address;
use super::network::BitcoinNetwork;
use crate::crypto::bip32::ExtendedKey;
use crate::crypto::secret::{SecretBytes, SecretString, zeroize};
use crate::crypto::{base58, bip39, hex, secp256k1};
use crate::json::{self, Value};
use crate::vault::{self, EncryptedVault};

const HD_TYPE: &str = "HD Key Tree";
const SIMPLE_TYPE: &str = "Simple Key Pair";
const KEY_LEN: usize = 32;
const MAX_KEY_TEXT: usize = 200;
/// WIF marker for keys whose public key is compressed.
const COMPRESSED_FLAG: u8 = 0x01;

pub enum Keyring {
    Hd { mnemonic: SecretString },
    Simple { private_key: SecretBytes },
}

pub struct BitcoinVault {
    pub keyring: Keyring,
    pub network: BitcoinNetwork,
}

impl std::fmt::Debug for BitcoinVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BitcoinVault(<redacted>)")
    }
}

fn key_array(bytes: &[u8]) -> Result<[u8; KEY_LEN], String> {
    let key = <[u8; KEY_LEN]>::try_from(bytes)
        .map_err(|_| "a Bitcoin private key is 32 bytes".to_string())?;
    if !secp256k1::is_valid_private_key(&key) {
        return Err("this is not a valid secp256k1 private key".to_string());
    }
    Ok(key)
}

/// Accepts the formats Bitcoin wallets export: a WIF key for this network (compressed, the
/// only kind a native SegWit address can use) or 64 hex digits. Returns the 32-byte secret.
pub fn parse_private_key(input: &str, network: BitcoinNetwork) -> Result<SecretBytes, String> {
    let text = input.trim();
    if text.len() > MAX_KEY_TEXT {
        return Err("private key is too long".to_string());
    }
    let digits = hex::strip_prefix(text);
    if digits.len() == 2 * KEY_LEN && digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut bytes = hex::decode(digits).map_err(|_| "private key contains invalid hex")?;
        let result = key_array(&bytes).map(|k| SecretBytes::new(k.to_vec()));
        zeroize(&mut bytes);
        return result;
    }
    let mut payload = base58::decode_check(text).map_err(|_| {
        "private key must be a WIF key (starting with K, L or c) or 64 hex characters".to_string()
    })?;
    let result = secret_from_wif(&payload, network);
    zeroize(&mut payload);
    result
}

fn secret_from_wif(payload: &[u8], network: BitcoinNetwork) -> Result<SecretBytes, String> {
    let (version, body) = payload.split_first().ok_or("private key is empty")?;
    if *version != network.wif_version() {
        let other = [0x80u8, 0xef].contains(version);
        return Err(if other {
            "this WIF key belongs to a different Bitcoin network than the wallet's".to_string()
        } else {
            "this is not a Bitcoin WIF private key".to_string()
        });
    }
    match body.len() {
        n if n == KEY_LEN + 1 && body[KEY_LEN] == COMPRESSED_FLAG => {
            key_array(&body[..KEY_LEN]).map(|k| SecretBytes::new(k.to_vec()))
        }
        n if n == KEY_LEN => Err(
            "this WIF key is for an uncompressed public key; native SegWit addresses need a compressed key (re-export it as compressed, or use its seed phrase)".to_string(),
        ),
        _ => Err("this is not a valid WIF private key".to_string()),
    }
}

/// The first account key of a mnemonic at the BIP 84 path of `network`.
pub fn derive_account_key(mnemonic: &str, network: BitcoinNetwork) -> Result<SecretBytes, String> {
    let seed = bip39::mnemonic_to_seed(mnemonic, "")?;
    let master = ExtendedKey::from_seed(seed.expose())?;
    let account = master.derive_path(&network.derivation_path())?;
    Ok(SecretBytes::new(account.private_key().to_vec()))
}

impl BitcoinVault {
    pub fn create_new(network: BitcoinNetwork) -> Result<BitcoinVault, String> {
        Ok(BitcoinVault {
            keyring: Keyring::Hd {
                mnemonic: bip39::generate_mnemonic()?,
            },
            network,
        })
    }

    /// Imports a 24-word seed phrase (checksum-validated).
    pub fn from_mnemonic(phrase: &str, network: BitcoinNetwork) -> Result<BitcoinVault, String> {
        let mnemonic = bip39::validate_mnemonic(phrase, bip39::REQUIRED_WORD_COUNT)?;
        Ok(BitcoinVault {
            keyring: Keyring::Hd { mnemonic },
            network,
        })
    }

    pub fn from_private_key(input: &str, network: BitcoinNetwork) -> Result<BitcoinVault, String> {
        Ok(BitcoinVault {
            keyring: Keyring::Simple {
                private_key: parse_private_key(input, network)?,
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
            Keyring::Hd { mnemonic } => derive_account_key(mnemonic.expose(), self.network),
        }
    }

    /// The secret as a fixed array for signing; the caller wipes it.
    pub fn private_key_array(&self) -> Result<[u8; KEY_LEN], String> {
        key_array(self.private_key()?.expose())
    }

    pub fn public_key(&self) -> Result<[u8; 33], String> {
        let mut key = self.private_key_array()?;
        let public = secp256k1::public_key_compressed(&key);
        zeroize(&mut key);
        public
    }

    pub fn address_kind(&self) -> Result<Address, String> {
        Ok(Address::p2wpkh_of_key(&self.public_key()?))
    }

    pub fn address(&self) -> Result<String, String> {
        Ok(self.address_kind()?.to_text(self.network))
    }

    /// The key in Wallet Import Format (compressed), which Bitcoin Core's `importprivkey`,
    /// Sparrow, Electrum and most wallets import.
    pub fn wif(&self) -> Result<SecretString, String> {
        let key = self.private_key()?;
        let mut payload = vec![self.network.wif_version()];
        payload.extend_from_slice(key.expose());
        payload.push(COMPRESSED_FLAG);
        let text = base58::encode_check(&payload);
        zeroize(&mut payload);
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
                        ("hdPath", Value::String(self.network.derivation_path())),
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
            ("bitcoinNetwork", Value::str(self.network.id())),
        ]);
        let text = doc.to_json();
        doc.wipe();
        SecretBytes::new(text.into_bytes())
    }

    pub fn from_plaintext(bytes: &[u8]) -> Result<BitcoinVault, String> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| "vault contents are corrupted".to_string())?;
        let mut doc = json::parse(text).map_err(|_| "vault contents are corrupted".to_string())?;
        let result = Self::from_value(&doc);
        doc.wipe();
        result
    }

    fn from_value(doc: &Value) -> Result<BitcoinVault, String> {
        let network = doc
            .get("bitcoinNetwork")
            .and_then(Value::as_str)
            .ok_or("vault has no network")
            .and_then(|n| BitcoinNetwork::parse(n).map_err(|_| "vault has an unknown network"))?;
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
                if data.get("hdPath").and_then(Value::as_str) != Some(&network.derivation_path()) {
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
                    private_key: parse_private_key(key, network)?,
                }
            }
            _ => return Err("unknown keyring type".to_string()),
        };
        Ok(BitcoinVault { keyring, network })
    }

    pub fn seal(&self, password: &str, iterations: u32) -> Result<EncryptedVault, String> {
        let plaintext = self.to_plaintext();
        vault::encrypt(password, plaintext.expose(), iterations)
    }

    pub fn unseal(vault: &EncryptedVault, password: &str) -> Result<BitcoinVault, String> {
        let plaintext = vault::decrypt(password, vault)?;
        BitcoinVault::from_plaintext(plaintext.expose())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::MIN_ITERATIONS;

    // BIP 84's published test vector for this well-known test phrase at m/84'/0'/0'/0/0.
    const ABOUT_PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const ABOUT_ADDRESS: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
    const ABOUT_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";
    const ABOUT_PUBKEY: &str = "0330d54fd0dd420a6e5f8d3624f5f3482cae350f79d5f0753bf5beef9c2d91af3c";

    fn abandon_art() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    fn art_vault() -> BitcoinVault {
        BitcoinVault::from_mnemonic(&abandon_art(), BitcoinNetwork::Mainnet).unwrap()
    }

    fn about_vault(network: BitcoinNetwork) -> BitcoinVault {
        let key = derive_account_key(ABOUT_PHRASE, network).unwrap();
        BitcoinVault {
            keyring: Keyring::Simple { private_key: key },
            network,
        }
    }

    #[test]
    fn derives_the_bip84_test_vector() {
        let vault = about_vault(BitcoinNetwork::Mainnet);
        assert_eq!(vault.address().unwrap(), ABOUT_ADDRESS);
        assert_eq!(hex::encode(&vault.public_key().unwrap()), ABOUT_PUBKEY);
        assert_eq!(vault.wif().unwrap().expose(), ABOUT_WIF);
    }

    #[test]
    fn test_networks_use_coin_type_one_and_tb_addresses() {
        let main = about_vault(BitcoinNetwork::Mainnet);
        for network in [BitcoinNetwork::Testnet, BitcoinNetwork::Signet] {
            let vault = about_vault(network);
            let address = vault.address().unwrap();
            assert!(address.starts_with("tb1q"), "{address}");
            assert_ne!(
                vault.private_key().unwrap().expose(),
                main.private_key().unwrap().expose()
            );
        }
        // The first address of m/84'/1'/0'/0/0 for the same phrase, from Bitcoin Core's
        // test-network derivation.
        assert_eq!(
            about_vault(BitcoinNetwork::Testnet).address().unwrap(),
            "tb1q6rz28mcfaxtmd6v789l9rrlrusdprr9pqcpvkl"
        );
    }

    #[test]
    fn derives_the_24_word_vector() {
        // Cross-checked with an independent BIP 32 / BIP 84 implementation (Python).
        let vault = art_vault();
        assert_eq!(
            vault.address().unwrap(),
            "bc1qzmtrqsfuaf6l6kkcsseumq26ukaphfj9skkug6"
        );
        let testnet = BitcoinVault::from_mnemonic(&abandon_art(), BitcoinNetwork::Testnet).unwrap();
        assert_eq!(
            testnet.address().unwrap(),
            "tb1qtk89me2ae95dmlp3yfl4q9ynpux8mxjujuf2fr"
        );
    }

    #[test]
    fn created_wallets_are_24_words_and_unique() {
        let a = BitcoinVault::create_new(BitcoinNetwork::Mainnet).unwrap();
        let b = BitcoinVault::create_new(BitcoinNetwork::Mainnet).unwrap();
        let (ma, mb) = (a.mnemonic().unwrap(), b.mnemonic().unwrap());
        assert_eq!(ma.expose().split_whitespace().count(), 24);
        assert_ne!(ma.expose(), mb.expose());
        assert_ne!(a.address().unwrap(), b.address().unwrap());
    }

    #[test]
    fn imports_only_24_word_checksummed_phrases() {
        assert!(BitcoinVault::from_mnemonic(&abandon_art(), BitcoinNetwork::Mainnet).is_ok());
        assert!(BitcoinVault::from_mnemonic(ABOUT_PHRASE, BitcoinNetwork::Mainnet).is_err());
        let bad = format!("{}abandon", "abandon ".repeat(23));
        assert!(BitcoinVault::from_mnemonic(&bad, BitcoinNetwork::Mainnet).is_err());
    }

    #[test]
    fn private_key_formats_all_give_the_same_key() {
        let vault = art_vault();
        let raw = vault.private_key().unwrap();
        for text in [
            vault.wif().unwrap().expose().to_string(),
            hex::encode(raw.expose()),
            format!("0x{}", hex::encode(raw.expose())),
            format!("  {}\n", hex::encode(raw.expose()).to_uppercase()),
        ] {
            let imported = BitcoinVault::from_private_key(&text, BitcoinNetwork::Mainnet).unwrap();
            assert_eq!(
                imported.private_key().unwrap().expose(),
                raw.expose(),
                "{text}"
            );
            assert_eq!(imported.address().unwrap(), vault.address().unwrap());
        }
    }

    #[test]
    fn wif_for_another_network_is_refused() {
        let testnet = about_vault(BitcoinNetwork::Testnet).wif().unwrap();
        assert!(testnet.expose().starts_with('c'));
        let err =
            BitcoinVault::from_private_key(testnet.expose(), BitcoinNetwork::Mainnet).unwrap_err();
        assert!(err.contains("different Bitcoin network"), "{err}");
        let main = about_vault(BitcoinNetwork::Mainnet).wif().unwrap();
        assert!(BitcoinVault::from_private_key(main.expose(), BitcoinNetwork::Testnet).is_err());
        assert!(BitcoinVault::from_private_key(testnet.expose(), BitcoinNetwork::Signet).is_ok());
    }

    #[test]
    fn uncompressed_wif_is_refused_with_an_explanation() {
        // The WIF of private key 1, uncompressed ("5HpHag...").
        let err = BitcoinVault::from_private_key(
            "5HpHagT65TZzG1PH3CSu63k8DbpvD8s5ip4nEB3kEsreAnchuDf",
            BitcoinNetwork::Mainnet,
        )
        .unwrap_err();
        assert!(err.contains("uncompressed"), "{err}");
    }

    #[test]
    fn rejects_bad_private_keys_without_echoing_them() {
        let zero = "00".repeat(32);
        let above_order = "ff".repeat(32);
        let long = "a".repeat(300);
        let ethereum_style = format!("0x{}", "12".repeat(20));
        for bad in [
            "",
            "0x1234",
            "zz".repeat(32).as_str(),
            "not a key",
            &long,
            &zero,
            &above_order,
            &ethereum_style,
            "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9e",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
        ] {
            let err = BitcoinVault::from_private_key(bad, BitcoinNetwork::Mainnet).unwrap_err();
            assert!(!err.contains("zzzz") && !err.contains("ffff"), "{err}");
        }
    }

    #[test]
    fn plaintext_round_trip_for_both_keyrings() {
        let hd = BitcoinVault::from_mnemonic(&abandon_art(), BitcoinNetwork::Testnet).unwrap();
        let back = BitcoinVault::from_plaintext(hd.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, BitcoinNetwork::Testnet);
        assert_eq!(back.mnemonic().unwrap().expose(), abandon_art());
        assert_eq!(back.address().unwrap(), hd.address().unwrap());

        let simple =
            BitcoinVault::from_private_key(&"11".repeat(32), BitcoinNetwork::Signet).unwrap();
        let back = BitcoinVault::from_plaintext(simple.to_plaintext().expose()).unwrap();
        assert_eq!(back.network, BitcoinNetwork::Signet);
        assert!(back.mnemonic().is_none());
        assert_eq!(back.address().unwrap(), simple.address().unwrap());
    }

    #[test]
    fn plaintext_uses_metamasks_keyring_layout() {
        let text = String::from_utf8(art_vault().to_plaintext().expose().to_vec()).unwrap();
        assert!(text.contains("\"HD Key Tree\"") && text.contains("m/84'/0'/0'/0/0"));
        let simple =
            BitcoinVault::from_private_key(&"11".repeat(32), BitcoinNetwork::Mainnet).unwrap();
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
            r#"{"keyrings":[],"bitcoinNetwork":"mainnet"}"#.to_string(),
            text.replace("mainnet", "nowhere"),
            text.replace("m/84'/0'/0'/0/0", "m/44'/0'/0'/0/0"),
            text.replace("HD Key Tree", "Mystery"),
            text.replace(",\"bitcoinNetwork\":\"mainnet\"", ""),
            // A derivation path of the wrong network must not be accepted either.
            text.replace(
                "\"bitcoinNetwork\":\"mainnet\"",
                "\"bitcoinNetwork\":\"signet\"",
            ),
        ] {
            assert!(
                BitcoinVault::from_plaintext(bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
        assert!(BitcoinVault::from_plaintext(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn seal_and_unseal() {
        let vault = art_vault();
        let sealed = vault.seal("correct horse", MIN_ITERATIONS).unwrap();
        assert!(!sealed.data.contains("abandon"));
        let opened = BitcoinVault::unseal(&sealed, "correct horse").unwrap();
        assert_eq!(opened.address().unwrap(), vault.address().unwrap());
        assert_eq!(
            BitcoinVault::unseal(&sealed, "wrong").unwrap_err(),
            vault::WRONG_PASSWORD
        );
    }

    #[test]
    fn debug_output_is_redacted() {
        assert_eq!(format!("{:?}", art_vault()), "BitcoinVault(<redacted>)");
    }
}
