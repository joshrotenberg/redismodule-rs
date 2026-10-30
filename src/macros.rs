/// Evaluates to the module API function `$name`, or panics with its name when
/// the running Redis does not provide it. Use it in an `unsafe` context.
#[doc(hidden)]
#[macro_export]
macro_rules! __required_api {
    ($name:ident) => {
        match $crate::raw::$name {
            Some(function) => function,
            None => $crate::raw::api_unavailable(stringify!($name)),
        }
    };
}

/// Evaluates to the module API function `$name`, or returns `Err` from the
/// enclosing function, converted with `Into`, when the running Redis does not
/// provide it. Use it in an `unsafe` context.
#[doc(hidden)]
#[macro_export]
macro_rules! __available_api {
    ($name:ident) => {
        match $crate::raw::$name {
            Some(function) => function,
            None => return Err($crate::raw::api_unavailable_error(stringify!($name)).into()),
        }
    };
}

/// With two arguments, returns a [`crate::CommandBuilder`] that generates a
/// panic-safe callback: `redis_command!("example.get", get).read_only().key(1)`.
/// Builders may be mixed with legacy tuples in `redis_module!`'s command list.
///
/// The legacy form registers a command directly. `redis_module!` calls it for
/// tuple entries in `commands`.
///
/// The flags are a constant string of flag names separated by single spaces,
/// checked when the module compiles:
///
/// ```compile_fail,E0080
/// use redis_module::{redis_module, Context, RedisResult, RedisString};
///
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult {
///     Ok("pong".into())
/// }
///
/// redis_module! {
///     name: "typo",
///     version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     commands: [["typo.ping", ping, "readonly fsat", 0, 0, 0, ""]],
/// }
/// # fn main() {}
/// ```
///
/// ```no_run
/// use redis_module::{redis_module, Context, RedisResult, RedisString};
///
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult {
///     Ok("pong".into())
/// }
///
/// redis_module! {
///     name: "typo",
///     version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     commands: [["typo.ping", ping, "readonly fast", 0, 0, 0, ""]],
/// }
/// # fn main() {}
/// ```
/// Mixed lists preserve compile-time checking for legacy flag strings:
///
/// ```compile_fail,E0080
/// use redis_module::{Context, RedisResult, RedisString, redis_command, redis_module};
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult { Ok("PONG".into()) }
/// redis_module! {
///     name: "bad_mixed_flags", version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     commands: [
///         redis_command!("valid.ping", ping).read_only(),
///         ["invalid.ping", ping, "readonly fsat", 0, 0, 0, ""],
///     ],
/// }
/// ```
/// A malformed legacy entry in a mixed list is rejected too:
///
/// ```compile_fail
/// use redis_module::{Context, RedisResult, RedisString, redis_command, redis_module};
/// fn ping(_: &Context, _: Vec<RedisString>) -> RedisResult { Ok("PONG".into()) }
/// redis_module! {
///     name: "bad_mixed_entry", version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     commands: [
///         redis_command!("valid.ping", ping),
///         ["invalid.ping", ping, "readonly", 0, 0, 0],
///     ],
/// }
/// ```
#[macro_export]
macro_rules! redis_command {
    ($name:expr, $handler:expr $(,)?) => {
        $crate::commands::CommandBuilder::__new($name, $crate::__redis_command_callback!($handler))
    };
    ($ctx:expr, $name:expr, $handler:expr, $flags:expr,
     $first:expr, $last:expr, $step:expr, $required:expr $(, $optional:expr)?) => {{
        use $crate::AclCategory;
        const _: () = $crate::commands::__check_command_flags($flags);
        let command = $crate::commands::CommandBuilder::__new(
            $name, $crate::__redis_command_callback!($handler),
        ).__legacy($flags, $first, $last, $step).acl_categories(AclCategory::from($required))
            $(.optional_acl_categories($optional))?;
        if let Err(error) = command.register($ctx) {
            ($ctx).log_warning(&error.to_string());
            return $crate::Status::Err as i32;
        }
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __redis_command_callback {
    ($handler:expr) => {{
        enum __RedisModuleCommandCallback {}
        impl __RedisModuleCommandCallback {
            extern "C" fn invoke(
                ctx: *mut $crate::raw::RedisModuleCtx,
                argv: *mut *mut $crate::raw::RedisModuleString,
                argc: i32,
            ) -> i32 {
                let command = || {
                    // SAFETY: Redis associates this SDK module callback with its module.
                    let context = unsafe { $crate::Context::from_module_context(ctx) };
                    // SAFETY: Redis supplies live argv/count for this callback.
                    let args = unsafe { $crate::decode_args(ctx, argv, argc) };
                    let response = $handler(&context, args);
                    context.reply(response.map(|value| value.into())) as i32
                };
                // SAFETY: Redis passed this live command context.
                unsafe { $crate::panic::run_command(ctx, command) }
            }
        }
        __RedisModuleCommandCallback::invoke
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __redis_register_commands {
    // Preserve the original flat expansion for legacy-only lists.
    ($ctx:expr; $([$name:expr, $handler:expr, $flags:expr, $first:expr,
                  $last:expr, $step:expr, $required:expr $(, $optional:expr)?]),* $(,)*) => {
        $( $crate::redis_command!($ctx, $name, $handler, $flags, $first, $last, $step, $required $(, $optional)?); )*
    };
    ($ctx:expr; $($entries:tt)*) => {
        $crate::__private::__register_command_list!(($crate), $ctx, [$($entries)*]);
    };
}

/// Adapts a Rust function into a panic-safe [`crate::KeyspaceEventHandler`].
///
/// Pass `redis_event_handler!(handler)` to
/// [`EventContext::subscribe_to_keyspace_events`](crate::EventContext::subscribe_to_keyspace_events)
/// with a mask selected at runtime. Handlers are static functions or
/// noncapturing expressions; captured state is not supported.
///
/// The existing three-argument form subscribes immediately and returns a native
/// error status from the enclosing function on failure. `redis_module!` uses
/// that form for each entry in `event_handlers`.
///
/// Both forms require an [`EventContext`](crate::EventContext), which SDK load
/// and command contexts also expose. Worker guards cannot subscribe. Custom
/// native callbacks must meet [`EventContext::new`](crate::EventContext::new)'s
/// module ownership and Redis lock requirements.
///
/// The key slice a handler receives is valid only while the handler runs, so a
/// handler cannot ask to keep it:
///
/// ```compile_fail,E0716
/// use redis_module::{redis_module, EventContext, NotifyEvent};
///
/// fn on_event(_: &EventContext, _: NotifyEvent, _: &str, _key: &'static [u8]) {}
///
/// redis_module! {
///     name: "keep_key",
///     version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     event_handlers: [[@STRING: on_event]],
/// }
/// # fn main() {}
/// ```
///
/// The runtime adapter enforces the same lifetime:
///
/// ```compile_fail,E0716
/// use redis_module::{EventContext, NotifyEvent, redis_event_handler};
/// fn on_event(_: &EventContext, _: NotifyEvent, _: &str, _: &'static [u8]) {}
/// let _handler = redis_event_handler!(on_event);
/// ```
///
/// A handler that borrows the key for the call compiles:
///
/// ```no_run
/// use redis_module::{redis_module, EventContext, NotifyEvent};
///
/// fn on_event(_: &EventContext, _: NotifyEvent, _: &str, key: &[u8]) {
///     let _owned = key.to_vec();
/// }
///
/// redis_module! {
///     name: "borrow_key",
///     version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
///     event_handlers: [[@STRING: on_event]],
/// }
/// # fn main() {}
/// ```
#[macro_export]
macro_rules! redis_event_handler {
    ($event_handler:expr) => {{
        enum __RedisModuleEventCallback {}
        impl __RedisModuleEventCallback {
            extern "C" fn invoke(
                ctx: *mut $crate::raw::RedisModuleCtx,
                event_type: ::std::os::raw::c_int,
                event: *const ::std::os::raw::c_char,
                key: *mut $crate::raw::RedisModuleString,
            ) -> ::std::os::raw::c_int {
                fn borrow_key<'key>(_: &'key $crate::EventContext, key: &'key [u8]) -> &'key [u8] {
                    key
                }
                $crate::panic::run_callback(
                    "keyspace event handler",
                    || {
                        // SAFETY: Redis associates this SDK module callback with its module.
                        let context = unsafe { $crate::EventContext::new(ctx) };
                        // SAFETY: Redis supplies a live, read-only key for this
                        // synchronous invocation. The context anchors its lifetime,
                        // including handlers which would otherwise request 'static.
                        // LOADED-event keys cannot be retained. Never make this
                        // pointer into an owning RedisString.
                        let redis_key = borrow_key(&context, unsafe {
                            $crate::RedisString::string_as_slice(key)
                        });
                        let event_str = unsafe { ::std::ffi::CStr::from_ptr(event) };
                        $event_handler(
                            &context,
                            $crate::NotifyEvent::from_bits_truncate(event_type),
                            &event_str.to_string_lossy(),
                            redis_key,
                        );
                    },
                    || (),
                );
                $crate::raw::Status::Ok as ::std::os::raw::c_int
            }
        }
        // SAFETY: The adapter catches panics and bounds borrowed data to the call.
        unsafe { $crate::KeyspaceEventHandler::__new(__RedisModuleEventCallback::invoke) }
    }};
    ($ctx:expr, $event_type:expr, $event_handler:expr) => {{
        let __module_context: &$crate::EventContext = $ctx;
        if let Err(error) = __module_context
            .subscribe_to_keyspace_events($event_type, $crate::redis_event_handler!($event_handler))
        {
            __module_context.log_warning(&error.to_string());
            return $crate::raw::Status::Err as ::std::os::raw::c_int;
        }
    }};
}

/// Defines a Redis module.
///
/// It registers the defined module, sets it up and initialises properly,
/// registers all the commands and types.
///
/// `version:` accepts an integer expression. Use [`module_version!`](crate::module_version!)
/// to derive it from the calling crate's Cargo package version at compile time.
///
/// `name:` must be a constant `&str` expression without NUL bytes. Literals,
/// constants, `concat!` and `env!` are supported. Its C-string representation
/// is built at compile time, without a fixed length limit or an allocation
/// before the Redis allocator is initialized.
///
/// ```compile_fail,E0080
/// # macro_rules! assert { ($($tokens:tt)*) => {}; }
/// redis_module::redis_module! {
///     name: "truncated\0name", version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
/// }
/// ```
///
/// Runtime name expressions are rejected even if they return `&str`:
///
/// ```compile_fail,E0015
/// fn module_name() -> &'static str { "runtime_name" }
/// redis_module::redis_module! {
///     name: module_name(), version: 1,
///     allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
///     data_types: [],
/// }
/// ```
///
/// Custom `acl_categories` are shared across modules and remain registered
/// after unload. Existing categories are reused; other registration errors
/// fail the module load. If the server lacks custom category registration, one
/// notice is logged per load and those custom names are omitted from optional
/// command categories. Required categories still have to be accepted by Redis.
///
/// Initialization hooks receive a borrowed [`RegistrationContext`](crate::RegistrationContext).
/// Load-only APIs require it; command handlers continue receiving [`Context`](crate::Context).
#[macro_export]
macro_rules! redis_module {
    (
        name: $module_name:expr,
        // An explicit integer or `module_version!()` for the Cargo package version.
        version: $module_version:expr,
        // Global allocator for the redis module defined.
        // In most of the cases, the Redis allocator ([crate::alloc::RedisAlloc])
        // should be used.
        allocator: ($allocator_type:ty, $allocator_init:expr),
        data_types: [
            $($data_type:ident),* $(,)*
        ],
        // eg: `acl_category: [ "name_of_module_acl_category", ],`
        // This will add the specified (optional) ACL categories.
        $(acl_categories: [
            $($module_acl_category:expr,)*
        ],)?
        $(init: $init_func:ident,)* $(,)*
        $(deinit: $deinit_func:ident,)* $(,)*
        $(info: $info_func:ident,)?
        $(commands: [$($commands:tt)*] $(,)*)?
        $(event_handlers: [
            $([
                $(@$event_type:ident) +:
                $event_handler:expr
            ]),* $(,)*
        ] $(,)* )?
        $(configurations: [
            $(i64:[$([
                $i64_configuration_name:expr,
                $i64_configuration_val:expr,
                $i64_default:expr,
                $i64_min:expr,
                $i64_max:expr,
                $i64_flags_options:expr,
                $i64_on_changed:expr
            ]),* $(,)*],)?
            $(string:[$([
                $string_configuration_name:expr,
                $string_configuration_val:expr,
                $string_default:expr,
                $string_flags_options:expr,
                $string_on_changed:expr
            ]),* $(,)*],)?
            $(bool:[$([
                $bool_configuration_name:expr,
                $bool_configuration_val:expr,
                $bool_default:expr,
                $bool_flags_options:expr,
                $bool_on_changed:expr
            ]),* $(,)*],)?
            $(enum:[$([
                $enum_configuration_name:expr,
                $enum_configuration_val:expr,
                $enum_default:expr,
                $enum_flags_options:expr,
                $enum_on_changed:expr
            ]),* $(,)*],)?
            $(module_args_as_configuration:$use_module_args:expr,)?
            $(module_config_get:$module_config_get_command:expr,)?
            $(module_config_set:$module_config_set_command:expr,)?
        ])?
    ) => {
        /// Redis module allocator.
        #[global_allocator]
        static REDIS_MODULE_ALLOCATOR: $allocator_type = $allocator_init;

        // The old-style info command handler, if specified.
        $(
            #[redis_module_macros::info_command_handler]
            #[inline]
            fn module_info(ctx: &InfoContext, for_crash_report: bool) -> RedisResult<()> {
                $info_func(ctx, for_crash_report);

                Ok(())
            }
        )?

        unsafe extern "C" fn __info_func(
            ctx: *mut $crate::raw::RedisModuleInfoCtx,
            for_crash_report: i32,
        ) {
            $crate::panic::run_callback(
                "INFO handler",
                // SAFETY: Redis passes a valid info context for this call.
                || $crate::basic_info_command_handler(&unsafe { $crate::InfoContext::new(ctx) }, for_crash_report == 1),
                || (),
            );
        }

        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub unsafe extern "C" fn RedisModule_OnLoad(
            ctx: *mut $crate::raw::RedisModuleCtx,
            argv: *mut *mut $crate::raw::RedisModuleString,
            argc: std::os::raw::c_int,
        ) -> std::os::raw::c_int {
            use std::os::raw::c_int;

            use $crate::raw;
            use $crate::RedisString;
            use $crate::server_events::register_server_events;
            use $crate::configuration::register_i64_configuration;
            use $crate::configuration::register_string_configuration;
            use $crate::configuration::register_bool_configuration;
            use $crate::configuration::register_enum_configuration;
            use $crate::configuration::module_config_get;
            use $crate::configuration::module_config_set;
            use $crate::configuration::get_i64_default_config_value;
            use $crate::configuration::get_string_default_config_value;
            use $crate::configuration::get_bool_default_config_value;
            use $crate::configuration::get_enum_default_config_value;

            // Init sets up the allocator, so prepare the C string entirely in
            // constant evaluation. Keep its final zero byte even for long names.
            // An anonymous const also reports bad inputs during cargo check.
            // Local let bindings stay hygienic with caller-defined constants.
            const _: () = {
                let source: &::core::primitive::str = $module_name;
                let mut index = 0;
                while index < source.len() {
                    ::core::assert!(source.as_bytes()[index] != 0, "module name must not contain NUL bytes");
                    index += 1;
                }
            };
            let __redis_module_name_bytes = &const {
                let source: &::core::primitive::str = $module_name;
                let mut buffer = [0; ($module_name).len() + 1];
                let mut index = 0;
                while index < source.len() {
                    buffer[index] = source.as_bytes()[index];
                    index += 1;
                }
                buffer
            };

            let module_version = $module_version as c_int;

            // SAFETY: the checked name has no interior zero, its extra byte
            // terminates it, and the buffer outlives Init, which copies it.
            if unsafe { raw::Export_RedisModule_Init(
                ctx,
                __redis_module_name_bytes.as_ptr().cast::<::core::ffi::c_char>(),
                module_version,
                raw::REDISMODULE_APIVER_1 as c_int,
            ) } == raw::Status::Err as c_int { return raw::Status::Err as c_int; }

            // With the system-allocator-fallback feature, an allocation made
            // before Init picked the system allocator for the whole process.
            if $crate::alloc::__uses_system_allocator() {
                unsafe { $crate::RuntimeContext::new(ctx) }.log_warning(
                    "The module allocated before RedisModule_OnLoad and uses the system allocator. \
                     It was built with the system-allocator-fallback feature, which is meant for tests.",
                );
                return raw::Status::Err as c_int;
            }

            // SAFETY: Init succeeded and this is the module's native OnLoad.
            let configuration_load = match unsafe { $crate::__private::ConfigurationLoad::begin(ctx) } {
                Ok(load) => load,
                Err(error) => {
                    unsafe { raw::redis_log(ctx, &error.to_string()) };
                    return raw::Status::Err as c_int;
                }
            };
            // Whether this load stored MODULE_CONTEXT, which a failed load frees.
            let mut detached_stored = false;
            // Panics before Init cannot be caught: the Redis allocator, which a
            // panic needs, is not available until then.
            let status = $crate::panic::run_callback(
                "module load",
                || {
                    // SAFETY: Redis associates this SDK module callback with its module.
                    let runtime_context = unsafe { $crate::EventContext::new(ctx) };
                    // A handler installed now could outlive a load Redis rejects.
                    let _blocking_load = $crate::__private::BlockingCallLoadGuard::begin();
                    // SAFETY: Init succeeded; this is the module's single OnLoad.
                    let timer_load = match unsafe { $crate::__private::TimerLoadGuard::begin(&runtime_context) } {
                        Ok(guard) => guard,
                        Err(error) => {
                            runtime_context.log_warning(&format!("{error}"));
                            return raw::Status::Err as c_int;
                        }
                    };
                    // SAFETY: Init succeeded and this scope is inside OnLoad.
                    let context = unsafe { $crate::RegistrationContext::from_context(&runtime_context) };
                    // A context left by an earlier load of a retained image
                    // refers to a module Redis has freed.
                    match $crate::MODULE_CONTEXT.__replace_for_load(&context) {
                        Ok(replaced) => {
                            detached_stored = true;
                            if replaced {
                                context.log_warning("replaced a detached module context left by an earlier load");
                            }
                        }
                        // Without the API the module has no detached context, as before.
                        Err(_) => (),
                    }
                    // SAFETY: Redis supplies live callback argv/count with legal retention.
                    let args = unsafe { $crate::decode_args(ctx, argv, argc) };

                    $(
                        if (&$data_type).create_data_type(&context).is_err() {
                            return raw::Status::Err as c_int;
                        }
                    )*

                    let context = match context.__register_acl_categories([
                        $($($crate::AclCategory::from($module_acl_category),)*)?
                    ]) {
                        Ok(context) => context,
                        Err(error) => {
                            runtime_context.log_warning(&error.to_string());
                            return raw::Status::Err as c_int;
                        }
                    };

                    $(
                        $crate::__redis_register_commands!(&context; $($commands)*);
                    )?

                    if $crate::commands::register_commands(&context) == raw::Status::Err {
                        return raw::Status::Err as c_int;
                    }

                    if let Err(e) = $crate::defrag::register_defrag_functions(&context) {
                        context.log_warning(&format!("{e}"));
                        return raw::Status::Err as c_int;
                    }

                    $(
                        $(
                            $crate::redis_event_handler!(&context, $(raw::NotifyEvent::$event_type |)+ raw::NotifyEvent::empty(), $event_handler);
                        )*
                    )?

                    $(
                        // Bound once here: used inside the groups below, the
                        // meta-variable would have to repeat with each group.
                        let use_module_args: bool = false $(|| $use_module_args)?;
                        $(
                            $(
                                let default = if use_module_args {
                                    let value = if ($i64_flags_options).contains($crate::configuration::ConfigurationFlags::MEMORY) {
                                        $crate::configuration::get_memory_default_config_value(&args, $i64_configuration_name, $i64_default)
                                    } else {
                                        get_i64_default_config_value(&args, $i64_configuration_name, $i64_default)
                                    };
                                    match value {
                                        Ok(v) => v,
                                        Err(e) => {
                                            context.log_warning(&format!("{e}"));
                                            return raw::Status::Err as c_int;
                                        }
                                    }
                                } else {
                                    $i64_default
                                };
                                if let Err(error) = register_i64_configuration(&context, $i64_configuration_name, $i64_configuration_val, default, $i64_min, $i64_max, $i64_flags_options, $i64_on_changed) {
                                    context.log_warning(&error.to_string());
                                    return raw::Status::Err as c_int;
                                }
                            )*
                        )?
                        $(
                            $(
                                let default = if use_module_args {
                                    match get_string_default_config_value(&args, $string_configuration_name, $string_default) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            context.log_warning(&format!("{e}"));
                                            return raw::Status::Err as c_int;
                                        }
                                    }
                                } else {
                                    $string_default
                                };
                                if let Err(error) = register_string_configuration(&context, $string_configuration_name, $string_configuration_val, default, $string_flags_options, $string_on_changed) {
                                    context.log_warning(&error.to_string());
                                    return raw::Status::Err as c_int;
                                }
                            )*
                        )?
                        $(
                            $(
                                let default = if use_module_args {
                                    match get_bool_default_config_value(&args, $bool_configuration_name, $bool_default) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            context.log_warning(&format!("{e}"));
                                            return raw::Status::Err as c_int;
                                        }
                                    }
                                } else {
                                    $bool_default
                                };
                                if let Err(error) = register_bool_configuration(&context, $bool_configuration_name, $bool_configuration_val, default, $bool_flags_options, $bool_on_changed) {
                                    context.log_warning(&error.to_string());
                                    return raw::Status::Err as c_int;
                                }
                            )*
                        )?
                        $(
                            $(
                                let default = if use_module_args {
                                    match get_enum_default_config_value(&args, $enum_configuration_name, $enum_default) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            context.log_warning(&format!("{e}"));
                                            return raw::Status::Err as c_int;
                                        }
                                    }
                                } else {
                                    $enum_default
                                };
                                if let Err(error) = register_enum_configuration(&context, $enum_configuration_name, $enum_configuration_val, default, $enum_flags_options, $enum_on_changed) {
                                    context.log_warning(&error.to_string());
                                    return raw::Status::Err as c_int;
                                }
                            )*
                        )?
                        // A Redis without LoadConfigs (before 7.0) has no module
                        // configurations, so nothing was registered to load.
                        // SAFETY: read the API pointer by value, not by reference.
                        if unsafe { raw::RedisModule_LoadConfigs }.is_some() {
                            if let Err(error) = $crate::configuration::load_configurations(&context) {
                                context.log_warning(&error.to_string());
                                return raw::Status::Err as c_int;
                            }
                        }

                        $(
                            $crate::redis_command!(&context, $module_config_get_command, |ctx, args: Vec<RedisString>| {
                                module_config_get(ctx, args, const { $module_name })
                            }, "", 0, 0, 0, "");
                        )?

                        $(
                            $crate::redis_command!(&context, $module_config_set_command, |ctx, args: Vec<RedisString>| {
                                module_config_set(ctx, args, const { $module_name })
                            }, "", 0, 0, 0, "");
                        )?
                    )?

                    // SAFETY: ctx is the module's load context.
                    unsafe { raw::register_info_function(ctx, Some(__info_func)) };

                    if let Err(e) = register_server_events(&context) {
                        context.log_warning(&format!("{e}"));
                        return raw::Status::Err as c_int;
                    }

                    $(
                        if $init_func(&context, &args) == $crate::Status::Err {
                            return $crate::Status::Err as c_int;
                        }
                    )*

                    timer_load.commit();
                    raw::Status::Ok as c_int
                },
                || raw::Status::Err as c_int,
            );
            // Captured destructors run after init panic containment, never while
            // unwinding the load callback.
            configuration_load.finish(status);
            if status != raw::Status::Ok as c_int {
                $crate::server_events::__clear_runtime_handlers();
            }
            if detached_stored && status != raw::Status::Ok as c_int {
                // Redis frees a module whose load fails without calling OnUnload.
                // SAFETY: OnLoad holds the Redis lock and the module still exists.
                unsafe { $crate::MODULE_CONTEXT.__release() };
            }
            status
        }

        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub unsafe extern "C" fn RedisModule_OnUnload(
            ctx: *mut $crate::raw::RedisModuleCtx
        ) -> std::os::raw::c_int {
            use std::os::raw::c_int;

            $crate::panic::run_callback(
                "module unload",
                || {
                    // SAFETY: Redis associates this SDK module callback with its module.
                    let context = unsafe { $crate::EventContext::new(ctx) };
                    // Check native CONFIG quiescence before user deinit.
                    // SAFETY: This is the module's native OnUnload callback.
                    let configuration_unload = match unsafe { $crate::__private::ConfigurationUnload::begin(&context) } {
                        Ok(guard) => guard,
                        Err(error) => {
                            context.log_warning(&format!("{error}"));
                            return $crate::Status::Err as c_int;
                        }
                    };
                    // SAFETY: This is the module's native OnUnload callback.
                    let timer_unload = match unsafe { $crate::__private::TimerUnloadGuard::begin(&context) } {
                        Ok(guard) => guard,
                        Err(error) => {
                            context.log_warning(&format!("{error}"));
                            return $crate::Status::Err as c_int;
                        }
                    };
                    // Redis calls a pending unblock handler, and reads this
                    // module, when its call unblocks. The guard also refuses
                    // blocking calls from deinit.
                    let _blocking_unload = match $crate::__private::BlockingCallUnloadGuard::begin() {
                        Ok(guard) => guard,
                        Err(error) => {
                            context.log_warning(&format!("{error}"));
                            return $crate::Status::Err as c_int;
                        }
                    };
                    $(
                        if $deinit_func(&context) == $crate::Status::Err {
                            return $crate::Status::Err as c_int;
                        }
                    )*

                    if let Err(error) = configuration_unload.commit() {
                        context.log_warning(&format!("{error}"));
                        return $crate::Status::Err as c_int;
                    }
                    timer_unload.commit();
                    $crate::server_events::__clear_runtime_handlers();
                    // A later load of a retained image needs a fresh context.
                    // SAFETY: OnUnload holds the Redis lock and the module still exists.
                    unsafe { $crate::MODULE_CONTEXT.__release() };
                    $crate::raw::Status::Ok as c_int
                },
                || $crate::raw::Status::Err as c_int,
            )
        }
    }
}
