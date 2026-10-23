//! Stdio transport: newline-delimited JSON-RPC messages, one per line.

use std::io::{self, BufRead, Read, Write};

use crate::crypto::secret::zeroize_string;
use crate::json::Value;
use crate::protocol::{self, INVALID_REQUEST};
use crate::tools::Toolbox;

/// Upper bound for a single incoming message, to avoid unbounded memory use.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Text(String),
    TooLong,
    InvalidUtf8,
}

/// Reads one line of at most `max` bytes (excluding the newline). Returns `None` at EOF.
/// Oversized lines are discarded up to their newline and reported as `Line::TooLong`.
pub fn read_bounded_line<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Option<Line>> {
    let mut buf = Vec::new();
    let read = reader
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut buf)?;
    if read == 0 {
        return Ok(None);
    }
    let had_newline = buf.last() == Some(&b'\n');
    if had_newline {
        buf.pop();
    }
    if buf.len() > max {
        if !had_newline {
            discard_until_newline(reader)?;
        }
        return Ok(Some(Line::TooLong));
    }
    Ok(Some(match String::from_utf8(buf) {
        Ok(s) => Line::Text(s),
        Err(_) => Line::InvalidUtf8,
    }))
}

fn discard_until_newline<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let (done, used) = {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                return Ok(());
            }
            match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (true, i + 1),
                None => (false, chunk.len()),
            }
        };
        reader.consume(used);
        if done {
            return Ok(());
        }
    }
}

/// Produces the response for one input line, or `None` if nothing should be sent.
pub fn process_line(line: Line, tools: &Toolbox) -> Option<String> {
    match line {
        Line::Text(text) if text.trim().is_empty() => None,
        Line::Text(mut text) => {
            let response = protocol::handle_message(text.trim_end_matches('\r'), tools);
            zeroize_string(&mut text);
            response
        }
        Line::TooLong => Some(invalid_request_response("Message too large")),
        Line::InvalidUtf8 => Some(invalid_request_response("Message is not valid UTF-8")),
    }
}

fn invalid_request_response(message: &str) -> String {
    Value::object(vec![
        ("jsonrpc", Value::str("2.0")),
        ("id", Value::Null),
        (
            "error",
            Value::object(vec![
                ("code", Value::int(INVALID_REQUEST)),
                ("message", Value::str(message)),
            ]),
        ),
    ])
    .to_json()
}

/// Serves requests until the input is closed.
pub fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    tools: &Toolbox,
) -> io::Result<()> {
    while let Some(line) = read_bounded_line(&mut reader, MAX_LINE_BYTES)? {
        if let Some(mut response) = process_line(line, tools) {
            let written = writer
                .write_all(response.as_bytes())
                .and_then(|_| writer.write_all(b"\n"))
                .and_then(|_| writer.flush());
            zeroize_string(&mut response);
            written?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testing::toolbox_fixture;
    use std::io::Cursor;

    fn read(input: &[u8], max: usize) -> Vec<Line> {
        let mut cur = Cursor::new(input.to_vec());
        let mut out = Vec::new();
        while let Some(l) = read_bounded_line(&mut cur, max).unwrap() {
            out.push(l);
        }
        out
    }

    #[test]
    fn reads_lines_with_and_without_trailing_newline() {
        assert_eq!(
            read(b"a\nbc\nd", 10),
            vec![
                Line::Text("a".into()),
                Line::Text("bc".into()),
                Line::Text("d".into())
            ]
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(read(b"", 10).is_empty());
    }

    #[test]
    fn line_exactly_at_limit_is_accepted() {
        assert_eq!(read(b"abcd\n", 4), vec![Line::Text("abcd".into())]);
        assert_eq!(read(b"abcd", 4), vec![Line::Text("abcd".into())]);
    }

    #[test]
    fn oversized_line_is_discarded_and_next_line_still_read() {
        assert_eq!(
            read(b"abcdefghij\nok\n", 4),
            vec![Line::TooLong, Line::Text("ok".into())]
        );
        assert_eq!(
            read(b"abcde\nok\n", 4),
            vec![Line::TooLong, Line::Text("ok".into())]
        );
        assert_eq!(read(b"abcdefghij", 4), vec![Line::TooLong]);
    }

    #[test]
    fn invalid_utf8_is_reported() {
        assert_eq!(
            read(b"\xff\xfe\nok\n", 10),
            vec![Line::InvalidUtf8, Line::Text("ok".into())]
        );
    }

    #[test]
    fn blank_lines_produce_no_response() {
        let (_f, tb) = toolbox_fixture();
        assert_eq!(process_line(Line::Text("   ".into()), &tb), None);
        assert_eq!(process_line(Line::Text(String::new()), &tb), None);
    }

    #[test]
    fn crlf_is_tolerated() {
        let (_f, tb) = toolbox_fixture();
        let out = process_line(
            Line::Text("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r".into()),
            &tb,
        );
        assert!(out.unwrap().contains("\"result\""));
    }

    #[test]
    fn bad_lines_get_invalid_request_errors() {
        let (_f, tb) = toolbox_fixture();
        for line in [Line::TooLong, Line::InvalidUtf8] {
            let out = process_line(line, &tb).unwrap();
            assert!(out.contains("-32600"));
            assert!(out.contains("\"id\":null"));
        }
    }

    #[test]
    fn serve_answers_each_request_on_its_own_line() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"get_info\"}}\n",
        );
        let mut output = Vec::new();
        let (_f, tb) = toolbox_fixture();
        serve(Cursor::new(input.as_bytes()), &mut output, &tb).unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"id\":1"));
        assert!(lines[1].contains("Version 0.0"));
    }
}
