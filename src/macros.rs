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

/// Registers a command. `redis_module!` calls this for each entry in `commands`.
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
#[macro_export]
macro_rules! redis_command {
    ($ctx:expr,
     $command_name:expr,
     $command_handler:expr,
     $command_flags:expr,
     $firstkey:expr,
     $lastkey:expr,
     $keystep:expr,
     $mandatory_acl_categories:expr
     $(, $optional_acl_categories:expr)?
    ) => {{
        use $crate::AclCategory;

        // A &Context is proof the module is loading with a valid context; a
        // raw pointer argument would let safe code pass any pointer.
        let __module_ctx: *mut $crate::raw::RedisModuleCtx = $crate::Context::get_raw($ctx);

        // A misspelled flag fails to compile here instead of at load.
        const _: () = $crate::commands::__check_command_flags($command_flags);
        let name = CString::new($command_name).unwrap();
        let flags = CString::new($command_flags).unwrap();

        /////////////////////
        extern "C" fn __do_command(
            ctx: *mut $crate::raw::RedisModuleCtx,
            argv: *mut *mut $crate::raw::RedisModuleString,
            argc: c_int,
        ) -> c_int {
            let command = || {
                let context = unsafe { $crate::Context::new(ctx) };

                // SAFETY: Redis supplies live callback argv/count with legal retention.
                let args = unsafe { $crate::decode_args(ctx, argv, argc) };
                let response = $command_handler(&context, args);
                context.reply(response.map(|v| v.into())) as c_int
            };
            // SAFETY: ctx is the context Redis passed to this command.
            unsafe { $crate::panic::run_command(ctx, command) }
        }
        /////////////////////

        if unsafe {
            $crate::__required_api!(RedisModule_CreateCommand)(
                __module_ctx,
                name.as_ptr(),
                Some(__do_command),
                flags.as_ptr(),
                $firstkey,
                $lastkey,
                $keystep,
            )
        } == $crate::raw::Status::Err as c_int
        {
            // SAFETY: __module_ctx comes from the load Context.
            unsafe { $crate::raw::redis_log(
                __module_ctx,
                &format!("Error: failed to create command {}", $command_name),
            ) };
            return $crate::raw::Status::Err as c_int;
        }

        let mandatory = AclCategory::from($mandatory_acl_categories);
        // SAFETY: Redis sets the API pointers before OnLoad runs.
        if let Some(RM_SetCommandACLCategories) = unsafe { $crate::raw::RedisModule_SetCommandACLCategories } {
            let command =
            unsafe { $crate::__required_api!(RedisModule_GetCommand)(__module_ctx, name.as_ptr()) };
            if command.is_null() {
                // SAFETY: __module_ctx comes from the load Context.
                unsafe { $crate::raw::redis_log(
                    __module_ctx,
                    &format!("Error: failed to get command {}", $command_name),
                ) };
                return $crate::raw::Status::Err as c_int;
            }
            let mut optional_failed = true;
            let mut acl_categories = CString::default();
            $(
                let optional = AclCategory::from($optional_acl_categories);
                assert!(optional != AclCategory::None);
                optional_failed = false;
                if mandatory != AclCategory::None {
                    acl_categories = CString::new(format!("{mandatory} {optional}")).unwrap();
                } else {
                    acl_categories = CString::new(format!("{optional}")).unwrap();
                }
                // Warn if optional ACL categories are not set, but don't fail.
                // SAFETY: command is the command just created; the string outlives the call.
                if unsafe { RM_SetCommandACLCategories(command, acl_categories.as_ptr()) } == $crate::raw::Status::Err as c_int {
                    optional_failed = true;
                    // SAFETY: __module_ctx comes from the load Context.
                    unsafe { $crate::raw::redis_log(
                        __module_ctx,
                        &format!(
                            "Warning: failed to set command `{}` ACL categories `{}`",
                            $command_name, &acl_categories.to_string_lossy()
                        ),
                    ) };
                }
            )?
            if optional_failed && mandatory != AclCategory::None {
                acl_categories = CString::new(format!("{mandatory}")).unwrap();

                // Fail if mandatory ACL categories are not set.
                // SAFETY: as above.
                if unsafe { RM_SetCommandACLCategories(command, acl_categories.as_ptr()) }
                    == $crate::raw::Status::Err as c_int
                {
                    // SAFETY: __module_ctx comes from the load Context.
                    unsafe { $crate::raw::redis_log(
                        __module_ctx,
                        &format!(
                            "Error: failed to set command `{}` mandatory ACL categories `{mandatory}`",
                            $command_name
                        ),
                    ) };
                    return $crate::raw::Status::Err as c_int;
                }
            }
        } else if mandatory != AclCategory::None {
            // SAFETY: __module_ctx comes from the load Context.
            unsafe { $crate::raw::redis_log(
                __module_ctx,
                "Error: Redis version does not support ACL categories",
            ) };
            return $crate::raw::Status::Err as c_int;
        }
    }};
}

