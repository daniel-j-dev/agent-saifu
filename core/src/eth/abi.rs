//! Contract ABI encoding/decoding for the common non-array types.

use super::address::{Address, parse_address, to_checksum};
use super::u256::U256;
use crate::crypto::{hex, keccak::keccak256};
use crate::json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiType {
    Address,
    Bool,
    Uint(u32),
    Int(u32),
    FixedBytes(usize),
    Bytes,
    String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiValue {
    Address(Address),
    Bool(bool),
    Uint(U256),
    /// Two's-complement 256-bit word.
    Int(U256),
    FixedBytes(Vec<u8>),
    Bytes(Vec<u8>),
    Str(String),
}

impl AbiType {
    pub fn is_dynamic(&self) -> bool {
        matches!(self, AbiType::Bytes | AbiType::String)
    }

    pub fn parse(s: &str) -> Result<AbiType, String> {
        let bits = |prefix: &str| -> Result<u32, String> {
            let rest = &s[prefix.len()..];
            if rest.is_empty() {
                return Ok(256);
            }
            let n: u32 = rest.parse().map_err(|_| format!("invalid type '{s}'"))?;
            if n == 0 || n > 256 || !n.is_multiple_of(8) || rest.starts_with('0') {
                return Err(format!("invalid type '{s}'"));
            }
            Ok(n)
        };
        match s {
            "address" => Ok(AbiType::Address),
            "bool" => Ok(AbiType::Bool),
            "string" => Ok(AbiType::String),
            "bytes" => Ok(AbiType::Bytes),
            _ if s.starts_with("uint") => Ok(AbiType::Uint(bits("uint")?)),
            _ if s.starts_with("int") => Ok(AbiType::Int(bits("int")?)),
            _ if s.starts_with("bytes") => {
                let n: usize = s[5..].parse().map_err(|_| format!("invalid type '{s}'"))?;
                if n == 0 || n > 32 || s[5..].starts_with('0') {
                    return Err(format!("invalid type '{s}'"));
                }
                Ok(AbiType::FixedBytes(n))
            }
            _ => Err(format!("unsupported type '{s}'")),
        }
    }

    pub fn canonical(&self) -> String {
        match self {
            AbiType::Address => "address".into(),
            AbiType::Bool => "bool".into(),
            AbiType::Uint(n) => format!("uint{n}"),
            AbiType::Int(n) => format!("int{n}"),
            AbiType::FixedBytes(n) => format!("bytes{n}"),
            AbiType::Bytes => "bytes".into(),
            AbiType::String => "string".into(),
        }
    }
}

/// Splits `name(type,type)` into the name and parsed types.
pub fn parse_signature(signature: &str) -> Result<(String, Vec<AbiType>), String> {
    let signature = signature.trim();
    let open = signature
        .find('(')
        .ok_or("function signature must look like name(type,...)")?;
    if !signature.ends_with(')') {
        return Err("function signature must end with ')'".to_string());
    }
    let name = &signature[..open];
    let valid_name = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if !valid_name {
        return Err("invalid function name".to_string());
    }
    let types = parse_type_list(&signature[open + 1..signature.len() - 1])?;
    Ok((name.to_string(), types))
}

/// Parses `a,b,c` (optionally wrapped in parentheses) into types. Empty input gives none.
pub fn parse_type_list(list: &str) -> Result<Vec<AbiType>, String> {
    let list = list.trim();
    let list = list
        .strip_prefix('(')
        .and_then(|l| l.strip_suffix(')'))
        .unwrap_or(list);
    if list.trim().is_empty() {
        return Ok(vec![]);
    }
    list.split(',').map(|t| AbiType::parse(t.trim())).collect()
}

pub fn canonical_signature(name: &str, types: &[AbiType]) -> String {
    let inner: Vec<String> = types.iter().map(AbiType::canonical).collect();
    format!("{name}({})", inner.join(","))
}

pub fn selector(canonical_signature: &str) -> [u8; 4] {
    let hash = keccak256(canonical_signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Converts a user-supplied string into a typed value.
pub fn parse_arg(ty: &AbiType, input: &str) -> Result<AbiValue, String> {
    match ty {
        AbiType::Address => parse_address(input).map(AbiValue::Address),
        AbiType::Bool => match input {
            "true" => Ok(AbiValue::Bool(true)),
            "false" => Ok(AbiValue::Bool(false)),
            _ => Err("bool must be 'true' or 'false'".to_string()),
        },
        AbiType::Uint(bits) => {
            let v = parse_uint(input)?;
            if *bits < 256 && v >= pow2(*bits) {
                return Err(format!("value does not fit in uint{bits}"));
            }
            Ok(AbiValue::Uint(v))
        }
        AbiType::Int(bits) => parse_int(input, *bits).map(AbiValue::Int),
        AbiType::FixedBytes(n) => {
            let bytes = hex::decode(input)?;
            if bytes.len() != *n {
                return Err(format!("bytes{n} needs exactly {n} bytes"));
            }
            Ok(AbiValue::FixedBytes(bytes))
        }
        AbiType::Bytes => hex::decode(input).map(AbiValue::Bytes),
        AbiType::String => Ok(AbiValue::Str(input.to_string())),
    }
}

fn parse_uint(input: &str) -> Result<U256, String> {
    if input.starts_with("0x") || input.starts_with("0X") {
        U256::from_hex_quantity(input)
    } else {
        U256::from_dec_str(input)
    }
}

fn pow2(bits: u32) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[31 - (bits / 8) as usize] = 1 << (bits % 8);
    U256::from_be_bytes(&bytes).expect("32 bytes")
}

fn negate(v: &U256) -> U256 {
    // two's complement: (MAX - v) + 1
    U256::MAX
        .checked_sub(v)
        .and_then(|x| x.checked_add(&U256::ONE))
        .unwrap_or(U256::ZERO)
}

fn parse_int(input: &str, bits: u32) -> Result<U256, String> {
    let (negative, digits) = match input.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, input),
    };
    let magnitude = U256::from_dec_str(digits)?;
    let limit = pow2(bits - 1);
    let fits = if negative {
        magnitude <= limit
    } else {
        magnitude < limit
    };
    if !fits {
        return Err(format!("value does not fit in int{bits}"));
    }
    Ok(if negative {
        negate(&magnitude)
    } else {
        magnitude
    })
}

