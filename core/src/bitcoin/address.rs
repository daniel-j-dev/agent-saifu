//! Bitcoin addresses: parsing, formatting and the output scripts they stand for.
//!
//! Sending is accepted to the five standard output types (P2PKH, P2SH, P2WPKH, P2WSH and
//! P2TR). Addresses for future SegWit versions are refused, because funds sent to a witness
//! program that nothing can spend yet might never be recoverable. The wallet's own address is
//! always native SegWit (P2WPKH), the type Bitcoin Core creates by default.

use super::network::BitcoinNetwork;
use crate::crypto::bech32::{self, Variant};
use crate::crypto::ripemd160::hash160;
use crate::crypto::{base58, hex};
use crate::eth::explorer::sanitize_text_max;

const MAX_ADDRESS_CHARS: usize = 100;
/// Core's `DUSTRELAYTXFEE`: 3 satoshis per virtual byte, the rate dust is measured at.
const DUST_RELAY_FEE_PER_VBYTE: u64 = 3;
/// Size of an input that spends a witness program (P2WPKH, P2WSH, P2TR) as Core counts it for
/// dust: 32 + 4 + 1 + 107/4 + 4.
const SEGWIT_SPEND_VBYTES: u64 = 67;
/// Size of an input that spends a non-witness output: 32 + 4 + 1 + 107 + 4.
const LEGACY_SPEND_VBYTES: u64 = 148;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    P2pkh([u8; 20]),
    P2sh([u8; 20]),
    P2wpkh([u8; 20]),
    P2wsh([u8; 32]),
    P2tr([u8; 32]),
}

fn operation_push(script: &mut Vec<u8>, data: &[u8]) {
    script.push(data.len() as u8);
    script.extend_from_slice(data);
}

/// Bytes of a Bitcoin variable-length integer.
pub fn varint(value: u64) -> Vec<u8> {
    match value {
        0..=0xfc => vec![value as u8],
        0xfd..=0xffff => {
            let mut v = vec![0xfd];
            v.extend_from_slice(&(value as u16).to_le_bytes());
            v
        }
        0x1_0000..=0xffff_ffff => {
            let mut v = vec![0xfe];
            v.extend_from_slice(&(value as u32).to_le_bytes());
            v
        }
        _ => {
            let mut v = vec![0xff];
            v.extend_from_slice(&value.to_le_bytes());
            v
        }
    }
}

pub fn varint_len(value: u64) -> usize {
    varint(value).len()
}

