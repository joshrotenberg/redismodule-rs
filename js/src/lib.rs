mod json_size;
mod watch_limits;

use json_size::serialized_json_bytes;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use watch_limits::{
    MAX_KEY_BYTES, MAX_NAME_BYTES, MAX_OWNER_BYTES, MAX_WATCHES, definition_bytes, definition_fits,
};

use redis_module::{
    AclPermissions, CallOptionsBuilder, CallReply, CallResult, CommandFlags, Context, ContextFlags,
    EventContext, KeyRange, NextArg, NotifyEvent, RedisError, RedisResult, RedisString, RedisValue,
    RegistrationContext, RuntimeContext, Status, redis_command, redis_module,
};
use rquickjs::context::intrinsic;
use rquickjs::{
    Context as JsContext, FromJs, Function, Object, Runtime, String as JsString, Value,
};

const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_FIELDS: usize = 1024;
const JS_HEAP_BYTES: usize = 8 * 1024 * 1024;
const JS_STACK_BYTES: usize = 256 * 1024;
const JS_DEADLINE: Duration = Duration::from_millis(25);
const MAX_LAST_ERROR_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WatchId {
    db: i32,
    name: Vec<u8>,
}

#[derive(Clone)]
struct Watch {
    generation: u64,
    source: Vec<u8>,
    destination: Vec<u8>,
    function: String,
    owner: Vec<u8>,
    runs: u64,
    successes: u64,
    failures: u64,
    coalesced: u64,
    skips: u64,
    pending: bool,
    last_duration_us: u64,
    last_error: String,
}

impl Watch {
    fn retained_bytes(&self, id: &WatchId) -> usize {
        id.name.len()
            + self.source.len()
            + self.destination.len()
            + self.function.len()
            + self.owner.len()
    }
}

#[derive(Default)]
struct Registry {
    watches: BTreeMap<WatchId, Watch>,
    pending: BTreeSet<(WatchId, u64)>,
    retained_bytes: usize,
    next_generation: u64,
    drain_scheduled: bool,
    draining: bool,
    stopping: bool,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    watches: BTreeMap::new(),
    pending: BTreeSet::new(),
    retained_bytes: 0,
    next_generation: 0,
    drain_scheduled: false,
    draining: false,
    stopping: false,
});