fn is_negative_word(word: &U256) -> bool {
    word.to_be_bytes()[0] & 0x80 != 0
}

fn word_of_bytes_left(bytes: &[u8]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[..bytes.len()].copy_from_slice(bytes);
    w
}

fn pad_right(data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    out.resize(data.len().div_ceil(32) * 32, 0);
    out
}

/// ABI-encodes values (head/tail layout), without a selector.
pub fn encode_values(values: &[AbiValue]) -> Vec<u8> {
    let head_len = values.len() * 32;
    let mut head = Vec::with_capacity(head_len);
    let mut tail: Vec<u8> = Vec::new();
    for value in values {
        match value {
            AbiValue::Address(a) => {
                let mut w = [0u8; 32];
                w[12..].copy_from_slice(a);
                head.extend_from_slice(&w);
            }
            AbiValue::Bool(b) => {
                let mut w = [0u8; 32];
                w[31] = u8::from(*b);
                head.extend_from_slice(&w);
            }
            AbiValue::Uint(v) | AbiValue::Int(v) => head.extend_from_slice(&v.to_be_bytes()),
            AbiValue::FixedBytes(b) => head.extend_from_slice(&word_of_bytes_left(b)),
            AbiValue::Bytes(_) | AbiValue::Str(_) => {
                let data: &[u8] = match value {
                    AbiValue::Bytes(b) => b,
                    AbiValue::Str(s) => s.as_bytes(),
                    _ => unreachable!(),
                };
                let offset = U256::from_u64((head_len + tail.len()) as u64);
                head.extend_from_slice(&offset.to_be_bytes());
                tail.extend_from_slice(&U256::from_u64(data.len() as u64).to_be_bytes());
                tail.extend(pad_right(data));
            }
        }
    }
    head.extend(tail);
    head
}

/// Builds calldata for `signature` with string arguments.
pub fn encode_call(signature: &str, args: &[String]) -> Result<Vec<u8>, String> {
    let (name, types) = parse_signature(signature)?;
    if types.len() != args.len() {
        return Err(format!(
            "{name} expects {} argument(s) but {} were given",
            types.len(),
            args.len()
        ));
    }
    let values: Vec<AbiValue> = types
        .iter()
        .zip(args)
        .enumerate()
        .map(|(i, (t, a))| parse_arg(t, a).map_err(|e| format!("argument {}: {e}", i + 1)))
        .collect::<Result<_, _>>()?;
    let mut data = selector(&canonical_signature(&name, &types)).to_vec();
    data.extend(encode_values(&values));
    Ok(data)
}

