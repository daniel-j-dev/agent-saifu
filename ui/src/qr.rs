//! QR codes as SVG paths, so an address can be scanned from another phone.

use qrcode::{Color, QrCode};

/// The QR code for `text` as `(modules per side, SVG path)` with a one-unit square per dark
/// module. `None` when the text is too long for a QR code.
pub fn qr_path(text: &str) -> Option<(usize, String)> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let mut path = String::new();
    for (index, color) in colors.iter().enumerate() {
        if *color == Color::Dark {
            let (x, y) = (index % width, index / width);
            path.push_str(&format!("M{x} {y}h1v1h-1z"));
        }
    }
    Some((width, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_an_address() {
        let (width, path) = qr_path("0x3c7cb8F6c7e321CDe057F50Aa3D455D9D3C5cc21").unwrap();
        assert!((21..=41).contains(&width), "{width}");
        assert!(path.starts_with('M'));
        // Finder pattern: the top-left module is always dark.
        assert!(path.contains("M0 0h1v1h-1z"));
    }

    #[test]
    fn refuses_text_too_long_for_a_code() {
        assert!(qr_path(&"a".repeat(8000)).is_none());
    }
}
