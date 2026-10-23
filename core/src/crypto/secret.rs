//! Containers that wipe their memory on drop.

use std::fmt;
use std::sync::atomic::{Ordering, compiler_fence};

/// Overwrites `buf` with zeros in a way the optimizer may not remove.
pub fn zeroize(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference to a u8.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Wipes the heap contents of a `String` and clears it.
pub fn zeroize_string(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8, so the string stays well-formed.
    zeroize(unsafe { s.as_bytes_mut() });
    s.clear();
}

/// Byte buffer that is zeroed on drop and never printed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        SecretBytes(bytes)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        zeroize(&mut self.0);
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes(<redacted>)")
    }
}

/// String that is zeroed on drop and never printed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: String) -> Self {
        SecretString(s)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        zeroize_string(&mut self.0);
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

/// Compares two byte slices without early exit on the first difference.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroize_clears_buffer() {
        let mut buf = vec![1u8, 2, 3, 4];
        zeroize(&mut buf);
        assert_eq!(buf, vec![0, 0, 0, 0]);
    }

    #[test]
    fn zeroize_string_clears_and_empties() {
        let mut s = String::from("seed words");
        zeroize_string(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn debug_never_leaks() {
        let b = SecretBytes::new(vec![0xde, 0xad]);
        let s = SecretString::new("hunter2".to_string());
        assert!(!format!("{b:?}").contains("222"));
        assert!(!format!("{s:?}").contains("hunter2"));
        assert!(format!("{s:?}").contains("redacted"));
    }

    #[test]
    fn expose_returns_contents() {
        assert_eq!(SecretBytes::new(vec![1, 2]).expose(), &[1, 2]);
        assert_eq!(SecretBytes::new(vec![1, 2]).len(), 2);
        assert_eq!(SecretString::new("x".into()).expose(), "x");
    }

    #[test]
    fn constant_time_eq_behaves() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