fn error_text(error: &RedisError) -> String {
    let mut text = error.to_string();
    if text.len() > MAX_LAST_ERROR_BYTES {
        let mut end = MAX_LAST_ERROR_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

fn writable_primary(ctx: &RuntimeContext) -> bool {
    let flags = ctx.get_flags();
    let stopping = REGISTRY.lock().unwrap().stopping;
    !stopping
        && flags.contains(ContextFlags::MASTER)
        && !flags.intersects(
            ContextFlags::LOADING
                | ContextFlags::ASYNC_LOADING
                | ContextFlags::READONLY
                | ContextFlags::REPLICATED,
        )
}

fn build_runtime(deadline: Instant) -> Result<(Runtime, JsContext), RedisError> {
    let runtime =
        Runtime::new().map_err(|_| RedisError::Str("ERR could not create JavaScript runtime"))?;
    runtime.set_memory_limit(JS_HEAP_BYTES);
    runtime.set_max_stack_size(JS_STACK_BYTES);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = JsContext::builder()
        .with::<intrinsic::Date>()
        .with::<intrinsic::Eval>()
        .with::<intrinsic::RegExpCompiler>()
        .with::<intrinsic::RegExp>()
        .with::<intrinsic::Json>()
        .with::<intrinsic::Proxy>()
        .with::<intrinsic::MapSet>()
        .with::<intrinsic::TypedArrays>()
        .build(&runtime)
        .map_err(|_| RedisError::Str("ERR could not create JavaScript context"))?;
    Ok((runtime, context))
}

fn validate_function(source: &str) -> Result<(), RedisError> {
    let deadline = Instant::now() + JS_DEADLINE;
    let (runtime, context) = build_runtime(deadline)?;
    let wrapped_source = format!("({source})");
    let result = context.with(|js| {
        js.eval::<Function<'_>, _>(wrapped_source.as_bytes())
            .map(|_| ())
            .map_err(|_| RedisError::Str("ERR invalid JavaScript function expression"))
    });
    drop(context);
    drop(runtime);
    if Instant::now() >= deadline {
        Err(RedisError::Str("ERR JavaScript projection timed out"))
    } else {
        result
    }
}

fn project(input: Vec<u8>, source: &str) -> Result<BTreeMap<String, String>, RedisError> {
    project_until(input, source, Instant::now() + JS_DEADLINE)
}

fn project_until(
    input: Vec<u8>,
    source: &str,
    outer_deadline: Instant,
) -> Result<BTreeMap<String, String>, RedisError> {
    let deadline = std::cmp::min(Instant::now() + JS_DEADLINE, outer_deadline);
    let (runtime, context) = build_runtime(deadline)?;

    // Promise is intentionally absent: this command is synchronous and never runs jobs.
    // No loader is installed, and the crate does not enable rquickjs's loader or futures features.
    let wrapped_source = format!("({source})");
    let projection = context.with(|js| -> Result<BTreeMap<String, String>, RedisError> {
        let input = js
            .json_parse(input)
            .map_err(|_| RedisError::Str("ERR source must contain a valid JSON object"))?;
        if !input.is_object() || input.is_array() {
            return Err(RedisError::Str("ERR source must contain a JSON object"));
        }

        let function: Function<'_> = js
            .eval(wrapped_source.as_bytes())
            .map_err(|_| RedisError::Str("ERR invalid JavaScript function expression"))?;
        let output: Value<'_> = function
            .call((input,))
            .map_err(|_| RedisError::Str("ERR JavaScript projection failed"))?;
        if !output.is_object() || output.is_array() {
            return Err(RedisError::Str("ERR projection must return a flat object"));
        }

        let object = Object::from_value(output)
            .map_err(|_| RedisError::Str("ERR projection must return a flat object"))?;
        let mut fields = BTreeMap::new();
        let mut raw_output_bytes = 0usize;
        for entry in object.props::<JsString<'_>, Value<'_>>() {
            if fields.len() == MAX_OUTPUT_FIELDS {
                return Err(RedisError::Str("ERR projection has too many fields"));
            }
            let (name, value): (JsString<'_>, Value<'_>) =
                entry.map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            if !value.is_string() {
                return Err(RedisError::Str("ERR projection values must all be strings"));
            }
            let value = JsString::from_js(&js, value)
                .map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            // CString length is safe to inspect, but its string accessors assume
            // QuickJS returned UTF-8. JavaScript can contain unmatched UTF-16
            // surrogates, so copy only through JsString's checked conversion.
            let value_bytes = value
                .clone()
                .to_cstring()
                .map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            let name_bytes = name
                .clone()
                .to_cstring()
                .map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            raw_output_bytes = raw_output_bytes
                .checked_add(name_bytes.len())
                .and_then(|size| size.checked_add(value_bytes.len()))
                .ok_or(RedisError::Str("ERR projection output is too large"))?;
            if raw_output_bytes > MAX_OUTPUT_BYTES {
                return Err(RedisError::Str("ERR projection output is too large"));
            }
            let name = name
                .to_string()
                .map_err(|_| RedisError::Str("ERR projection output must be UTF-8"))?;
            let value = value
                .to_string()
                .map_err(|_| RedisError::Str("ERR projection output must be UTF-8"))?;
            fields.insert(name, value);
        }
        if fields.is_empty() {
            return Err(RedisError::Str("ERR projection must not be empty"));
        }

        // Account for canonical compact JSON entirely in host code. Re-entering
        // the VM here would let script-controlled prototypes affect validation.
        if serialized_json_bytes(&fields)
            .is_none_or(|serialized_bytes| serialized_bytes > MAX_OUTPUT_BYTES)
        {
            return Err(RedisError::Str("ERR projection output is too large"));
        }

        Ok(fields)
    });

    // All context-owned JavaScript values are gone before the context and runtime.
    drop(context);
    drop(runtime);
    if Instant::now() >= deadline {
        return Err(RedisError::Str("ERR JavaScript projection timed out"));
    }
    projection
}

fn read_source(ctx: &RuntimeContext, key: &RedisString) -> Result<Vec<u8>, RedisError> {
    let options = CallOptionsBuilder::new()
        .no_writes()
        .script_mode()
        .verify_acl()
        .errors_as_replies()
        .build();
    let reply: CallResult<'_> = ctx.call_ext("GET", &options, &[key]);
    match reply? {
        CallReply::String(value) => {
            let bytes = value.as_bytes();
            if bytes.len() > MAX_INPUT_BYTES {
                return Err(RedisError::Str("ERR source JSON is too large"));
            }
            std::str::from_utf8(bytes)
                .map_err(|_| RedisError::Str("ERR source must be UTF-8 JSON"))?;
            Ok(bytes.to_vec())
        }
        CallReply::Null(_) => Err(RedisError::Str("ERR source key does not exist")),
        _ => Err(RedisError::Str(
            "ERR source GET returned an unexpected reply",
        )),
    }
}

fn write_projection(
    ctx: &RuntimeContext,
    destination: &RedisString,
    fields: &BTreeMap<String, String>,
) -> RedisResult {
    let mut args: Vec<&[u8]> = Vec::with_capacity(1 + fields.len() * 2);
    args.push(destination.as_slice());
    for (name, value) in fields {
        args.push(name.as_bytes());
        args.push(value.as_bytes());
    }
    let options = CallOptionsBuilder::new()
        .script_mode()
        .verify_acl()
        .verify_oom()
        .errors_as_replies()
        .replicate()
        .build();
    let reply: CallResult<'_> = ctx.call_ext("HSET", &options, args.as_slice());
    match reply? {
        CallReply::I64(value) => Ok(RedisValue::Integer(value.to_i64())),
        _ => Err(RedisError::Str("ERR HSET returned an unexpected reply")),
    }
}

fn owner(ctx: &Context) -> Result<Vec<u8>, RedisError> {
    let owner = ctx.get_current_user().ok_or(RedisError::Str(
        "ERR no ACL user is attached to this client",
    ))?;
    if owner.len() > MAX_OWNER_BYTES {
        return Err(RedisError::Str("ERR ACL username is too long"));
    }
    Ok(owner.as_slice().to_vec())
}

fn validate_name(name: &RedisString) -> Result<Vec<u8>, RedisError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(RedisError::Str(
            "ERR watch name must be 1 to 128 UTF-8 bytes",
        ));
    }
    name.try_as_str()
        .map_err(|_| RedisError::Str("ERR watch name must be UTF-8"))?;
    Ok(name.as_slice().to_vec())
}

