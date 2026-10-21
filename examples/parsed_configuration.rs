//! Parsed string configs keep accepted text and typed values together.
use redis_module::{
    Context, RedisError, RedisGILGuard, RedisResult, RedisString, RedisValue, RegistrationContext,
    Status,
    configuration::{ConfigurationContext, ConfigurationFlags, ParsedStringConfig},
    redis_command, redis_module,
};
use std::sync::LazyLock;

fn parse_workers(raw: &str) -> Result<u16, RedisError> {
    let value = raw
        .trim()
        .parse::<u16>()
        .map_err(|_| RedisError::Str("workers must be 1..=64"))?;
    (1..=64)
        .contains(&value)
        .then_some(value)
        .ok_or(RedisError::Str("workers must be 1..=64"))
}

fn validate_prefix(raw: &str) -> Result<(), RedisError> {
    if raw.len() < 2 || !raw.ends_with(':') || raw.contains('\0') {
        return Err(RedisError::Str(
            "prefix must be nonempty and end with ':' without NUL",
        ));
    }
    Ok(())
}

static WORKERS: LazyLock<ParsedStringConfig<u16>> =
    LazyLock::new(|| ParsedStringConfig::new("2", parse_workers).unwrap());
static PREFIX: LazyLock<ParsedStringConfig<()>> =
    LazyLock::new(|| ParsedStringConfig::new("seed:", validate_prefix).unwrap());
static OBSERVED: RedisGILGuard<Vec<(String, u16)>> = RedisGILGuard::new(Vec::new());

fn changed(ctx: &ConfigurationContext, _: &str, value: &'static ParsedStringConfig<u16>) {
    let pair = value.with_value(ctx, |raw, parsed| (raw.to_owned(), *parsed));
    OBSERVED.lock(ctx).push(pair);
}

fn init(ctx: &RegistrationContext<'_>, _: &[RedisString]) -> Status {
    // LoadConfigs has already validated and installed the registration default
    // or module arguments. Discard observer history from earlier image loads.
    OBSERVED.lock(ctx).clear();
    Status::Ok
}

fn value(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(WORKERS.with_value(ctx, |raw, parsed| {
        RedisValue::Array(vec![raw.to_owned().into(), i64::from(*parsed).into()])
    }))
}

fn prefix(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(PREFIX.with_value(ctx, |raw, _| raw.to_owned()).into())
}

fn observed(ctx: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::Array(
        OBSERVED
            .lock(ctx)
            .iter()
            .map(|(raw, parsed)| {
                RedisValue::Array(vec![raw.clone().into(), i64::from(*parsed).into()])
            })
            .collect(),
    ))
}

redis_module! {
    name: "parsed_configuration", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [], init: init,
    commands: [
        redis_command!("parsed_config.value", value).read_only(),
        redis_command!("parsed_config.prefix", prefix).read_only(),
        redis_command!("parsed_config.observed", observed).read_only(),
    ],
    configurations: [
        string: [
            ["workers", &*WORKERS, "4", ConfigurationFlags::DEFAULT, Some(Box::new(changed))],
            ["prefix", &*PREFIX, "items:", ConfigurationFlags::DEFAULT, None],
        ],
        module_args_as_configuration: true,
    ]
}
