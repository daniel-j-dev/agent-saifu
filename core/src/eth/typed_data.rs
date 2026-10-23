//! EIP-712 typed structured data hashing (`eth_signTypedData_v4`).

use crate::crypto::hex;
use crate::crypto::keccak::keccak256;
use crate::eth::address::parse_address;
use crate::json::{self, Value};

pub const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_ARRAY_LEN: usize = 2048;
const MAX_TYPES: usize = 64;
const MAX_MEMBERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone)]
pub struct TypedData {
    pub types: Vec<(String, Vec<Member>)>,
    pub primary_type: String,
    pub domain: Value,
    pub message: Value,
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && s.len() <= 64
}

// Hyperliquid uses names such as `Hyperliquid:AcceptTerms`. Keep the exact
// name for hashing, but still exclude encodeType delimiters and whitespace.
fn is_type_name(s: &str) -> bool {
    s.len() <= 64 && s.split(':').all(is_identifier)
}

/// Parses the typed-data JSON, given as a string or an already-parsed object.
pub fn parse(input: &Value) -> Result<TypedData, String> {
    let owned;
    let doc = match input {
        Value::String(s) => {
            if s.len() > MAX_INPUT_BYTES {
                return Err("typed data is too large".to_string());
            }
            owned = json::parse(s).map_err(|e| format!("typed data is not valid JSON: {e}"))?;
            &owned
        }
        other => other,
    };
    let types_json = match doc.get("types") {
        Some(Value::Object(pairs)) => pairs,
        _ => return Err("typed data has no 'types'".to_string()),
    };
    if types_json.len() > MAX_TYPES {
        return Err("typed data has too many types".to_string());
    }
    let mut types = vec![];
    for (name, members) in types_json {
        if !is_type_name(name) {
            return Err(format!("invalid type name '{name}'"));
        }
        let list = members
            .as_array()
            .ok_or_else(|| format!("type '{name}' must be a list of members"))?;
        if list.len() > MAX_MEMBERS {
            return Err(format!("type '{name}' has too many members"));
        }
        let mut parsed = vec![];
        for m in list {
            let member_name = m
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("type '{name}' has a member without a name"))?;
            let ty = m
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("member '{member_name}' has no type"))?;
            if !is_identifier(member_name) {
                return Err(format!("invalid member name '{member_name}'"));
            }
            parsed.push(Member {
                name: member_name.to_string(),
                ty: ty.to_string(),
            });
        }
        types.push((name.clone(), parsed));
    }
    let primary_type = doc
        .get("primaryType")
        .and_then(Value::as_str)
        .ok_or("typed data has no 'primaryType'")?
        .to_string();
    let domain = doc
        .get("domain")
        .cloned()
        .ok_or("typed data has no 'domain'")?;
    let message = doc
        .get("message")
        .cloned()
        .ok_or("typed data has no 'message'")?;
    if !matches!(domain, Value::Object(_)) || !matches!(message, Value::Object(_)) {
        return Err("'domain' and 'message' must be objects".to_string());
    }
    let mut data = TypedData {
        types,
        primary_type,
        domain,
        message,
    };
    data.ensure_domain_type();
    Ok(data)
}

fn domain_members(domain: &Value) -> Vec<Member> {
    let known = [
        ("name", "string"),
        ("version", "string"),
        ("chainId", "uint256"),
        ("verifyingContract", "address"),
        ("salt", "bytes32"),
    ];
    known
        .iter()
        .filter(|(name, _)| domain.get(name).is_some())
        .map(|(name, ty)| Member {
            name: name.to_string(),
            ty: ty.to_string(),
        })
        .collect()
}

impl TypedData {
    fn ensure_domain_type(&mut self) {
        if self.members("EIP712Domain").is_none() {
            let members = domain_members(&self.domain);
            self.types.push(("EIP712Domain".to_string(), members));
        }
    }

    fn members(&self, name: &str) -> Option<&Vec<Member>> {
        self.types.iter().find(|(n, _)| n == name).map(|(_, m)| m)
    }

