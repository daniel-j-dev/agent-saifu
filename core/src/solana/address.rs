//! Solana addresses (public keys), well-known program IDs and program-derived addresses.

use crate::crypto::{base58, ed25519, sha2::sha256};

pub type Pubkey = [u8; 32];

pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";
pub const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
pub const MEMO_PROGRAM_V1: &str = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo";
pub const ADDRESS_LOOKUP_TABLE_PROGRAM: &str = "AddressLookupTab1e1111111111111111111111111";
pub const WRAPPED_SOL_MINT: &str = "So11111111111111111111111111111111111111112";

const MIN_ADDRESS_CHARS: usize = 32;
const MAX_ADDRESS_CHARS: usize = 44;

/// A built-in program ID; the constants above are known to be valid.
pub fn id(text: &str) -> Pubkey {
    base58::decode_fixed::<32>(text).expect("built-in program ID is valid base58")
}

pub fn to_text(key: &Pubkey) -> String {
    base58::encode(key)
}

/// A Solana address is 32 bytes written in base58 (32 to 44 characters).
pub fn parse(input: &str) -> Result<Pubkey, String> {
    let text = input.trim();
    if text.len() < MIN_ADDRESS_CHARS || text.len() > MAX_ADDRESS_CHARS {
        return Err("a Solana address is 32 to 44 base58 characters".to_string());
    }
    base58::decode_fixed::<32>(text)
        .map_err(|_| "not a valid Solana address (base58 of 32 bytes)".to_string())
}

/// Program-derived address: the first bump seed (counting down from 255) whose hash is not a
/// valid Ed25519 point, so no private key can exist for it.
pub fn find_program_address(seeds: &[&[u8]], program: &Pubkey) -> Option<(Pubkey, u8)> {
    for bump in (0..=255u8).rev() {
        let mut data: Vec<u8> = vec![];
        for seed in seeds {
            data.extend_from_slice(seed);
        }
        data.push(bump);
        data.extend_from_slice(program);
        data.extend_from_slice(b"ProgramDerivedAddress");
        let candidate = sha256(&data);
        if !ed25519::is_on_curve(&candidate) {
            return Some((candidate, bump));
        }
    }
    None
}

/// The associated token account of `owner` for `mint` under a token program.
pub fn associated_token_address(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<Pubkey, String> {
    find_program_address(
        &[owner.as_slice(), token_program.as_slice(), mint.as_slice()],
        &id(ASSOCIATED_TOKEN_PROGRAM),
    )
    .map(|(address, _)| address)
    .ok_or_else(|| "cannot derive the associated token account".to_string())
}

/// Programs that must never receive a plain transfer: funds sent to them are lost.
pub fn is_burn_or_program_address(key: &Pubkey) -> bool {
    [
        SYSTEM_PROGRAM,
        TOKEN_PROGRAM,
        TOKEN_2022_PROGRAM,
        ASSOCIATED_TOKEN_PROGRAM,
        COMPUTE_BUDGET_PROGRAM,
        MEMO_PROGRAM,
        MEMO_PROGRAM_V1,
        ADDRESS_LOOKUP_TABLE_PROGRAM,
    ]
    .iter()
    .any(|p| id(p) == *key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_ids_decode_to_32_bytes() {
        assert_eq!(id(SYSTEM_PROGRAM), [0u8; 32]);
        for p in [
            TOKEN_PROGRAM,
            TOKEN_2022_PROGRAM,
            ASSOCIATED_TOKEN_PROGRAM,
            COMPUTE_BUDGET_PROGRAM,
            MEMO_PROGRAM,
            MEMO_PROGRAM_V1,
            ADDRESS_LOOKUP_TABLE_PROGRAM,
            WRAPPED_SOL_MINT,
        ] {
            assert_eq!(to_text(&id(p)), p);
        }
    }

    #[test]
    fn parse_accepts_only_32_byte_base58() {
        let key = [7u8; 32];
        assert_eq!(parse(&format!("  {}\n", to_text(&key))).unwrap(), key);
        assert_eq!(parse(SYSTEM_PROGRAM).unwrap(), [0u8; 32]);
        for bad in [
            "",
            "abc",
            "0x0000000000000000000000000000000000000000",
            &"1".repeat(31),
            &"z".repeat(45),
            // right length but with a character outside the base58 alphabet
            "0OIl1111111111111111111111111111",
            // valid base58 of a 33-byte value
            &base58::encode(&[9u8; 33]),
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    // Verified against mainnet: `getTokenAccountsByOwner` reports this as the USDC account of the
    // wallet below (an associated token account holding 2039280 lamports of rent).
    #[test]
    fn associated_token_address_matches_the_network() {
        let owner = parse("2ojv9BAiHUrvsm9gxDe7fJSzbNZSJcxZvf8dqmWGHG8S").unwrap();
        let usdc = parse("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
        let ata = associated_token_address(&owner, &usdc, &id(TOKEN_PROGRAM)).unwrap();
        assert_eq!(
            to_text(&ata),
            "9KaA7vEBUdRCcBWxfuMjxYwKfvu8Us3Cg5gkhVFt2LNk"
        );
    }

    #[test]
    fn derived_addresses_are_off_the_curve_and_deterministic() {
        let owner = [3u8; 32];
        let mint = [4u8; 32];
        let a = associated_token_address(&owner, &mint, &id(TOKEN_PROGRAM)).unwrap();
        let b = associated_token_address(&owner, &mint, &id(TOKEN_PROGRAM)).unwrap();
        assert_eq!(a, b);
        assert!(!ed25519::is_on_curve(&a));
        let other_program =
            associated_token_address(&owner, &mint, &id(TOKEN_2022_PROGRAM)).unwrap();
        assert_ne!(a, other_program);
        let other_owner = associated_token_address(&[5u8; 32], &mint, &id(TOKEN_PROGRAM)).unwrap();
        assert_ne!(a, other_owner);
    }

    #[test]
    fn burn_and_program_addresses_are_flagged() {
        assert!(is_burn_or_program_address(&[0u8; 32]));
        assert!(is_burn_or_program_address(&id(TOKEN_PROGRAM)));
        assert!(!is_burn_or_program_address(&[1u8; 32]));
    }
}
