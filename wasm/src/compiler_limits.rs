//! Resource limits are applied by the fresh executable before reading input or
//! constructing Wasmtime. A missing or ineffective platform implementation fails.
use anyhow::{Context as _, ensure};

pub fn apply(allowance: u64, cpu_seconds: u64) -> anyhow::Result<(u64, u64)> {
    ensure!(
        (64 << 20..=16 << 30).contains(&allowance),
        "invalid compiler address-space allowance"
    );
    ensure!(
        (1..=600).contains(&cpu_seconds),
        "invalid compiler CPU limit"
    );
    let baseline = virtual_size()?;
    let ceiling = baseline
        .checked_add(allowance)
        .context("compiler address-space limit overflow")?;
    set_limit(libc::RLIMIT_AS, ceiling)?;
    set_limit(libc::RLIMIT_CPU, cpu_seconds)?;
    // No core image of potentially large compiler state after a failed limit.
    set_limit(libc::RLIMIT_CORE, 0)?;
    Ok((baseline, ceiling))
}

#[cfg(target_os = "linux")]
fn set_limit(resource: libc::__rlimit_resource_t, value: u64) -> anyhow::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    ensure!(
        unsafe { libc::setrlimit(resource, &limit) } == 0,
        "setrlimit({resource}): {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_limit(resource: libc::c_int, value: u64) -> anyhow::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    ensure!(
        unsafe { libc::setrlimit(resource, &limit) } == 0,
        "setrlimit({resource}): {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn virtual_size() -> anyhow::Result<u64> {
    let pages: u64 = std::fs::read_to_string("/proc/self/statm")?
        .split_whitespace()
        .next()
        .context("missing virtual size")?
        .parse()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    ensure!(page_size > 0, "cannot query page size");
    pages
        .checked_mul(page_size as u64)
        .context("virtual size overflow")
}

#[cfg(target_os = "macos")]
fn virtual_size() -> anyhow::Result<u64> {
    // Mach's startup VM includes the shared cache reservation. XNU refuses an
    // absolute limit below that VM size. Disable NanoZone in the helper's
    // environment and enforce a finite additional allowance over this trusted
    // startup baseline, before any component bytes or compiler arena exist.
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
    let mut count = (std::mem::size_of::<libc::mach_task_basic_info>()
        / std::mem::size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;
    #[allow(deprecated)]
    let status = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    ensure!(
        status == libc::KERN_SUCCESS,
        "cannot query compiler startup VM: {status}"
    );
    Ok(unsafe { info.assume_init() }.virtual_size)
}

pub fn cpu_milliseconds() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return 0;
    }
    let usage = unsafe { usage.assume_init() };
    (usage.ru_utime.tv_sec.max(0) as u64 + usage.ru_stime.tv_sec.max(0) as u64) * 1000
        + (usage.ru_utime.tv_usec.max(0) as u64 + usage.ru_stime.tv_usec.max(0) as u64) / 1000
}
