//! Running-image admission. Filesystem work exists only in explicit preparation.
use std::ffi::c_void;
use std::fs::File;
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
use std::io::Read;
use std::ptr;

use crate::{RedisError, raw};

const REFUSED: RedisError =
    RedisError::Str("ERR asynchronous authentication runtime is not prepared");

#[derive(Clone, Copy)]
pub(super) struct Manifest {
    #[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
    pub length: u64,
    #[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
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
        self.consistent_with(owner, || unsafe { metadata() })
    }
    fn consistent_with(
        &self,
        owner: &std::sync::Arc<()>,
        read: impl FnOnce() -> Result<[Vec<u8>; 4], RedisError>,
    ) -> Result<(), RedisError> {
        if self.process != std::process::id() || !std::sync::Arc::ptr_eq(&self.owner, owner) {
            return Err(REFUSED);
        }
        let data = read()?;
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
        use std::os::fd::AsRawFd;
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
        verify_image(&mut RunningImage(&mut image), manifest)?;
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

// Shared by the real running-file reader and fault-injected inert readers.
// The latter replace only I/O; all length/hash/ELF/stability decisions stay here.
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct ImageStamp {
    regular: bool,
    fields: (u64, u64, u64, i64, i64, i64, i64, u64),
}
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
trait ImageSource: Read {
    fn stamp(&mut self) -> std::io::Result<ImageStamp>;
    fn current(&mut self) -> std::io::Result<ImageStamp>;
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
struct RunningImage<'a>(&'a mut File);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Read for RunningImage<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn stamp(metadata: std::fs::Metadata) -> ImageStamp {
    use std::os::unix::fs::MetadataExt;
    ImageStamp {
        regular: metadata.is_file(),
        fields: (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.nlink(),
        ),
    }
}
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl ImageSource for RunningImage<'_> {
    fn stamp(&mut self) -> std::io::Result<ImageStamp> {
        self.0.metadata().map(stamp)
    }
    fn current(&mut self) -> std::io::Result<ImageStamp> {
        std::fs::metadata("/proc/self/exe").map(stamp)
    }
}
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
fn verify_image(image: &mut impl ImageSource, manifest: Manifest) -> Result<(), RedisError> {
    use sha2::{Digest, Sha256};
    let before = image.stamp().map_err(|_| REFUSED)?;
    if !before.regular || before.fields.2 != manifest.length || manifest.length > 256 * 1024 * 1024
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
    let after = image.stamp().map_err(|_| REFUSED)?;
    let current = image.current().map_err(|_| REFUSED)?;
    if total != manifest.length
        || before != after
        || after != current
        || header.len() < 64
        || &header[..6] != b"\x7fELF\x02\x01"
        || u16::from_le_bytes([header[18], header[19]]) != 62
        || <[u8; 32]>::from(digest.finalize()) != manifest.sha256
    {
        return Err(REFUSED);
    }
    Ok(())
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
    struct ImageFixture {
        bytes: std::io::Cursor<Vec<u8>>,
        stamp_calls: usize,
        mode: usize,
        interrupted: bool,
    }
    impl Read for ImageFixture {
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            if self.mode == 1 {
                return Err(std::io::ErrorKind::Other.into());
            }
            if self.mode == 2 && !self.interrupted {
                self.interrupted = true;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            // Small successful chunks exercise header accumulation as well.
            let size = target.len().min(7);
            self.bytes.read(&mut target[..size])
        }
    }
    impl ImageSource for ImageFixture {
        fn stamp(&mut self) -> std::io::Result<ImageStamp> {
            self.stamp_calls += 1;
            if (self.mode == 3 && self.stamp_calls == 1)
                || (self.mode == 4 && self.stamp_calls == 2)
            {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            let mut stamp = ImageStamp {
                regular: true,
                fields: (1, 2, 80, 3, 4, 5, 6, 1),
            };
            if self.mode == 5 && self.stamp_calls == 2 {
                stamp.fields.5 += 1;
            }
            if self.mode == 6 {
                stamp.regular = false;
            }
            if self.mode == 7 {
                stamp.fields.2 += 1;
            }
            Ok(stamp)
        }
        fn current(&mut self) -> std::io::Result<ImageStamp> {
            if self.mode == 8 {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            let mut stamp = self.stamp()?;
            if self.mode == 9 {
                stamp.fields.1 += 1;
            }
            Ok(stamp)
        }
    }
    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 80];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[18] = 62;
        bytes
    }
    fn fixture(bytes: Vec<u8>, mode: usize) -> ImageFixture {
        ImageFixture {
            bytes: std::io::Cursor::new(bytes),
            stamp_calls: 0,
            mode,
            interrupted: false,
        }
    }
    #[test]
    fn image_reader_refuses_io_length_hash_elf_and_object_changes() {
        use sha2::{Digest, Sha256};
        let bytes = elf();
        let manifest = Manifest {
            length: 80,
            sha256: Sha256::digest(&bytes).into(),
            build_id: b"expected-build",
        };
        assert!(verify_image(&mut fixture(bytes.clone(), 0), manifest).is_ok());
        assert!(verify_image(&mut fixture(bytes.clone(), 2), manifest).is_ok());
        for mode in [1, 3, 4, 5, 6, 7, 8, 9] {
            assert!(
                verify_image(&mut fixture(bytes.clone(), mode), manifest).is_err(),
                "fault {mode}"
            );
        }
        for length in [0, 20, 63, 79, 81] {
            let mut changed = bytes.clone();
            changed.resize(length, 0);
            assert!(
                verify_image(&mut fixture(changed, 0), manifest).is_err(),
                "length {length}"
            );
        }
        for offset in [0, 4, 5, 18, 79] {
            let mut changed = bytes.clone();
            changed[offset] ^= 1;
            // Hash matching an invalid ELF still refuses its format/machine.
            let expected = if offset < 19 {
                Manifest {
                    sha256: Sha256::digest(&changed).into(),
                    ..manifest
                }
            } else {
                manifest
            };
            assert!(
                verify_image(&mut fixture(changed, 0), expected).is_err(),
                "byte {offset}"
            );
        }
        let excessive = Manifest {
            length: 256 * 1024 * 1024 + 1,
            ..manifest
        };
        assert!(verify_image(&mut fixture(bytes, 0), excessive).is_err());
    }
    #[test]
    fn receipt_checks_process_load_and_metadata_without_reopening_image() {
        let owner = std::sync::Arc::new(());
        let file = File::open(std::env::current_exe().unwrap()).unwrap();
        let mut receipt = Receipt {
            generation: 1,
            owner: owner.clone(),
            process: std::process::id(),
            manifest: Manifest {
                length: 80,
                sha256: [0; 32],
                build_id: b"expected-build",
            },
            _image: file,
        };
        let calls = std::cell::Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            Ok([
                b"8.11.242".to_vec(),
                b"a93d7d19".to_vec(),
                b"0".to_vec(),
                b"expected-build".to_vec(),
            ])
        };
        assert!(receipt.consistent_with(&owner, read).is_ok());
        assert_eq!(calls.get(), 1);
        assert!(
            receipt
                .consistent_with(&std::sync::Arc::new(()), read)
                .is_err()
        );
        receipt.process = receipt.process.wrapping_add(1);
        assert!(receipt.consistent_with(&owner, read).is_err());
        assert_eq!(
            calls.get(),
            1,
            "owner/PID refusal must precede any native reader"
        );
        receipt.process = std::process::id();
        assert!(receipt.consistent_with(&owner, || Err(REFUSED)).is_err());
        assert!(
            receipt
                .consistent_with(&owner, || Ok([
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    b"other".to_vec()
                ]))
                .is_err()
        );
    }
}