/// Subscribes a keyspace event handler. `redis_module!` calls this for each
/// entry in `event_handlers`.
///
/// The key slice a handler receives is valid only while the handler runs, so a
/// handler cannot ask to keep it:
///
/// ```compile_fail,E0716
/// use redis_module::{redis_module, Context, NotifyEvent};
///
/// fn on_event(_: &Context, _: NotifyEvent, _: &str, _key: &'static [u8]) {}
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
/// A handler that borrows the key for the call compiles:
///
/// ```no_run
/// use redis_module::{redis_module, Context, NotifyEvent};
///
/// fn on_event(_: &Context, _: NotifyEvent, _: &str, key: &[u8]) {
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
    (
        $ctx: expr,
        $event_type: expr,
        $event_handler: expr
    ) => {{
        // See redis_command!: a &Context instead of a raw pointer.
        let __module_context: &$crate::Context = $ctx;
        let __module_ctx: *mut $crate::raw::RedisModuleCtx = __module_context.get_raw();
        extern "C" fn __handle_event(
            ctx: *mut $crate::raw::RedisModuleCtx,
            event_type: c_int,
            event: *const c_char,
            key: *mut $crate::raw::RedisModuleString,
        ) -> c_int {
            fn borrow_key<'key>(_: &'key $crate::Context, key: &'key [u8]) -> &'key [u8] {
                key
            }
            $crate::panic::run_callback(
                "keyspace event handler",
                || {
                    let context = unsafe { $crate::Context::new(ctx) };

                    // SAFETY: Redis supplies a live, read-only key for this synchronous
                    // callback. The local anchor bounds this view to the invocation,
                    // including handlers which would otherwise request 'static. It
                    // must not be retained or invalidated through a native alias.
                    // In particular, LOADED-event keys cannot be retained. Do not turn
                    // this borrowed pointer into an owning RedisString.
                    let redis_key = borrow_key(&context, unsafe {
                        $crate::RedisString::string_as_slice(key)
                    });
                    let event_str = unsafe { CStr::from_ptr(event) };
                    $event_handler(
                        &context,
                        $crate::NotifyEvent::from_bits_truncate(event_type),
                        &event_str.to_string_lossy(),
                        redis_key,
                    );
                },
                || (),
            );

            $crate::raw::Status::Ok as c_int
        }

        let all_available_notification_flags = $crate::raw::get_keyspace_notification_flags_all();
        let available_wanted_notification_flags = $event_type.intersection(all_available_notification_flags);
        if !all_available_notification_flags.contains($event_type) {
            let not_supported = $event_type.difference(all_available_notification_flags);
            __module_context.log_notice(&format!(
                "These event notification flags set aren't supported: {not_supported:?}. These flags will be used: {available_wanted_notification_flags:?}"
            ));
        }

        if !available_wanted_notification_flags.is_empty() && unsafe {
            $crate::__required_api!(RedisModule_SubscribeToKeyspaceEvents)(
                __module_ctx,
                available_wanted_notification_flags.bits(),
                Some(__handle_event),
            )
        } == $crate::raw::Status::Err as c_int
        {
            return $crate::raw::Status::Err as c_int;
        }
    }};
}

