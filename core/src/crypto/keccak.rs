//! Keccak-256 (the original Keccak padding used by Ethereum, not NIST SHA3).

const ROUND_CONSTANTS: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808a,
    0x8000000080008000,
    0x000000000000808b,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008a,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000a,
    0x000000008000808b,
    0x800000000000008b,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800a,
    0x800000008000000a,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

const ROTATIONS: [u32; 24] = [
    1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
];

const PI_LANES: [usize; 24] = [
    10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
];

const RATE: usize = 136;

fn keccak_f(state: &mut [u64; 25]) {
    for rc in ROUND_CONSTANTS {
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                state[x + 5 * y] ^= d;
            }
        }
        let mut last = state[1];
        for i in 0..24 {
            let j = PI_LANES[i];
            let tmp = state[j];
            state[j] = last.rotate_left(ROTATIONS[i]);
            last = tmp;
        }
        for y in 0..5 {
            let row = [
                state[5 * y],
                state[5 * y + 1],
                state[5 * y + 2],
                state[5 * y + 3],
                state[5 * y + 4],
            ];
            for x in 0..5 {
                state[5 * y + x] = row[x] ^ (!row[(x + 1) % 5] & row[(x + 2) % 5]);
            }
        }
        state[0] ^= rc;
    }
}

fn absorb_block(state: &mut [u64; 25], block: &[u8]) {
    for (i, lane) in block.chunks_exact(8).enumerate() {
        state[i] ^= u64::from_le_bytes(lane.try_into().expect("8-byte lane"));
    }
    keccak_f(state);
}

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut state = [0u64; 25];
    let mut chunks = data.chunks_exact(RATE);
    for block in &mut chunks {
        absorb_block(&mut state, block);
    }
    let rest = chunks.remainder();
    let mut last = [0u8; RATE];
    last[..rest.len()].copy_from_slice(rest);
    last[rest.len()] ^= 0x01;
    last[RATE - 1] ^= 0x80;
    absorb_block(&mut state, &last);
    let mut out = [0u8; 32];
    for (i, lane) in state.iter().take(4).enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    #[test]
    fn known_vectors() {
        assert_eq!(
            hex::encode(&keccak256(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
        assert_eq!(
            hex::encode(&keccak256(b"abc")),
            "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"
        );
        assert_eq!(
            hex::encode(&keccak256(b"transfer(address,uint256)")[..4]),
            "a9059cbb"
        );
    }

    #[test]
    fn block_boundary_lengths_are_distinct_and_stable() {
        let mut seen = std::collections::HashSet::new();
        for len in [0, 1, 135, 136, 137, 271, 272, 273] {
            let digest = keccak256(&vec![0x61u8; len]);
            assert!(seen.insert(digest), "collision at {len}");
            assert_eq!(digest, keccak256(&vec![0x61u8; len]));
        }
    }

    #[test]
    fn multi_block_vectors() {
        assert_eq!(
            hex::encode(&keccak256(b"The quick brown fox jumps over the lazy dog")),
            "4d741b6f1eb29cb2a9b9911c82f56fa8d73b04959d3d9d222895df6c0b28aa15"
        );
        assert_eq!(
            hex::encode(&keccak256(&[b'a'; 200])),
            "96ea54061def936c4be90b518992fdc6f12f535068a256229aca54267b4d084d"
        );
    }
}
