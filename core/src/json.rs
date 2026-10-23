//! Minimal JSON parser and serializer (no external dependencies).

use std::fmt;

pub const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Raw, grammar-validated number text so ids round-trip exactly.
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::String(s.to_string())
    }

    pub fn int(n: i64) -> Value {
        Value::Number(n.to_string())
    }

    pub fn object(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Returns the value of the first matching key of an object.
    pub fn array(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Non-negative integer, from a JSON number or a decimal string.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => n.parse::<u64>().ok(),
            Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                s.parse::<u64>().ok()
            }
            _ => None,
        }
    }

    /// Overwrites every string in the tree with zeros; used after handling secrets.
    pub fn wipe(&mut self) {
        match self {
            Value::String(s) | Value::Number(s) => crate::crypto::secret::zeroize_string(s),
            Value::Array(items) => items.iter_mut().for_each(Value::wipe),
            Value::Object(pairs) => {
                for (k, v) in pairs.iter_mut() {
                    crate::crypto::secret::zeroize_string(k);
                    v.wipe();
                }
            }
            Value::Null | Value::Bool(_) => {}
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out);
        out
    }
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(n),
        Value::String(s) => out.push_str(&escape_string(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(pairs) => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_string(k));
                out.push(':');
                write_value(v, out);
            }
            out.push('}');
        }
    }
}