fn check_owner(current: &[u8], watch: &Watch) -> Result<(), RedisError> {
    if current == watch.owner {
        Ok(())
    } else {
        Err(RedisError::Str("ERR no such watch or not owner"))
    }
}

// Redis 7.2's user lookup accepts disabled accounts. Inspect only the first
// ACL token, which Redis emits as on/off, and release all callback-local handles.
// Missing APIs, missing users and unexpected encodings fail closed.
fn check_owner_enabled(owner: &RedisString) -> Result<(), RedisError> {
    use redis_module::raw;
    let unavailable = || RedisError::Str("ERR owner enabled-state API is unavailable");
    let (get_user, free_user, describe, bytes, free_string) = unsafe {
        (
            raw::RedisModule_GetModuleUserFromUserName.ok_or_else(unavailable)?,
            raw::RedisModule_FreeModuleUser.ok_or_else(unavailable)?,
            raw::RedisModule_GetModuleUserACLString.ok_or_else(unavailable)?,
            raw::RedisModule_StringPtrLen.ok_or_else(unavailable)?,
            raw::RedisModule_FreeString.ok_or_else(unavailable)?,
        )
    };
    let enabled = unsafe {
        let user = get_user(owner.as_ptr());
        if user.is_null() {
            return Err(RedisError::Str("ERR owner does not exist or is disabled"));
        }
        let acl = describe(user);
        free_user(user);
        if acl.is_null() {
            false
        } else {
            let mut len = 0;
            let data = bytes(acl, &mut len);
            // No ACL-sized copy or scan: the enabled token is at most three bytes.
            let enabled = !data.is_null()
                && len >= 3
                && std::slice::from_raw_parts(data.cast::<u8>(), 3) == b"on ";
            free_string(ptr::null_mut(), acl);
            enabled
        }
    };
    if enabled {
        Ok(())
    } else {
        Err(RedisError::Str("ERR owner does not exist or is disabled"))
    }
}

