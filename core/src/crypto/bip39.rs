//! BIP39 mnemonic phrases (English wordlist).

use super::pbkdf2::pbkdf2_sha512;
use super::random;
use super::secret::{SecretBytes, SecretString};
use super::sha2::sha256;

/// agent-saifu only creates and accepts 24-word (256-bit) phrases.
pub const REQUIRED_WORD_COUNT: usize = 24;

const WORDLIST_TEXT: &str = include_str!("english.txt");

fn wordlist() -> Vec<&'static str> {
    WORDLIST_TEXT.lines().collect()
}

fn word_index(word: &str) -> Option<usize> {
    wordlist().iter().position(|w| *w == word)
}

/// Lowercases and collapses all whitespace runs into single spaces.
pub fn normalize_mnemonic(phrase: &str) -> SecretString {
    let joined = phrase
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    SecretString::new(joined)
}

fn bit_at(bytes: &[u8], index: usize) -> bool {
    bytes[index / 8] & (0x80 >> (index % 8)) != 0
}

/// Encodes entropy (16–32 bytes, multiple of 4) as a mnemonic.
pub fn entropy_to_mnemonic(entropy: &[u8]) -> Result<SecretString, String> {
    if entropy.len() < 16 || entropy.len() > 32 || !entropy.len().is_multiple_of(4) {
        return Err("entropy must be 16 to 32 bytes and a multiple of 4".to_string());
    }
    let checksum_bits = entropy.len() * 8 / 32;
    let total_bits = entropy.len() * 8 + checksum_bits;
    let hash = sha256(entropy);
    let words = wordlist();
    let mut out = Vec::with_capacity(total_bits / 11);
    for group in 0..total_bits / 11 {
        let mut index = 0usize;
        for bit in 0..11 {
            let pos = group * 11 + bit;
            let set = if pos < entropy.len() * 8 {
                bit_at(entropy, pos)
            } else {
                bit_at(&hash, pos - entropy.len() * 8)
            };
            index = (index << 1) | usize::from(set);
        }
        out.push(words[index]);
    }
    Ok(SecretString::new(out.join(" ")))
}

/// Generates a fresh 24-word mnemonic from OS randomness.
pub fn generate_mnemonic() -> Result<SecretString, String> {
    let entropy = SecretBytes::new(random::random_bytes(32)?);
    entropy_to_mnemonic(entropy.expose())
}

/// Checks wordlist membership, word count and checksum. Returns the normalized phrase.
pub fn validate_mnemonic(phrase: &str, expected_words: usize) -> Result<SecretString, String> {
    let normalized = normalize_mnemonic(phrase);
    let words: Vec<&str> = normalized.expose().split(' ').collect();
    if normalized.is_empty() || words.len() != expected_words {
        return Err(format!(
            "seed phrase must be exactly {expected_words} words"
        ));
    }
    let list = wordlist();
    let mut bits: Vec<bool> = Vec::with_capacity(words.len() * 11);
    for word in &words {
        let index = list.iter().position(|w| w == word).ok_or_else(|| {
            "seed phrase contains a word that is not in the BIP39 wordlist".to_string()
        })?;
        for bit in (0..11).rev() {
            bits.push((index >> bit) & 1 == 1);
        }
    }
    let entropy_bits = words.len() * 11 * 32 / 33;
    let mut entropy = vec![0u8; entropy_bits / 8];
    for (i, bit) in bits.iter().take(entropy_bits).enumerate() {
        if *bit {
            entropy[i / 8] |= 0x80 >> (i % 8);
        }
    }
    let hash = sha256(&entropy);
    let valid = (0..bits.len() - entropy_bits).all(|i| bits[entropy_bits + i] == bit_at(&hash, i));
    super::secret::zeroize(&mut entropy);
    if !valid {
        return Err("seed phrase checksum is invalid".to_string());
    }
    Ok(normalized)
}