/// Produces a quoted, escaped JSON string literal.
pub fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn parse(input: &str) -> Result<Value, ParseError> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, msg: &str) -> ParseError {
        ParseError(format!("{msg} at byte {}", self.pos))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect_literal(&mut self, lit: &str, value: Value) -> Result<Value, ParseError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'n') => self.expect_literal("null", Value::Null),
            Some(b't') => self.expect_literal("true", Value::Bool(true)),
            Some(b'f') => self.expect_literal("false", Value::Bool(false)),
            Some(b'"') => self.parse_string().map(Value::String),
            Some(b'[') => self.parse_array(depth),
            Some(b'{') => self.parse_object(depth),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, ParseError> {
        self.pos += 1;
        let mut pairs = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(pairs));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected object key"));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            self.skip_whitespace();
            let value = self.parse_value(depth + 1)?;
            pairs.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(pairs));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.skip_digits(),
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number fraction"));
            }
            self.skip_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number exponent"));
            }
            self.skip_digits();
        }
        // Slicing is safe: only ASCII bytes were consumed.
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("0");
        Ok(Value::Number(text.to_string()))
    }

    fn skip_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, ParseError> {
        let end = self.pos + 4;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.error("truncated unicode escape"))?;
        if !slice.iter().all(u8::is_ascii_hexdigit) {
            return Err(self.error("invalid unicode escape"));
        }
        // All bytes are ASCII hex digits.
        let text = std::str::from_utf8(slice).unwrap_or("0");
        let code =
            u32::from_str_radix(text, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.pos = end;
        Ok(code)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ParseError> {
        let first = self.parse_hex4()?;
        let code = match first {
            0xD800..=0xDBFF => {
                if !self.bytes[self.pos..].starts_with(b"\\u") {
                    return Err(self.error("unpaired surrogate"));
                }
                self.pos += 2;
                let second = self.parse_hex4()?;
                if !(0xDC00..=0xDFFF).contains(&second) {
                    return Err(self.error("invalid low surrogate"));
                }
                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(self.error("unpaired surrogate")),
            other => other,
        };
        char::from_u32(code).ok_or_else(|| self.error("invalid code point"))
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // Input came from a &str and we stop only at ASCII bytes, so this is valid UTF-8.
            out.push_str(
                std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| self.error("invalid utf-8"))?,
            );
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                Some(_) => return Err(self.error("control character in string")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert_eq!(parse("null"), Ok(Value::Null));
        assert_eq!(parse(" true "), Ok(Value::Bool(true)));
        assert_eq!(parse("false"), Ok(Value::Bool(false)));
        assert_eq!(parse("\"hi\""), Ok(Value::str("hi")));
    }

    #[test]
    fn parses_numbers_verbatim() {
        for n in ["0", "-0", "12", "-3.5", "1e10", "1.5E-3", "0.0"] {
            assert_eq!(parse(n), Ok(Value::Number(n.to_string())), "{n}");
        }
    }

    #[test]
    fn rejects_bad_numbers() {
        for n in ["01", "-", "1.", ".5", "1e", "1e+", "+1", "--1"] {
            assert!(parse(n).is_err(), "{n}");
        }
    }

    #[test]
    fn parses_containers() {
        let v = parse(r#"{"a":[1,2,{"b":null}],"c":"d"}"#).unwrap();
        assert_eq!(v.get("c").and_then(Value::as_str), Some("d"));
        assert_eq!(
            v.get("a"),
            Some(&Value::Array(vec![
                Value::int(1),
                Value::int(2),
                Value::object(vec![("b", Value::Null)]),
            ]))
        );
        assert_eq!(parse("[]"), Ok(Value::Array(vec![])));
        assert_eq!(parse("{}"), Ok(Value::Object(vec![])));
    }

    #[test]
    fn parses_string_escapes() {
        assert_eq!(parse(r#""a\n\t\"\\\/""#), Ok(Value::str("a\n\t\"\\/")));
        assert_eq!(parse(r#""\u0041""#), Ok(Value::str("A")));
        assert_eq!(parse(r#""\ud83d\ude00""#), Ok(Value::str("😀")));
        assert_eq!(parse("\"héllo 😀\""), Ok(Value::str("héllo 😀")));
    }

    #[test]
    fn rejects_bad_strings() {
        for s in [
            r#""\ud83d""#,
            r#""\ude00""#,
            r#""\ud83d\u0041""#,
            r#""\u12""#,
            r#""\uZZZZ""#,
            r#""\x""#,
            "\"abc",
            "\"a\nb\"",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
    }

    #[test]
    fn rejects_malformed_documents() {
        for s in [
            "",
            "{",
            "[1,]",
            "{\"a\"}",
            "{\"a\":1,}",
            "[1 2]",
            "nul",
            "{1:2}",
            "1 2",
            "tru",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
    }

    #[test]
    fn rejects_excessive_nesting() {
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert!(parse(&deep).is_err());
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn escapes_strings() {
        assert_eq!(
            escape_string("a\"b\\c\n\u{01}"),
            "\"a\\\"b\\\\c\\n\\u0001\""
        );
        assert_eq!(escape_string("é😀"), "\"é😀\"");
    }

    #[test]
    fn serializes_and_round_trips() {
        let v = Value::object(vec![
            ("id", Value::int(7)),
            (
                "list",
                Value::Array(vec![Value::Null, Value::Bool(true), Value::str("x\"y")]),
            ),
        ]);
        let text = v.to_json();
        assert_eq!(text, r#"{"id":7,"list":[null,true,"x\"y"]}"#);
        assert_eq!(parse(&text), Ok(v));
    }

    #[test]
    fn typed_accessors() {
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Null.as_bool(), None);
        assert_eq!(Value::Array(vec![]).as_array().map(Vec::len), Some(0));
        assert_eq!(Value::int(42).as_u64(), Some(42));
        assert_eq!(Value::str("42").as_u64(), Some(42));
        for bad in [
            Value::int(-1),
            Value::Number("1.5".into()),
            Value::str(""),
            Value::str("-3"),
            Value::str("1e3"),
            Value::Null,
            Value::str("99999999999999999999"),
        ] {
            assert_eq!(bad.as_u64(), None, "{bad:?}");
        }
    }

    #[test]
    fn wipe_clears_every_string() {
        let mut v = parse(r#"{"password":"hunter2","a":["x",{"seed":"words"}],"n":12}"#).unwrap();
        v.wipe();
        let text = v.to_json();
        assert!(!text.contains("hunter2") && !text.contains("words") && !text.contains("password"));
    }

    #[test]
    fn get_on_non_object_is_none() {
        assert_eq!(Value::Null.get("a"), None);
        assert_eq!(Value::int(1).as_str(), None);
    }
}
