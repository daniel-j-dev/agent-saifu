//! Bitcoin transactions: serialization, transaction IDs, weight, the BIP 143 signature hash and
//! signing of native SegWit (P2WPKH) inputs.
//!
//! Every signature is checked against the signer's public key before it is used, as Bitcoin
//! Core does, and is deterministic (RFC 6979) and low-S, which relay policy requires.

use super::address::{varint, varint_len};
use crate::crypto::secp256k1;
use crate::crypto::sha2::sha256d;
use crate::crypto::{hex, ripemd160::hash160};

/// `SIGHASH_ALL`: the signature commits to every input and output.
pub const SIGHASH_ALL: u8 = 0x01;
/// Signals BIP 125 replaceability (any value below 0xfffffffe does), and keeps `lock_time`
/// effective. This is the sequence Bitcoin Core uses by default.
pub const RBF_SEQUENCE: u32 = 0xffff_fffd;
/// Largest transaction relay policy accepts, in weight units.
pub const MAX_STANDARD_WEIGHT: u64 = 400_000;
/// Largest DER signature plus sighash byte of a low-S signature: 33-byte r, 32-byte s.
pub const MAX_SIGNATURE_BYTES: u64 = 72;
const PUBLIC_KEY_BYTES: u64 = 33;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutPoint {
    /// In the byte order used inside transactions (the reverse of the displayed id).
    pub txid: [u8; 32],
    pub vout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxIn {
    pub previous: OutPoint,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
    pub witness: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxOut {
    pub value: u64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
}

/// The displayed (reversed) hex of a transaction id.
pub fn txid_to_hex(txid: &[u8; 32]) -> String {
    let mut reversed = *txid;
    reversed.reverse();
    hex::encode(&reversed)
}

/// Reads a displayed 64-hex-digit transaction id into transaction byte order.
pub fn parse_txid(text: &str) -> Result<[u8; 32], String> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a Bitcoin transaction id is 64 hexadecimal characters".to_string());
    }
    let mut bytes: [u8; 32] = hex::decode_fixed(text)?;
    bytes.reverse();
    Ok(bytes)
}

fn serialize_output(out: &TxOut) -> Vec<u8> {
    let mut bytes = out.value.to_le_bytes().to_vec();
    bytes.extend(varint(out.script_pubkey.len() as u64));
    bytes.extend_from_slice(&out.script_pubkey);
    bytes
}

fn serialize_outpoint(point: &OutPoint) -> Vec<u8> {
    let mut bytes = point.txid.to_vec();
    bytes.extend_from_slice(&point.vout.to_le_bytes());
    bytes
}

/// The DER encoding of a compact `r || s` signature.
pub fn der_signature(compact: &[u8; 64]) -> Vec<u8> {
    fn integer(bytes: &[u8]) -> Vec<u8> {
        let start = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        let trimmed = &bytes[start..];
        let mut out = vec![0x02];
        if trimmed[0] & 0x80 != 0 {
            out.push(trimmed.len() as u8 + 1);
            out.push(0);
        } else {
            out.push(trimmed.len() as u8);
        }
        out.extend_from_slice(trimmed);
        out
    }
    let mut body = integer(&compact[..32]);
    body.extend(integer(&compact[32..]));
    let mut out = vec![0x30, body.len() as u8];
    out.extend(body);
    out
}

/// Splits a DER signature plus sighash byte into compact `r || s`. Strict DER only.
pub fn compact_from_der(der: &[u8]) -> Option<[u8; 64]> {
    let (&sighash, der) = der.split_last()?;
    if sighash != SIGHASH_ALL || der.len() < 8 || der[0] != 0x30 {
        return None;
    }
    if usize::from(der[1]) != der.len() - 2 || der[2] != 0x02 {
        return None;
    }
    let r_len = usize::from(der[3]);
    let s_marker = 4 + r_len;
    if r_len == 0 || r_len > 33 || der.len() < s_marker + 2 || der[s_marker] != 0x02 {
        return None;
    }
    let s_len = usize::from(der[s_marker + 1]);
    if s_len == 0 || s_len > 33 || der.len() != s_marker + 2 + s_len {
        return None;
    }
    let mut out = [0u8; 64];
    for (offset, int) in [(0usize, &der[4..s_marker]), (32, &der[s_marker + 2..])] {
        let trimmed = match int {
            [0, rest @ ..] => rest,
            other => other,
        };
        if trimmed.len() > 32 {
            return None;
        }
        out[offset + 32 - trimmed.len()..offset + 32].copy_from_slice(trimmed);
    }
    Some(out)
}

