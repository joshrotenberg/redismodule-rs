//! Registration attributes and reply derives for the `redis-module` SDK.
//!
//! The generated handlers use SDK context and reply types. See the
//! [SDK migration guide](https://github.com/joshrotenberg/redismodule-rs/blob/master/docs/MIGRATING-3.0.md)
//! for their public paths and capabilities.

use proc_macro::TokenStream;

mod command;
mod info_section;
mod redis_value;
mod registration;

/// This proc macro allow to specify that the follow function is a Redis command.
/// The macro accept the following arguments that discribe the command properties:
/// * name (optional) - The command name. in case not given, the function name will be taken.
/// * flags - An array of command flag names, such as `ReadOnly`, `Write` or `DenyOOM`.
/// * enterprise_flags - An array of enterprise command flag names; accepts `ProxyFiltered`.
/// * summary (optional) - Command summary
/// * complexity (optional) - Command compexity
/// * since (optional) - At which module version the command was first introduce
/// * tips (optional) - Command tips for proxy, see [Redis command tips](https://redis.io/docs/latest/develop/reference/command-tips/).
/// * arity - Number of arguments, including the command name itself. A positive number specifies an exact number of arguments and a negative number
///   specifies a minimum number of arguments.
/// * key_spec - A list of specs representing how to find the keys that the command might touch. the following options are available:
///    * notes (optional) - Some note about the key spec.
///    * flags - List of flags reprenting how the keys are accessed, the following options are available:
///       * Readonly - Read-Only. Reads the value of the key, but doesn't necessarily return it.
///       * ReadWrite - Read-Write. Modifies the data stored in the value of the key or its metadata.
///       * Overwrite - Overwrite. Overwrites the data stored in the value of the key.
///       * Remove - Deletes the key.
///       * Access - Returns, copies or uses the user data from the value of the key.
///       * Update - Updates data to the value, new value may depend on the old value.
///       * Insert - Adds data to the value with no chance of modification or deletion of existing data.
///       * Delete - Explicitly deletes some content from the value of the key.
///       * NotKey - The key is not actually a key, but should be routed in cluster mode as if it was a key.
///       * Incomplete - The keyspec might not point out all the keys it should cover.
///       * VariableFlags - Some keys might have different flags depending on arguments.
///    * begin_search - Represents how Redis should start looking for keys.
///      There are 2 possible options:
///       * Index - start looking for keys from a given position.
///       * Keyword - Search for a specific keyward and start looking for keys from this keyword
///    * FindKeys - After Redis finds the location from where it needs to start looking for keys,
///      Redis will start finding keys base on the information in this struct.
///      There are 2 possible options:
///       * Range - An object of three element `last_key`, `steps`, `limit`.
///          * last_key - Index of the last key relative to the result of the
///            begin search step. Can be negative, in which case it's not
///            relative. -1 indicates the last argument, -2 one before the
///            last and so on.
///          * steps - How many arguments should we skip after finding a
///            key, in order to find the next one.
///          * limit - If `lastkey` is -1, we use `limit` to stop the search
///            by a factor. 0 and 1 mean no limit. 2 means 1/2 of the
///            remaining args, 3 means 1/3, and so on.
///       * Keynum -  An object of 3 elements `keynumidx`, `firstkey`, `keystep`.
///          * keynumidx - Index of the argument containing the number of
///            keys to come, relative to the result of the begin search step.
///          * firstkey - Index of the fist key relative to the result of the
///            begin search step. (Usually it's just after `keynumidx`, in
///            which case it should be set to `keynumidx + 1`.)
///          * keystep - How many arguments should we skip after finding a
///            key, in order to find the next one?
///
/// Example:
/// The following example will register a command called `foo`.
/// ```rust,no_run,ignore
/// #[command(
/// {
///    name: "test",
///    flags: [ReadOnly],
///    arity: -2,
///    key_spec: [
///        {
///            notes: "test command that define all the arguments at even possition as keys",
///            flags: [ReadOnly, Access],
///            begin_search: Keyword({ keyword : "foo", startfrom : 1 }),
///            find_keys: Range({ last_key :- 1, steps : 2, limit : 0 }),
///        }
///    ]
/// }
/// )]
/// fn test_command(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
///     Ok(RedisValue::SimpleStringStatic("OK"))
/// }
/// ```
///
/// **Notice**, by default Redis does not validate the command spec. User should validate the command keys on the module command code. The command spec is used for validation on cluster so Redis can raise a cross slot error when needed.
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    command::redis_command(attr, item)
}