/// Defines a Redis module.
///
/// It registers the defined module, sets it up and initialises properly,
/// registers all the commands and types.
///
/// Custom `acl_categories` are shared across modules and remain registered
/// after unload. Existing categories are reused; other registration errors
/// fail the module load.
#[macro_export]
macro_rules! redis_module {
    (
        name: $module_name:expr,
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
        $(commands: [
            $([
                $name:expr,
                $command:expr,
                $flags:expr,
                $firstkey:expr,
                $lastkey:expr,
                $keystep:expr,
                $mandatory_command_acl_categories:expr
                $(, $optional_command_acl_categories:expr)?
              ]),* $(,)*
        ] $(,)*)?
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
            use std::os::raw::{c_int, c_char};
            use std::ffi::{CString, CStr};

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

            // We use a statically sized buffer to avoid allocating.
            // This is needed since we use a custom allocator that relies on the Redis allocator,
            // which isn't yet ready at this point.
            let mut name_buffer = [0; 64];
            unsafe {
                std::ptr::copy(
                    $module_name.as_ptr(),
                    name_buffer.as_mut_ptr(),
                    $module_name.len(),
                );
            }

            let module_version = $module_version as c_int;

            if unsafe { raw::Export_RedisModule_Init(
                ctx,
                name_buffer.as_ptr().cast::<c_char>(),
                module_version,
                raw::REDISMODULE_APIVER_1 as c_int,
            ) } == raw::Status::Err as c_int { return raw::Status::Err as c_int; }

            // With the system-allocator-fallback feature, an allocation made
            // before Init picked the system allocator for the whole process.
            if $crate::alloc::__uses_system_allocator() {
                unsafe { $crate::Context::new(ctx) }.log_warning(
                    "The module allocated before RedisModule_OnLoad and uses the system allocator. \
                     It was built with the system-allocator-fallback feature, which is meant for tests.",
                );
                return raw::Status::Err as c_int;
            }

            // Panics before Init cannot be caught: the Redis allocator, which a
            // panic needs, is not available until then.
            $crate::panic::run_callback(
                "module load",
                || {
                    let context = unsafe { $crate::Context::new(ctx) };
                    unsafe {
                        let _ = $crate::MODULE_CONTEXT.set_context(&context);
                    }
                    // SAFETY: Redis supplies live callback argv/count with legal retention.
                    let args = unsafe { $crate::decode_args(ctx, argv, argc) };

                    $(
                        if (&$data_type).create_data_type(&context).is_err() {
                            return raw::Status::Err as c_int;
                        }
                    )*

                    $(
                        $(
                            // SAFETY: Redis sets the API pointers before OnLoad runs.
                            if let Some(RM_AddACLCategory) = unsafe { raw::RedisModule_AddACLCategory } {
                                let module_acl_category = AclCategory::from($module_acl_category);
                                if module_acl_category != AclCategory::None {
                                    let category = CString::new(format!("{module_acl_category}")).unwrap();
                                    // SAFETY: ctx is the load context; the string outlives the call.
                                    if unsafe { RM_AddACLCategory(ctx, category.as_ptr()) } == raw::Status::Err as c_int {
                                        // Capture errno before formatting or logging can change it.
                                        let error = ::std::io::Error::last_os_error();
                                        // Redis keeps categories after unload and permits every
                                        // module to use them. Only EBUSY means the name exists.
                                        if error.raw_os_error() != Some($crate::__private::EBUSY) {
                                            // SAFETY: ctx is the module's load context.
                                            unsafe { raw::redis_log(ctx, &format!("Error: failed to add ACL category `{module_acl_category}`")) };
                                            return raw::Status::Err as c_int;
                                        }
                                    }
                                }
                            } else {
                                // SAFETY: ctx is the module's load context.
                                unsafe { raw::redis_log(ctx, "Warning: Redis version does not support adding new ACL categories") };
                            }
                        )*
                    )?

                    $(
                        $(
                            $crate::redis_command!(&context, $name, $command, $flags, $firstkey, $lastkey, $keystep, $mandatory_command_acl_categories $(, $optional_command_acl_categories)?);
                        )*
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
                                module_config_get(ctx, args, $module_name)
                            }, "", 0, 0, 0, "");
                        )?

                        $(
                            $crate::redis_command!(&context, $module_config_set_command, |ctx, args: Vec<RedisString>| {
                                module_config_set(ctx, args, $module_name)
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

                    raw::Status::Ok as c_int
                },
                || raw::Status::Err as c_int,
            )
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
                    let context = unsafe { $crate::Context::new(ctx) };
                    $(
                        if $deinit_func(&context) == $crate::Status::Err {
                            return $crate::Status::Err as c_int;
                        }
                    )*

                    $crate::raw::Status::Ok as c_int
                },
                || $crate::raw::Status::Err as c_int,
            )
        }
    }
}
