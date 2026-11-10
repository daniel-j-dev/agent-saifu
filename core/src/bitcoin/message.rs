//! Signing messages with a Bitcoin address, following BIP 322 ("simple" signatures).
//!
//! The original `signmessage` format only works for legacy P2PKH addresses (Bitcoin Core's
//! `signmessage` refuses every other address type). BIP 322 generalises it to any script by
//! signing a virtual transaction, so the wallet's native SegWit address can prove ownership.
//! The signature commits to the message through a transaction that can never be valid on the
//! network (it spends an output that does not exist), so a message signature can never be
//! replayed as a payment.

use super::address::Address;
use super::tx::{OutPoint, Transaction, TxIn, TxOut, compact_from_der, serialize_witness};
use crate::crypto::sha2::sha256;
use crate::crypto::{base64, ripemd160::hash160, secp256k1};

const TAG: &[u8] = b"BIP0322-signed-message";
/// Variant prefix of a "simple" signature (BIP 322 version 1.0 and later).
pub const SIMPLE_PREFIX: &str = "smp";
const MAX_MESSAGE_BYTES: usize = 100_000;

/// BIP 340 tagged hash of the message.
pub fn message_hash(message: &[u8]) -> [u8; 32] {
    let tag = sha256(TAG);
    let mut data = tag.to_vec();
    data.extend_from_slice(&tag);
    data.extend_from_slice(message);
    sha256(&data)
}

fn to_spend(message: &[u8], script_pubkey: &[u8]) -> Transaction {
    let mut script_sig = vec![0x00, 0x20];
    script_sig.extend_from_slice(&message_hash(message));
    Transaction {
        version: 0,
        inputs: vec![TxIn {
            previous: OutPoint {
                txid: [0; 32],
                vout: 0xffff_ffff,
            },
            script_sig,
            sequence: 0,
            witness: vec![],
        }],
        outputs: vec![TxOut {
            value: 0,
            script_pubkey: script_pubkey.to_vec(),
        }],
        lock_time: 0,
    }
}

fn to_sign(to_spend_txid: [u8; 32]) -> Transaction {
    Transaction {
        version: 0,
        inputs: vec![TxIn {
            previous: OutPoint {
                txid: to_spend_txid,
                vout: 0,
            },
            script_sig: vec![],
            sequence: 0,
            witness: vec![],
        }],
        outputs: vec![TxOut {
            value: 0,
            script_pubkey: vec![0x6a],
        }],
        lock_time: 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMessage {
    /// The signature as BIP 322 now specifies it: `smp` followed by the base64 witness.
    pub signature: String,
    /// The same witness without the prefix, which verifiers written before the prefix was
    /// added expect.
    pub signature_unprefixed: String,
}

/// Signs `message` for the P2WPKH address of `private_key`.
pub fn sign_simple(private_key: &[u8; 32], message: &[u8]) -> Result<SignedMessage, String> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "message is too long (at most {MAX_MESSAGE_BYTES} bytes)"
        ));
    }
    let public_key = secp256k1::public_key_compressed(private_key)?;
    let script = Address::p2wpkh_of_key(&public_key).script_pubkey();
    let spend = to_spend(message, &script);
    let mut sign = to_sign(spend.txid());
    sign.sign_p2wpkh_input(0, 0, private_key)?;
    let witness = base64::encode(&serialize_witness(&sign.inputs[0].witness));
    Ok(SignedMessage {
        signature: format!("{SIMPLE_PREFIX}{witness}"),
        signature_unprefixed: witness,
    })
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (first, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*first)
    }

    fn varint(&mut self) -> Option<usize> {
        match self.byte()? {
            n @ 0..=0xfc => Some(usize::from(n)),
            0xfd => Some(usize::from(u16::from_le_bytes([
                self.byte()?,
                self.byte()?,
            ]))),
            _ => None,
        }
    }

    fn take(&mut self, len: usize) -> Option<&[u8]> {
        if len > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(head)
    }
}

/// Reads a witness stack; `None` if it is malformed or has trailing bytes.
fn parse_witness(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut reader = Reader(bytes);
    let count = reader.varint()?;
    if count > 16 {
        return None;
    }
    let mut items = vec![];
    for _ in 0..count {
        let len = reader.varint()?;
        items.push(reader.take(len)?.to_vec());
    }
    reader.0.is_empty().then_some(items)
}