fn preflight(
    ctx: &RuntimeContext,
    owner: &[u8],
    source: &[u8],
    destination: &[u8],
) -> Result<(), RedisError> {
    let owner = ctx.create_string(owner);
    check_owner_enabled(&owner)?;
    let get = [ctx.create_string("GET"), ctx.create_string(source)];
    let hset = [
        ctx.create_string("HSET"),
        ctx.create_string(destination),
        ctx.create_string("field"),
        ctx.create_string("value"),
    ];
    ctx.acl_check_command_permission(&owner, &get)?;
    ctx.acl_check_command_permission(&owner, &hset)?;
    ctx.acl_check_key_permission(&owner, &get[1], &AclPermissions::ACCESS)?;
    ctx.acl_check_key_permission(
        &owner,
        &hset[1],
        &(AclPermissions::INSERT | AclPermissions::UPDATE),
    )?;
    Ok(())
}

fn check_registration(
    registry: &Registry,
    id: &WatchId,
    source: &[u8],
    destination: &[u8],
    retained: usize,
) -> Result<(), RedisError> {
    if registry.watches.contains_key(id) {
        return Err(RedisError::Str("ERR watch name already exists"));
    }
    if registry.watches.len() == MAX_WATCHES {
        return Err(RedisError::Str("ERR watch limit reached"));
    }
    if !definition_fits(registry.retained_bytes, retained) {
        return Err(RedisError::Str(
            "ERR retained watch definition limit reached",
        ));
    }
    if registry.watches.iter().any(|(other_id, watch)| {
        other_id.db == id.db
            && (watch.source == source
                || watch.source == destination
                || watch.destination == source
                || watch.destination == destination)
    }) {
        return Err(RedisError::Str("ERR watch keys overlap an existing watch"));
    }
    Ok(())
}