fn word_at(data: &[u8], offset: usize) -> Result<[u8; 32], String> {
    let end = offset.checked_add(32).ok_or("ABI offset overflow")?;
    data.get(offset..end)
        .map(|s| <[u8; 32]>::try_from(s).expect("32 bytes"))
        .ok_or_else(|| "ABI data is too short".to_string())
}

fn usize_at(data: &[u8], offset: usize) -> Result<usize, String> {
    let v = U256::from_be_bytes(&word_at(data, offset)?)?;
    v.to_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= data.len())
        .ok_or_else(|| "ABI offset or length is out of range".to_string())
}

fn decode_one(ty: &AbiType, data: &[u8], head_pos: usize) -> Result<AbiValue, String> {
    let word = word_at(data, head_pos)?;
    match ty {
        AbiType::Address => {
            if word[..12].iter().any(|b| *b != 0) {
                return Err("address word has non-zero padding".to_string());
            }
            Ok(AbiValue::Address(word[12..].try_into().expect("20 bytes")))
        }
        AbiType::Bool => match word {
            w if w[..31].iter().all(|b| *b == 0) && w[31] <= 1 => Ok(AbiValue::Bool(w[31] == 1)),
            _ => Err("invalid bool encoding".to_string()),
        },
        AbiType::Uint(_) => Ok(AbiValue::Uint(U256::from_be_bytes(&word)?)),
        AbiType::Int(_) => Ok(AbiValue::Int(U256::from_be_bytes(&word)?)),
        AbiType::FixedBytes(n) => Ok(AbiValue::FixedBytes(word[..*n].to_vec())),
        AbiType::Bytes | AbiType::String => {
            let offset = usize_at(data, head_pos)?;
            let len = usize_at(data, offset)?;
            let start = offset + 32;
            let end = start.checked_add(len).ok_or("ABI length overflow")?;
            let bytes = data
                .get(start..end)
                .ok_or("ABI data is too short")?
                .to_vec();
            if matches!(ty, AbiType::String) {
                String::from_utf8(bytes)
                    .map(AbiValue::Str)
                    .map_err(|_| "string output is not valid UTF-8".to_string())
            } else {
                Ok(AbiValue::Bytes(bytes))
            }
        }
    }
}

pub fn decode_values(types: &[AbiType], data: &[u8]) -> Result<Vec<AbiValue>, String> {
    types
        .iter()
        .enumerate()
        .map(|(i, t)| decode_one(t, data, i * 32))
        .collect()
}

/// Renders a decoded value as JSON (big numbers as decimal strings).
pub fn value_to_json(value: &AbiValue) -> Value {
    match value {
        AbiValue::Address(a) => Value::String(to_checksum(a)),
        AbiValue::Bool(b) => Value::Bool(*b),
        AbiValue::Uint(v) => Value::String(v.to_dec_string()),
        AbiValue::Int(v) => {
            if is_negative_word(v) {
                Value::String(format!("-{}", negate(v).to_dec_string()))
            } else {
                Value::String(v.to_dec_string())
            }
        }
        AbiValue::FixedBytes(b) | AbiValue::Bytes(b) => Value::String(hex::encode_prefixed(b)),
        AbiValue::Str(s) => Value::String(s.clone()),
    }
}

const ERROR_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
const PANIC_SELECTOR: [u8; 4] = [0x4e, 0x48, 0x7b, 0x71];

/// Turns revert data into a readable message, if it is a standard Error/Panic payload.
pub fn decode_revert_reason(data: &[u8]) -> Option<String> {
    if data.len() >= 4
        && data[..4] == ERROR_SELECTOR
        && let Ok(values) = decode_values(&[AbiType::String], &data[4..])
        && let Some(AbiValue::Str(s)) = values.into_iter().next()
    {
        return Some(s);
    }
    if data.len() >= 36 && data[..4] == PANIC_SELECTOR {
        let code = U256::from_be_bytes(&data[4..36]).ok()?.to_u64()?;
        return Some(format!("Panic(0x{code:02x}): {}", panic_meaning(code)));
    }
    None
}