/// Checks a BIP 322 simple signature (with or without the `smp` prefix) for a P2WPKH address
/// whose public key hash is `pubkey_hash`.
pub fn verify_simple(pubkey_hash: &[u8; 20], message: &[u8], signature: &str) -> bool {
    let text = signature.strip_prefix(SIMPLE_PREFIX).unwrap_or(signature);
    let Ok(raw) = base64::decode(text) else {
        return false;
    };
    let Some(items) = parse_witness(&raw) else {
        return false;
    };
    let [der, public_key] = items.as_slice() else {
        return false;
    };
    if hash160(public_key) != *pubkey_hash {
        return false;
    }
    let Some(compact) = compact_from_der(der) else {
        return false;
    };
    let script = Address::P2wpkh(*pubkey_hash).script_pubkey();
    let spend = to_spend(message, &script);
    let sign = to_sign(spend.txid());
    let Ok(digest) = sign.sighash_p2wpkh(0, pubkey_hash, 0) else {
        return false;
    };
    secp256k1::verify_low_s(public_key, &digest, &compact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::network::BitcoinNetwork;
    use crate::bitcoin::wallet::BitcoinVault;
    use crate::crypto::hex;

    // BIP 322's published test vectors (basic-test-vectors.json).
    const KEY_WIF: &str = "L3VFeEujGtevx9w18HD1fhRbCH67Az2dpCymeRE1SoPK6XQtaN2k";
    const ADDRESS: &str = "bc1q9vza2e8x573nczrlzms0wvx3gsqjx7vavgkx0l";
    const EMPTY_SIGNATURE_HASH_GRINDED: &str = "smpAkcwRAIgM2gBAQqvZX15ZiysmKmQpDrG83avLIT492QBzLnQIxYCIBaTpOaD20qRlEylyxFSeEA2ba9YOixpX8z46TSDtS40ASECx/EgAxlkQpQ9hYjgGu6EBCPMVPwVIVJqO4XCsMvViHI=";
    const EMPTY_SIGNATURE_PLAIN: &str = "smpAkgwRQIhAPkJ1Q4oYS0htvyuSFHLxRQpFAY56b70UvE7Dxazen0ZAiAtZfFz1S6T6I23MWI2lK/pcNTWncuyL8UL+oMdydVgzAEhAsfxIAMZZEKUPYWI4BruhAQjzFT8FSFSajuFwrDL1Yhy";
    const HELLO_SIGNATURE_GRINDED: &str = "smpAkcwRAIgZRfIY3p7/DoVTty6YZbWS71bc5Vct9p9Fia83eRmw2QCICK/ENGfwLtptFluMGs2KsqoNSk89pO7F29zJLUx9a/sASECx/EgAxlkQpQ9hYjgGu6EBCPMVPwVIVJqO4XCsMvViHI=";
    const HELLO_SIGNATURE_PLAIN: &str = "smpAkgwRQIhAOzyynlqt93lOKJr+wmmxIens//zPzl9tqIOua93wO6MAiBi5n5EyAcPScOjf1lAqIUIQtr3zKNeavYabHyR8eGhowEhAsfxIAMZZEKUPYWI4BruhAQjzFT8FSFSajuFwrDL1Yhy";

    fn vector_vault() -> BitcoinVault {
        BitcoinVault::from_private_key(KEY_WIF, BitcoinNetwork::Mainnet).unwrap()
    }

    fn vector_key() -> [u8; 32] {
        vector_vault().private_key_array().unwrap()
    }

    fn pubkey_hash() -> [u8; 20] {
        hash160(&vector_vault().public_key().unwrap())
    }

    #[test]
    fn the_vector_key_owns_the_vector_address() {
        assert_eq!(vector_vault().address().unwrap(), ADDRESS);
    }

    #[test]
    fn message_hashes_match_the_vectors() {
        assert_eq!(
            hex::encode(&message_hash(b"")),
            "c90c269c4f8fcbe6880f72a721ddfbf1914268a794cbb21cfafee13770ae19f1"
        );
        assert_eq!(
            hex::encode(&message_hash(b"Hello World")),
            "f0eb03b1a75ac6d9847f55c624a99169b5dccba2a31f5b23bea77ba270de0a7a"
        );
        assert_eq!(
            hex::encode(&message_hash(
                "UTF-8 support: öäüéàè 测试文本 😄".as_bytes()
            )),
            "43936b237ea38c7794eb5d755e0d220b6db92ebfc5c8f482759d22b1286376d7"
        );
    }

    #[test]
    fn virtual_transaction_ids_match_the_vectors() {
        let script = Address::parse(ADDRESS, BitcoinNetwork::Mainnet)
            .unwrap()
            .script_pubkey();
        for (message, spend_id, sign_id) in [
            (
                &b""[..],
                "c5680aa69bb8d860bf82d4e9cd3504b55dde018de765a91bb566283c545a99a7",
                "1e9654e951a5ba44c8604c4de6c67fd78a27e81dcadcfe1edf638ba3aaebaed6",
            ),
            (
                b"Hello World",
                "b79d196740ad5217771c1098fc4a4b51e0535c32236c71f1ea4d61a2d603352b",
                "88737ae86f2077145f93cc4b153ae9a1cb8d56afa511988c149c5c8c9d93bddf",
            ),
        ] {
            let spend = to_spend(message, &script);
            assert_eq!(spend.txid_hex(), spend_id);
            assert_eq!(to_sign(spend.txid()).txid_hex(), sign_id);
        }
    }

    #[test]
    fn signing_reproduces_the_plain_rfc6979_vectors() {
        let key = vector_key();
        let empty = sign_simple(&key, b"").unwrap();
        assert_eq!(empty.signature, EMPTY_SIGNATURE_PLAIN);
        assert_eq!(empty.signature_unprefixed, &EMPTY_SIGNATURE_PLAIN[3..]);
        assert_eq!(
            sign_simple(&key, b"Hello World").unwrap().signature,
            HELLO_SIGNATURE_PLAIN
        );
    }

    #[test]
    fn other_implementations_signatures_verify() {
        // The first vectors were produced with low-R grinding; they must still verify.
        for (message, signature) in [
            (&b""[..], EMPTY_SIGNATURE_HASH_GRINDED),
            (b"", EMPTY_SIGNATURE_PLAIN),
            (b"Hello World", HELLO_SIGNATURE_GRINDED),
            (b"Hello World", HELLO_SIGNATURE_PLAIN),
        ] {
            assert!(
                verify_simple(&pubkey_hash(), message, signature),
                "{signature}"
            );
            // Also without the prefix older verifiers expect.
            assert!(verify_simple(&pubkey_hash(), message, &signature[3..]));
        }
    }

    #[test]
    fn verification_rejects_the_wrong_message_address_or_signature() {
        assert!(!verify_simple(
            &pubkey_hash(),
            b"Wrong message",
            EMPTY_SIGNATURE_PLAIN
        ));
        assert!(!verify_simple(&[7; 20], b"", EMPTY_SIGNATURE_PLAIN));
        for bad in [
            "",
            "smp",
            "smpAA==",
            "not base64!!",
            "smpAkcw",
            EMPTY_SIGNATURE_PLAIN.trim_end_matches('y'),
        ] {
            assert!(!verify_simple(&pubkey_hash(), b"", bad), "{bad}");
        }
    }

    #[test]
    fn every_message_round_trips() {
        let key = [3u8; 32];
        let hash = hash160(&secp256k1::public_key_compressed(&key).unwrap());
        for message in [&b""[..], b"hi", &[0u8, 255, 7], &[b'x'; 5000]] {
            let signed = sign_simple(&key, message).unwrap();
            assert!(verify_simple(&hash, message, &signed.signature));
            assert!(verify_simple(&hash, message, &signed.signature_unprefixed));
            assert!(!verify_simple(&hash, b"other", &signed.signature));
        }
    }

    #[test]
    fn oversized_messages_are_refused() {
        assert!(sign_simple(&[3u8; 32], &vec![0u8; MAX_MESSAGE_BYTES + 1]).is_err());
        assert!(sign_simple(&[3u8; 32], &vec![0u8; MAX_MESSAGE_BYTES]).is_ok());
    }

    #[test]
    fn the_signed_transaction_can_never_be_a_payment() {
        let spend = to_spend(b"x", &[0x00, 0x14]);
        // It spends the all-zero outpoint, which no transaction ever created, and the signed
        // transaction's only output is an unspendable OP_RETURN of value zero.
        assert_eq!(spend.inputs[0].previous.txid, [0; 32]);
        assert_eq!(spend.inputs[0].previous.vout, 0xffff_ffff);
        let sign = to_sign(spend.txid());
        assert_eq!(sign.outputs[0].script_pubkey, vec![0x6a]);
        assert_eq!(sign.outputs[0].value, 0);
    }

    #[test]
    fn strict_der_parsing() {
        assert!(compact_from_der(&[]).is_none());
        assert!(compact_from_der(&[0x30, 0x00, 0x01]).is_none());
        let mut compact = [0u8; 64];
        compact[31] = 5;
        compact[63] = 9;
        let mut der = super::super::tx::der_signature(&compact);
        der.push(super::super::tx::SIGHASH_ALL);
        assert_eq!(compact_from_der(&der), Some(compact));
        // A wrong sighash type, a wrong length byte and trailing bytes are all refused.
        let mut wrong_type = der.clone();
        *wrong_type.last_mut().unwrap() = 0x02;
        assert!(compact_from_der(&wrong_type).is_none());
        let mut wrong_len = der.clone();
        wrong_len[1] += 1;
        assert!(compact_from_der(&wrong_len).is_none());
        let mut trailing = der;
        trailing.insert(trailing.len() - 1, 0);
        assert!(compact_from_der(&trailing).is_none());
    }
}
