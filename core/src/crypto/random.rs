//! Operating-system randomness.

pub fn fill(buf: &mut [u8]) -> Result<(), String> {
    getrandom::fill(buf).map_err(|e| format!("random number generator failure: {e}"))
}

pub fn random_bytes(len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len];
    fill(&mut buf)?;
    Ok(buf)
}

pub fn random_array<const N: usize>() -> Result<[u8; N], String> {
    let mut buf = [0u8; N];
    fill(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_requested_length() {
        assert_eq!(random_bytes(0).unwrap().len(), 0);
        assert_eq!(random_bytes(33).unwrap().len(), 33);
    }

    #[test]
    fn successive_outputs_differ() {
        assert_ne!(random_array::<32>().unwrap(), random_array::<32>().unwrap());
    }

    #[test]
    fn output_is_not_all_zero() {
        assert!(random_array::<32>().unwrap().iter().any(|b| *b != 0));
    }
}