/// Stretches a validated mnemonic into a 64-byte seed (no passphrase support needed
/// beyond the standard empty one, but the parameter keeps the function BIP39-complete).
pub fn mnemonic_to_seed(mnemonic: &str, passphrase: &str) -> Result<SecretBytes, String> {
    let salt = format!("mnemonic{passphrase}");
    let seed = pbkdf2_sha512(mnemonic.as_bytes(), salt.as_bytes(), 2048, 64)?;
    Ok(SecretBytes::new(seed))
}

/// Only used to support lookups in tests and callers that need an index.
pub fn index_of(word: &str) -> Option<usize> {
    word_index(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn abandon_24() -> String {
        format!("{}art", "abandon ".repeat(23))
    }

    #[test]
    fn wordlist_is_the_official_one() {
        assert_eq!(wordlist().len(), 2048);
        assert_eq!(
            hex::encode(&sha256(WORDLIST_TEXT.as_bytes())),
            "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda"
        );
        assert_eq!(index_of("abandon"), Some(0));
        assert_eq!(index_of("zoo"), Some(2047));
        assert_eq!(index_of("notaword"), None);
    }

    #[test]
    fn entropy_vectors() {
        assert_eq!(
            entropy_to_mnemonic(&[0u8; 16]).unwrap().expose(),
            ABANDON_12
        );
        assert_eq!(
            entropy_to_mnemonic(&[0u8; 32]).unwrap().expose(),
            abandon_24()
        );
        assert_eq!(
            entropy_to_mnemonic(&[0xffu8; 32]).unwrap().expose(),
            format!("{}vote", "zoo ".repeat(23))
        );
        assert_eq!(
            entropy_to_mnemonic(&[0x7fu8; 16]).unwrap().expose(),
            "legal winner thank year wave sausage worth useful legal winner thank yellow"
        );
    }

    #[test]
    fn rejects_bad_entropy_lengths() {
        for len in [0, 8, 15, 17, 20 + 1, 36] {
            assert!(entropy_to_mnemonic(&vec![0u8; len]).is_err(), "{len}");
        }
    }

    #[test]
    fn generated_mnemonics_are_valid_and_unique() {
        let a = generate_mnemonic().unwrap();
        let b = generate_mnemonic().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.expose().split(' ').count(), 24);
        assert!(validate_mnemonic(a.expose(), 24).is_ok());
    }

    #[test]
    fn validation_accepts_messy_whitespace_and_case() {
        let messy = format!("  {}\n", abandon_24().replace(' ', "  ").to_uppercase());
        assert_eq!(
            validate_mnemonic(&messy, 24).unwrap().expose(),
            abandon_24()
        );
    }

    #[test]
    fn validation_enforces_word_count() {
        assert!(validate_mnemonic(ABANDON_12, 24).is_err());
        assert!(validate_mnemonic(ABANDON_12, 12).is_ok());
        assert!(validate_mnemonic("", 24).is_err());
        assert!(validate_mnemonic("   ", 24).is_err());
    }

    #[test]
    fn validation_rejects_bad_checksum_and_unknown_words() {
        let bad_checksum = format!("{}abandon", "abandon ".repeat(23));
        assert!(
            validate_mnemonic(&bad_checksum, 24)
                .unwrap_err()
                .contains("checksum")
        );
        let unknown = format!("{}zzzz", "abandon ".repeat(23));
        assert!(
            validate_mnemonic(&unknown, 24)
                .unwrap_err()
                .contains("wordlist")
        );
    }

    #[test]
    fn seed_vectors() {
        let seed = mnemonic_to_seed(&abandon_24(), "TREZOR").unwrap();
        assert_eq!(
            hex::encode(seed.expose()),
            "bda85446c68413707090a52022edd26a1c9462295029f2e60cd7c4f2bbd3097170af7a4d73245cafa9c3cca8d561a7c3de6f5d4a10be8ed2a5e608d68f92fcc8"
        );
        let seed12 = mnemonic_to_seed(ABANDON_12, "").unwrap();
        assert_eq!(
            hex::encode(seed12.expose()),
            "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4"
        );
    }
}