    /// The chain the domain binds the signature to, if it names one.
    pub fn domain_chain_id(&self) -> Result<Option<u64>, String> {
        match self.domain.get("chainId") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match parse_int(v) {
                Ok((false, magnitude)) => magnitude_to_u64(&magnitude)
                    .map(Some)
                    .ok_or_else(|| "domain chainId is too large".to_string()),
                _ => Err("domain chainId is not a valid number".to_string()),
            },
        }
    }

    /// `encodeType` for a struct: the type followed by every referenced type, sorted by name.
    fn encode_type(&self, name: &str) -> Result<String, String> {
        let mut deps: Vec<String> = vec![];
        self.collect_dependencies(name, &mut deps, 0)?;
        deps.retain(|d| d != name);
        deps.sort();
        let mut out = String::new();
        for ty in std::iter::once(name.to_string()).chain(deps) {
            let members = self
                .members(&ty)
                .ok_or_else(|| format!("type '{ty}' is not defined"))?;
            out.push_str(&ty);
            out.push('(');
            let parts: Vec<String> = members
                .iter()
                .map(|m| format!("{} {}", m.ty, m.name))
                .collect();
            out.push_str(&parts.join(","));
            out.push(')');
        }
        Ok(out)
    }

    fn collect_dependencies(
        &self,
        name: &str,
        found: &mut Vec<String>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("typed data types are nested too deeply".to_string());
        }
        if found.iter().any(|f| f == name) {
            return Ok(());
        }
        let Some(members) = self.members(name) else {
            return Ok(());
        };
        found.push(name.to_string());
        for m in members {
            let base = base_type(&m.ty);
            if self.members(base).is_some() {
                self.collect_dependencies(base, found, depth + 1)?;
            }
        }
        Ok(())
    }

    fn type_hash(&self, name: &str) -> Result<[u8; 32], String> {
        Ok(keccak256(self.encode_type(name)?.as_bytes()))
    }

    pub fn hash_struct(&self, name: &str, data: &Value) -> Result<[u8; 32], String> {
        self.hash_struct_at(name, data, 0)
    }

    fn hash_struct_at(&self, name: &str, data: &Value, depth: usize) -> Result<[u8; 32], String> {
        if depth > MAX_DEPTH {
            return Err("typed data is nested too deeply".to_string());
        }
        let members = self
            .members(name)
            .ok_or_else(|| format!("type '{name}' is not defined"))?;
        let mut encoded = self.type_hash(name)?.to_vec();
        for m in members {
            let value = data
                .get(&m.name)
                .ok_or_else(|| format!("'{name}.{}' is missing from the message", m.name))?;
            let word = self
                .encode_value(&m.ty, value, depth + 1)
                .map_err(|e| format!("{name}.{}: {e}", m.name))?;
            encoded.extend_from_slice(&word);
        }
        Ok(keccak256(&encoded))
    }

    fn encode_value(&self, ty: &str, value: &Value, depth: usize) -> Result<[u8; 32], String> {
        if depth > MAX_DEPTH {
            return Err("typed data is nested too deeply".to_string());
        }
        if let Some((element, len)) = split_array(ty)? {
            let items = value
                .as_array()
                .ok_or_else(|| format!("expected an array for {ty}"))?;
            if items.len() > MAX_ARRAY_LEN {
                return Err("array is too long".to_string());
            }
            if let Some(n) = len
                && items.len() != n
            {
                return Err(format!("expected {n} items for {ty}"));
            }
            let mut joined = Vec::with_capacity(items.len() * 32);
            for item in items {
                joined.extend_from_slice(&self.encode_value(element, item, depth + 1)?);
            }
            return Ok(keccak256(&joined));
        }
        if self.members(ty).is_some() {
            return match value {
                Value::Null => Ok([0u8; 32]),
                Value::Object(_) => self.hash_struct_at(ty, value, depth + 1),
                _ => Err(format!("expected an object for {ty}")),
            };
        }
        encode_atomic(ty, value)
    }

    /// The 32-byte digest that gets signed.
    pub fn signing_hash(&self) -> Result<[u8; 32], String> {
        let domain_hash = self.hash_struct("EIP712Domain", &self.domain)?;
        let mut data = vec![0x19, 0x01];
        data.extend_from_slice(&domain_hash);
        if self.primary_type != "EIP712Domain" {
            if self.members(&self.primary_type).is_none() {
                return Err(format!(
                    "primaryType '{}' is not defined",
                    self.primary_type
                ));
            }
            data.extend_from_slice(&self.hash_struct(&self.primary_type, &self.message)?);
        }
        Ok(keccak256(&data))
    }
}

