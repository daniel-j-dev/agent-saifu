//! Password-encrypted vault using the same scheme as MetaMask's `browser-passworder`:
//! PBKDF2-HMAC-SHA256 (32-byte salt) derives an AES-256-GCM key; the vault is stored as
//! `{"data","iv","salt","keyMetadata"}` with base64 fields (ciphertext carries the GCM tag).

use crate::crypto::secret::SecretBytes;
use crate::crypto::{aead, base64, pbkdf2, random};
use crate::json::Value;

/// MetaMask's current default.
pub const DEFAULT_ITERATIONS: u32 = 900_000;
/// Vaults claiming fewer iterations are refused (downgrade protection).
pub const MIN_ITERATIONS: u32 = 10_000;
/// Upper bound so a tampered vault cannot make us spin for hours.
pub const MAX_ITERATIONS: u32 = 10_000_000;
pub const SALT_LEN: usize = 32;

pub const WRONG_PASSWORD: &str = "incorrect password or corrupted vault";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedVault {
    pub data: String,
    pub iv: String,
    pub salt: String,
    pub iterations: u32,
}

pub fn derive_key(password: &str, salt: &[u8], iterations: u32) -> Result<SecretBytes, String> {
    pbkdf2::pbkdf2_sha256(password.as_bytes(), salt, iterations, aead::KEY_LEN)
        .map(SecretBytes::new)
}

pub fn encrypt(
    password: &str,
    plaintext: &[u8],
    iterations: u32,
) -> Result<EncryptedVault, String> {
    check_iterations(iterations)?;
    let salt = random::random_bytes(SALT_LEN)?;
    let iv = random::random_bytes(aead::IV_LEN)?;
    encrypt_with(password, plaintext, iterations, &salt, &iv)
}

/// Deterministic core of `encrypt`, split out so it can be tested with fixed inputs.
pub fn encrypt_with(
    password: &str,
    plaintext: &[u8],
    iterations: u32,
    salt: &[u8],
    iv: &[u8],
) -> Result<EncryptedVault, String> {
    let key = derive_key(password, salt, iterations)?;
    let ciphertext = aead::encrypt(key.expose(), iv, plaintext)?;
    Ok(EncryptedVault {
        data: base64::encode(&ciphertext),
        iv: base64::encode(iv),
        salt: base64::encode(salt),
        iterations,
    })
}

pub fn decrypt(password: &str, vault: &EncryptedVault) -> Result<SecretBytes, String> {
    check_iterations(vault.iterations)?;
    let salt = base64::decode(&vault.salt).map_err(|_| "vault salt is corrupted".to_string())?;
    let iv = base64::decode(&vault.iv).map_err(|_| "vault iv is corrupted".to_string())?;
    let data = base64::decode(&vault.data).map_err(|_| "vault data is corrupted".to_string())?;
    if salt.len() < 16 || iv.len() != aead::IV_LEN {
        return Err("vault parameters are corrupted".to_string());
    }
    let key = derive_key(password, &salt, vault.iterations)?;
    aead::decrypt(key.expose(), &iv, &data)
        .map(SecretBytes::new)
        .map_err(|_| WRONG_PASSWORD.to_string())
}

fn check_iterations(iterations: u32) -> Result<(), String> {
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
        return Err(format!(
            "PBKDF2 iterations must be between {MIN_ITERATIONS} and {MAX_ITERATIONS}"
        ));
    }
    Ok(())
}

impl EncryptedVault {
    pub fn to_json(&self) -> Value {
        Value::object(vec![
            ("data", Value::str(&self.data)),
            ("iv", Value::str(&self.iv)),
            ("salt", Value::str(&self.salt)),
            (
                "keyMetadata",
                Value::object(vec![
                    ("algorithm", Value::str("PBKDF2")),
                    (
                        "params",
                        Value::object(vec![("iterations", Value::int(i64::from(self.iterations)))]),
                    ),
                ]),
            ),
        ])
    }

