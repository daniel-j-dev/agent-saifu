//! RIPEMD-160 and Bitcoin's `HASH160` (RIPEMD-160 of SHA-256), used for Bitcoin addresses.

use super::sha2::sha256;

const LEFT_WORD: [usize; 80] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5,
    2, 14, 11, 8, 3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12, 1, 9, 11, 10, 0, 8, 12, 4,
    13, 3, 7, 15, 14, 5, 6, 2, 4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13,
];
const RIGHT_WORD: [usize; 80] = [
    5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, 6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12,
    4, 9, 1, 2, 15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13, 8, 6, 4, 1, 3, 11, 15, 0, 5,
    12, 2, 13, 9, 7, 10, 14, 12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11,
];
const LEFT_SHIFT: [u32; 80] = [
    11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8, 7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15,
    9, 11, 7, 13, 12, 11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5, 11, 12, 14, 15, 14,
    15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12, 9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6,
];
const RIGHT_SHIFT: [u32; 80] = [
    8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6, 9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12,
    7, 6, 15, 13, 11, 9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5, 15, 5, 8, 11, 14, 14,
    6, 14, 6, 9, 12, 9, 12, 5, 15, 8, 8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11,
];
const LEFT_CONSTANT: [u32; 5] = [
    0x0000_0000,
    0x5a82_7999,
    0x6ed9_eba1,
    0x8f1b_bcdc,
    0xa953_fd4e,
];
const RIGHT_CONSTANT: [u32; 5] = [
    0x50a2_8be6,
    0x5c4d_d124,
    0x6d70_3ef3,
    0x7a6d_76e9,
    0x0000_0000,
];

fn mix(round: usize, x: u32, y: u32, z: u32) -> u32 {
    match round {
        0 => x ^ y ^ z,
        1 => (x & y) | (!x & z),
        2 => (x | !y) ^ z,
        3 => (x & z) | (y & !z),
        _ => x ^ (y | !z),
    }
}

fn compress(state: &mut [u32; 5], block: &[u8]) {
    let mut words = [0u32; 16];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    let [mut al, mut bl, mut cl, mut dl, mut el] = *state;
    let [mut ar, mut br, mut cr, mut dr, mut er] = *state;
    for step in 0..80 {
        let round = step / 16;
        let t = al
            .wrapping_add(mix(round, bl, cl, dl))
            .wrapping_add(words[LEFT_WORD[step]])
            .wrapping_add(LEFT_CONSTANT[round])
            .rotate_left(LEFT_SHIFT[step])
            .wrapping_add(el);
        al = el;
        el = dl;
        dl = cl.rotate_left(10);
        cl = bl;
        bl = t;
        let t = ar
            .wrapping_add(mix(4 - round, br, cr, dr))
            .wrapping_add(words[RIGHT_WORD[step]])
            .wrapping_add(RIGHT_CONSTANT[round])
            .rotate_left(RIGHT_SHIFT[step])
            .wrapping_add(er);
        ar = er;
        er = dr;
        dr = cr.rotate_left(10);
        cr = br;
        br = t;
    }
    let t = state[1].wrapping_add(cl).wrapping_add(dr);
    state[1] = state[2].wrapping_add(dl).wrapping_add(er);
    state[2] = state[3].wrapping_add(el).wrapping_add(ar);
    state[3] = state[4].wrapping_add(al).wrapping_add(br);
    state[4] = state[0].wrapping_add(bl).wrapping_add(cr);
    state[0] = t;
}

pub fn ripemd160(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x6745_2301,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for block in padded.chunks_exact(64) {
        compress(&mut state, block);
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_exact_mut(4).zip(state) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// `RIPEMD160(SHA256(data))`: the hash inside P2PKH, P2SH and P2WPKH addresses.
pub fn hash160(data: &[u8]) -> [u8; 20] {
    ripemd160(&sha256(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hex;

    fn digest(data: &[u8]) -> String {
        hex::encode(&ripemd160(data))
    }

    #[test]
    fn matches_the_published_vectors() {
        assert_eq!(digest(b""), "9c1185a5c5e9fc54612808977ee8f548b2258d31");
        assert_eq!(digest(b"a"), "0bdc9d2d256b3ee9daae347be6f4dc835a467ffe");
        assert_eq!(digest(b"abc"), "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc");
        assert_eq!(
            digest(b"message digest"),
            "5d0689ef49d2fae572b881b123a85ffa21595f36"
        );
        assert_eq!(
            digest(b"abcdefghijklmnopqrstuvwxyz"),
            "f71c27109c692c1b56bbdceb5b9d2865b3708dbc"
        );
        assert_eq!(
            digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "12a053384a9c0c88e405a06c27dcf49ada62eb2b"
        );
        assert_eq!(
            digest(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "9b752e45573d4b39f4dbd3323cab82bf63326bfb"
        );
    }

    #[test]
    fn handles_every_padding_length_around_a_block() {
        // Computed with Python's hashlib; 55, 56, 63, 64 and 65 bytes straddle the padding
        // boundaries.
        for (len, expected) in [
            (55, "0d8a8c9063a48576a7c97e9f95253a6e53ff6765"),
            (56, "e72334b46c83cc70bef979e15453706c95b888be"),
            (63, "e640041293fe663b9bf3f8c21ffecac03819e6b2"),
            (64, "9dfb7d374ad924f3f88de96291c33e9abed53e32"),
            (65, "99724bb11811e7166af38f671b6a082d8ab4960b"),
            (128, "8dfdfb32b2ed5cb41a73478b4fd60cc5b4648b15"),
        ] {
            assert_eq!(digest(&vec![0x61u8; len]), expected, "{len} bytes");
        }
    }

    #[test]
    fn hash160_of_a_public_key() {
        // The generator point's compressed key; its HASH160 is the well-known
        // 751e76e8199196d454941c45d1b3a323f1433bd6.
        let key = hex::decode("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798")
            .unwrap();
        assert_eq!(
            hex::encode(&hash160(&key)),
            "751e76e8199196d454941c45d1b3a323f1433bd6"
        );
    }
}
