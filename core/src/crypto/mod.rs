//! Cryptographic building blocks.
//!
//! Hashing, MAC, KDF and encoding primitives are implemented here and checked
//! against published test vectors. Only the primitives where a hand-rolled
//! implementation would be risky (AES-GCM, ChaCha20-Poly1305, secp256k1, Ed25519, X25519,
//! the OS RNG) are provided by audited crates, wrapped in `aead`, `chacha`, `secp256k1`,
//! `ed25519`, `x25519` and `random`.

pub mod aead;
pub mod base58;
pub mod base64;
pub mod bip32;
pub mod bip39;
pub mod chacha;
pub mod ed25519;
pub mod hex;
pub mod hkdf;
pub mod hmac;
pub mod keccak;
pub mod pbkdf2;
pub mod random;
pub mod secp256k1;
pub mod secret;
pub mod sha2;
pub mod x25519;
