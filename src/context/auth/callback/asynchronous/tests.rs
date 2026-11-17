use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64};

static UNBLOCKS: AtomicUsize = AtomicUsize::new(0);
static STOPS: AtomicUsize = AtomicUsize::new(0);
static IN_SUBMIT: AtomicBool = AtomicBool::new(false);
static HOLD_SUBMIT: AtomicBool = AtomicBool::new(false);
static SUBMIT_OWNED: AtomicBool = AtomicBool::new(false);
static TIMER_PRIVATE: AtomicUsize = AtomicUsize::new(0);
static TIMER_ID: AtomicU64 = AtomicU64::new(u64::MAX);
static OUTSIDE_DROPS: AtomicUsize = AtomicUsize::new(0);

struct Probe(Arc<AtomicUsize>);
impl Drop for Probe {
    fn drop(&mut self) {
        if !registry().executing {
            OUTSIDE_DROPS.fetch_add(1, Ordering::Relaxed);
        }
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
fn setup() -> MutexGuard<'static, ()> {
    let serial = users::tests::PROTOCOL_TEST.lock().unwrap();
    assert_eq!(OWNED.load(Ordering::Relaxed), 0);
    assert!(registry().jobs.is_empty());
    assert!(registry().cleanup.is_empty());
    let mut state = users::registry();
    assert!(!state.has_auth_ownership());
    state.phase = users::Phase::Active;
    state.owner = Some(Arc::new(()));
    UNBLOCKS.store(0, Ordering::Relaxed);
    STOPS.store(0, Ordering::Relaxed);
    OUTSIDE_DROPS.store(0, Ordering::Relaxed);
    HOLD_SUBMIT.store(false, Ordering::Relaxed);
    IN_SUBMIT.store(false, Ordering::Relaxed);
    SUBMIT_OWNED.store(false, Ordering::Relaxed);
    drop(state);
    serial
}
fn reap() {
    let _lease = Reaping::begin();
    collect_ready(ptr::null_mut());
    drain_cleanup(1000);
}
#[test]
fn empty_reaper_execution_lease_still_refuses_unload() {
    let _serial = setup();
    assert!(!has_ownership());
    let lease = Reaping::begin();
    assert!(has_ownership());
    assert!(users::registry().has_auth_ownership());
    drop(lease);
    assert!(!has_ownership());
    finish();
}
fn finish() {
    reap();
    assert_eq!(OWNED.load(Ordering::Relaxed), 0);
    assert!(registry().jobs.is_empty());
    assert!(registry().cleanup.is_empty());
    assert_eq!(OUTSIDE_DROPS.load(Ordering::Relaxed), 0);
    let mut state = users::registry();
    assert!(!state.has_auth_ownership());
    state.phase = users::Phase::Unloaded;
    state.owner = None;
}
unsafe extern "C" fn clock() -> u64 {
    1000
}
unsafe extern "C" fn predicate(_: *mut raw::RedisModuleCtx) -> i32 {
    0
}
unsafe extern "C" fn handle(_: *mut raw::RedisModuleCtx) -> *mut raw::RedisModuleBlockedClient {
    ptr::null_mut()
}
unsafe extern "C" fn private(_: *mut raw::RedisModuleCtx) -> *mut c_void {
    ptr::null_mut()
}
unsafe extern "C" fn block(
    _: *mut raw::RedisModuleCtx,
    _: raw::RedisModuleAuthCallback,
    _: Option<unsafe extern "C" fn(*mut raw::RedisModuleCtx, *mut c_void)>,
) -> *mut raw::RedisModuleBlockedClient {
    ptr::null_mut()
}
unsafe extern "C" fn set_private(_: *mut raw::RedisModuleBlockedClient, _: *mut c_void) {}
unsafe extern "C" fn disconnect(
    _: *mut raw::RedisModuleBlockedClient,
    _: raw::RedisModuleDisconnectFunc,
) {
}
unsafe extern "C" fn create(
    _: *mut raw::RedisModuleCtx,
    _: i64,
    _: raw::RedisModuleTimerProc,
    _: *mut c_void,
) -> u64 {
    0
}
unsafe extern "C" fn stop(_: *mut raw::RedisModuleCtx, id: u64, data: *mut *mut c_void) -> i32 {
    STOPS.fetch_add(1, Ordering::Relaxed);
    TIMER_ID.store(id, Ordering::Relaxed);
    unsafe {
        *data = TIMER_PRIVATE.load(Ordering::Relaxed) as *mut c_void;
    }
    raw::REDISMODULE_OK as i32
}
unsafe extern "C" fn unblock(_: *mut raw::RedisModuleBlockedClient, data: *mut c_void) -> i32 {
    UNBLOCKS.fetch_add(1, Ordering::Relaxed);
    unsafe { freed(ptr::null_mut(), data) };
    if let Some(job) = unsafe { from_cookie(data) } {
        let inner = job.inner();
        SUBMIT_OWNED.store(
            !inner.native_live && inner.submitting == 1 && SUBMISSIONS.load(Ordering::Relaxed) == 1,
            Ordering::Release,
        );
    }
    IN_SUBMIT.store(true, Ordering::Release);
    let start = std::time::Instant::now();
    while HOLD_SUBMIT.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(5) {
        std::thread::yield_now();
    }
    raw::REDISMODULE_OK as i32
}
fn api() -> Api {
    Api {
        clock,
        block,
        set_private,
        disconnect,
        unblock,
        create_timer: create,
        stop_timer: stop,
        reply: predicate,
        timeout: predicate,
        disconnected: predicate,
        get_handle: handle,
        get_private: private,
        flags: predicate,
        info: client_info,
    }
}
fn job(starter: Option<Starter>, producers: usize) -> Arc<Job> {
    let owner = users::registry().owner.clone().unwrap();
    let mut state = registry();
    state.next += 1;
    let key = state.next;
    let job = Arc::new(Job {
        client: users::ClientId(7),
        owner,
        receipt_generation: 1,
        deadline: 10000,
        cookie: Box::new(Cookie { key }),
        api: api(),
        inner: Mutex::new(Inner {
            state: NativeState::Preparing,
            handle: 0,
            winner: false,
            outcome: None,
            starter,
            native_live: false,
            submitting: 0,
            producers,
            timer: None,
            timer_unknown: false,
        }),
    });
    state.jobs.insert(key, job.clone());
    OWNED.fetch_add(1, Ordering::Relaxed);
    job
}
fn target(drops: Arc<AtomicUsize>) -> AuthTarget {
    let owner = users::registry().owner.clone().unwrap();
    let probe = Probe(drops);
    AuthTarget::module_user(users::ModuleUserHandle {
        owner,
        generation: 1,
    })
    .on_changed(Box::new(move |_| drop(probe)))
}

#[test]
fn abandoned_and_refused_async_captures_require_ordinary_cleanup() {
    let _serial = setup();
    let drops = Arc::new(AtomicUsize::new(0));
    let probe = Probe(drops.clone());
    drop(DeferredAuth::new(Duration::from_secs(1), move |_| {
        drop(probe)
    }));
    let mut target = target(drops.clone());
    target.callback.ordinary_async = true;
    drop(target);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(users::NativeOperation::begin());
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(users::registry().has_auth_ownership());
    reap();
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    finish();
}

#[test]
fn failed_attachment_and_panicking_capture_retire_only_under_reaper_lease() {
    let _serial = setup();
    let drops = Arc::new(AtomicUsize::new(0));
    struct Panicking(Arc<AtomicUsize>);
    impl Drop for Panicking {
        fn drop(&mut self) {
            if !registry().executing {
                OUTSIDE_DROPS.fetch_add(1, Ordering::Relaxed);
            }
            self.0.fetch_add(1, Ordering::Relaxed);
            panic!("fixed cleanup panic");
        }
    }
    let capture = Panicking(drops.clone());
    let mut callback = users::PendingCallback::new(Some(Box::new(move |_| drop(capture))));
    callback.ordinary_async = true;
    users::attach_identity(
        users::ClientId(7),
        callback,
        false,
        users::AttachmentOrigin::NativeAuth,
        |_, _, _| raw::REDISMODULE_ERR as i32,
    )
    .expect_err("native refused inert attachment");
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(users::registry().subscriptions.is_empty());
    drop(users::NativeOperation::begin());
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    reap();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(users::LIVE_SUBSCRIPTIONS.load(Ordering::Relaxed), 0);
    finish();
}

#[test]
fn missing_attachment_api_retains_async_target_from_method_entry() {
    let _serial = setup();
    assert!(unsafe { raw::RedisModule_AuthenticateClientWithUser }.is_none());
    let drops = Arc::new(AtomicUsize::new(0));
    let mut selected = target(drops.clone());
    selected.callback.ordinary_async = true;
    let invocation = Invocation {
        ctx: ptr::null_mut(),
        client: users::ClientId(7),
    };
    invocation
        .attach(selected, &std::cell::Cell::new(false))
        .expect_err("missing inert API");
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    reap();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    finish();
}

#[test]
fn block_result_classes_keep_unknown_and_committed_ownership_distinct() {
    let _serial = setup();
    for (handle, proof, state) in [
        (0, true, NativeState::Preparing),
        (1, true, NativeState::Blocked),
        (1, false, NativeState::Unknown),
        (0, false, NativeState::Unknown),
    ] {
        let drops = Arc::new(AtomicUsize::new(0));
        let probe = Probe(drops.clone());
        let job = job(Some(Box::new(move |_| drop(probe))), 0);
        assert_eq!(record_block_result(&job, handle, proof), state);
        if state == NativeState::Blocked {
            choose(&job, Outcome::Deny(PublicAuthError::default()));
        } else if state == NativeState::Unknown {
            choose(&job, Outcome::Deny(PublicAuthError::default()));
            collect_ready(ptr::null_mut());
            assert!(registry().jobs.contains_key(&job.cookie.key));
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            // Synthetic fixture teardown only. Production has no unknown-state
            // repair, private free, counter adjustment or requeue path.
            let mut inner = job.inner();
            inner.state = NativeState::Freed;
            inner.native_live = false;
        }
        drop(job);
        reap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
    assert_eq!(UNBLOCKS.load(Ordering::Relaxed), 1);
    finish();
}

#[test]
fn free_before_submit_return_retains_producer_and_late_outcome() {
    let _serial = setup();
    let drops = Arc::new(AtomicUsize::new(0));
    let job = job(None, 1);
    record_block_result(&job, 1, true);
    let completion = AuthCompletion {
        job: Some(job.clone()),
    };
    let selected = target(drops.clone());
    HOLD_SUBMIT.store(true, Ordering::Release);
    let worker = std::thread::spawn(move || completion.accept(selected));
    let start = std::time::Instant::now();
    while !IN_SUBMIT.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(2) {
        std::thread::yield_now();
    }
    let entered = IN_SUBMIT.load(Ordering::Acquire);
    collect_ready(ptr::null_mut());
    let retained = registry().jobs.contains_key(&job.cookie.key);
    let before = drops.load(Ordering::Relaxed);
    HOLD_SUBMIT.store(false, Ordering::Release);
    worker.join().unwrap();
    assert!(
        registry().cleanup.is_empty(),
        "consuming the token must not retire a second denial from Drop"
    );
    assert!(entered && retained && SUBMIT_OWNED.load(Ordering::Acquire));
    assert_eq!(before, 0);
    // A losing worker outcome is also retained, rather than dropped on submit.
    let mut late = target(drops.clone());
    late.callback.ordinary_async = true;
    choose(&job, Outcome::Accept(late));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(job);
    reap();
    assert_eq!(drops.load(Ordering::Relaxed), 2);
    assert_eq!(UNBLOCKS.load(Ordering::Relaxed), 1);
    finish();
}

#[test]
fn timer_zero_key_and_firing_timer_have_native_ownership() {
    let _serial = setup();
    let first = job(None, 0);
    record_block_result(&first, 1, true);
    choose(&first, Outcome::Deny(PublicAuthError::default()));
    first.inner().timer = Some(0);
    TIMER_PRIVATE.store(first.private() as usize, Ordering::Relaxed);
    drop(first);
    reap();
    assert_eq!(STOPS.load(Ordering::Relaxed), 1);
    assert_eq!(TIMER_ID.load(Ordering::Relaxed), 0);
    STOPS.store(0, Ordering::Relaxed);
    let second = job(None, 0);
    record_block_result(&second, 2, true);
    second.inner().timer = Some(123);
    unsafe { deadline(ptr::null_mut(), second.private()) };
    assert!(second.inner().timer.is_none());
    drop(second);
    reap();
    assert_eq!(STOPS.load(Ordering::Relaxed), 0);
    finish();
}

#[test]
fn disconnect_ignores_retained_freed_rows_with_reused_native_addresses() {
    let _serial = setup();
    let handle = std::ptr::dangling_mut::<raw::RedisModuleBlockedClient>();
    let old = job(None, 0);
    record_block_result(&old, handle as usize, true);
    choose(&old, Outcome::Deny(PublicAuthError::default()));
    assert_eq!(old.inner().state, NativeState::Freed);
    let current = job(None, 0);
    record_block_result(&current, handle as usize, true);
    unsafe { disconnected(ptr::null_mut(), handle) };
    assert_eq!(current.inner().state, NativeState::Freed);
    assert_eq!(UNBLOCKS.load(Ordering::Relaxed), 2);
    drop(old);
    drop(current);
    finish();
}

#[test]
fn synchronous_replacement_preserves_async_delivery_policy() {
    let _serial = setup();
    let drops = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = Probe(drops.clone());
    let consumer = calls.clone();
    let mut callback = users::PendingCallback::new(Some(Box::new(move |_| {
        consumer.fetch_add(1, Ordering::Relaxed);
        drop(probe);
    })));
    callback.ordinary_async = true;
    let client = users::ClientId(7);
    users::attach_identity(
        client,
        callback,
        false,
        users::AttachmentOrigin::NativeAuth,
        |_, _, id| {
            unsafe { *id = 7 };
            raw::REDISMODULE_OK as i32
        },
    )
    .unwrap();
    let old = users::registry()
        .subscriptions
        .values_mut()
        .next()
        .unwrap()
        .cookie
        .as_mut() as *mut users::Cookie;
    let sync = Arc::new(AtomicUsize::new(0));
    let sync_consumer = sync.clone();
    let callback = users::PendingCallback::new(Some(Box::new(move |_| {
        sync_consumer.fetch_add(1, Ordering::Relaxed);
    })));
    users::attach_identity(
        client,
        callback,
        false,
        users::AttachmentOrigin::Command,
        |_, _, id| {
            unsafe {
                users::changed(7, old.cast());
                *id = 7
            };
            raw::REDISMODULE_OK as i32
        },
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(users::NativeOperation::begin());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let current = users::registry()
        .subscriptions
        .values_mut()
        .next()
        .unwrap()
        .cookie
        .as_mut() as *mut users::Cookie;
    unsafe { users::changed(7, current.cast()) };
    assert_eq!(sync.load(Ordering::Relaxed), 1);
    reap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    finish();
}

#[test]
fn deadline_validation_uses_checked_monotonic_arithmetic() {
    assert!(deadline_at(1, Duration::ZERO).is_err());
    assert!(deadline_at(u64::MAX - 100, Duration::from_millis(1)).is_err());
    assert!(deadline_at(1, Duration::from_secs(86401)).is_err());
    assert_eq!(
        deadline_at(1000, Duration::from_millis(2)).unwrap(),
        (3000, 2)
    );
}

#[test]
fn explicit_secret_copy_owns_binary_bytes_after_borrowed_storage_changes() {
    let mut username = vec![0, b'u', 255];
    let mut password = vec![b'p', 0, 255];
    let owned = {
        let request = AuthRequest {
            username: &username,
            password: super::super::Password {
                bytes: &password,
                invocation: std::marker::PhantomData,
            },
        };
        request.copy_for_async()
    };
    username.fill(1);
    password.fill(2);
    assert_eq!(owned.username(), &[0, b'u', 255]);
    assert_eq!(owned.password(), &[b'p', 0, 255]);
}

static INFO_MODE: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn client_info(data: *mut c_void, id: u64) -> i32 {
    let mode = INFO_MODE.load(Ordering::Relaxed);
    if mode == 2 {
        return raw::REDISMODULE_ERR as i32;
    }
    if !data.is_null() {
        let info = unsafe { &mut *data.cast::<raw::RedisModuleClientInfo>() };
        info.id = id;
        info.flags = if mode == 1 {
            raw::REDISMODULE_CLIENTINFO_FLAG_BLOCKED as u64
        } else {
            0
        };
    }
    raw::REDISMODULE_OK as i32
}
unsafe extern "C" fn yes(_: *mut raw::RedisModuleCtx) -> i32 {
    1
}
unsafe extern "C" fn busy_handle(
    _: *mut raw::RedisModuleCtx,
) -> *mut raw::RedisModuleBlockedClient {
    std::ptr::dangling_mut()
}
static FLAGS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn flags(_: *mut raw::RedisModuleCtx) -> i32 {
    FLAGS.load(Ordering::Relaxed) as i32
}
#[test]
fn sealed_initial_role_refuses_reply_loading_lua_multi_and_blocked_inputs() {
    let _serial = setup();
    let _boundary = super::super::BoundaryLease::begin();
    let _operation = users::NativeOperation::begin();
    let invocation = Invocation {
        ctx: std::ptr::dangling_mut(),
        client: users::ClientId(7),
    };
    INFO_MODE.store(0, Ordering::Relaxed);
    let mut selected = api();
    selected.flags = flags;
    FLAGS.store(0, Ordering::Relaxed);
    assert!(unsafe { initial_role(&invocation, selected) }.is_ok());
    for flag in [
        raw::REDISMODULE_CTX_FLAGS_LUA,
        raw::REDISMODULE_CTX_FLAGS_MULTI,
        raw::REDISMODULE_CTX_FLAGS_DENY_BLOCKING,
        raw::REDISMODULE_CTX_FLAGS_LOADING,
        raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING,
    ] {
        FLAGS.store(flag as usize, Ordering::Relaxed);
        assert!(unsafe { initial_role(&invocation, selected) }.is_err());
    }
    FLAGS.store(0, Ordering::Relaxed);
    for role in 0..4 {
        let mut selected = selected;
        match role {
            0 => selected.reply = yes,
            1 => selected.timeout = yes,
            2 => selected.disconnected = yes,
            _ => selected.get_handle = busy_handle,
        }
        assert!(unsafe { initial_role(&invocation, selected) }.is_err());
    }
    for mode in [1, 2] {
        INFO_MODE.store(mode, Ordering::Relaxed);
        assert!(unsafe { initial_role(&invocation, selected) }.is_err());
    }
    INFO_MODE.store(0, Ordering::Relaxed);
    drop(_operation);
    drop(_boundary);
    finish();
}

#[test]
fn resume_role_refuses_nested_native_and_loading_scopes() {
    let _serial = setup();
    let callback = CallbackLease::begin();
    let ctx = std::ptr::dangling_mut();
    let mut selected = api();
    selected.reply = yes;
    selected.flags = flags;
    FLAGS.store(0, Ordering::Relaxed);
    assert!(unsafe { resume_role(ctx, selected) }.is_ok());
    let operation = users::NativeOperation::begin();
    assert!(unsafe { resume_role(ctx, selected) }.is_err());
    drop(operation);
    let boundary = super::super::BoundaryLease::begin();
    assert!(unsafe { resume_role(ctx, selected) }.is_err());
    drop(boundary);
    for flag in [
        raw::REDISMODULE_CTX_FLAGS_LUA,
        raw::REDISMODULE_CTX_FLAGS_MULTI,
        raw::REDISMODULE_CTX_FLAGS_DENY_BLOCKING,
        raw::REDISMODULE_CTX_FLAGS_LOADING,
        raw::REDISMODULE_CTX_FLAGS_ASYNC_LOADING,
    ] {
        FLAGS.store(flag as usize, Ordering::Relaxed);
        assert!(unsafe { resume_role(ctx, selected) }.is_err());
    }
    FLAGS.store(0, Ordering::Relaxed);
    drop(callback);
    finish();
}