fn js_watch(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter();
    let _command = args.next_arg()?;
    let name = args.next_arg()?;
    let source = args.next_arg()?;
    let destination = args.next_arg()?;
    let function = args.next_arg()?;
    args.done()?;

    if ctx.get_flags().contains(ContextFlags::CLUSTER) {
        return Err(RedisError::Str("ERR JS.WATCH requires standalone Redis"));
    }
    if source.as_slice() == destination.as_slice() {
        return Err(RedisError::Str(
            "ERR source and destination keys must be different",
        ));
    }
    if source.len() > MAX_KEY_BYTES || destination.len() > MAX_KEY_BYTES {
        return Err(RedisError::Str("ERR watch keys must be at most 1024 bytes"));
    }
    if function.len() > MAX_SOURCE_BYTES {
        return Err(RedisError::Str("ERR JavaScript source is too large"));
    }
    let name = validate_name(&name)?;
    let owner = owner(ctx)?;
    let function_text = function
        .try_as_str()
        .map_err(|_| RedisError::Str("ERR JavaScript source must be UTF-8"))?;
    let id = WatchId {
        db: ctx.get_selected_db(),
        name,
    };
    let retained = definition_bytes(&[
        id.name.len(),
        source.len(),
        destination.len(),
        function.len(),
        owner.len(),
    ])
    .ok_or(RedisError::Str(
        "ERR retained watch definition limit reached",
    ))?;
    {
        let registry = REGISTRY.lock().unwrap();
        check_registration(
            &registry,
            &id,
            source.as_slice(),
            destination.as_slice(),
            retained,
        )?;
    }
    validate_function(function_text)?;
    preflight(ctx, &owner, source.as_slice(), destination.as_slice())?;

    let mut registry = REGISTRY.lock().unwrap();
    check_registration(
        &registry,
        &id,
        source.as_slice(),
        destination.as_slice(),
        retained,
    )?;
    registry.next_generation = registry.next_generation.wrapping_add(1);
    let generation = registry.next_generation;
    registry.retained_bytes += retained;
    registry.watches.insert(
        id,
        Watch {
            generation,
            source: source.as_slice().to_vec(),
            destination: destination.as_slice().to_vec(),
            function: function_text.to_owned(),
            owner,
            runs: 0,
            successes: 0,
            failures: 0,
            coalesced: 0,
            skips: 0,
            pending: false,
            last_duration_us: 0,
            last_error: String::new(),
        },
    );
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn js_unwatch(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter();
    let _command = args.next_arg()?;
    let name = validate_name(&args.next_arg()?)?;
    args.done()?;
    let current = owner(ctx)?;
    let id = WatchId {
        db: ctx.get_selected_db(),
        name,
    };
    let mut registry = REGISTRY.lock().unwrap();
    let watch = registry
        .watches
        .get(&id)
        .ok_or(RedisError::Str("ERR no such watch or not owner"))?;
    check_owner(&current, watch)?;
    let watch = registry.watches.remove(&id).unwrap();
    registry.retained_bytes -= watch.retained_bytes(&id);
    registry.pending.remove(&(id, watch.generation));
    Ok(RedisValue::SimpleStringStatic("OK"))
}

fn js_info(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter();
    let _command = args.next_arg()?;
    let name = validate_name(&args.next_arg()?)?;
    args.done()?;
    let current = owner(ctx)?;
    let id = WatchId {
        db: ctx.get_selected_db(),
        name,
    };
    let registry = REGISTRY.lock().unwrap();
    let watch = registry
        .watches
        .get(&id)
        .ok_or(RedisError::Str("ERR no such watch or not owner"))?;
    check_owner(&current, watch)?;
    Ok(RedisValue::InsertionOrderedMap(vec![
        ("source".into(), watch.source.clone().into()),
        ("destination".into(), watch.destination.clone().into()),
        ("runs".into(), (watch.runs as i64).into()),
        ("successes".into(), (watch.successes as i64).into()),
        ("failures".into(), (watch.failures as i64).into()),
        ("coalesced".into(), (watch.coalesced as i64).into()),
        ("skips".into(), (watch.skips as i64).into()),
        ("pending".into(), watch.pending.into()),
        (
            "last_duration_us".into(),
            (watch.last_duration_us as i64).into(),
        ),
        ("last_error".into(), watch.last_error.clone().into()),
    ]))
}

fn execute_watch(ctx: &EventContext, id: &WatchId, generation: u64, drain_deadline: Instant) {
    let watch = {
        let mut registry = REGISTRY.lock().unwrap();
        let Some(watch) = registry.watches.get_mut(id) else {
            return;
        };
        if watch.generation != generation {
            return;
        }
        watch.pending = false;
        watch.clone()
    };
    let started = Instant::now();
    let result = if !writable_primary(ctx) {
        Err(RedisError::Str(
            "ERR projection skipped because server is not a writable primary",
        ))
    } else if Instant::now() >= drain_deadline {
        Err(RedisError::Str(
            "ERR shared projection drain budget exhausted",
        ))
    } else {
        ctx.with_db(id.db, |ctx| {
            let owner = ctx.create_string(watch.owner.clone());
            let source = ctx.create_string(watch.source.clone());
            let destination = ctx.create_string(watch.destination.clone());
            let watch_command = [
                ctx.create_string("JS.WATCH"),
                ctx.create_string(id.name.clone()),
                source.safe_clone(ctx),
                destination.safe_clone(ctx),
                ctx.create_string(watch.function.clone()),
            ];
            ctx.acl_check_command_permission(&owner, &watch_command)
                .and_then(|()| preflight(ctx, &watch.owner, &watch.source, &watch.destination))
                .and_then(|()| {
                    ctx.with_call_user(&owner, |ctx| read_source(ctx, &source))
                        .and_then(|v| v)
                })
                .and_then(|input| project_until(input, &watch.function, drain_deadline))
                .and_then(|fields| {
                    if Instant::now() >= drain_deadline {
                        return Err(RedisError::Str(
                            "ERR shared projection drain budget exhausted",
                        ));
                    }
                    ctx.with_call_user(&owner, |ctx| write_projection(ctx, &destination, &fields))
                        .and_then(|v| v)
                        .map(|_| ())
                })
        })
        .and_then(|result| result)
    };
    let elapsed = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    let mut registry = REGISTRY.lock().unwrap();
    let Some(current) = registry.watches.get_mut(id) else {
        return;
    };
    if current.generation != generation {
        return;
    }
    current.runs = current.runs.saturating_add(1);
    current.last_duration_us = elapsed;
    match result {
        Ok(()) => {
            current.successes = current.successes.saturating_add(1);
            current.last_error.clear();
        }
        Err(error) => {
            current.failures = current.failures.saturating_add(1);
            if error.to_string().contains("skipped")
                || error.to_string().contains("budget exhausted")
            {
                current.skips = current.skips.saturating_add(1);
            }
            current.last_error = error_text(&error);
        }
    }
}

fn drain(ctx: &EventContext) {
    let drain_deadline = Instant::now() + JS_DEADLINE;
    let pending: Vec<_> = {
        let mut registry = REGISTRY.lock().unwrap();
        registry.draining = true;
        std::mem::take(&mut registry.pending)
            .into_iter()
            .take(MAX_WATCHES)
            .collect()
    };
    for (id, generation) in pending {
        execute_watch(ctx, &id, generation, drain_deadline);
    }
    let mut registry = REGISTRY.lock().unwrap();
    registry.draining = false;
    registry.drain_scheduled = false;
}

fn on_key_event(ctx: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
    if !writable_primary(ctx) {
        return;
    }
    let db = ctx.get_selected_db();
    let should_schedule = {
        let mut registry = REGISTRY.lock().unwrap();
        // GET may lazily expire a source. That event is already represented by
        // this drain; never enqueue a continuation while executing it.
        if registry.draining {
            return;
        }
        let ids: Vec<_> = registry
            .watches
            .iter()
            .filter(|(id, watch)| id.db == db && watch.source == key && watch.destination != key)
            .map(|(id, watch)| (id.clone(), watch.generation))
            .collect();
        if ids.is_empty() {
            return;
        }
        for (id, generation) in ids {
            let watch = registry.watches.get_mut(&id).unwrap();
            if watch.pending {
                watch.coalesced = watch.coalesced.saturating_add(1);
            } else {
                watch.pending = true;
                registry.pending.insert((id, generation));
            }
        }
        if registry.drain_scheduled {
            false
        } else {
            registry.drain_scheduled = true;
            true
        }
    };
    if should_schedule && !schedule_drain(ctx) {
        let mut registry = REGISTRY.lock().unwrap();
        registry.drain_scheduled = false;
        let pending = std::mem::take(&mut registry.pending);
        for (id, generation) in pending {
            if let Some(watch) = registry
                .watches
                .get_mut(&id)
                .filter(|w| w.generation == generation)
            {
                watch.pending = false;
                watch.failures = watch.failures.saturating_add(1);
                watch.skips = watch.skips.saturating_add(1);
                watch.last_error = "ERR post-notification job submission rejected".into();
            }
        }
    }
}

#[cfg(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
))]
fn schedule_drain(ctx: &EventContext) -> bool {
    ctx.add_post_notification_job(drain) == Status::Ok
}