/// A witness stack in consensus encoding: item count, then each item with its length.
pub fn serialize_witness(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = varint(items.len() as u64);
    for item in items {
        out.extend(varint(item.len() as u64));
        out.extend_from_slice(item);
    }
    out
}

impl Transaction {
    fn has_witness(&self) -> bool {
        self.inputs.iter().any(|i| !i.witness.is_empty())
    }

    fn serialize_with(&self, witness: bool) -> Vec<u8> {
        let mut out = self.version.to_le_bytes().to_vec();
        if witness {
            out.extend_from_slice(&[0x00, 0x01]);
        }
        out.extend(varint(self.inputs.len() as u64));
        for input in &self.inputs {
            out.extend(serialize_outpoint(&input.previous));
            out.extend(varint(input.script_sig.len() as u64));
            out.extend_from_slice(&input.script_sig);
            out.extend_from_slice(&input.sequence.to_le_bytes());
        }
        out.extend(varint(self.outputs.len() as u64));
        for output in &self.outputs {
            out.extend(serialize_output(output));
        }
        if witness {
            for input in &self.inputs {
                out.extend(serialize_witness(&input.witness));
            }
        }
        out.extend_from_slice(&self.lock_time.to_le_bytes());
        out
    }

    /// The network serialization (with witnesses when any input has one).
    pub fn serialize(&self) -> Vec<u8> {
        self.serialize_with(self.has_witness())
    }

    /// The serialization without witnesses, which the transaction id hashes.
    pub fn serialize_without_witness(&self) -> Vec<u8> {
        self.serialize_with(false)
    }

    /// Transaction id in transaction byte order; use `txid_to_hex` to display it.
    pub fn txid(&self) -> [u8; 32] {
        sha256d(&self.serialize_without_witness())
    }

    pub fn txid_hex(&self) -> String {
        txid_to_hex(&self.txid())
    }

    /// BIP 141 weight: three times the size without witnesses plus the full size.
    pub fn weight(&self) -> u64 {
        let base = self.serialize_without_witness().len() as u64;
        let total = self.serialize().len() as u64;
        base * 3 + total
    }

    /// Virtual size: weight divided by four, rounded up. Fees are charged per virtual byte.
    pub fn vsize(&self) -> u64 {
        self.weight().div_ceil(4)
    }

    pub fn total_output_value(&self) -> Result<u64, String> {
        self.outputs.iter().try_fold(0u64, |sum, o| {
            sum.checked_add(o.value)
                .ok_or_else(|| "output values overflow".to_string())
        })
    }

    /// BIP 143 signature hash of input `index` (`SIGHASH_ALL`) for a P2WPKH output of
    /// `value` satoshis paid to `pubkey_hash`.
    pub fn sighash_p2wpkh(
        &self,
        index: usize,
        pubkey_hash: &[u8; 20],
        value: u64,
    ) -> Result<[u8; 32], String> {
        let input = self.inputs.get(index).ok_or("input index out of range")?;
        let prevouts: Vec<u8> = self
            .inputs
            .iter()
            .flat_map(|i| serialize_outpoint(&i.previous))
            .collect();
        let sequences: Vec<u8> = self
            .inputs
            .iter()
            .flat_map(|i| i.sequence.to_le_bytes())
            .collect();
        let outputs: Vec<u8> = self.outputs.iter().flat_map(serialize_output).collect();
        let mut script_code = vec![0x19, 0x76, 0xa9, 0x14];
        script_code.extend_from_slice(pubkey_hash);
        script_code.extend_from_slice(&[0x88, 0xac]);

        let mut preimage = self.version.to_le_bytes().to_vec();
        preimage.extend_from_slice(&sha256d(&prevouts));
        preimage.extend_from_slice(&sha256d(&sequences));
        preimage.extend(serialize_outpoint(&input.previous));
        preimage.extend(script_code);
        preimage.extend_from_slice(&value.to_le_bytes());
        preimage.extend_from_slice(&input.sequence.to_le_bytes());
        preimage.extend_from_slice(&sha256d(&outputs));
        preimage.extend_from_slice(&self.lock_time.to_le_bytes());
        preimage.extend_from_slice(&u32::from(SIGHASH_ALL).to_le_bytes());
        Ok(sha256d(&preimage))
    }