fn base_type(ty: &str) -> &str {
    ty.split('[').next().unwrap_or(ty)
}

/// Splits `T[]` or `T[n]` into the element type and optional fixed length.
fn split_array(ty: &str) -> Result<Option<(&str, Option<usize>)>, String> {
    let Some(stripped) = ty.strip_suffix(']') else {
        return Ok(None);
    };
    let open = stripped
        .rfind('[')
        .ok_or_else(|| format!("malformed type '{ty}'"))?;
    let (element, len) = (&stripped[..open], &stripped[open + 1..]);
    if element.is_empty() {
        return Err(format!("malformed type '{ty}'"));
    }
    if len.is_empty() {
        return Ok(Some((element, None)));
    }
    let n = len
        .parse::<usize>()
        .map_err(|_| format!("malformed array length in '{ty}'"))?;
    Ok(Some((element, Some(n))))
}

fn encode_atomic(ty: &str, value: &Value) -> Result<[u8; 32], String> {
    match ty {
        "bool" => {
            let flag = match value {
                Value::Bool(b) => *b,
                Value::Number(n) if n == "0" || n == "1" => n == "1",
                Value::String(s) if s == "true" || s == "false" => s == "true",
                _ => return Err("expected a boolean".to_string()),
            };
            let mut word = [0u8; 32];
            word[31] = u8::from(flag);
            Ok(word)
        }
        "address" => {
            let s = value.as_str().ok_or("expected an address string")?;
            let address = parse_address(s)?;
            let mut word = [0u8; 32];
            word[12..].copy_from_slice(&address);
            Ok(word)
        }
        "string" => {
            let s = value.as_str().ok_or("expected a string")?;
            Ok(keccak256(s.as_bytes()))
        }
        "bytes" => {
            let s = value.as_str().ok_or("expected a hex string")?;
            Ok(keccak256(&hex::decode(s)?))
        }
        _ if ty.starts_with("bytes") => {
            let n: usize = ty[5..]
                .parse()
                .map_err(|_| format!("unknown type '{ty}'"))?;
            if !(1..=32).contains(&n) {
                return Err(format!("unknown type '{ty}'"));
            }
            let s = value.as_str().ok_or("expected a hex string")?;
            let bytes = hex::decode(s)?;
            if bytes.len() > n {
                return Err(format!("value is longer than {ty}"));
            }
            let mut word = [0u8; 32];
            word[..bytes.len()].copy_from_slice(&bytes);
            Ok(word)
        }
        _ if ty.starts_with("uint") || ty.starts_with("int") => encode_integer(ty, value),
        _ => Err(format!("unknown type '{ty}'")),
    }
}