/// Proc macro which is set on a function that need to be called whenever the server role changes.
/// The function must accept `&redis_module::EventContext` and
/// `redis_module::server_events::ServerRole`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[role_changed_event_handler]
/// fn role_changed_event_handler(ctx: &EventContext, values: ServerRole) { ... }
/// ```
#[proc_macro_attribute]
pub fn role_changed_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::ROLE_CHANGED_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever a loading event happened.
/// The function must accept `&redis_module::EventContext` and
/// `redis_module::server_events::LoadingSubevent`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[loading_event_handler]
/// fn loading_event_handler(ctx: &EventContext, values: LoadingSubevent) { ... }
/// ```
#[proc_macro_attribute]
pub fn loading_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::LOADING_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever a flush event happened.
/// The function must accept `&redis_module::EventContext` and
/// `redis_module::server_events::FlushSubevent`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[flush_event_handler]
/// fn flush_event_handler(ctx: &EventContext, values: FlushSubevent) { ... }
/// ```
#[proc_macro_attribute]
pub fn flush_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::FLUSH_SERVER_EVENTS_LIST),
    )
}

/// Registers a flush callback receiving its phase and copied database details.
/// The phase-only `flush_event_handler` can be used in the same module.
/// Each handler has its own panic boundary; ordering within either list is
/// unspecified. Requires the Redis server-event API (Redis 6.0+).
///
/// ```ignore
/// use redis_module::EventContext;
/// use redis_module_macros::flush_info_event_handler;
/// use redis_module::server_events::{FlushInfo, FlushSubevent};
/// #[flush_info_event_handler]
/// fn on_flush(ctx: &EventContext, phase: FlushSubevent, info: FlushInfo) {
///     if phase == FlushSubevent::Started && info.db == Some(2) {
///         ctx.log_notice("database 2 is being flushed");
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn flush_info_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::FLUSH_INFO_SERVER_EVENTS_LIST),
    )
}

/// Registers a callback after a successful `SWAPDB`. The copied payload names
/// both databases; use it rather than the event context's selected database.
/// Requires the Redis server-event API (Redis 6.0+).
///
/// ```ignore
/// use redis_module::EventContext;
/// use redis_module_macros::swapdb_event_handler;
/// use redis_module::server_events::SwapDbInfo;
/// #[swapdb_event_handler]
/// fn on_swap(ctx: &EventContext, info: SwapDbInfo) {
///     ctx.log_notice(&format!("swapped {} and {}", info.first, info.second));
/// }
/// ```
#[proc_macro_attribute]
pub fn swapdb_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::SWAPDB_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever a module is loaded or unloaded on the server.
/// The function must accept `&redis_module::EventContext` and
/// `redis_module::server_events::ModuleChangeSubevent`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[module_changed_event_handler]
/// fn module_changed_event_handler(ctx: &EventContext, values: ModuleChangeSubevent) { ... }
/// ```
#[proc_macro_attribute]
pub fn module_changed_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::MODULE_CHANGED_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever a configuration change
/// event is happening. The function must accept `&redis_module::EventContext` and
/// `&[&str]` containing the names of the configuration values that changed.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[config_changed_event_handler]
/// fn configuration_changed_event_handler(ctx: &EventContext, values: &[&str]) { ... }
/// ```
#[proc_macro_attribute]
pub fn config_changed_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::CONFIG_CHANGED_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called on Redis cron.
/// The function must accept `&redis_module::EventContext` and a [u64] representing the cron hz.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[cron_event_handler]
/// fn cron_event_handler(ctx: &EventContext, hz: u64) { ... }
/// ```
#[proc_macro_attribute]
pub fn cron_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::CRON_SERVER_EVENTS_LIST),
    )
}

/// Proc macro which is set on a function that needs to be called when Redis
/// shuts down. The function must accept `&redis_module::EventContext`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[shutdown_event_handler]
/// fn shutdown_event_handler(ctx: &EventContext) { ... }
/// ```
#[proc_macro_attribute]
pub fn shutdown_event_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::SHUTDOWN_SERVER_EVENTS_LIST),
    )
}

