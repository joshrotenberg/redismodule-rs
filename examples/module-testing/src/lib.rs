//! A tiny author-owned byte parser, independent of Redis and the SDK.

/// Parses a nonempty sequence of ASCII decimal digits into a `u16`.
/// Leading zeroes are accepted; signs, whitespace, binary data and overflow
/// are rejected. A module can call this on `ArgCursor::next_bytes()` input.
pub fn parse_limit(input: &[u8]) -> Option<u16> {
    if input.is_empty() {
        return None;
    }
    input.iter().try_fold(0u16, |value, byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use super::parse_limit;

    #[test]
    fn boundaries_and_rejected_encodings() {
        for (input, expected) in [
            (b"0".as_slice(), Some(0)),
            (b"00042".as_slice(), Some(42)),
            (b"65535".as_slice(), Some(u16::MAX)),
            (b"65536".as_slice(), None),
            (b"".as_slice(), None),
            (b"+1".as_slice(), None),
            (b"-1".as_slice(), None),
            (b" 1".as_slice(), None),
            (b"1\n".as_slice(), None),
            (b"1\0".as_slice(), None),
            (b"\xff".as_slice(), None),
        ] {
            assert_eq!(parse_limit(input), expected, "{input:?}");
        }
    }
}