#[cfg(not(any(
    feature = "min-redis-compatibility-version-8-0",
    feature = "min-redis-compatibility-version-7-4",
    feature = "min-redis-compatibility-version-7-2"
)))]
fn schedule_drain(ctx: &EventContext) -> bool {
    matches!(ctx.add_post_notification_job(drain), Ok(Status::Ok))
}

fn set_watch_key_specs(ctx: &RegistrationContext<'_>) -> Result<(), RedisError> {
    use redis_module::raw;

    let get_command = unsafe { raw::RedisModule_GetCommand }
        .ok_or(RedisError::Str("ERR command metadata API is unavailable"))?;
    let set_info = unsafe { raw::RedisModule_SetCommandInfo }
        .ok_or(RedisError::Str("ERR command metadata API is unavailable"))?;
    let name = CString::new("js.watch").unwrap();
    let command = unsafe { get_command(ctx.get_raw(), name.as_ptr()) };
    if command.is_null() {
        return Err(RedisError::Str("ERR JS.WATCH command was not found"));
    }
    let index = |pos| raw::RedisModuleCommandKeySpec__bindgen_ty_1 {
        index: raw::RedisModuleCommandKeySpec__bindgen_ty_1__bindgen_ty_1 { pos },
    };
    let one_key = raw::RedisModuleCommandKeySpec__bindgen_ty_2 {
        range: raw::RedisModuleCommandKeySpec__bindgen_ty_2__bindgen_ty_1 {
            lastkey: 0,
            keystep: 1,
            limit: 0,
        },
    };
    let mut specs = [
        raw::RedisModuleCommandKeySpec {
            notes: ptr::null(),
            flags: u64::from(raw::REDISMODULE_CMD_KEY_RO | raw::REDISMODULE_CMD_KEY_ACCESS),
            begin_search_type: raw::RedisModuleKeySpecBeginSearchType_REDISMODULE_KSPEC_BS_INDEX,
            bs: index(2),
            find_keys_type: raw::RedisModuleKeySpecFindKeysType_REDISMODULE_KSPEC_FK_RANGE,
            fk: one_key,
        },
        raw::RedisModuleCommandKeySpec {
            notes: ptr::null(),
            flags: u64::from(raw::REDISMODULE_CMD_KEY_RW | raw::REDISMODULE_CMD_KEY_UPDATE),
            begin_search_type: raw::RedisModuleKeySpecBeginSearchType_REDISMODULE_KSPEC_BS_INDEX,
            bs: index(3),
            find_keys_type: raw::RedisModuleKeySpecFindKeysType_REDISMODULE_KSPEC_FK_RANGE,
            fk: one_key,
        },
        unsafe { std::mem::zeroed() },
    ];
    let version = raw::RedisModuleCommandInfoVersion {
        version: 1,
        sizeof_historyentry: std::mem::size_of::<raw::RedisModuleCommandHistoryEntry>(),
        sizeof_keyspec: std::mem::size_of::<raw::RedisModuleCommandKeySpec>(),
        sizeof_arg: std::mem::size_of::<raw::RedisModuleCommandArg>(),
    };
    let info = raw::RedisModuleCommandInfo {
        version: &version,
        summary: ptr::null(),
        complexity: ptr::null(),
        since: ptr::null(),
        history: ptr::null_mut(),
        tips: ptr::null(),
        arity: 5,
        key_specs: specs.as_mut_ptr(),
        args: ptr::null_mut(),
    };
    if unsafe { set_info(command, &info) } == raw::Status::Ok as i32 {
        Ok(())
    } else {
        Err(RedisError::Str("ERR failed to set JS.WATCH key metadata"))
    }
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    // A retained native image can keep Rust statics across Redis module loads.
    // Clear owned definitions before any fallible load work so failed loads
    // also begin and end empty.
    *REGISTRY.lock().unwrap() = Registry::default();
    if unsafe { redis_module::raw::RedisModule_AddPostNotificationJob }.is_none() {
        ctx.log_warning("redis_module_js requires Redis 7.2+ post-notification jobs");
        return Status::Err;
    }
    if let Err(error) = set_watch_key_specs(ctx) {
        ctx.log_warning(&error.to_string());
        return Status::Err;
    }
    Status::Ok
}

