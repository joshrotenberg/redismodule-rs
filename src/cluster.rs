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

/// Generate an ASCII key whose [`key_slot`] is `slot`.
///
/// Returns `None` when `slot >= SLOTS`. This needs no server and works with
/// every Redis version, including those without the native canonical-name API.
/// The key consists of a nonempty hash tag in braces. A prefix without `{`
/// and any suffix can be added without changing its slot.
///
/// This only chooses a name. It does not discover the slot's owner or grant
/// permission to access it. The name is not Redis's canonical slot name, and
/// its exact bytes are not a cross-version contract; retain the returned name
/// when it identifies stored data.
///
/// The first valid call builds a shared 48 KiB tag table by a bounded search;
/// later calls only look up the tag and allocate the returned five-byte key.
///
/// ```
/// use redis_module::cluster::{key_for_slot, key_slot, SLOTS};
/// let key = key_for_slot(1234).unwrap();
/// assert_eq!(key_slot(key.as_bytes()), 1234);
/// let named = format!("module:{key}:events");
/// assert_eq!(key_slot(named.as_bytes()), 1234);
/// assert!(key_for_slot(SLOTS).is_none());
/// ```
#[must_use]
pub fn key_for_slot(slot: u16) -> Option<String> {
    if slot >= SLOTS {
        return None;
    }
    let tag = &slot_tags()[usize::from(slot)];
    let mut key = String::with_capacity(5);
    key.push('{');
    key.extend(tag.iter().copied().map(char::from));
    key.push('}');
    Some(key)
}

fn slot_tags() -> &'static [[u8; 3]; SLOTS as usize] {
    use std::sync::OnceLock;
    static TAGS: OnceLock<[[u8; 3]; SLOTS as usize]> = OnceLock::new();
    TAGS.get_or_init(|| {
        // None of these bytes can delimit a hash tag. Exhaustive tests prove
        // that three-byte combinations cover every slot. Stop once all slots
        // are filled; the search is bounded by 64^3 candidates.
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut tags = [[0; 3]; SLOTS as usize];
        let mut remaining = usize::from(SLOTS);
        for &a in ALPHABET {
            for &b in ALPHABET {
                for &c in ALPHABET {
                    let tag = [a, b, c];
                    let entry = &mut tags[usize::from(key_slot(&tag))];
                    if entry[0] == 0 {
                        *entry = tag;
                        remaining -= 1;
                        if remaining == 0 {
                            return tags;
                        }
                    }
                }
            }
        }
        unreachable!("the ASCII tag alphabet covers every cluster slot")
    })
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
    fn generated_keys_cover_every_slot() {
        for slot in 0..SLOTS {
            let key = key_for_slot(slot).expect("every slot has a key");
            assert!(key.is_ascii());
            assert_eq!(key.len(), 5);
            assert_eq!(key_slot(key.as_bytes()), slot);
            let named = format!("module:{key}:events");
            assert_eq!(key_slot(named.as_bytes()), slot);
        }
        assert_eq!(key_for_slot(SLOTS), None);
        assert_eq!(key_for_slot(u16::MAX), None);
    }

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
        // An empty first tag: the whole key is hashed, not "bar".
        assert_eq!(key_slot(b"foo{}{bar}"), 8363);
        assert_eq!(key_slot(b"foo{}{bar}"), crc16(b"foo{}{bar}") % SLOTS);
        assert_ne!(key_slot(b"foo{}{bar}"), key_slot(b"bar"));
        assert_eq!(key_slot(b""), 0);
    }
}
