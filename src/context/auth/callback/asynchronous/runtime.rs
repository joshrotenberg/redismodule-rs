//! Running-image admission. Filesystem work exists only in explicit preparation.
use std::ffi::c_void;
use std::fs::File;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::io::Read;
use std::ptr;

use crate::{RedisError, raw};

const REFUSED: RedisError =
    RedisError::Str("ERR asynchronous authentication runtime is not prepared");

#[derive(Clone, Copy)]
pub(super) struct Manifest {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub length: u64,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub sha256: [u8; 32],
    pub build_id: &'static [u8],
}

// Enrollment is an explicit reviewed source change after matching native builds.
// Neither a runtime report nor preparation may add a manifest entry.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const APPROVED: &[Manifest] = &[Manifest {
    length: 13877504,
    sha256: [
        0x63, 0xf2, 0x65, 0xf3, 0xb6, 0x2d, 0xa5, 0x23, 0xa0, 0x1e, 0x2d, 0xfe, 0xd5, 0xf2, 0xf8,
        0xf2, 0x8b, 0x7f, 0x83, 0x3d, 0x22, 0xdd, 0x98, 0x83, 0x63, 0x60, 0xdd, 0x8a, 0x52, 0x03,
        0x02, 0x0d,
    ],
    build_id: b"b1d9bdfc5371d887",
}];

pub(super) struct Receipt {
    pub generation: u64,
    pub owner: std::sync::Arc<()>,
    process: u32,
    manifest: Manifest,
    // Keep the measured kernel object, not a pathname to a replacement file.
    _image: File,
}

#[derive(Clone, Copy)]
struct ReadApi {
    version: unsafe extern "C" fn() -> i32,
    info: unsafe extern "C" fn(
        *mut raw::RedisModuleCtx,
        *const std::ffi::c_char,
    ) -> *mut raw::RedisModuleServerInfoData,
    free_info: unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleServerInfoData),
    field: unsafe extern "C" fn(
        *mut raw::RedisModuleCtx,
        *mut raw::RedisModuleServerInfoData,
        *const std::ffi::c_char,
    ) -> *mut raw::RedisModuleString,
    bytes:
        unsafe extern "C" fn(*const raw::RedisModuleString, *mut usize) -> *const std::ffi::c_char,
    free_string: unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString),
}
unsafe fn metadata() -> Result<[Vec<u8>; 4], RedisError> {
    // Snapshot every reader/free before the producer allocates anything.
    let api = ReadApi {
        version: unsafe { raw::RedisModule_GetServerVersion }.ok_or(REFUSED)?,
        info: unsafe { raw::RedisModule_GetServerInfo }.ok_or(REFUSED)?,
        free_info: unsafe { raw::RedisModule_FreeServerInfo }.ok_or(REFUSED)?,
        field: unsafe { raw::RedisModule_ServerInfoGetField }.ok_or(REFUSED)?,
        bytes: unsafe { raw::RedisModule_StringPtrLen }.ok_or(REFUSED)?,
        free_string: unsafe { raw::RedisModule_FreeString }.ok_or(REFUSED)?,
    };
    unsafe { metadata_with(api) }
}
unsafe fn metadata_with(api: ReadApi) -> Result<[Vec<u8>; 4], RedisError> {
    let ReadApi {
        version,
        info,
        free_info,
        field,
        bytes,
        free_string,
    } = api;
    if unsafe { version() } != 0x0008_0bf2 {
        return Err(REFUSED);
    }
    struct Info(
        *mut raw::RedisModuleServerInfoData,
        unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleServerInfoData),
    );
    impl Drop for Info {
        fn drop(&mut self) {
            unsafe { (self.1)(ptr::null_mut(), self.0) }
        }
    }
    struct StringOwner(
        *mut raw::RedisModuleString,
        unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut raw::RedisModuleString),
    );
    impl Drop for StringOwner {
        fn drop(&mut self) {
            unsafe { (self.1)(ptr::null_mut(), self.0) }
        }
    }
    // Exact literal server avoids both RM_Call filters and module INFO hooks.
    let value = unsafe { info(ptr::null_mut(), c"server".as_ptr()) };
    if value.is_null() {
        return Err(REFUSED);
    }
    let value = Info(value, free_info);
    let mut result = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for (slot, key) in result.iter_mut().zip([
        c"redis_version",
        c"redis_git_sha1",
        c"redis_git_dirty",
        c"redis_build_id",
    ]) {
        let string = unsafe { field(ptr::null_mut(), value.0, key.as_ptr()) };
        if string.is_null() {
            return Err(REFUSED);
        }
        let string = StringOwner(string, free_string);
        let mut length = 0;
        let data = unsafe { bytes(string.0, &mut length) };
        if data.is_null() || length == 0 || length > 64 {
            return Err(REFUSED);
        }
        *slot = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) }.to_vec();
    }
    if result[0] != b"8.11.242" || result[1] != b"a93d7d19" || result[2] != b"0" {
        return Err(REFUSED);
    }
    Ok(result)
}

