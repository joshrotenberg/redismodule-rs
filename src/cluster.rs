//! Cluster helpers that need no server.

/// The number of hash slots in a Redis cluster.
pub const SLOTS: u16 = 16384;

/// The hash slot of `key`, as Redis computes it: the CRC16 of the key, or of
/// its hash tag (the part between the first `{` and the next `}`, when that
/// part is not empty), modulo 16384. It needs no server, so it works on every
/// Redis version; `RedisModule_ClusterKeySlot` was added in 7.4.
#[must_use]
pub fn key_slot(key: &[u8]) -> u16 {
    let hashed = key
        .iter()
        .position(|&b| b == b'{')
        .and_then(|open| {
            let rest = &key[open + 1..];
            let close = rest.iter().position(|&b| b == b'}')?;
            (close > 0).then(|| &rest[..close])
        })
        .unwrap_or(key);
    crc16(hashed) % SLOTS
}

/// CRC16-CCITT (XMODEM), the CRC that Redis Cluster uses.
fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |crc, &byte| {
        (0..8).fold(crc ^ (u16::from(byte) << 8), |crc, _| {
            if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_match_redis() {
        // The check value of CRC16-XMODEM, and slots from the Redis cluster
        // documentation and CLUSTER KEYSLOT.
        assert_eq!(crc16(b"123456789"), 0x31c3);
        assert_eq!(key_slot(b"somekey"), 11058);
        assert_eq!(key_slot(b"foo{hash_tag}"), 2515);
        assert_eq!(key_slot(b"foo"), 12182);
        // A hash tag decides the slot; an empty one does not.
        assert_eq!(
            key_slot(b"{user1000}.following"),
            key_slot(b"{user1000}.followers")
        );
        assert_eq!(key_slot(b"{user1000}.following"), key_slot(b"user1000"));
        assert_eq!(key_slot(b"foo{}{bar}"), key_slot(b"foo{}{bar}"));
        assert_ne!(key_slot(b"foo{}{bar}"), key_slot(b"bar"));
        assert_eq!(key_slot(b""), 0);
    }
}