impl Address {
    /// The wallet's own address type: native SegWit from a compressed public key.
    pub fn p2wpkh_of_key(public_key: &[u8; 33]) -> Address {
        Address::P2wpkh(hash160(public_key))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Address::P2pkh(_) => "P2PKH (legacy)",
            Address::P2sh(_) => "P2SH",
            Address::P2wpkh(_) => "P2WPKH (native SegWit)",
            Address::P2wsh(_) => "P2WSH (native SegWit script)",
            Address::P2tr(_) => "P2TR (Taproot)",
        }
    }

    pub fn script_pubkey(&self) -> Vec<u8> {
        let mut script = vec![];
        match self {
            Address::P2pkh(hash) => {
                script.extend_from_slice(&[0x76, 0xa9]);
                operation_push(&mut script, hash);
                script.extend_from_slice(&[0x88, 0xac]);
            }
            Address::P2sh(hash) => {
                script.push(0xa9);
                operation_push(&mut script, hash);
                script.push(0x87);
            }
            Address::P2wpkh(hash) => {
                script.push(0x00);
                operation_push(&mut script, hash);
            }
            Address::P2wsh(hash) => {
                script.push(0x00);
                operation_push(&mut script, hash);
            }
            Address::P2tr(key) => {
                script.push(0x51);
                operation_push(&mut script, key);
            }
        }
        script
    }

    fn is_witness_program(&self) -> bool {
        matches!(
            self,
            Address::P2wpkh(_) | Address::P2wsh(_) | Address::P2tr(_)
        )
    }

    /// The smallest output value the network relays for this script, as Bitcoin Core computes
    /// it: an output is dust when spending it would cost more than a third of its value at
    /// 3 sat/vB. 294 satoshis for P2WPKH, 330 for P2WSH and P2TR, 546 for P2PKH, 540 for P2SH.
    pub fn dust_threshold(&self) -> u64 {
        dust_threshold_for(&self.script_pubkey(), self.is_witness_program())
    }

    pub fn to_text(&self, network: BitcoinNetwork) -> String {
        match self {
            Address::P2pkh(hash) => base58_address(network.p2pkh_version(), hash),
            Address::P2sh(hash) => base58_address(network.p2sh_version(), hash),
            Address::P2wpkh(hash) => segwit_address(network, 0, hash),
            Address::P2wsh(hash) => segwit_address(network, 0, hash),
            Address::P2tr(key) => segwit_address(network, 1, key),
        }
    }

    /// Reads an address for `network`. An address from another Bitcoin network is refused with
    /// a message that says so.
    pub fn parse(text: &str, network: BitcoinNetwork) -> Result<Address, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("address is empty".to_string());
        }
        if text.len() > MAX_ADDRESS_CHARS {
            return Err("address is too long".to_string());
        }
        let lower = text.to_ascii_lowercase();
        let is_segwit = ["bc1", "tb1"].iter().any(|p| lower.starts_with(p));
        let result = if is_segwit {
            parse_segwit(text, network)
        } else {
            parse_base58(text, network)
        };
        result.map_err(|e| {
            format!(
                "'{}' is not a valid Bitcoin {} address: {e}",
                sanitize_text_max(text, 70),
                network.id()
            )
        })
    }
}

/// Dust threshold of an output script (see `Address::dust_threshold`).
pub fn dust_threshold_for(script: &[u8], spends_as_witness: bool) -> u64 {
    let output_size = 8 + varint_len(script.len() as u64) as u64 + script.len() as u64;
    let input_size = if spends_as_witness {
        SEGWIT_SPEND_VBYTES
    } else {
        LEGACY_SPEND_VBYTES
    };
    (output_size + input_size) * DUST_RELAY_FEE_PER_VBYTE
}

fn base58_address(version: u8, hash: &[u8; 20]) -> String {
    let mut payload = vec![version];
    payload.extend_from_slice(hash);
    base58::encode_check(&payload)
}

fn segwit_address(network: BitcoinNetwork, version: u8, program: &[u8]) -> String {
    let mut data = vec![version];
    data.extend(bech32::convert_bits(program, 8, 5, true).expect("8-bit input always converts"));
    let variant = if version == 0 {
        Variant::Bech32
    } else {
        Variant::Bech32m
    };
    bech32::encode(network.hrp(), &data, variant).expect("a valid witness program encodes")
}

fn parse_segwit(text: &str, network: BitcoinNetwork) -> Result<Address, String> {
    let (hrp, data, variant) = bech32::decode(text)?;
    if hrp != network.hrp() {
        return Err(format!(
            "it belongs to {}, not {}",
            if hrp == "bc" {
                "Bitcoin mainnet"
            } else {
                "a Bitcoin test network"
            },
            if network.is_mainnet() {
                "Bitcoin mainnet"
            } else {
                "a Bitcoin test network"
            }
        ));
    }
    let (version, rest) = data.split_first().ok_or("it has no witness version")?;
    if *version > 16 {
        return Err("the witness version is out of range".to_string());
    }
    let program = bech32::convert_bits(rest, 5, 8, false)?;
    if !(2..=40).contains(&program.len()) {
        return Err("the witness program has an invalid length".to_string());
    }
    match (*version, variant) {
        (0, Variant::Bech32) => match program.len() {
            20 => Ok(Address::P2wpkh(program.try_into().expect("20 bytes"))),
            32 => Ok(Address::P2wsh(program.try_into().expect("32 bytes"))),
            _ => Err("a version 0 witness program must be 20 or 32 bytes".to_string()),
        },
        (0, Variant::Bech32m) => {
            Err("a version 0 address must use the bech32 checksum, not bech32m".to_string())
        }
        (_, Variant::Bech32) => {
            Err("an address above version 0 must use the bech32m checksum".to_string())
        }
        (1, Variant::Bech32m) if program.len() == 32 => {
            Ok(Address::P2tr(program.try_into().expect("32 bytes")))
        }
        (1, Variant::Bech32m) => Err("a Taproot program must be 32 bytes".to_string()),
        (v, Variant::Bech32m) => Err(format!(
            "SegWit version {v} addresses are not supported yet; funds sent there could be unspendable"
        )),
    }
}

