//! RED tests for Task 5 — ReconnectCursor (SSE Last-Event-ID, v0.4).

use ferridis_protocol::events::ReconnectCursor;

/// Valid cursor round-trips through as_str.
#[test]
fn cursor_as_str_roundtrips() {
    let c = ReconnectCursor::new("42").unwrap();
    assert_eq!(c.as_str(), "42");
}

/// Empty string is rejected — an empty Last-Event-ID has no meaning.
#[test]
fn cursor_rejects_empty_string() {
    assert!(ReconnectCursor::new("").is_err());
}

/// Whitespace-only string is rejected.
#[test]
fn cursor_rejects_whitespace_only() {
    assert!(ReconnectCursor::new("   ").is_err());
}

/// Two cursors with the same value compare equal.
#[test]
fn cursor_equality() {
    let a = ReconnectCursor::new("99").unwrap();
    let b = ReconnectCursor::new("99").unwrap();
    assert_eq!(a, b);
}

/// Display produces the raw cursor value (used as the header value).
#[test]
fn cursor_display_is_the_raw_value() {
    let c = ReconnectCursor::new("evt-123").unwrap();
    assert_eq!(c.to_string(), "evt-123");
}
