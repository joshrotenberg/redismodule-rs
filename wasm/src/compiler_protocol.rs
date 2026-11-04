//! Private framed pipe protocol. Neither endpoint accepts serialized code from
//! Redis commands, persistence, a cache path or another externally supplied file.
// Each executable uses the opposite half of this shared protocol.
#![allow(dead_code)]
use anyhow::{bail, ensure};

pub const REQUEST_MAGIC: [u8; 8] = *b"RWCreq01";
pub const RESPONSE_MAGIC: [u8; 8] = *b"RWCres01";
pub const VERSION: u32 = 1;
pub const MAX_INPUT: usize = 64 << 20;
pub const MAX_ARTIFACT: usize = 512 << 20;
pub const MAX_DIAGNOSTIC: usize = 8192;
pub const REQUEST_SIZE: usize = 84;
pub const RESPONSE_SIZE: usize = 148;

pub fn request(
    nonce: [u8; 32],
    fingerprint: [u8; 32],
    len: usize,
) -> anyhow::Result<[u8; REQUEST_SIZE]> {
    ensure!(
        len > 0 && len <= MAX_INPUT,
        "component size must be between 1 and {MAX_INPUT} bytes"
    );
    let mut bytes = [0; REQUEST_SIZE];
    bytes[..8].copy_from_slice(&REQUEST_MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[12..44].copy_from_slice(&nonce);
    bytes[44..76].copy_from_slice(&fingerprint);
    bytes[76..84].copy_from_slice(&(len as u64).to_le_bytes());
    Ok(bytes)
}

pub fn parse_request(
    bytes: &[u8; REQUEST_SIZE],
    expected: [u8; 32],
) -> anyhow::Result<([u8; 32], usize)> {
    ensure!(
        bytes[..8] == REQUEST_MAGIC && u32_at(bytes, 8) == VERSION,
        "invalid compiler request framing/version"
    );
    ensure!(
        bytes[44..76] == expected,
        "compiler build/configuration mismatch"
    );
    let len = usize::try_from(u64_at(bytes, 76))?;
    ensure!(
        len > 0 && len <= MAX_INPUT,
        "component input exceeds compiler bound"
    );
    Ok((bytes[12..44].try_into().unwrap(), len))
}

#[derive(Debug)]
pub struct Response {
    pub nonce: [u8; 32],
    pub fingerprint: [u8; 32],
    pub failed: bool,
    pub len: usize,
    pub baseline: u64,
    pub ceiling: u64,
    pub cpu_ms: u64,
    pub checksum: [u8; 32],
}

impl Response {
    pub fn encode(&self) -> [u8; RESPONSE_SIZE] {
        let mut bytes = [0; RESPONSE_SIZE];
        bytes[..8].copy_from_slice(&RESPONSE_MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[12..44].copy_from_slice(&self.nonce);
        bytes[44..76].copy_from_slice(&self.fingerprint);
        bytes[76..80].copy_from_slice(&u32::from(self.failed).to_le_bytes());
        bytes[80..88].copy_from_slice(&(self.len as u64).to_le_bytes());
        bytes[88..96].copy_from_slice(&self.baseline.to_le_bytes());
        bytes[96..104].copy_from_slice(&self.ceiling.to_le_bytes());
        bytes[104..112].copy_from_slice(&self.cpu_ms.to_le_bytes());
        bytes[112..144].copy_from_slice(&self.checksum);
        bytes
    }

    pub fn parse(
        bytes: &[u8; RESPONSE_SIZE],
        nonce: [u8; 32],
        fingerprint: [u8; 32],
    ) -> anyhow::Result<Self> {
        ensure!(
            bytes[..8] == RESPONSE_MAGIC && u32_at(bytes, 8) == VERSION && bytes[144..] == [0; 4],
            "invalid compiler response framing/version"
        );
        ensure!(
            bytes[12..44] == nonce && bytes[44..76] == fingerprint,
            "compiler response identity mismatch"
        );
        let failed = match u32_at(bytes, 76) {
            0 => false,
            1 => true,
            _ => bail!("invalid compiler status"),
        };
        let len = usize::try_from(u64_at(bytes, 80))?;
        ensure!(
            len > 0 && len <= if failed { MAX_DIAGNOSTIC } else { MAX_ARTIFACT },
            "compiler response exceeds output bound"
        );
        let baseline = u64_at(bytes, 88);
        let ceiling = u64_at(bytes, 96);
        ensure!(
            ceiling > baseline,
            "compiler did not report an enforced address-space ceiling"
        );
        Ok(Self {
            nonce,
            fingerprint,
            failed,
            len,
            baseline,
            ceiling,
            cpu_ms: u64_at(bytes, 104),
            checksum: bytes[112..144].try_into().unwrap(),
        })
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_refuses_bad_identity_lengths_and_versions() {
        assert!(request([1; 32], [2; 32], 0).is_err());
        assert!(request([1; 32], [2; 32], MAX_INPUT + 1).is_err());
        let bytes = request([1; 32], [2; 32], 10).unwrap();
        assert_eq!(parse_request(&bytes, [2; 32]).unwrap(), ([1; 32], 10));
        assert!(parse_request(&bytes, [3; 32]).is_err());
        let mut bad = bytes;
        bad[8] = 2;
        assert!(parse_request(&bad, [2; 32]).is_err());
    }
    #[test]
    fn response_refuses_foreign_artifacts_and_oversized_frames() {
        let response = Response {
            nonce: [1; 32],
            fingerprint: [2; 32],
            failed: false,
            len: 7,
            baseline: 10,
            ceiling: 20,
            cpu_ms: 1,
            checksum: [3; 32],
        };
        let bytes = response.encode();
        assert_eq!(Response::parse(&bytes, [1; 32], [2; 32]).unwrap().len, 7);
        assert!(Response::parse(&bytes, [0; 32], [2; 32]).is_err());
        let mut bad = bytes;
        bad[80..88].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Response::parse(&bad, [1; 32], [2; 32]).is_err());
        let mut bad = bytes;
        bad[144] = 1;
        assert!(Response::parse(&bad, [1; 32], [2; 32]).is_err());
    }
}
