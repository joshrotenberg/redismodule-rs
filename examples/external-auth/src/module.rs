//! Experimental module. See ../README.md for bootstrap and drain commands.
use external_auth::{Endpoint, Provider, Role, Workers, valid_credentials, valid_username};
use redis_module::{
    AuthContext, AuthDecision, AuthRequest, AuthTarget, Context, EventContext, ModuleUserHandle,
    PublicAuthError, RedisError, RedisGILGuard, RedisResult, RedisString, Status, redis_command,
    redis_module,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const AUTH_TIMEOUT: Duration = Duration::from_millis(200);
const WORK_TIMEOUT: Duration = Duration::from_millis(500);
const MAPPING_LIMIT: usize = 8;
const CONFIG_ERROR: RedisError = RedisError::Str("ERR external auth configuration refused");
const PENDING: RedisError = RedisError::Str("ERR external auth drain pending");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Inactive,
    Configured,
    Active,
    Draining,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Configured => "configured",
            Self::Active => "active",
            Self::Draining => "draining",
        }
    }
}
struct Config {
    endpoint: Endpoint,
    reader: Option<ModuleUserHandle>,
    writer: Option<ModuleUserHandle>,
    complete: bool,
    prepared: bool,
    mappings: BTreeMap<Vec<u8>, Role>,
    provider: Option<Provider>,
}
struct App {
    phase: Phase,
    config: Option<Config>,
    workers: Option<Workers>,
}
static APP: RedisGILGuard<App> = RedisGILGuard::new(App {
    phase: Phase::Inactive,
    config: None,
    workers: None,
});
static STARTED: AtomicUsize = AtomicUsize::new(0);