fn panic_meaning(code: u64) -> &'static str {
    match code {
        0x01 => "assertion failed",
        0x11 => "arithmetic overflow or underflow",
        0x12 => "division or modulo by zero",
        0x21 => "invalid enum value",
        0x22 => "invalid storage byte array encoding",
        0x31 => "pop on empty array",
        0x32 => "array index out of bounds",
        0x41 => "out of memory",
        0x51 => "call to zero-initialized function",
        _ => "unknown panic code",
    }
}

// ---- ERC-20 helpers -------------------------------------------------------

pub fn erc20_balance_of(owner: &Address) -> Vec<u8> {
    call_with_address_and_optional_uint("balanceOf(address)", owner, None)
}

pub fn erc20_transfer(to: &Address, amount: &U256) -> Vec<u8> {
    call_with_address_and_optional_uint("transfer(address,uint256)", to, Some(amount))
}

pub fn erc20_approve(spender: &Address, amount: &U256) -> Vec<u8> {
    call_with_address_and_optional_uint("approve(address,uint256)", spender, Some(amount))
}

pub fn erc20_allowance(owner: &Address, spender: &Address) -> Vec<u8> {
    let mut data = selector("allowance(address,address)").to_vec();
    data.extend(encode_values(&[
        AbiValue::Address(*owner),
        AbiValue::Address(*spender),
    ]));
    data
}

pub fn erc20_decimals() -> Vec<u8> {
    selector("decimals()").to_vec()
}

pub fn erc20_symbol() -> Vec<u8> {
    selector("symbol()").to_vec()
}

fn call_with_address_and_optional_uint(
    sig: &str,
    addr: &Address,
    amount: Option<&U256>,
) -> Vec<u8> {
    let mut values = vec![AbiValue::Address(*addr)];
    if let Some(a) = amount {
        values.push(AbiValue::Uint(*a));
    }
    let mut data = selector(sig).to_vec();
    data.extend(encode_values(&values));
    data
}

pub fn decode_uint256(data: &[u8]) -> Result<U256, String> {
    U256::from_be_bytes(&word_at(data, 0)?)
}