impl Receipt {
    // No filesystem operation belongs in this method, including on refusal.
    pub(super) unsafe fn consistent(&self, owner: &std::sync::Arc<()>) -> Result<(), RedisError> {
        if self.process != std::process::id() || !std::sync::Arc::ptr_eq(&self.owner, owner) {
            return Err(REFUSED);
        }
        let data = unsafe { metadata()? };
        if data[3] != self.manifest.build_id {
            return Err(REFUSED);
        }
        Ok(())
    }
}

pub(super) unsafe fn prepare(
    owner: std::sync::Arc<()>,
    generation: u64,
) -> Result<Receipt, RedisError> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use sha2::{Digest, Sha256};
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let data = unsafe { metadata()? };
        let manifest = *APPROVED
            .iter()
            .find(|m| m.build_id == data[3])
            .ok_or(REFUSED)?;
        let proc = File::open("/proc").map_err(|_| REFUSED)?;
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(proc.as_raw_fd(), fs.as_mut_ptr()) } != 0
            || unsafe { fs.assume_init() }.f_type != libc::PROC_SUPER_MAGIC
        {
            return Err(REFUSED);
        }
        let descriptor = unsafe {
            libc::openat(
                proc.as_raw_fd(),
                c"self/exe".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(REFUSED);
        }
        use std::os::fd::FromRawFd;
        let mut image = unsafe { File::from_raw_fd(descriptor) };
        fn stamp(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64, u64) {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
                m.nlink(),
            )
        }
        let before = image.metadata().map_err(|_| REFUSED)?;
        if !before.is_file() || before.len() != manifest.length || before.len() > 256 * 1024 * 1024
        {
            return Err(REFUSED);
        }
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut header = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let length = match image.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(REFUSED),
            };
            if length == 0 {
                break;
            }
            if header.len() < 64 {
                header.extend_from_slice(&buffer[..length.min(64 - header.len())]);
            }
            total = total.checked_add(length as u64).ok_or(REFUSED)?;
            if total > manifest.length {
                return Err(REFUSED);
            }
            digest.update(&buffer[..length]);
        }
        let after = image.metadata().map_err(|_| REFUSED)?;
        let current = std::fs::metadata("/proc/self/exe").map_err(|_| REFUSED)?;
        if total != manifest.length
            || stamp(&before) != stamp(&after)
            || stamp(&after) != stamp(&current)
            || header.len() < 64
            || &header[..6] != b"\x7fELF\x02\x01"
            || u16::from_le_bytes([header[18], header[19]]) != 62
            || <[u8; 32]>::from(digest.finalize()) != manifest.sha256
        {
            return Err(REFUSED);
        }
        Ok(Receipt {
            generation,
            owner,
            process: std::process::id(),
            manifest,
            _image: image,
        })
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = (owner, generation);
        Err(REFUSED)
    }
}