fn ordinary(ctx: &Context) -> Result<(), RedisError> {
    // This must precede app lifecycle mutation, shutdown or taking handles.
    // A callback/nested/worker invocation cannot partially drain app ownership.
    ctx.reap_async_auth(0).map(|_| ())
}
fn configure(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 2 {
        return Err(RedisError::WrongArity);
    }
    let endpoint = Endpoint::parse(args[1].try_as_str()?).ok_or(CONFIG_ERROR)?;
    {
        let mut app = APP.lock(ctx);
        if app.phase != Phase::Inactive || app.config.is_some() || app.workers.is_some() {
            return Err(CONFIG_ERROR);
        }
        app.phase = Phase::Configured;
        app.config = Some(Config {
            endpoint,
            reader: None,
            writer: None,
            complete: false,
            prepared: false,
            mappings: BTreeMap::new(),
            provider: None,
        });
    }
    // Store each successful creation before any later fallible SDK operation.
    // A partial ACL failure retains the handles and requires explicit drain.
    for (role, name, rules) in [
        (Role::Reader, "http-reader", "on ~http:* +ping +get"),
        (Role::Writer, "http-writer", "on ~http:* +ping +get +set"),
    ] {
        let user = ctx.create_module_user(name)?;
        {
            let mut app = APP.lock(ctx);
            let config = app.config.as_mut().ok_or(CONFIG_ERROR)?;
            match role {
                Role::Reader => config.reader = Some(user.clone()),
                Role::Writer => config.writer = Some(user.clone()),
            }
        }
        // The strings are fixed consumer policy. Provider data never enters ACL.
        ctx.set_module_user_acl_string(&user, rules)?;
    }
    APP.lock(ctx).config.as_mut().ok_or(CONFIG_ERROR)?.complete = true;
    Ok("OK".into())
}
fn map(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 3 {
        return Err(RedisError::WrongArity);
    }
    let username = args[1].as_slice();
    let role = Role::parse(args[2].try_as_str()?).ok_or(CONFIG_ERROR)?;
    if !valid_username(username) {
        return Err(CONFIG_ERROR);
    }
    let mut app = APP.lock(ctx);
    if app.phase != Phase::Configured {
        return Err(CONFIG_ERROR);
    }
    let config = app.config.as_mut().ok_or(CONFIG_ERROR)?;
    if !config.complete
        || (config.mappings.len() >= MAPPING_LIMIT && !config.mappings.contains_key(username))
    {
        return Err(CONFIG_ERROR);
    }
    config.mappings.insert(username.to_vec(), role);
    Ok("OK".into())
}
fn prepare(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    {
        let mut app = APP.lock(ctx);
        if app.phase != Phase::Configured {
            return Err(CONFIG_ERROR);
        }
        app.config.as_mut().ok_or(CONFIG_ERROR)?.prepared = false;
    }
    let receipt = ctx.prepare_async_auth_runtime()?;
    APP.lock(ctx).config.as_mut().ok_or(CONFIG_ERROR)?.prepared = true;
    Ok((receipt.generation as i64).into())
}
fn activate(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    let mut app = APP.lock(ctx);
    if app.phase != Phase::Configured || app.workers.is_some() {
        return Err(CONFIG_ERROR);
    }
    let config = app.config.as_mut().ok_or(CONFIG_ERROR)?;
    if !config.complete
        || !config.prepared
        || config.mappings.is_empty()
        || config.reader.is_none()
        || config.writer.is_none()
    {
        return Err(CONFIG_ERROR);
    }
    config.provider = Some(Provider::new(config.endpoint));
    app.workers = Some(Workers::default());
    app.phase = Phase::Active;
    Ok("OK".into())
}
fn authenticate(ctx: &AuthContext<'_>, request: AuthRequest<'_>) -> AuthDecision {
    let selected = {
        let app = APP.lock(ctx);
        match app.phase {
            // Explicit bootstrap fallback. Only administrative commands can
            // enter Active or intentionally leave it through drain.
            Phase::Inactive | Phase::Configured => return AuthDecision::NotHandled,
            Phase::Draining => return AuthDecision::Deny(PublicAuthError::default()),
            Phase::Active => (),
        }
        if !valid_credentials(request.username(), request.password().as_bytes()) {
            return AuthDecision::Deny(PublicAuthError::default());
        }
        app.config.as_ref().and_then(|config| {
            let role = *config.mappings.get(request.username())?;
            let user = match role {
                Role::Reader => config.reader.as_ref()?,
                Role::Writer => config.writer.as_ref()?,
            }
            .clone();
            let provider = config.provider.clone()?;
            let permit = app.workers.as_ref()?.reserve()?;
            Some((role, user, provider, permit))
        })
    };
    let Some((role, user, provider, permit)) = selected else {
        return AuthDecision::Deny(PublicAuthError::default());
    };
    let credentials = request.copy_for_async();
    let deadline = Instant::now() + WORK_TIMEOUT;
    ctx.defer(AUTH_TIMEOUT, move |completion| {
        permit.start(move |cancelled| {
            STARTED.fetch_add(1, Ordering::Relaxed);
            if provider.verify(
                credentials.username(),
                credentials.password(),
                role,
                &cancelled,
                deadline,
            ) {
                completion.accept(AuthTarget::module_user(user));
            } else {
                completion.deny(PublicAuthError::default());
            }
            // SDK token completion above does not prove this stack has returned.
            // Its JoinHandle remains charged until an ordinary command joins it.
        });
    })
}
fn reap(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    let workers = APP.lock(ctx).workers.clone();
    if let Some(workers) = &workers {
        workers.reap_finished();
    }
    ctx.reap_async_auth(1024)?;
    Ok((workers.map_or(0, |w| w.retained()) as i64).into())
}
fn drain(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    let (workers, provider) = {
        let mut app = APP.lock(ctx);
        app.phase = Phase::Draining;
        (
            app.workers.clone(),
            app.config.as_ref().and_then(|c| c.provider.clone()),
        )
    };
    if let Some(workers) = &workers {
        workers.cancel();
    }
    if let Some(provider) = provider {
        provider.cancel();
    }
    ctx.cancel_async_auth()?;
    if let Some(workers) = &workers {
        workers.reap_finished();
    }
    // Do not hold APP or worker locks across SDK reaping. A retained starter
    // can drop its reservation here even when no worker was ever started.
    ctx.reap_async_auth(1024)?;
    if workers.as_ref().is_some_and(|w| !w.quiescent()) {
        return Err(PENDING);
    }
    ctx.drain_client_identities()?;
    ctx.reap_async_auth(1024)?;
    for role in [Role::Reader, Role::Writer] {
        let user = APP.lock(ctx).config.as_ref().and_then(|config| match role {
            Role::Reader => config.reader.clone(),
            Role::Writer => config.writer.clone(),
        });
        if let Some(user) = user {
            ctx.remove_module_user(&user)?;
            let mut app = APP.lock(ctx);
            let config = app.config.as_mut().ok_or(CONFIG_ERROR)?;
            match role {
                Role::Reader => config.reader = None,
                Role::Writer => config.writer = None,
            }
        }
    }
    let mut app = APP.lock(ctx);
    app.workers = None;
    app.config = None;
    app.phase = Phase::Inactive;
    Ok("OK".into())
}
fn status(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
    ordinary(ctx)?;
    if args.len() != 1 {
        return Err(RedisError::WrongArity);
    }
    let app = APP.lock(ctx);
    let values: Vec<redis_module::RedisValue> = vec![
        app.phase.name().into(),
        (app.workers.as_ref().map_or(0, Workers::retained) as i64).into(),
        (STARTED.load(Ordering::Relaxed) as i64).into(),
    ];
    Ok(values.into())
}
fn deinit(ctx: &EventContext) -> Status {
    let app = APP.lock(ctx);
    if app.phase == Phase::Inactive && app.config.is_none() && app.workers.is_none() {
        Status::Ok
    } else {
        Status::Err
    }
}
redis_module! {
    name: "external_auth", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], deinit: deinit, auth: authenticate,
    commands: [
        redis_command!("external_auth.configure", configure).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.map", map).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.prepare", prepare).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.activate", activate).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.reap", reap).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.drain", drain).flags(redis_module::CommandFlags::ADMIN),
        redis_command!("external_auth.status", status).flags(redis_module::CommandFlags::ADMIN).read_only(),
    ],
}