    /// Signs input `index`, which spends a P2WPKH output of `value` satoshis to `private_key`'s
    /// own address, and stores the witness. The signature is verified before it is stored.
    pub fn sign_p2wpkh_input(
        &mut self,
        index: usize,
        value: u64,
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        let public_key = secp256k1::public_key_compressed(private_key)?;
        let digest = self.sighash_p2wpkh(index, &hash160(&public_key), value)?;
        let (compact, _) = secp256k1::sign_recoverable(private_key, &digest)?;
        if !secp256k1::verify_low_s(&public_key, &digest, &compact) {
            return Err("the signature did not verify; nothing was signed".to_string());
        }
        let mut signature = der_signature(&compact);
        signature.push(SIGHASH_ALL);
        self.inputs[index].witness = vec![signature, public_key.to_vec()];
        Ok(())
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8], String> {
        if len > self.0.len() {
            return Err("the transaction is truncated".to_string());
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn varint(&mut self) -> Result<usize, String> {
        let first = self.take(1)?[0];
        let value = match first {
            0xfd => u64::from(u16::from_le_bytes(
                self.take(2)?.try_into().expect("2 bytes"),
            )),
            0xfe => u64::from(self.u32()?),
            0xff => u64::from_le_bytes(self.take(8)?.try_into().expect("8 bytes")),
            n => u64::from(n),
        };
        // Nothing in a transaction can be longer than the transaction itself.
        if value > 4_000_000 {
            return Err("the transaction has an implausible length field".to_string());
        }
        Ok(value as usize)
    }

    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.varint()?;
        Ok(self.take(len)?.to_vec())
    }
}

impl Transaction {
    /// Reads a transaction in network serialization (with or without witnesses). Anything left
    /// over, or anything cut short, is an error.
    pub fn deserialize(bytes: &[u8]) -> Result<Transaction, String> {
        let mut reader = Reader(bytes);
        let version = reader.u32()?;
        let segwit = reader.0.starts_with(&[0x00, 0x01]);
        if segwit {
            reader.take(2)?;
        }
        let input_count = reader.varint()?;
        let mut inputs = Vec::with_capacity(input_count.min(1000));
        for _ in 0..input_count {
            let txid: [u8; 32] = reader.take(32)?.try_into().expect("32 bytes");
            let vout = reader.u32()?;
            let script_sig = reader.bytes()?;
            let sequence = reader.u32()?;
            inputs.push(TxIn {
                previous: OutPoint { txid, vout },
                script_sig,
                sequence,
                witness: vec![],
            });
        }
        let output_count = reader.varint()?;
        let mut outputs = Vec::with_capacity(output_count.min(1000));
        for _ in 0..output_count {
            let value = u64::from_le_bytes(reader.take(8)?.try_into().expect("8 bytes"));
            outputs.push(TxOut {
                value,
                script_pubkey: reader.bytes()?,
            });
        }
        if segwit {
            for input in &mut inputs {
                let items = reader.varint()?;
                for _ in 0..items {
                    input.witness.push(reader.bytes()?);
                }
            }
        }
        let lock_time = reader.u32()?;
        if !reader.0.is_empty() {
            return Err("the transaction has trailing bytes".to_string());
        }
        if inputs.is_empty() {
            return Err("the transaction has no inputs".to_string());
        }
        Ok(Transaction {
            version,
            inputs,
            outputs,
            lock_time,
        })
    }
}

impl Transaction {
    /// Checks the witness of input `index`, which spends a P2WPKH output of `value` satoshis:
    /// a strict-DER, low-S `SIGHASH_ALL` signature by the key in the witness. Whether that key
    /// is the one the output was paid to is for the caller to check.
    pub fn verify_p2wpkh_input(&self, index: usize, value: u64) -> bool {
        let Some(input) = self.inputs.get(index) else {
            return false;
        };
        let [signature, public_key] = input.witness.as_slice() else {
            return false;
        };
        let Some(compact) = compact_from_der(signature) else {
            return false;
        };
        let Ok(digest) = self.sighash_p2wpkh(index, &hash160(public_key), value) else {
            return false;
        };
        secp256k1::verify_low_s(public_key, &digest, &compact)
    }
}

/// Weight of one P2WPKH input at its largest: 41 bytes outside the witness, and a witness of
/// the item count, a 72-byte signature and a 33-byte key.
pub fn p2wpkh_input_weight() -> u64 {
    let witness = 1 + (1 + MAX_SIGNATURE_BYTES) + (1 + PUBLIC_KEY_BYTES);
    41 * 4 + witness
}

pub fn output_weight(script_len: usize) -> u64 {
    4 * (8 + varint_len(script_len as u64) as u64 + script_len as u64)
}

/// Weight of everything except inputs and outputs: version, counts, lock time and (when any
/// input is SegWit) the marker and flag.
pub fn overhead_weight(inputs: usize, outputs: usize, segwit: bool) -> u64 {
    let base = 4 + varint_len(inputs as u64) + varint_len(outputs as u64) + 4;
    4 * base as u64 + if segwit { 2 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(text: &str) -> Vec<u8> {
        hex::decode(text).unwrap()
    }

    fn txid_bytes(text: &str) -> [u8; 32] {
        bytes(text).try_into().unwrap()
    }

    /// The unsigned transaction of BIP 143's native P2WPKH example.
    fn bip143_transaction() -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![
                TxIn {
                    previous: OutPoint {
                        txid: txid_bytes(
                            "fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f",
                        ),
                        vout: 0,
                    },
                    script_sig: vec![],
                    sequence: 0xffff_ffee,
                    witness: vec![],
                },
                TxIn {
                    previous: OutPoint {
                        txid: txid_bytes(
                            "ef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a",
                        ),
                        vout: 1,
                    },
                    script_sig: vec![],
                    sequence: 0xffff_ffff,
                    witness: vec![],
                },
            ],
            outputs: vec![
                TxOut {
                    value: 112_340_000,
                    script_pubkey: bytes("76a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac"),
                },
                TxOut {
                    value: 223_450_000,
                    script_pubkey: bytes("76a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac"),
                },
            ],
            lock_time: 17,
        }
    }