fn parse_base58(text: &str, network: BitcoinNetwork) -> Result<Address, String> {
    let payload = base58::decode_check(text)?;
    if payload.len() != 21 {
        return Err("it has the wrong length".to_string());
    }
    let hash: [u8; 20] = payload[1..].try_into().expect("20 bytes");
    let version = payload[0];
    if version == network.p2pkh_version() {
        Ok(Address::P2pkh(hash))
    } else if version == network.p2sh_version() {
        Ok(Address::P2sh(hash))
    } else if [0x00u8, 0x05, 0x6f, 0xc4].contains(&version) {
        Err("it belongs to a different Bitcoin network".to_string())
    } else {
        Err(format!(
            "its version byte 0x{} is not a Bitcoin address",
            hex::encode(&[version])
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: BitcoinNetwork = BitcoinNetwork::Mainnet;
    const TEST: BitcoinNetwork = BitcoinNetwork::Testnet;

    #[test]
    fn segwit_addresses_from_bip173_and_bip350() {
        for (text, network, script) in [
            (
                "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
                MAIN,
                "0014751e76e8199196d454941c45d1b3a323f1433bd6",
            ),
            (
                "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7",
                TEST,
                "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262",
            ),
            (
                "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
                MAIN,
                "512079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            ),
            (
                "tb1pqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesf3hn0c",
                TEST,
                "5120000000c4a5cad46221b2a187905e5266362b99d5e91c6ce24d165dab93e86433",
            ),
        ] {
            let address = Address::parse(text, network).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(hex::encode(&address.script_pubkey()), script, "{text}");
            assert_eq!(address.to_text(network), text.to_lowercase(), "{text}");
        }
    }

    #[test]
    fn legacy_addresses_round_trip() {
        for (text, network, script) in [
            (
                "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
                MAIN,
                "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac",
            ),
            (
                "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
                MAIN,
                "a914b472a266d0bd89c13706a4132ccfb16f7c3b9fcb87",
            ),
            (
                "mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn",
                TEST,
                "76a914243f1394f44554f4ce3fd68649c19adc483ce92488ac",
            ),
        ] {
            let address = Address::parse(text, network).unwrap();
            assert_eq!(hex::encode(&address.script_pubkey()), script, "{text}");
            assert_eq!(address.to_text(network), text);
        }
    }

    #[test]
    fn the_abandon_wallet_address_has_the_known_hash() {
        let key = hex::decode("0330d54fd0dd420a6e5f8d3624f5f3482cae350f79d5f0753bf5beef9c2d91af3c")
            .unwrap();
        let address = Address::p2wpkh_of_key(&key.try_into().unwrap());
        assert_eq!(
            address.to_text(MAIN),
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"
        );
    }

    #[test]
    fn invalid_segwit_addresses_from_the_bips_are_refused() {
        for text in [
            "tc1qw508d6qejxtdg4y5r3zarvary0c5xw7kg3g4ty",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5",
            "BC13W508D6QEJXTDG4Y5R3ZARVARY0C5XW7KN40WF2",
            "bc1rw5uspcuh",
            "bc10w508d6qejxtdg4y5r3zarvary0c5xw7kw508d6qejxtdg4y5r3zarvary0c5xw7kw5rljs90",
            "BC1QR508D6QEJXTDG4Y5R3ZARVARYV98GJ9P",
            "bc1zw508d6qejxtdg4y5r3zarvaryvqyzf3du",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vq47Zagq",
            "bc1gmk9yu",
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3pjxtptv",
            "bc1p38j9r5y49hruaue7wxjce0updqjuyyx0kh56v8s25huc6995vvpql3jow4",
        ] {
            assert!(
                Address::parse(text, MAIN).is_err() && Address::parse(text, TEST).is_err(),
                "{text} should be refused"
            );
        }
    }

    #[test]
    fn future_witness_versions_are_refused_with_a_reason() {
        // A valid bech32m version 2 address (BIP 350 test vector).
        let error = Address::parse("bc1zw508d6qejxtdg4y5r3zarvaryvaxxpcs", MAIN).unwrap_err();
        assert!(error.contains("not supported yet"), "{error}");
    }

    #[test]
    fn addresses_of_another_network_are_refused_clearly() {
        let error = Address::parse("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", MAIN).unwrap_err();
        assert!(error.contains("not Bitcoin mainnet"), "{error}");
        let error = Address::parse("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", TEST).unwrap_err();
        assert!(error.contains("different Bitcoin network"), "{error}");
        let error = Address::parse("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", TEST).unwrap_err();
        assert!(error.contains("not a Bitcoin test network"), "{error}");
    }

    #[test]
    fn garbage_is_refused() {
        for text in [
            "",
            "   ",
            "hello",
            "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNb",
            "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLz",
            &"1".repeat(200),
        ] {
            assert!(Address::parse(text, MAIN).is_err(), "{text:?}");
        }
    }

    #[test]
    fn upper_case_segwit_is_accepted_but_mixed_case_is_not() {
        let upper = "BC1QCR8TE4KR609GCAWUTMRZA0J4XV80JY8Z306FYU";
        assert!(Address::parse(upper, MAIN).is_ok());
        let mixed = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyU";
        assert!(Address::parse(mixed, MAIN).is_err());
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert!(Address::parse("  1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa\n", MAIN).is_ok());
    }

    #[test]
    fn dust_thresholds_match_bitcoin_core() {
        assert_eq!(Address::P2pkh([0; 20]).dust_threshold(), 546);
        assert_eq!(Address::P2sh([0; 20]).dust_threshold(), 540);
        assert_eq!(Address::P2wpkh([0; 20]).dust_threshold(), 294);
        assert_eq!(Address::P2wsh([0; 32]).dust_threshold(), 330);
        assert_eq!(Address::P2tr([0; 32]).dust_threshold(), 330);
    }

    #[test]
    fn script_lengths_are_standard() {
        assert_eq!(Address::P2pkh([1; 20]).script_pubkey().len(), 25);
        assert_eq!(Address::P2sh([1; 20]).script_pubkey().len(), 23);
        assert_eq!(Address::P2wpkh([1; 20]).script_pubkey().len(), 22);
        assert_eq!(Address::P2wsh([1; 32]).script_pubkey().len(), 34);
        assert_eq!(Address::P2tr([1; 32]).script_pubkey().len(), 34);
    }

    #[test]
    fn varints_use_the_shortest_form() {
        assert_eq!(varint(0), vec![0]);
        assert_eq!(varint(0xfc), vec![0xfc]);
        assert_eq!(varint(0xfd), vec![0xfd, 0xfd, 0]);
        assert_eq!(varint(0xffff), vec![0xfd, 0xff, 0xff]);
        assert_eq!(varint(0x10000), vec![0xfe, 0, 0, 1, 0]);
        assert_eq!(varint(0x1_0000_0000).len(), 9);
        assert_eq!(varint_len(300), 3);
    }
}
