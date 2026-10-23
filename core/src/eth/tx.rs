//! Ethereum transaction construction and signing (legacy EIP-155 and EIP-1559).

use super::address::{Address, address_from_private_key};
use super::rlp;
use super::u256::U256;
use crate::crypto::keccak::keccak256;
use crate::crypto::secp256k1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fees {
    Eip1559 {
        max_priority_fee_per_gas: U256,
        max_fee_per_gas: U256,
    },
    Legacy {
        gas_price: U256,
    },
}

impl Fees {
    /// Highest per-gas price the sender could be charged.
    pub fn max_gas_price(&self) -> U256 {
        match self {
            Fees::Eip1559 {
                max_fee_per_gas, ..
            } => *max_fee_per_gas,
            Fees::Legacy { gas_price } => *gas_price,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedTx {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_limit: u64,
    pub to: Option<Address>,
    pub value: U256,
    pub data: Vec<u8>,
    pub fees: Fees,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedTx {
    pub raw: Vec<u8>,
    pub hash: [u8; 32],
}

fn encode_to(to: &Option<Address>) -> Vec<u8> {
    match to {
        Some(addr) => rlp::encode_bytes(addr),
        None => rlp::encode_bytes(&[]),
    }
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

impl UnsignedTx {
    fn base_fields(&self) -> Vec<Vec<u8>> {
        match &self.fees {
            Fees::Eip1559 {
                max_priority_fee_per_gas,
                max_fee_per_gas,
            } => vec![
                rlp::encode_u64(self.chain_id),
                rlp::encode_u64(self.nonce),
                rlp::encode_u256(max_priority_fee_per_gas),
                rlp::encode_u256(max_fee_per_gas),
                rlp::encode_u64(self.gas_limit),
                encode_to(&self.to),
                rlp::encode_u256(&self.value),
                rlp::encode_bytes(&self.data),
                rlp::encode_list(&[]),
            ],
            Fees::Legacy { gas_price } => vec![
                rlp::encode_u64(self.nonce),
                rlp::encode_u256(gas_price),
                rlp::encode_u64(self.gas_limit),
                encode_to(&self.to),
                rlp::encode_u256(&self.value),
                rlp::encode_bytes(&self.data),
            ],
        }
    }

    /// The digest that gets signed.
    pub fn signing_hash(&self) -> [u8; 32] {
        let mut fields = self.base_fields();
        match self.fees {
            Fees::Eip1559 { .. } => {
                let mut payload = vec![0x02];
                payload.extend(rlp::encode_list(&fields));
                keccak256(&payload)
            }
            Fees::Legacy { .. } => {
                fields.push(rlp::encode_u64(self.chain_id));
                fields.push(rlp::encode_u64(0));
                fields.push(rlp::encode_u64(0));
                keccak256(&rlp::encode_list(&fields))
            }
        }
    }

    /// Signs with `private_key`, verifying that the signature recovers to the sender.
    pub fn sign(&self, private_key: &[u8; 32]) -> Result<SignedTx, String> {
        if self.chain_id == 0 {
            return Err("chain id must be non-zero".to_string());
        }
        let digest = self.signing_hash();
        let (sig, recid) = secp256k1::sign_recoverable(private_key, &digest)?;
        let recovered = secp256k1::recover_public_key(&digest, &sig, recid)?;
        let expected = secp256k1::public_key_uncompressed(private_key)?;
        if recovered != expected {
            return Err("internal error: signature self-check failed".to_string());
        }
        let r = rlp::encode_bytes(strip_leading_zeros(&sig[..32]));
        let s = rlp::encode_bytes(strip_leading_zeros(&sig[32..]));
        let mut fields = self.base_fields();
        let raw = match self.fees {
            Fees::Eip1559 { .. } => {
                fields.push(rlp::encode_u64(u64::from(recid)));
                fields.push(r);
                fields.push(s);
                let mut raw = vec![0x02];
                raw.extend(rlp::encode_list(&fields));
                raw
            }
            Fees::Legacy { .. } => {
                let v = self
                    .chain_id
                    .checked_mul(2)
                    .and_then(|x| x.checked_add(35 + u64::from(recid)))
                    .ok_or("chain id too large for EIP-155")?;
                fields.push(rlp::encode_u64(v));
                fields.push(r);
                fields.push(s);
                rlp::encode_list(&fields)
            }
        };
        let hash = keccak256(&raw);
        Ok(SignedTx { raw, hash })
    }

    /// Maximum wei the sender can be charged: `gas_limit * max_gas_price + value`.
    pub fn max_cost(&self) -> Option<U256> {
        self.fees
            .max_gas_price()
            .checked_mul_u64(self.gas_limit)?
            .checked_add(&self.value)
    }

    pub fn sender_for(&self, private_key: &[u8; 32]) -> Result<Address, String> {
        address_from_private_key(private_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;
    use crate::eth::address::parse_address;

    fn eip155_example() -> UnsignedTx {
        UnsignedTx {
            chain_id: 1,
            nonce: 9,
            gas_limit: 21000,
            to: Some(parse_address("0x3535353535353535353535353535353535353535").unwrap()),
            value: U256::parse_units("1", 18).unwrap(),
            data: vec![],
            fees: Fees::Legacy {
                gas_price: U256::from_u64(20_000_000_000),
            },
        }
    }

    #[test]
    fn eip155_official_example() {
        let tx = eip155_example();
        assert_eq!(
            hex::encode(&tx.signing_hash()),
            "daf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53"
        );
        let signed = tx.sign(&[0x46u8; 32]).unwrap();
        assert_eq!(
            hex::encode(&signed.raw),
            "f86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83"
        );
        assert_eq!(
            hex::encode(&signed.hash),
            hex::encode(&keccak256(&signed.raw))
        );
    }

    fn eip1559_tx() -> UnsignedTx {
        UnsignedTx {
            chain_id: 1,
            nonce: 0,
            gas_limit: 21000,
            to: Some([0xabu8; 20]),
            value: U256::from_u64(1),
            data: vec![],
            fees: Fees::Eip1559 {
                max_priority_fee_per_gas: U256::from_u64(1_000_000_000),
                max_fee_per_gas: U256::from_u64(30_000_000_000),
            },
        }
    }

    #[test]
    fn eip1559_payload_layout() {
        let tx = eip1559_tx();
        let mut key = [0u8; 32];
        key[31] = 1;
        let signed = tx.sign(&key).unwrap();
        assert_eq!(signed.raw[0], 0x02);
        // 0x02 || rlp([chainId,nonce,tip,maxFee,gas,to,value,data,accessList,yParity,r,s])
        assert!(signed.raw[1] >= 0xf8);
        let unsigned_hash = {
            let mut payload = vec![0x02];
            payload.extend(rlp::encode_list(&[
                rlp::encode_u64(1),
                rlp::encode_u64(0),
                rlp::encode_u64(1_000_000_000),
                rlp::encode_u64(30_000_000_000),
                rlp::encode_u64(21000),
                rlp::encode_bytes(&[0xab; 20]),
                rlp::encode_u64(1),
                rlp::encode_bytes(&[]),
                rlp::encode_list(&[]),
            ]));
            keccak256(&payload)
        };
        assert_eq!(tx.signing_hash(), unsigned_hash);
    }

    #[test]
    fn signature_recovers_to_sender_for_both_types() {
        let mut key = [0u8; 32];
        key[31] = 7;
        for tx in [eip1559_tx(), eip155_example()] {
            let signed = tx.sign(&key).unwrap();
            assert!(!signed.raw.is_empty());
            let (sig, recid) = secp256k1::sign_recoverable(&key, &tx.signing_hash()).unwrap();
            let public = secp256k1::recover_public_key(&tx.signing_hash(), &sig, recid).unwrap();
            assert_eq!(public, secp256k1::public_key_uncompressed(&key).unwrap());
        }
    }

    #[test]
    fn signing_is_deterministic() {
        let mut key = [0u8; 32];
        key[31] = 3;
        assert_eq!(eip1559_tx().sign(&key), eip1559_tx().sign(&key));
    }

    #[test]
    fn contract_creation_encodes_empty_to() {
        let mut tx = eip1559_tx();
        tx.to = None;
        let mut key = [0u8; 32];
        key[31] = 1;
        assert!(tx.sign(&key).is_ok());
        assert_eq!(encode_to(&None), vec![0x80]);
    }

    #[test]
    fn different_fields_change_the_hash() {
        let a = eip1559_tx();
        let mut b = eip1559_tx();
        b.nonce = 1;
        let mut c = eip1559_tx();
        c.chain_id = 8453;
        let mut d = eip1559_tx();
        d.data = vec![1];
        let hashes = [
            a.signing_hash(),
            b.signing_hash(),
            c.signing_hash(),
            d.signing_hash(),
        ];
        for i in 0..4 {
            for j in i + 1..4 {
                assert_ne!(hashes[i], hashes[j]);
            }
        }
    }

    #[test]
    fn rejects_zero_chain_id_and_invalid_key() {
        let mut tx = eip1559_tx();
        tx.chain_id = 0;
        let mut key = [0u8; 32];
        key[31] = 1;
        assert!(tx.sign(&key).is_err());
        assert!(eip1559_tx().sign(&[0u8; 32]).is_err());
    }

    #[test]
    fn max_cost_adds_value_and_gas() {
        let tx = eip1559_tx();
        assert_eq!(
            tx.max_cost(),
            Some(U256::from_u64(30_000_000_000u64 * 21000 + 1))
        );
        let mut huge = eip1559_tx();
        huge.value = U256::MAX;
        assert_eq!(huge.max_cost(), None);
    }

    #[test]
    fn legacy_v_includes_chain_id() {
        let mut tx = eip155_example();
        tx.chain_id = 8453;
        let mut key = [0u8; 32];
        key[31] = 1;
        let signed = tx.sign(&key).unwrap();
        let v_bytes = 8453u64 * 2 + 35;
        let v_enc = rlp::encode_u64(v_bytes);
        let v_enc1 = rlp::encode_u64(v_bytes + 1);
        let raw = &signed.raw;
        assert!(
            raw.windows(v_enc.len()).any(|w| w == v_enc.as_slice())
                || raw.windows(v_enc1.len()).any(|w| w == v_enc1.as_slice())
        );
    }
}
