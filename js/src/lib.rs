use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use redis_module::{
    CallOptionsBuilder, CallReply, CallResult, CommandFlags, Context, KeyRange, NextArg,
    RedisError, RedisResult, RedisString, RedisValue, redis_command, redis_module,
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

fn project(input: Vec<u8>, source: &str) -> Result<BTreeMap<String, String>, RedisError> {
    let runtime =
        Runtime::new().map_err(|_| RedisError::Str("ERR could not create JavaScript runtime"))?;
    runtime.set_memory_limit(JS_HEAP_BYTES);
    runtime.set_max_stack_size(JS_STACK_BYTES);
    let deadline = Instant::now() + JS_DEADLINE;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));

    // Promise is intentionally absent: this command is synchronous and never runs jobs.
    // No loader is installed, and the crate does not enable rquickjs's loader or futures features.
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
            let value = value
                .to_cstring()
                .map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            let name = name
                .to_cstring()
                .map_err(|_| RedisError::Str("ERR could not read projection output"))?;
            raw_output_bytes = raw_output_bytes
                .checked_add(name.len())
                .and_then(|size| size.checked_add(value.len()))
                .ok_or(RedisError::Str("ERR projection output is too large"))?;
            if raw_output_bytes > MAX_OUTPUT_BYTES {
                return Err(RedisError::Str("ERR projection output is too large"));
            }
            fields.insert(name.as_str().to_owned(), value.as_str().to_owned());
        }
        if fields.is_empty() {
            return Err(RedisError::Str("ERR projection must not be empty"));
        }

        // Serialize a new plain object so the limit describes exactly the validated
        // field/value snapshot, without invoking methods on the script's object again.
        let json = js
            .json_stringify(&fields)
            .map_err(|_| RedisError::Str("ERR could not serialize projection output"))?
            .ok_or(RedisError::Str("ERR could not serialize projection output"))?;
        let json = json
            .to_cstring()
            .map_err(|_| RedisError::Str("ERR could not serialize projection output"))?;
        if json.len() > MAX_OUTPUT_BYTES {
            return Err(RedisError::Str("ERR projection output is too large"));
        }

        Ok(fields)
    });

    // All context-owned JavaScript values are gone before the context and runtime.
    drop(context);
    drop(runtime);
    projection
}

fn read_source(ctx: &Context, key: &RedisString) -> Result<Vec<u8>, RedisError> {
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
    ctx: &Context,
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
    commands: [
        redis_command!("js.project", js_project)
            .flags(CommandFlags::WRITE | CommandFlags::DENY_OOM | CommandFlags::DENY_SCRIPT | CommandFlags::MAY_REPLICATE)
            .keys(KeyRange::new(1).last(2))
            .acl_categories("write"),
    ],
}