fn deinit(_: &EventContext) -> Status {
    let mut registry = REGISTRY.lock().unwrap();
    // Redis still owns the callback even if UNWATCH emptied pending.
    // Refusal must leave definitions active so the accepted callback can finish.
    if registry.drain_scheduled {
        return Status::Err;
    }
    registry.stopping = true;
    registry.watches.clear();
    registry.pending.clear();
    registry.retained_bytes = 0;
    Status::Ok
}

fn js_project(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    let mut args = args.into_iter();
    let _command = args.next_arg()?;
    let source_key = args.next_arg()?;
    let destination_key = args.next_arg()?;
    let function = args.next_arg()?;
    args.done()?;

    if source_key.as_slice() == destination_key.as_slice() {
        return Err(RedisError::Str(
            "ERR source and destination keys must be different",
        ));
    }
    if function.len() > MAX_SOURCE_BYTES {
        return Err(RedisError::Str("ERR JavaScript source is too large"));
    }
    let function = function
        .try_as_str()
        .map_err(|_| RedisError::Str("ERR JavaScript source must be UTF-8"))?;

    let input = read_source(ctx, &source_key)?;
    let fields = project(input, function)?;
    write_projection(ctx, &destination_key, &fields)
}

redis_module! {
    name: "redis_module_js",
    version: redis_module::module_version!(),
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    init: init,
    deinit: deinit,
    commands: [
        redis_command!("js.project", js_project)
            .flags(CommandFlags::WRITE | CommandFlags::DENY_OOM | CommandFlags::DENY_SCRIPT | CommandFlags::MAY_REPLICATE)
            .keys(KeyRange::new(1).last(2))
            .acl_categories("write"),
        redis_command!("js.watch", js_watch)
            .flags(CommandFlags::WRITE | CommandFlags::DENY_SCRIPT | CommandFlags::NO_CLUSTER)
            .acl_categories("write"),
        redis_command!("js.unwatch", js_unwatch)
            .flags(CommandFlags::WRITE | CommandFlags::DENY_SCRIPT)
            .acl_categories("write"),
        redis_command!("js.info", js_info)
            .flags(CommandFlags::READ_ONLY | CommandFlags::DENY_SCRIPT)
            .acl_categories("read"),
    ],
    event_handlers: [[@GENERIC @STRING @LIST @SET @HASH @ZSET @EXPIRED @EVICTED @STREAM @MODULE: on_key_event]],
}