    #[test]
    fn serializes_the_bip143_example() {
        assert_eq!(
            hex::encode(&bip143_transaction().serialize()),
            "0100000002fff7f7881a8099afa6940d42d1e7f6362bec38171ea3edf433541db4e4ad969f0000000000eeffffffef51e1b804cc89d182d279655c3aa89e815b1b309fe287d9b2b55d57b90ec68a0100000000ffffffff02202cb206000000001976a9148280b37df378db99f66f85c95a783a76ac7a6d5988ac9093510d000000001976a9143bde42dbee7e4dbe6a21b2d50ce2f0167faa815988ac11000000"
        );
    }

    #[test]
    fn bip143_signature_hash_matches() {
        // BIP 143, "Native P2WPKH": input 1 spends 6 BTC to the key 025476c2...
        let tx = bip143_transaction();
        let public_key =
            bytes("025476c2e83188368da1ff3e292e7acafcdb3566bb0ad253f62fc70f07aeee6357");
        let digest = tx
            .sighash_p2wpkh(1, &hash160(&public_key), 600_000_000)
            .unwrap();
        assert_eq!(
            hex::encode(&digest),
            "c37af31116d1b27caf68aae9e3ac82f1477929014d5b917657d0eb49478cb670"
        );
    }

    #[test]
    fn bip143_signing_produces_a_valid_signature() {
        let mut tx = bip143_transaction();
        let key: [u8; 32] =
            txid_bytes("619c335025c7f4012e556c2a58b2506e30b8511b53ade95ea316fd8c3286feb9");
        tx.sign_p2wpkh_input(1, 600_000_000, &key).unwrap();
        let witness = &tx.inputs[1].witness;
        assert_eq!(witness.len(), 2);
        assert_eq!(
            hex::encode(&witness[1]),
            "025476c2e83188368da1ff3e292e7acafcdb3566bb0ad253f62fc70f07aeee6357"
        );
        assert_eq!(witness[0].last(), Some(&SIGHASH_ALL));
        assert_eq!(witness[0][0], 0x30);
        // Signing is deterministic.
        let mut again = bip143_transaction();
        again.sign_p2wpkh_input(1, 600_000_000, &key).unwrap();
        assert_eq!(again, tx);
        // The first input stays unsigned, and the id does not depend on the witness.
        assert!(tx.inputs[0].witness.is_empty());
        assert_eq!(tx.txid(), bip143_transaction().txid());
    }

    #[test]
    fn a_changed_amount_changes_the_signature_hash() {
        let tx = bip143_transaction();
        let hash = [7u8; 20];
        assert_ne!(
            tx.sighash_p2wpkh(0, &hash, 1).unwrap(),
            tx.sighash_p2wpkh(0, &hash, 2).unwrap()
        );
        assert!(tx.sighash_p2wpkh(2, &hash, 1).is_err());
    }

    #[test]
    fn signed_serialization_uses_the_segwit_format() {
        let mut tx = bip143_transaction();
        let base = tx.serialize();
        tx.sign_p2wpkh_input(1, 600_000_000, &[9u8; 32]).unwrap();
        let signed = tx.serialize();
        assert_eq!(&signed[4..6], &[0x00, 0x01]);
        assert_eq!(tx.serialize_without_witness(), base);
        assert!(signed.len() > base.len());
        assert_eq!(tx.weight(), base.len() as u64 * 3 + signed.len() as u64);
        assert_eq!(tx.vsize(), tx.weight().div_ceil(4));
    }