    pub fn from_json(value: &Value) -> Result<EncryptedVault, String> {
        let field = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("vault is missing '{name}'"))
        };
        let meta = value
            .get("keyMetadata")
            .ok_or("vault is missing 'keyMetadata'")?;
        if meta.get("algorithm").and_then(Value::as_str) != Some("PBKDF2") {
            return Err("unsupported vault key derivation algorithm".to_string());
        }
        let iterations = meta
            .get("params")
            .and_then(|p| p.get("iterations"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("vault has invalid iteration count")?;
        check_iterations(iterations)?;
        Ok(EncryptedVault {
            data: field("data")?,
            iv: field("iv")?,
            salt: field("salt")?,
            iterations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    const PASSWORD: &str = "correct horse battery staple";
    // Produced independently with WebCrypto, mirroring @metamask/browser-passworder.
    const WEBCRYPTO_VAULT: &str = r#"{"data":"ry/EMkSbqj17zDCP5/kzx2PIvoIQugy6sXuUuPAcjmGwpJDrX6BZ2pGo8V/e","iv":"ZGVmZ2hpamtsbW5vcHFycw==","salt":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=","keyMetadata":{"algorithm":"PBKDF2","params":{"iterations":10000}}}"#;

    #[test]
    fn decrypts_vault_made_by_webcrypto() {
        let vault = EncryptedVault::from_json(&json::parse(WEBCRYPTO_VAULT).unwrap()).unwrap();
        let plain = decrypt(PASSWORD, &vault).unwrap();
        assert_eq!(plain.expose(), br#"{"hello":"vault","n":[1,2,3]}"#);
    }

    #[test]
    fn encrypt_with_matches_webcrypto_output() {
        let salt: Vec<u8> = (0..32).collect();
        let iv: Vec<u8> = (100..116).collect();
        let vault = encrypt_with(
            PASSWORD,
            br#"{"hello":"vault","n":[1,2,3]}"#,
            10_000,
            &salt,
            &iv,
        )
        .unwrap();
        assert_eq!(vault.to_json().to_json(), WEBCRYPTO_VAULT);
    }

    #[test]
    fn round_trip_with_random_salt_and_iv() {
        let v1 = encrypt("pw-12345678", b"secret", MIN_ITERATIONS).unwrap();
        let v2 = encrypt("pw-12345678", b"secret", MIN_ITERATIONS).unwrap();
        assert_ne!(v1.salt, v2.salt);
        assert_ne!(v1.iv, v2.iv);
        assert_ne!(v1.data, v2.data);
        assert_eq!(decrypt("pw-12345678", &v1).unwrap().expose(), b"secret");
        assert_eq!(base64::decode(&v1.salt).unwrap().len(), SALT_LEN);
        assert_eq!(base64::decode(&v1.iv).unwrap().len(), 16);
    }

    #[test]
    fn wrong_password_fails_with_generic_message() {
        let v = encrypt("right-password", b"secret", MIN_ITERATIONS).unwrap();
        assert_eq!(
            decrypt("wrong-password", &v),
            Err(WRONG_PASSWORD.to_string())
        );
        assert_eq!(decrypt("", &v), Err(WRONG_PASSWORD.to_string()));
    }

    #[test]
    fn unicode_passwords_work() {
        let v = encrypt("pässwörd-密码-🔐", b"x", MIN_ITERATIONS).unwrap();
        assert!(decrypt("pässwörd-密码-🔐", &v).is_ok());
        assert!(decrypt("passwörd-密码-🔐", &v).is_err());
    }

    #[test]
    fn tampered_vault_is_rejected() {
        let v = encrypt("pw-12345678", b"secret", MIN_ITERATIONS).unwrap();
        let mut bytes = base64::decode(&v.data).unwrap();
        bytes[0] ^= 1;
        let bad = EncryptedVault {
            data: base64::encode(&bytes),
            ..v.clone()
        };
        assert!(decrypt("pw-12345678", &bad).is_err());
        let bad_iter = EncryptedVault {
            iterations: MIN_ITERATIONS + 1,
            ..v
        };
        assert!(decrypt("pw-12345678", &bad_iter).is_err());
    }

    #[test]
    fn corrupted_fields_are_reported() {
        let v = encrypt("pw-12345678", b"secret", MIN_ITERATIONS).unwrap();
        assert!(
            decrypt(
                "pw-12345678",
                &EncryptedVault {
                    salt: "!!".into(),
                    ..v.clone()
                }
            )
            .is_err()
        );
        assert!(
            decrypt(
                "pw-12345678",
                &EncryptedVault {
                    iv: base64::encode(&[0u8; 12]),
                    ..v.clone()
                }
            )
            .is_err()
        );
        assert!(
            decrypt(
                "pw-12345678",
                &EncryptedVault {
                    salt: base64::encode(&[1u8; 4]),
                    ..v
                }
            )
            .is_err()
        );
    }

    #[test]
    fn iteration_bounds_are_enforced() {
        assert!(encrypt("pw", b"x", MIN_ITERATIONS - 1).is_err());
        assert!(encrypt("pw", b"x", MAX_ITERATIONS + 1).is_err());
        let json = WEBCRYPTO_VAULT.replace("10000", "9999");
        assert!(EncryptedVault::from_json(&json::parse(&json).unwrap()).is_err());
        let json = WEBCRYPTO_VAULT.replace("10000", "4294967296");
        assert!(EncryptedVault::from_json(&json::parse(&json).unwrap()).is_err());
    }

    #[test]
    fn json_round_trip_and_validation() {
        let v = encrypt("pw-12345678", b"abc", MIN_ITERATIONS).unwrap();
        assert_eq!(EncryptedVault::from_json(&v.to_json()).unwrap(), v);
        assert!(EncryptedVault::from_json(&json::parse("{}").unwrap()).is_err());
        let wrong_alg = WEBCRYPTO_VAULT.replace("PBKDF2", "scrypt");
        assert!(EncryptedVault::from_json(&json::parse(&wrong_alg).unwrap()).is_err());
        let missing = WEBCRYPTO_VAULT.replace("\"data\"", "\"dat\"");
        assert!(EncryptedVault::from_json(&json::parse(&missing).unwrap()).is_err());
    }
}
