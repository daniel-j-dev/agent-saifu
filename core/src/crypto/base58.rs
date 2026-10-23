//! Base58 (Bitcoin alphabet), encode only: used for `did:key` identifiers.

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn encode(data: &[u8]) -> String {
    let zeros = data.iter().take_while(|b| **b == 0).count();
    let mut digits: Vec<u8> = vec![];
    for &byte in &data[zeros..] {
        let mut carry = u32::from(byte);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = String::with_capacity(zeros + digits.len());
    out.extend(std::iter::repeat_n('1', zeros));
    out.extend(digits.iter().rev().map(|d| ALPHABET[*d as usize] as char));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"hello world"), "StV1DL6CwTryKyV");
        assert_eq!(encode(&[0, 0, 0x28, 0x7f, 0xb4, 0xcd]), "11233QC4");
        assert_eq!(encode(&[0]), "1");
        assert_eq!(encode(&[0x61]), "2g");
    }
}