pub(super) type ClientInfo = unsafe extern "C" fn(*mut c_void, u64) -> i32;
pub(super) unsafe fn blocked(
    get: ClientInfo,
    ctx: *mut raw::RedisModuleCtx,
    id: u64,
    expected: bool,
) -> Result<(), RedisError> {
    let mut info: raw::RedisModuleClientInfo = unsafe { std::mem::zeroed() };
    info.version = raw::REDISMODULE_CLIENTINFO_VERSION as u64;
    if ctx.is_null()
        || unsafe {
            get(
                (&mut info as *mut raw::RedisModuleClientInfo).cast::<c_void>(),
                id,
            )
        } != raw::REDISMODULE_OK as i32
        || info.id != id
        || (info.flags & raw::REDISMODULE_CLIENTINFO_FLAG_BLOCKED as u64 != 0) != expected
    {
        return Err(REFUSED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, c_char};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static MODE: AtomicUsize = AtomicUsize::new(0);
    static INFO_FREES: AtomicUsize = AtomicUsize::new(0);
    static STRING_FREES: AtomicUsize = AtomicUsize::new(0);
    static BAD_CONTEXT: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn version() -> i32 {
        if MODE.load(Ordering::Relaxed) == 1 {
            0x0008_0a02
        } else {
            0x0008_0bf2
        }
    }
    unsafe extern "C" fn info(
        ctx: *mut raw::RedisModuleCtx,
        section: *const c_char,
    ) -> *mut raw::RedisModuleServerInfoData {
        if !ctx.is_null() || unsafe { CStr::from_ptr(section) }.to_bytes() != b"server" {
            BAD_CONTEXT.store(true, Ordering::Relaxed);
        }
        if MODE.load(Ordering::Relaxed) == 2 {
            ptr::null_mut()
        } else {
            Box::into_raw(Box::new(0u8)).cast()
        }
    }
    unsafe extern "C" fn free_info(
        ctx: *mut raw::RedisModuleCtx,
        data: *mut raw::RedisModuleServerInfoData,
    ) {
        if !ctx.is_null() {
            BAD_CONTEXT.store(true, Ordering::Relaxed);
        }
        INFO_FREES.fetch_add(1, Ordering::Relaxed);
        drop(unsafe { Box::from_raw(data.cast::<u8>()) });
    }
    unsafe extern "C" fn field(
        ctx: *mut raw::RedisModuleCtx,
        _: *mut raw::RedisModuleServerInfoData,
        key: *const c_char,
    ) -> *mut raw::RedisModuleString {
        if !ctx.is_null() {
            BAD_CONTEXT.store(true, Ordering::Relaxed);
        }
        let mode = MODE.load(Ordering::Relaxed);
        let key = unsafe { CStr::from_ptr(key) }.to_bytes();
        if mode == 3 && key == b"redis_git_sha1" {
            return ptr::null_mut();
        }
        let value = match key {
            b"redis_version" if mode == 4 => vec![b'x'; 65],
            b"redis_version" => b"8.11.242".to_vec(),
            b"redis_git_sha1" => b"a93d7d19".to_vec(),
            b"redis_git_dirty" if mode == 5 => b"1".to_vec(),
            b"redis_git_dirty" => b"0".to_vec(),
            _ => b"expected-build".to_vec(),
        };
        Box::into_raw(Box::new(value)).cast()
    }
    unsafe extern "C" fn bytes(
        data: *const raw::RedisModuleString,
        length: *mut usize,
    ) -> *const c_char {
        let value = unsafe { &*data.cast::<Vec<u8>>() };
        unsafe { *length = value.len() };
        value.as_ptr().cast()
    }
    unsafe extern "C" fn free_string(
        ctx: *mut raw::RedisModuleCtx,
        data: *mut raw::RedisModuleString,
    ) {
        if !ctx.is_null() {
            BAD_CONTEXT.store(true, Ordering::Relaxed);
        }
        let mut value = unsafe { Box::from_raw(data.cast::<Vec<u8>>()) };
        value.fill(0xff);
        drop(value);
        STRING_FREES.fetch_add(1, Ordering::Relaxed);
    }
    #[test]
    fn exact_server_reader_owns_every_field_and_frees_on_all_refusals() {
        let _serial = crate::context::auth::tests::PROTOCOL_TEST.lock().unwrap();
        let api = ReadApi {
            version,
            info,
            free_info,
            field,
            bytes,
            free_string,
        };
        for (mode, infos, strings) in [
            (0, 1, 4),
            (1, 0, 0),
            (2, 0, 0),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 4),
        ] {
            MODE.store(mode, Ordering::Relaxed);
            INFO_FREES.store(0, Ordering::Relaxed);
            STRING_FREES.store(0, Ordering::Relaxed);
            BAD_CONTEXT.store(false, Ordering::Relaxed);
            let result = unsafe { metadata_with(api) };
            if mode == 0 {
                let values = result.unwrap();
                assert_eq!(values[0], b"8.11.242");
                assert_eq!(
                    values[3], b"expected-build",
                    "field copies outlive native string poisoning/free"
                );
            } else {
                assert!(result.is_err());
            }
            assert_eq!(INFO_FREES.load(Ordering::Relaxed), infos);
            assert_eq!(STRING_FREES.load(Ordering::Relaxed), strings);
            assert!(!BAD_CONTEXT.load(Ordering::Relaxed));
        }
    }
}
