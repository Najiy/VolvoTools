//! Small helpers for user-facing sentences.

/// "1 byte", "2 bytes".
pub(crate) fn bytes(count: u64) -> String {
    if count == 1 {
        "1 byte".into()
    } else {
        format!("{count} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn singular_and_plural() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(302), "302 bytes");
    }
}