/// Generates a [From] implementation that converts the struct into
/// `redis_module::RedisValue`.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[derive(RedisValue)]
/// struct RedisValueDeriveInner {
///     i: i64,
/// }
///
/// #[derive(RedisValue)]
/// struct RedisValueDerive {
///     i: i64,
///     f: f64,
///     s: String,
///     u: usize,
///     v: Vec<i64>,
///     v2: Vec<RedisValueDeriveInner>,
///     hash_map: HashMap<String, String>,
///     hash_set: HashSet<String>,
///     ordered_map: BTreeMap<String, RedisValueDeriveInner>,
///     ordered_set: BTreeSet<String>,
/// }
///
/// #[command(
///     {
///         flags: [ReadOnly, NoMandatoryKeys],
///         arity: -1,
///         key_spec: [
///             {
///                 notes: "test redis value derive macro",
///                 flags: [ReadOnly, Access],
///                 begin_search: Index({ index : 0 }),
///                 find_keys: Range({ last_key : 0, steps : 0, limit : 0 }),
///             }
///         ]
///     }
/// )]
/// fn redis_value_derive(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
///     Ok(RedisValueDerive {
///         i: 10,
///         f: 1.1,
///         s: "s".to_owned(),
///         u: 20,
///         v: vec![1, 2, 3],
///         v2: vec![
///             RedisValueDeriveInner { i: 1 },
///             RedisValueDeriveInner { i: 2 },
///         ],
///         hash_map: HashMap::from([("key".to_owned(), "val`".to_owned())]),
///         hash_set: HashSet::from(["key".to_owned()]),
///         ordered_map: BTreeMap::from([("key".to_owned(), RedisValueDeriveInner { i: 10 })]),
///         ordered_set: BTreeSet::from(["key".to_owned()]),
///     }
///     .into())
/// }
/// ```
///
/// The [From] implementation produces a `redis_module::RedisValue::OrderedMap`:
/// field names become map keys, and each field value is converted with [Into].
///
/// The code above will generate the following reply (in resp3):
///
/// ```bash
/// 127.0.0.1:6379> redis_value_derive
/// 1# "f" => (double) 1.1
/// 2# "hash_map" => 1# "key" => "val"
/// 3# "hash_set" => 1~ "key"
/// 4# "i" => (integer) 10
/// 5# "ordered_map" => 1# "key" => 1# "i" => (integer) 10
/// 6# "ordered_set" => 1~ "key"
/// 7# "s" => "s"
/// 8# "u" => (integer) 20
/// 9# "v" =>
///    1) (integer) 1
///    2) (integer) 2
///    3) (integer) 3
/// 10# "v2" =>
///    1) 1# "i" => (integer) 1
///    2) 1# "i" => (integer) 2
/// ```
///
/// The derive proc macro can also be set on an Enum. In this case, the generated
/// code will check the enum variant (using a match statement) and perform [Into]
/// on the matched varient. This is usefull in case the command returns more than
/// a single reply type and the reply type need to be decided at runtime.
///
/// It is possible to specify a field attribute that will define a specific behavior
/// about the field. Supported attributes:
///
/// * flatten - indicate to inlines keys from a field into the parent struct.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[derive(RedisValue)]
/// struct RedisValueDeriveInner {
///     i2: i64,
/// }
///
/// #[derive(RedisValue)]
/// struct RedisValueDerive {
///     i1: i64,
///     #[RedisValueAttr{flatten: true}]
///     inner: RedisValueDeriveInner
/// }
///
/// #[command(
///     {
///         flags: [ReadOnly, NoMandatoryKeys],
///         arity: -1,
///         key_spec: [
///             {
///                 notes: "test redis value derive macro",
///                 flags: [ReadOnly, Access],
///                 begin_search: Index({ index : 0 }),
///                 find_keys: Range({ last_key : 0, steps : 0, limit : 0 }),
///             }
///         ]
///     }
/// )]
/// fn redis_value_derive(_ctx: &Context, _args: Vec<RedisString>) -> RedisResult {
///     Ok(RedisValueDerive {
///         i1: 10,
///         inner: RedisValueDeriveInner{ i2: 10 },
///     }
///     .into())
/// }
/// ```
///
/// The code above will generate the following reply (in resp3):
///
/// ```bash
/// 127.0.0.1:6379> redis_value_derive
/// 1# "i1" => 10
/// 2# "i2" => 10
/// ```
///
#[proc_macro_derive(RedisValue, attributes(RedisValueAttr))]
pub fn redis_value(item: TokenStream) -> TokenStream {
    redis_value::redis_value(item)
}

