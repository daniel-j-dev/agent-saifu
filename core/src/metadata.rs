//! Static metadata describing agent-saifu.

pub const NAME: &str = "agent-saifu";
pub const DESCRIPTION: &str =
    "All-in-one crypto wallet for Agents. Built from the ground up in Rust.";
pub const VERSION_NUMBER: &str = "0.0";
pub const VERSION: &str = "Version 0.0";
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// Protocol revisions this server can speak; the first entry is preferred.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Formats the metadata as the plain text returned by the `get_info` tool.
pub fn info_text() -> String {
    format!("{DESCRIPTION}\n{VERSION}")
}

/// Picks the protocol version to answer with: the client's if supported, else ours.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .copied()
        .unwrap_or(MCP_PROTOCOL_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_text_has_description_and_version() {
        assert_eq!(
            info_text(),
            "All-in-one crypto wallet for Agents. Built from the ground up in Rust.\nVersion 0.0"
        );
    }

    #[test]
    fn version_label_matches_number() {
        assert_eq!(VERSION, format!("Version {VERSION_NUMBER}"));
    }

    #[test]
    fn negotiates_supported_version() {
        assert_eq!(negotiate_protocol_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
    }

    #[test]
    fn negotiates_fallback_for_unknown_or_missing() {
        assert_eq!(
            negotiate_protocol_version(Some("1999-01-01")),
            MCP_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), MCP_PROTOCOL_VERSION);
    }
}