/// Decodes a `symbol()`/`name()` result that may be a string or a legacy bytes32.
pub fn decode_string_or_bytes32(data: &[u8]) -> Result<String, String> {
    if data.len() == 32 {
        let trimmed: Vec<u8> = data.iter().copied().take_while(|b| *b != 0).collect();
        return String::from_utf8(trimmed).map_err(|_| "token text is not valid UTF-8".to_string());
    }
    match decode_values(&[AbiType::String], data)?.into_iter().next() {
        Some(AbiValue::Str(s)) => Ok(s),
        _ => Err("could not decode token text".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        [byte; 20]
    }

    #[test]
    fn well_known_selectors() {
        assert_eq!(
            hex::encode(&selector("transfer(address,uint256)")),
            "a9059cbb"
        );
        assert_eq!(
            hex::encode(&selector("approve(address,uint256)")),
            "095ea7b3"
        );
        assert_eq!(hex::encode(&selector("balanceOf(address)")), "70a08231");
        assert_eq!(hex::encode(&selector("decimals()")), "313ce567");
        assert_eq!(hex::encode(&selector("symbol()")), "95d89b41");
        assert_eq!(
            hex::encode(&selector("allowance(address,address)")),
            "dd62ed3e"
        );
    }

    #[test]
    fn type_parsing() {
        assert_eq!(AbiType::parse("uint"), Ok(AbiType::Uint(256)));
        assert_eq!(AbiType::parse("uint8"), Ok(AbiType::Uint(8)));
        assert_eq!(AbiType::parse("int128"), Ok(AbiType::Int(128)));
        assert_eq!(AbiType::parse("bytes32"), Ok(AbiType::FixedBytes(32)));
        assert_eq!(AbiType::parse("bytes"), Ok(AbiType::Bytes));
        for bad in [
            "uint7",
            "uint0",
            "uint264",
            "bytes0",
            "bytes33",
            "uint08",
            "float",
            "uint256[]",
            "",
            "tuple",
        ] {
            assert!(AbiType::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn signature_parsing() {
        let (name, types) = parse_signature("transfer(address, uint256)").unwrap();
        assert_eq!(name, "transfer");
        assert_eq!(
            canonical_signature(&name, &types),
            "transfer(address,uint256)"
        );
        assert_eq!(parse_signature("decimals()").unwrap().1, vec![]);
        assert_eq!(
            canonical_signature("f", &parse_signature("f(uint,int)").unwrap().1),
            "f(uint256,int256)"
        );
        for bad in [
            "transfer",
            "transfer(address",
            "1abc()",
            "(address)",
            "a-b(uint256)",
            "",
        ] {
            assert!(parse_signature(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn type_list_parsing() {
        assert_eq!(parse_type_list("").unwrap(), vec![]);
        assert_eq!(parse_type_list("()").unwrap(), vec![]);
        assert_eq!(
            parse_type_list("(string, uint8)").unwrap(),
            vec![AbiType::String, AbiType::Uint(8)]
        );
        assert!(parse_type_list("uint256,").is_err());
    }

    #[test]
    fn erc20_transfer_calldata() {
        let data = erc20_transfer(&addr(0x11), &U256::from_u64(1_000_000));
        assert_eq!(data.len(), 4 + 64);
        assert_eq!(
            hex::encode(&data),
            "a9059cbb\
             0000000000000000000000001111111111111111111111111111111111111111\
             00000000000000000000000000000000000000000000000000000000000f4240"
        );
    }

    #[test]
    fn encode_call_from_strings() {
        let a = to_checksum(&addr(0x22));
        let data = encode_call("transfer(address,uint256)", &[a, "5".to_string()]).unwrap();
        assert_eq!(data, erc20_transfer(&addr(0x22), &U256::from_u64(5)));
        assert!(encode_call("transfer(address,uint256)", &["x".to_string()]).is_err());
        assert!(
            encode_call(
                "transfer(address,uint256)",
                &["0x1".to_string(), "5".to_string()]
            )
            .is_err()
        );
    }

    #[test]
    fn dynamic_encoding_matches_spec_example() {
        // From the Solidity ABI spec: f(string) with "Hello, world!"
        let enc = encode_values(&[AbiValue::Str("Hello, world!".to_string())]);
        assert_eq!(
            hex::encode(&enc),
            "0000000000000000000000000000000000000000000000000000000000000020\
             000000000000000000000000000000000000000000000000000000000000000d\
             48656c6c6f2c20776f726c642100000000000000000000000000000000000000"
        );
    }

    #[test]
    fn mixed_static_and_dynamic_round_trip() {
        let values = vec![
            AbiValue::Uint(U256::from_u64(7)),
            AbiValue::Str("héllo".to_string()),
            AbiValue::Bool(true),
            AbiValue::Bytes(vec![1, 2, 3]),
            AbiValue::Address(addr(0xaa)),
            AbiValue::FixedBytes(vec![9; 4]),
        ];
        let types = [
            AbiType::Uint(256),
            AbiType::String,
            AbiType::Bool,
            AbiType::Bytes,
            AbiType::Address,
            AbiType::FixedBytes(4),
        ];
        let enc = encode_values(&values);
        assert_eq!(decode_values(&types, &enc).unwrap(), values);
    }

    #[test]
    fn argument_range_checks() {
        assert!(parse_arg(&AbiType::Uint(8), "255").is_ok());
        assert!(parse_arg(&AbiType::Uint(8), "256").is_err());
        assert!(parse_arg(&AbiType::Uint(256), "-1").is_err());
        assert!(parse_arg(&AbiType::Uint(256), "0x10").is_ok());
        assert!(parse_arg(&AbiType::Int(8), "127").is_ok());
        assert!(parse_arg(&AbiType::Int(8), "128").is_err());
        assert!(parse_arg(&AbiType::Int(8), "-128").is_ok());
        assert!(parse_arg(&AbiType::Int(8), "-129").is_err());
        assert!(parse_arg(&AbiType::Bool, "yes").is_err());
        assert!(parse_arg(&AbiType::FixedBytes(2), "0x01").is_err());
        assert!(parse_arg(&AbiType::Bytes, "0xzz").is_err());
        assert_eq!(
            parse_arg(&AbiType::Bytes, "0x").unwrap(),
            AbiValue::Bytes(vec![])
        );
    }

    #[test]
    fn signed_integers() {
        let minus_one = parse_arg(&AbiType::Int(256), "-1").unwrap();
        assert_eq!(minus_one, AbiValue::Int(U256::MAX));
        assert_eq!(value_to_json(&minus_one), Value::str("-1"));
        let enc = encode_values(&[parse_arg(&AbiType::Int(256), "-2").unwrap()]);
        let dec = decode_values(&[AbiType::Int(256)], &enc).unwrap();
        assert_eq!(value_to_json(&dec[0]), Value::str("-2"));
        assert_eq!(
            value_to_json(&AbiValue::Int(U256::from_u64(5))),
            Value::str("5")
        );
    }

    #[test]
    fn decoding_is_bounds_checked() {
        assert!(decode_values(&[AbiType::Uint(256)], &[0u8; 31]).is_err());
        let mut evil = vec![0u8; 64];
        evil[31] = 0xff; // offset beyond data
        assert!(decode_values(&[AbiType::String], &evil).is_err());
        let mut huge_len = encode_values(&[AbiValue::Str("a".into())]);
        huge_len[63] = 0xff;
        assert!(decode_values(&[AbiType::String], &huge_len).is_err());
        let mut dirty_addr = vec![0u8; 32];
        dirty_addr[0] = 1;
        assert!(decode_values(&[AbiType::Address], &dirty_addr).is_err());
        let mut bad_bool = vec![0u8; 32];
        bad_bool[31] = 2;
        assert!(decode_values(&[AbiType::Bool], &bad_bool).is_err());
        assert!(decode_values(&[], &[]).unwrap().is_empty());
    }

    #[test]
    fn json_rendering() {
        assert_eq!(
            value_to_json(&AbiValue::Uint(U256::MAX)).as_str(),
            Some(U256::MAX.to_dec_string().as_str())
        );
        assert_eq!(
            value_to_json(&AbiValue::Bytes(vec![0xab])),
            Value::str("0xab")
        );
        assert_eq!(value_to_json(&AbiValue::Bool(false)), Value::Bool(false));
        assert_eq!(
            value_to_json(&AbiValue::Address([0xde; 20]))
                .as_str()
                .map(|s| s.len()),
            Some(42)
        );
    }

    #[test]
    fn revert_reasons() {
        let mut data = ERROR_SELECTOR.to_vec();
        data.extend(encode_values(&[AbiValue::Str(
            "insufficient balance".into(),
        )]));
        assert_eq!(
            decode_revert_reason(&data),
            Some("insufficient balance".to_string())
        );
        let mut panic = PANIC_SELECTOR.to_vec();
        panic.extend(U256::from_u64(0x11).to_be_bytes());
        assert!(decode_revert_reason(&panic).unwrap().contains("overflow"));
        assert_eq!(decode_revert_reason(&[]), None);
        assert_eq!(decode_revert_reason(&[1, 2, 3, 4]), None);
        assert_eq!(decode_revert_reason(&ERROR_SELECTOR), None);
    }

    #[test]
    fn string_or_bytes32_tokens() {
        let dynamic = encode_values(&[AbiValue::Str("USDC".into())]);
        assert_eq!(decode_string_or_bytes32(&dynamic).unwrap(), "USDC");
        let mut legacy = [0u8; 32];
        legacy[..3].copy_from_slice(b"MKR");
        assert_eq!(decode_string_or_bytes32(&legacy).unwrap(), "MKR");
        assert!(decode_string_or_bytes32(&[]).is_err());
    }

    #[test]
    fn uint256_decoding() {
        assert_eq!(
            decode_uint256(&U256::from_u64(9).to_be_bytes()).unwrap(),
            U256::from_u64(9)
        );
        assert!(decode_uint256(&[1, 2]).is_err());
    }

    #[test]
    fn balance_of_and_approve_layouts() {
        assert_eq!(erc20_balance_of(&addr(1)).len(), 36);
        let approve = erc20_approve(&addr(1), &U256::ZERO);
        assert_eq!(&approve[..4], &selector("approve(address,uint256)"));
        assert!(approve[36..].iter().all(|b| *b == 0));
        assert_eq!(erc20_allowance(&addr(1), &addr(2)).len(), 68);
        assert_eq!(erc20_decimals(), selector("decimals()").to_vec());
        assert_eq!(erc20_symbol(), selector("symbol()").to_vec());
    }
}
