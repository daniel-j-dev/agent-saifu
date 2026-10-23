//! Cryptographic building blocks.
//!
//! Hashing, MAC, KDF and encoding primitives are implemented here and checked
//! against published test vectors. Only the primitives where a hand-rolled
//! implementation would be risky (AES-GCM, secp256k1, the OS RNG) are provided
//! by audited crates, wrapped in `aead`, `secp256k1` and `random`.

pub mod aead;
pub mod base64;
pub mod bip32;
pub mod bip39;
pub mod hex;
pub mod hmac;
pub mod keccak;
pub mod pbkdf2;
pub mod random;
pub mod secp256k1;
pub mod secret;
pub mod sha2;