/// Reads a number given as a JSON number, decimal string or `0x` hex string.
/// Returns (negative, 32-byte magnitude).
fn parse_int(value: &Value) -> Result<(bool, [u8; 32]), String> {
    let text = match value {
        Value::Number(n) => n.as_str(),
        Value::String(s) => s.as_str(),
        _ => return Err("expected a number".to_string()),
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let magnitude = if digits.starts_with("0x") || digits.starts_with("0X") {
        let bytes = hex::decode(digits)?;
        if bytes.len() > 32 {
            return Err("number is too large".to_string());
        }
        let mut word = [0u8; 32];
        word[32 - bytes.len()..].copy_from_slice(&bytes);
        word
    } else {
        crate::eth::u256::U256::from_dec_str(digits)?.to_be_bytes()
    };
    Ok((negative, magnitude))
}

fn magnitude_to_u64(magnitude: &[u8; 32]) -> Option<u64> {
    if magnitude[..24].iter().any(|b| *b != 0) {
        return None;
    }
    Some(u64::from_be_bytes(magnitude[24..].try_into().ok()?))
}

fn encode_integer(ty: &str, value: &Value) -> Result<[u8; 32], String> {
    let (signed, bits_text) = match ty.strip_prefix("uint") {
        Some(rest) => (false, rest),
        None => (true, &ty[3..]),
    };
    let bits: usize = if bits_text.is_empty() {
        256
    } else {
        bits_text
            .parse()
            .map_err(|_| format!("unknown type '{ty}'"))?
    };
    if bits == 0 || bits > 256 || !bits.is_multiple_of(8) {
        return Err(format!("unknown type '{ty}'"));
    }
    let (negative, magnitude) = parse_int(value)?;
    let window_start = 32 - bits / 8;
    if magnitude[..window_start].iter().any(|b| *b != 0) {
        return Err(format!("value does not fit in {ty}"));
    }
    if !signed {
        if negative && magnitude.iter().any(|b| *b != 0) {
            return Err(format!("{ty} cannot be negative"));
        }
        return Ok(magnitude);
    }
    let top_bit_set = magnitude[window_start] & 0x80 != 0;
    let rest_is_zero = magnitude[window_start] & 0x7f == 0
        && magnitude[window_start + 1..].iter().all(|b| *b == 0);
    if negative {
        if top_bit_set && !rest_is_zero {
            return Err(format!("value does not fit in {ty}"));
        }
        Ok(twos_complement(&magnitude))
    } else {
        if top_bit_set {
            return Err(format!("value does not fit in {ty}"));
        }
        Ok(magnitude)
    }
}

fn twos_complement(magnitude: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut carry = true;
    for i in (0..32).rev() {
        let (sum, overflow) = (!magnitude[i]).overflowing_add(u8::from(carry));
        out[i] = sum;
        carry = overflow;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = r#"{
      "types": {
        "EIP712Domain": [
          {"name":"name","type":"string"},
          {"name":"version","type":"string"},
          {"name":"chainId","type":"uint256"},
          {"name":"verifyingContract","type":"address"}
        ],
        "Person": [{"name":"name","type":"string"},{"name":"wallet","type":"address"}],
        "Mail": [
          {"name":"from","type":"Person"},
          {"name":"to","type":"Person"},
          {"name":"contents","type":"string"}
        ]
      },
      "primaryType": "Mail",
      "domain": {
        "name": "Ether Mail", "version": "1", "chainId": 1,
        "verifyingContract": "0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC"
      },
      "message": {
        "from": {"name":"Cow","wallet":"0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826"},
        "to": {"name":"Bob","wallet":"0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB"},
        "contents": "Hello, Bob!"
      }
    }"#;

    fn mail() -> TypedData {
        parse(&Value::str(MAIL)).unwrap()
    }

    #[test]
    fn eip712_example_vectors() {
        let data = mail();
        assert_eq!(
            data.encode_type("Mail").unwrap(),
            "Mail(Person from,Person to,string contents)Person(string name,address wallet)"
        );
        assert_eq!(
            hex::encode_prefixed(&data.hash_struct("EIP712Domain", &data.domain).unwrap()),
            "0xf2cee375fa42b42143804025fc449deafd50cc031ca257e0b194a650a912090f"
        );
        assert_eq!(
            hex::encode_prefixed(&data.hash_struct("Mail", &data.message).unwrap()),
            "0xc52c0ee5d84264471806290a3f2c4cecfc5490626bf912d01f240d7a274b371e"
        );
        assert_eq!(
            hex::encode_prefixed(&data.signing_hash().unwrap()),
            "0xbe609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2"
        );
    }

    #[test]
    fn accepts_an_already_parsed_object() {
        let value = json::parse(MAIL).unwrap();
        assert_eq!(
            parse(&value).unwrap().signing_hash().unwrap(),
            mail().signing_hash().unwrap()
        );
    }

    #[test]
    fn namespaced_type_names_are_preserved_in_hashes_and_dependencies() {
        let doc = MAIL
            .replace("Mail", "Hyperliquid:AcceptTerms")
            .replace("Person", "Hyperliquid:Person");
        let data = parse(&Value::str(&doc)).unwrap();
        assert_eq!(
            data.encode_type("Hyperliquid:AcceptTerms").unwrap(),
            "Hyperliquid:AcceptTerms(Hyperliquid:Person from,Hyperliquid:Person to,string contents)Hyperliquid:Person(string name,address wallet)"
        );
        let digest = data.signing_hash().unwrap();
        assert_ne!(digest, mail().signing_hash().unwrap());
        let renamed = parse(&Value::str(&doc.replace("Hyperliquid:", "Hyperliquid"))).unwrap();
        assert_ne!(digest, renamed.signing_hash().unwrap());
    }

    #[test]
    fn namespaced_type_names_remain_bounded_and_unambiguous() {
        for name in [
            "", ":T", "T:", "T::U", "T:1U", "T U", "T(U)", "T,U", "T[]", "T\nU", "T:é",
        ] {
            assert!(!is_type_name(name), "{name:?}");
        }
        assert!(is_type_name(&format!("T:{}", "x".repeat(62))));
        assert!(!is_type_name(&format!("T:{}", "x".repeat(63))));
        let bad_member = MAIL.replace("contents", "namespace:contents");
        assert!(
            parse(&Value::str(&bad_member))
                .unwrap_err()
                .contains("invalid member name")
        );
    }

    #[test]
    fn domain_type_is_derived_when_missing() {
        let doc = r#"{"types":{"T":[{"name":"a","type":"uint256"}]},"primaryType":"T",
          "domain":{"name":"x","chainId":5},"message":{"a":"7"}}"#;
        let data = parse(&Value::str(doc)).unwrap();
        assert_eq!(
            data.encode_type("EIP712Domain").unwrap(),
            "EIP712Domain(string name,uint256 chainId)"
        );
        assert!(data.signing_hash().is_ok());
        assert_eq!(data.domain_chain_id().unwrap(), Some(5));
    }

    #[test]
    fn domain_chain_id_accepts_strings_and_hex() {
        let doc = |chain: &str| {
            format!(
                r#"{{"types":{{"T":[]}},"primaryType":"T","domain":{{"chainId":{chain}}},"message":{{}}}}"#
            )
        };
        let id = |chain: &str| parse(&Value::String(doc(chain))).unwrap().domain_chain_id();
        assert_eq!(id("\"137\""), Ok(Some(137)));
        assert_eq!(id("\"0x89\""), Ok(Some(137)));
        assert!(id("\"abc\"").is_err());
        let none = parse(&Value::String(
            r#"{"types":{"T":[]},"primaryType":"T","domain":{},"message":{}}"#.into(),
        ))
        .unwrap();
        assert_eq!(none.domain_chain_id(), Ok(None));
    }

    fn word(ty: &str, v: &str) -> Result<String, String> {
        let value = json::parse(v).unwrap();
        encode_atomic(ty, &value).map(|w| hex::encode(&w))
    }

    #[test]
    fn atomic_encoding() {
        assert_eq!(word("bool", "true").unwrap(), format!("{:0>64}", "1"));
        assert_eq!(
            word("uint256", "\"255\"").unwrap(),
            format!("{:0>64}", "ff")
        );
        assert_eq!(word("uint8", "255").unwrap(), format!("{:0>64}", "ff"));
        assert_eq!(word("uint", "\"0x10\"").unwrap(), format!("{:0>64}", "10"));
        assert_eq!(
            word("bytes4", "\"0xdeadbeef\"").unwrap(),
            format!("deadbeef{:0<56}", "")
        );
        assert_eq!(
            word("bytes2", "\"0xab\"").unwrap(),
            format!("ab{:0<62}", "")
        );
        assert_eq!(
            word("address", "\"0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB\"").unwrap(),
            format!("{:0>24}{}", "", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn signed_integers_use_twos_complement() {
        assert_eq!(word("int256", "\"-1\"").unwrap(), "f".repeat(64));
        assert_eq!(
            word("int8", "-128").unwrap(),
            format!("{}80", "f".repeat(62))
        );
        assert_eq!(word("int8", "127").unwrap(), format!("{:0>64}", "7f"));
        assert!(word("int8", "128").is_err());
        assert!(word("int8", "-129").is_err());
        assert_eq!(word("int256", "0").unwrap(), "0".repeat(64));
    }

    #[test]
    fn out_of_range_and_malformed_values_are_rejected() {
        assert!(word("uint8", "256").is_err());
        assert!(word("uint256", "-1").is_err());
        assert!(word("uint256", "\"1.5\"").is_err());
        assert!(word("uint7", "1").is_err());
        assert!(word("uint264", "1").is_err());
        assert!(word("bytes33", "\"0x00\"").is_err());
        assert!(word("bytes2", "\"0x010203\"").is_err());
        assert!(word("bool", "\"yes\"").is_err());
        assert!(word("address", "\"0x1234\"").is_err());
        assert!(word("nonsense", "1").is_err());
        assert!(word("string", "5").is_err());
    }

    #[test]
    fn arrays_hash_their_elements_and_check_fixed_length() {
        let doc = |len: &str, items: &str| {
            format!(
                r#"{{"types":{{"T":[{{"name":"a","type":"uint256{len}"}}]}},"primaryType":"T","domain":{{}},"message":{{"a":{items}}}}}"#
            )
        };
        let data = parse(&Value::String(doc("[]", "[1,2]"))).unwrap();
        let expected = {
            let mut joined = vec![];
            for n in [1u8, 2] {
                let mut w = [0u8; 32];
                w[31] = n;
                joined.extend_from_slice(&w);
            }
            keccak256(&joined)
        };
        assert_eq!(
            data.encode_value("uint256[]", data.message.get("a").unwrap(), 0)
                .unwrap(),
            expected
        );
        let fixed = parse(&Value::String(doc("[3]", "[1,2]"))).unwrap();
        assert!(fixed.signing_hash().is_err());
        let ok = parse(&Value::String(doc("[2]", "[1,2]"))).unwrap();
        assert!(ok.signing_hash().is_ok());
    }

    #[test]
    fn missing_fields_and_undefined_types_are_errors() {
        let mut data = mail();
        data.message = json::parse(r#"{"from":{"name":"Cow"},"to":null,"contents":"x"}"#).unwrap();
        assert!(data.signing_hash().unwrap_err().contains("wallet"));
        let mut data = mail();
        data.primary_type = "Nope".into();
        assert!(data.signing_hash().is_err());
    }

    #[test]
    fn null_struct_hashes_as_zero_word() {
        let mut data = mail();
        data.message = json::parse(r#"{"from":null,"to":null,"contents":"x"}"#).unwrap();
        assert!(data.signing_hash().is_ok());
    }

    #[test]
    fn recursive_types_are_bounded() {
        let doc = r#"{"types":{"N":[{"name":"next","type":"N"}]},"primaryType":"N","domain":{},
          "message":{"next":{"next":{"next":null}}}}"#;
        let data = parse(&Value::str(doc)).unwrap();
        assert!(data.signing_hash().is_ok());
        let mut deep = String::from("null");
        for _ in 0..40 {
            deep = format!(r#"{{"next":{deep}}}"#);
        }
        let doc = format!(
            r#"{{"types":{{"N":[{{"name":"next","type":"N"}}]}},"primaryType":"N","domain":{{}},"message":{deep}}}"#
        );
        let data = parse(&Value::String(doc)).unwrap();
        assert!(data.signing_hash().unwrap_err().contains("deeply"));
    }

    #[test]
    fn structural_errors_are_reported() {
        for bad in [
            "not json",
            r#"{}"#,
            r#"{"types":{},"primaryType":"T","domain":{}}"#,
            r#"{"types":{"T":[{"name":"a"}]},"primaryType":"T","domain":{},"message":{}}"#,
            r#"{"types":{"T x":[]},"primaryType":"T","domain":{},"message":{}}"#,
            r#"{"types":{"T":[{"name":"a b","type":"string"}]},"primaryType":"T","domain":{},"message":{}}"#,
            r#"{"types":{"T":[]},"primaryType":"T","domain":[],"message":{}}"#,
        ] {
            assert!(parse(&Value::str(bad)).is_err(), "{bad}");
        }
        assert!(parse(&Value::String("x".repeat(MAX_INPUT_BYTES + 1))).is_err());
    }
}
