//! Writer for ZIP archives with "stored" (uncompressed) entries.

use crate::crypto::hex;

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;
const END_SIG: u32 = 0x0605_4b50;
/// Fixed timestamp (1980-01-01 00:00) so archives do not leak creation time.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = 0x0021;

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn valid_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.is_ascii()
        && !name.starts_with('/')
        && !name.contains("..")
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control())
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Builds a ZIP file from `(name, contents)` entries.
pub fn build_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    if entries.is_empty() || entries.len() > usize::from(u16::MAX) {
        return Err("invalid number of zip entries".to_string());
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        if !valid_entry_name(name) {
            return Err(format!(
                "invalid zip entry name '{}'",
                hex::encode(name.as_bytes())
            ));
        }
        let size = u32::try_from(data.len()).map_err(|_| "zip entry too large".to_string())?;
        let offset = u32::try_from(out.len()).map_err(|_| "zip archive too large".to_string())?;
        let crc = crc32(data);

        put32(&mut out, LOCAL_HEADER_SIG);
        put16(&mut out, 20);
        put16(&mut out, 0);
        put16(&mut out, 0);
        put16(&mut out, DOS_TIME);
        put16(&mut out, DOS_DATE);
        put32(&mut out, crc);
        put32(&mut out, size);
        put32(&mut out, size);
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        put32(&mut central, CENTRAL_HEADER_SIG);
        put16(&mut central, 20);
        put16(&mut central, 20);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, DOS_TIME);
        put16(&mut central, DOS_DATE);
        put32(&mut central, crc);
        put32(&mut central, size);
        put32(&mut central, size);
        put16(&mut central, name.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset =
        u32::try_from(out.len()).map_err(|_| "zip archive too large".to_string())?;
    let central_size =
        u32::try_from(central.len()).map_err(|_| "zip archive too large".to_string())?;
    out.extend(central);
    put32(&mut out, END_SIG);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, entries.len() as u16);
    put16(&mut out, entries.len() as u16);
    put32(&mut out, central_size);
    put32(&mut out, central_offset);
    put16(&mut out, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414f_a339
        );
    }

    #[test]
    fn archive_structure() {
        let zip = build_zip(&[("a.txt", b"hello"), ("b.txt", b"world!")]).unwrap();
        assert_eq!(&zip[..4], b"PK\x03\x04");
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([zip[end + 10], zip[end + 11]]), 2);
        let central_offset =
            u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
        assert_eq!(&zip[central_offset..central_offset + 4], b"PK\x01\x02");
        assert!(zip.windows(5).any(|w| w == b"hello"));
    }

    #[test]
    fn rejects_unsafe_names_and_empty_archives() {
        assert!(build_zip(&[]).is_err());
        for bad in [
            "",
            "../x",
            "/abs",
            "a\\b",
            "bad\nname",
            "ünï.txt",
            &"a".repeat(256),
        ] {
            assert!(build_zip(&[(bad, b"x")]).is_err(), "{bad}");
        }
    }

    #[test]
    fn output_is_deterministic() {
        let a = build_zip(&[("x.json", b"{}")]).unwrap();
        let b = build_zip(&[("x.json", b"{}")]).unwrap();
        assert_eq!(a, b);
    }
}
