//! The payload of `WASM.DUMP` and `WASM.RESTORE`, as `FUNCTION DUMP` has one for
//! Lua libraries: every library's name and component.
//!
//! ```text
//! "WASMLIBS"  version (1 byte)  count (u32)
//! for each library: name length (u32), name, component length (u32), component
//! checksum (u64): FNV-1a of everything before it
//! ```
//!
//! Integers are little-endian. A restore lists each library's functions again
//! from its component, as `WASM.LOAD` does, so the payload carries none.

use anyhow::{Context as _, bail, ensure};

const MAGIC: &[u8; 8] = b"WASMLIBS";
const VERSION: u8 = 1;

/// The payload for `libraries`, given as names and components.
pub(crate) fn dump<'a>(libraries: impl Iterator<Item = (&'a str, &'a [u8])>) -> Vec<u8> {
    let libraries: Vec<_> = libraries.collect();
    let mut payload = Vec::new();
    payload.extend_from_slice(MAGIC);
    payload.push(VERSION);
    push_len(&mut payload, libraries.len());
    for (name, component) in libraries {
        push_len(&mut payload, name.len());
        payload.extend_from_slice(name.as_bytes());
        push_len(&mut payload, component.len());
        payload.extend_from_slice(component);
    }
    let checksum = fnv1a(&payload);
    payload.extend_from_slice(&checksum.to_le_bytes());
    payload
}

/// The libraries in `payload`, as names and components.
pub(crate) fn parse(payload: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let body_len = payload
        .len()
        .checked_sub(8)
        .context("the payload is too short")?;
    let (body, checksum) = payload.split_at(body_len);
    ensure!(
        fnv1a(body).to_le_bytes() == checksum,
        "the payload's checksum does not match"
    );
    let mut reader = Reader(body);
    ensure!(
        reader.take(MAGIC.len())? == MAGIC,
        "this is not a WASM.DUMP payload"
    );
    let version = reader.take(1)?[0];
    ensure!(version == VERSION, "unknown payload version {version}");
    let count = reader.len()?;
    let mut libraries = Vec::new();
    for _ in 0..count {
        let name_len = reader.len()?;
        let name = String::from_utf8(reader.take(name_len)?.to_vec())
            .context("a library name is not UTF-8")?;
        let component_len = reader.len()?;
        libraries.push((name, reader.take(component_len)?.to_vec()));
    }
    if !reader.0.is_empty() {
        bail!("the payload has bytes after its libraries");
    }
    Ok(libraries)
}

fn push_len(payload: &mut Vec<u8>, len: usize) {
    let len = u32::try_from(len).expect("a library or name over 4 GiB");
    payload.extend_from_slice(&len.to_le_bytes());
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        ensure!(self.0.len() >= n, "the payload is truncated");
        let (taken, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(taken)
    }

    fn len(&mut self) -> anyhow::Result<usize> {
        let bytes: [u8; 4] = self.take(4)?.try_into()?;
        Ok(u32::from_le_bytes(bytes) as usize)
    }
}

/// 64-bit FNV-1a: enough to catch a truncated or altered payload.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_round_trips_and_damage_is_refused() {
        let payload = dump([("a", &b"\0asm1"[..]), ("bee", &b""[..])].into_iter());
        assert_eq!(
            parse(&payload).unwrap(),
            [
                ("a".to_owned(), b"\0asm1".to_vec()),
                ("bee".to_owned(), Vec::new())
            ]
        );
        assert!(parse(&payload[..payload.len() - 1]).is_err());
        let mut altered = payload.clone();
        altered[12] ^= 1;
        assert!(
            parse(&altered)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        assert!(parse(b"short").is_err());
        assert_eq!(parse(&dump(std::iter::empty())).unwrap(), []);
    }
}