/// A procedural macro which registers this function as the custom
/// `INFO` command handler. There might be more than one handler, each
/// adding new information to the context.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[info_command_handler]
/// fn info_command_handler(
///     ctx: &InfoContext,
///     for_crash_report: bool) -> RedisResult
/// {
///     ctx.builder()
///         .add_section("test_info")
///         .field("test_field1", "test_value1")?
///         .field("test_field2", "test_value2")?
///         .build_section()?
///         .build_info()?;
///
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn info_command_handler(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::server_events::INFO_COMMAND_HANDLER_LIST),
    )
}

/// Implements a corresponding [`From`] for this struct, to convert
/// objects of this struct to an information object to be sent to the
/// `redis_module::InfoContext` as a reply.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[derive(InfoSection)]
/// struct Info {
///     field_1: String,
///     field_2: u64,
///     dictionary_1: BTreeMap<String, String>,
/// }
/// ```
///
/// This procedural macro only implements an easy way to convert objects
/// of this struct, it doesn't automatically do anything. To actually
/// make use of this, we must return an object of this struct from the
/// corresponding handler (`info` handler):
///
/// ```rust,no_run,ignore
/// static mut INFO: Info = Info::new();
///
/// #[info_command_handler]
/// fn info_command_handler(
///     ctx: &InfoContext,
///     _for_crash_report: bool) -> RedisResult
/// {
///     ctx.build_one_section(INFO)
/// }
/// ```
///
/// # Notes
///
/// 1. The name of the struct is taken "as is", so if it starts with
///    a capital letter (written in the "Upper Camel Case"), like in this
///    example - `Info`, then it will be compiled into a string prefixed
///    with the module name, ending up being `"module_name_Info"`-named
///    section. The fields of the struct are also prefixed with the module
///    name, so the `field_1` will be prefixed with `module_name_` as well.
/// 2. In dictionaries, the type of dictionaries supported varies,
///    for now it is [`std::collections::BTreeMap`] and
///    [`std::collections::HashMap`].
/// 3. In dictionaries, the value type can be anything that can be
///    converted into an object of type
///    `redis_module::InfoContextBuilderFieldBottomLevelValue`, for
///    example, a [`std::string::String`] or [`u64`]. Please, refer to
///    `redis_module::InfoContextBuilderFieldBottomLevelValue` for more
///    information.
#[proc_macro_derive(InfoSection)]
pub fn info_section(item: TokenStream) -> TokenStream {
    info_section::info_section(item)
}

/// Proc macro which is set on a function that need to be called whenever server performs defrag.
/// The function must accept `&redis_module::defrag::DefragContext`.
/// If defrag is not supported by the Redis version
/// the function will never be called.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[defrag_function]
/// fn defrag(ctx: &DefragContext) { ... }
/// ```
#[proc_macro_attribute]
pub fn defrag_function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::defrag::DEFRAG_FUNCTIONS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever server start performs defrag.
/// The function must accept `&redis_module::defrag::DefragContext`.
/// If the defrag start event is not supported by the Redis version
/// the function will never be called.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[defrag_start_function]
/// fn defrag_start(ctx: &DefragContext) { ... }
/// ```
#[proc_macro_attribute]
pub fn defrag_start_function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::defrag::DEFRAG_START_FUNCTIONS_LIST),
    )
}

/// Proc macro which is set on a function that need to be called whenever server end performs defrag.
/// The function must accept `&redis_module::defrag::DefragContext`.
/// If the defrag end event is not supported by the Redis version
/// the function will never be called.
///
/// Example:
///
/// ```rust,no_run,ignore
/// #[defrag_end_function]
/// fn defrag_end(ctx: &DefragContext) { ... }
/// ```
#[proc_macro_attribute]
pub fn defrag_end_function(_attr: TokenStream, item: TokenStream) -> TokenStream {
    registration::handler(
        item,
        syn::parse_quote!(::redis_module::defrag::DEFRAG_END_FUNCTIONS_LIST),
    )
}