    #[test]
    fn txids_are_displayed_reversed() {
        let tx = bip143_transaction();
        let displayed = tx.txid_hex();
        assert_eq!(parse_txid(&displayed).unwrap(), tx.txid());
        let mut internal = tx.txid();
        internal.reverse();
        assert_eq!(displayed, hex::encode(&internal));
    }

    #[test]
    fn parse_txid_checks_its_input() {
        assert!(parse_txid(&"ab".repeat(32)).is_ok());
        assert!(parse_txid(&"ab".repeat(31)).is_err());
        assert!(parse_txid(&"zz".repeat(32)).is_err());
        assert!(parse_txid(&format!("0x{}", "ab".repeat(31))).is_err());
        assert!(parse_txid("").is_err());
    }

    #[test]
    fn der_encoding_pads_and_trims_integers() {
        let mut compact = [0u8; 64];
        compact[0] = 0x80;
        compact[31] = 1;
        compact[32 + 31] = 2;
        let der = der_signature(&compact);
        // r has its high bit set, so it gets a leading zero; s is trimmed to one byte.
        assert_eq!(der[0], 0x30);
        assert_eq!(der[2], 0x02);
        assert_eq!(der[3], 33);
        assert_eq!(der[4], 0);
        assert_eq!(der[5], 0x80);
        assert_eq!(&der[der.len() - 3..], &[0x02, 0x01, 0x02]);
        assert_eq!(der[1] as usize, der.len() - 2);
    }

    #[test]
    fn weights_match_a_real_transaction() {
        // One P2WPKH input and two P2WPKH outputs: Bitcoin Core's well-known 141 vbytes
        // (with a 71-byte signature; here the largest, 72, is assumed).
        let weight = overhead_weight(1, 2, true) + p2wpkh_input_weight() + 2 * output_weight(22);
        assert_eq!(weight, 42 + 272 + 2 * 124);
        assert_eq!(weight.div_ceil(4), 141);
    }

    #[test]
    fn estimated_weight_is_never_below_the_signed_weight() {
        let mut tx = Transaction {
            version: 2,
            inputs: vec![TxIn {
                previous: OutPoint {
                    txid: [1; 32],
                    vout: 0,
                },
                script_sig: vec![],
                sequence: RBF_SEQUENCE,
                witness: vec![],
            }],
            outputs: vec![
                TxOut {
                    value: 1000,
                    script_pubkey: vec![0; 22],
                },
                TxOut {
                    value: 2000,
                    script_pubkey: vec![0; 22],
                },
            ],
            lock_time: 0,
        };
        let estimate = overhead_weight(1, 2, true) + p2wpkh_input_weight() + 2 * output_weight(22);
        for n in 1..40u8 {
            tx.sign_p2wpkh_input(0, 50_000, &[n; 32]).unwrap();
            assert!(tx.weight() <= estimate, "key {n}");
            assert!(
                tx.weight() + 4 >= estimate - 4,
                "estimate is not wildly high"
            );
        }
    }

    #[test]
    fn total_output_value_detects_overflow() {
        let mut tx = bip143_transaction();
        assert_eq!(tx.total_output_value().unwrap(), 335_790_000);
        tx.outputs[0].value = u64::MAX;
        assert!(tx.total_output_value().is_err());
    }

    #[test]
    fn deserialization_inverts_serialization() {
        let mut tx = bip143_transaction();
        assert_eq!(Transaction::deserialize(&tx.serialize()).unwrap(), tx);
        tx.sign_p2wpkh_input(0, 1000, &[5u8; 32]).unwrap();
        tx.sign_p2wpkh_input(1, 2000, &[6u8; 32]).unwrap();
        let bytes = tx.serialize();
        assert_eq!(Transaction::deserialize(&bytes).unwrap(), tx);
    }

    #[test]
    fn deserialization_refuses_damaged_transactions() {
        let mut tx = bip143_transaction();
        tx.sign_p2wpkh_input(0, 1000, &[5u8; 32]).unwrap();
        let bytes = tx.serialize();
        for cut in [0, 3, 10, bytes.len() - 1] {
            assert!(
                Transaction::deserialize(&bytes[..cut]).is_err(),
                "cut at {cut}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Transaction::deserialize(&trailing).is_err());
        assert!(Transaction::deserialize(&[0; 10]).is_err());
        let huge = [vec![2, 0, 0, 0], vec![0xff; 9]].concat();
        assert!(Transaction::deserialize(&huge).is_err());
    }
}
