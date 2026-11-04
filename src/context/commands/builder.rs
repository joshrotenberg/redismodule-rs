use super::{
    COMMNAD_INFO_VERSION, CommandCallback, KeySpec, RedisModuleCommandArg, command_flags_valid,
    free_command_arg, free_redis_key_spec, get_redis_command_args, get_redis_key_spec,
};
use crate::RegistrationContext;
use crate::{AclCategory, RedisError, Status, raw};
use bitflags::bitflags;
use std::ffi::{CStr, CString};
use std::ptr;

macro_rules! define_command_flags {
    ($($name:ident = $bit:literal => $text:literal),* $(,)?) => {
        bitflags! {
            /// Typed flags for command registration. A running Redis that does
            /// not support a selected flag rejects the module load.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub struct CommandFlags: u32 { $(const $name = 1 << $bit;)* }
        }
        pub(super) const COMMAND_FLAGS: &[&str] = &[$($text),*];
        impl CommandFlags {
            fn redis_flags(self) -> Result<String, RedisError> {
                if self.bits() & !Self::all().bits() != 0 {
                    return Err(RedisError::Str("ERR unknown command flag bits"));
                }
                let mut names = Vec::new();
                $(if self.contains(Self::$name) { names.push($text); })*
                Ok(names.join(" "))
            }
        }
    };
}

define_command_flags! {
    WRITE = 0 => "write",
    READ_ONLY = 1 => "readonly",
    ADMIN = 2 => "admin",
    DENY_OOM = 3 => "deny-oom",
    DENY_SCRIPT = 4 => "deny-script",
    ALLOW_LOADING = 5 => "allow-loading",
    PUBSUB = 6 => "pubsub",
    RANDOM = 7 => "random",
    BLOCKING = 8 => "blocking",
    ALLOW_STALE = 9 => "allow-stale",
    NO_MONITOR = 10 => "no-monitor",
    NO_SLOWLOG = 11 => "no-slowlog",
    FAST = 12 => "fast",
    NO_AUTH = 13 => "no-auth",
    MAY_REPLICATE = 14 => "may-replicate",
    GETKEYS_API = 15 => "getkeys-api",
    GETCHANNELS_API = 16 => "getchannels-api",
    NO_CLUSTER = 17 => "no-cluster",
    NO_MANDATORY_KEYS = 18 => "no-mandatory-keys",
    ALLOW_BUSY = 19 => "allow-busy",
    INTERNAL = 20 => "internal",
    TOUCHES_ARBITRARY_KEYS = 21 => "touches-arbitrary-keys",
}

/// Positions of command arguments that are keys, counting the command as 0.
///
/// `KeyRange::new(1).last(-1).step(2)` finds arguments 1, 3, 5, and so on.
/// A negative last position counts backward from the end of the arguments.
/// Invalid ranges fail module loading before the command is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRange {
    first: i32,
    last: i32,
    step: i32,
}

impl KeyRange {
    /// A single key at `first`. Key positions must be greater than zero.
    #[must_use]
    pub const fn new(first: i32) -> Self {
        Self {
            first,
            last: first,
            step: 1,
        }
    }

    #[must_use]
    pub const fn last(mut self, last: i32) -> Self {
        self.last = last;
        self
    }

    #[must_use]
    pub const fn step(mut self, step: i32) -> Self {
        self.step = step;
        self
    }

    fn validate(self) -> Result<(), RedisError> {
        if self.first <= 0
            || self.step <= 0
            || self.last == 0
            || (self.last > 0 && self.last < self.first)
        {
            return Err(RedisError::Str("ERR invalid command key range"));
        }
        Ok(())
    }
}

/// An optional fluent command definition created by [`crate::redis_command!`].
///
/// Defaults: no flags, no keys, and no ACL categories. Definitions may be mixed
/// with legacy tuples in `redis_module!`'s `commands` list. Handlers retain the
/// same panic boundary and argument/reply conversion as the legacy syntax.
///
/// ```no_run
/// use redis_module::{Context, KeyRange, RedisResult, RedisString, redis_command};
/// fn get(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
///     ctx.call("GET", &[args.get(1).ok_or(redis_module::RedisError::WrongArity)?])
/// }
/// let command = redis_command!("example.get", get)
///     .read_only()
///     .deny_script()
///     .keys(KeyRange::new(1));
/// ```
///
/// A container groups subcommands under one name, as `XGROUP` or `CONFIG` do:
/// it has no handler, and each child is a definition of its own with a short
/// name. Redis calls a child for `<container> <child> [arg ...]`, and passes
/// the container and child names as the handler's first two arguments.
/// Containers need Redis 7.0 or later.
///
/// ```no_run
/// use redis_module::{CommandBuilder, Context, KeyRange, RedisResult, RedisString, redis_command};
/// fn create(ctx: &Context, args: Vec<RedisString>) -> RedisResult {
///     ctx.call("SET", &[args.get(2).ok_or(redis_module::RedisError::WrongArity)?, &args[3]])
/// }
/// let command = CommandBuilder::container("example.group")
///     .summary("Manages example groups")
///     .arity(-2)
///     .subcommand(
///         redis_command!("create", create)
///             .write()
///             .keys(KeyRange::new(2))
///             .arity(4)
///             .summary("Creates a group"),
///     );
/// ```
#[must_use]
pub struct CommandBuilder {
    name: Vec<u8>,
    /// `None` for a container.
    callback: Option<CommandCallback>,
    flags: CommandFlags,
    legacy_flags: Option<String>,
    keys: Option<KeyRange>,
    required_acl: AclCategory,
    optional_acl: Option<AclCategory>,
    info: Info,
    subcommands: Vec<CommandBuilder>,
}

/// The command info a definition sends in its one `SetCommandInfo` call.
#[derive(Default)]
struct Info {
    summary: Option<String>,
    complexity: Option<String>,
    since: Option<String>,
    history: Vec<(String, String)>,
    tips: Vec<String>,
    arity: Option<i32>,
    key_specs: Vec<KeySpec>,
    args: Vec<RedisModuleCommandArg>,
}

impl Info {
    fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.complexity.is_none()
            && self.since.is_none()
            && self.history.is_empty()
            && self.tips.is_empty()
            && self.arity.is_none()
            && self.key_specs.is_empty()
            && self.args.is_empty()
    }
}

impl CommandBuilder {
    #[doc(hidden)]
    pub fn __new(name: impl Into<Vec<u8>>, callback: CommandCallback) -> Self {
        Self::with_callback(name, Some(callback))
    }

    fn with_callback(name: impl Into<Vec<u8>>, callback: Option<CommandCallback>) -> Self {
        Self {
            name: name.into(),
            callback,
            flags: CommandFlags::empty(),
            legacy_flags: None,
            keys: None,
            required_acl: AclCategory::None,
            optional_acl: None,
            info: Info::default(),
            subcommands: Vec::new(),
        }
    }

    /// A container named `name`: a command without a handler that only groups
    /// the definitions added with [`subcommand`](Self::subcommand). Calling the
    /// container without a subcommand gets Redis's wrong-arity error, and an
    /// unknown subcommand gets Redis's unknown-subcommand error.
    ///
    /// Registration refuses a container without subcommands, which Redis would
    /// dispatch to its missing handler, and a container with keys or ACL
    /// categories: Redis grants a container's categories to every subcommand,
    /// so each subcommand sets its own. It also fails before Redis 7.0, which
    /// has no subcommands.
    pub fn container(name: impl Into<Vec<u8>>) -> Self {
        Self::with_callback(name, None)
    }

    /// Adds a subcommand to this container. `child` is named by its short
    /// name, such as `"create"`, and registers as `<container>|<child>`. It
    /// keeps its own flags, key range, ACL categories, summary and arity, and
    /// cannot have subcommands of its own. Names are unique ignoring ASCII
    /// case, as Redis looks them up.
    pub fn subcommand(mut self, child: CommandBuilder) -> Self {
        self.subcommands.push(child);
        self
    }

    /// Sets the summary that `COMMAND DOCS` reports. This and the other
    /// command-info setters (`complexity`, `since`, `history`, `tip`,
    /// `arity`, `key_spec`, `arg`) go to Redis in one `RedisModule_SetCommandInfo`
    /// call, which Redis accepts once per command; registration fails before
    /// Redis 7.0, which has no command info.
    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.info.summary = Some(summary.into());
        self
    }

    /// Sets the time complexity that `COMMAND DOCS` reports, such as `O(1)`.
    pub fn complexity(mut self, complexity: impl Into<String>) -> Self {
        self.info.complexity = Some(complexity.into());
        self
    }

    /// Sets the module version that added the command, which `COMMAND DOCS`
    /// reports.
    pub fn since(mut self, version: impl Into<String>) -> Self {
        self.info.since = Some(version.into());
        self
    }

    /// Adds a history entry, the version and what changed in it, which
    /// `COMMAND DOCS` reports; repeated calls accumulate in order.
    pub fn history(mut self, since: impl Into<String>, changes: impl Into<String>) -> Self {
        self.info.history.push((since.into(), changes.into()));
        self
    }

    /// Adds a command tip, such as `nondeterministic_output` or
    /// `request_policy:all_shards`, which `COMMAND INFO` reports; repeated
    /// calls accumulate. A tip cannot contain a space, since Redis splits
    /// tips on spaces.
    pub fn tip(mut self, tip: impl Into<String>) -> Self {
        self.info.tips.push(tip.into());
        self
    }

    /// Sets the arity that `COMMAND INFO` reports and Redis enforces: the
    /// exact number of arguments including the command name, or, if negative,
    /// the minimum. For a subcommand it counts the container and subcommand
    /// names, so it cannot be 1. A container's arity must require a
    /// subcommand (2 or more, or -2 or less; the default is -2), since Redis
    /// would otherwise run its missing handler.
    pub fn arity(mut self, arity: i32) -> Self {
        self.info.arity = Some(arity);
        self
    }

    /// Adds a key spec; repeated calls accumulate in order. Key specs replace
    /// the key range's single spec in `COMMAND INFO`, `COMMAND GETKEYSANDFLAGS`
    /// and key extraction, and their flags decide what `%R~` and `%W~` ACL
    /// key patterns allow: a range's spec allows only a user with full access
    /// to the keys. The types are those `#[command]` takes through `key_spec`.
    pub fn key_spec(mut self, spec: KeySpec) -> Self {
        self.info.key_specs.push(spec);
        self
    }

    /// Adds an argument description, which `COMMAND DOCS` reports; repeated
    /// calls accumulate in order. A key argument's `key_spec_index` refers to
    /// the definition's key specs.
    pub fn arg(mut self, arg: RedisModuleCommandArg) -> Self {
        self.info.args.push(arg);
        self
    }

    /// Adds flags to this definition; repeated calls accumulate.
    pub fn flags(mut self, flags: CommandFlags) -> Self {
        self.flags.insert(flags);
        self
    }

    pub fn read_only(self) -> Self {
        self.flags(CommandFlags::READ_ONLY)
    }
    pub fn write(self) -> Self {
        self.flags(CommandFlags::WRITE)
    }
    pub fn deny_script(self) -> Self {
        self.flags(CommandFlags::DENY_SCRIPT)
    }
    pub fn fast(self) -> Self {
        self.flags(CommandFlags::FAST)
    }

    /// Sets a single key position, replacing any previous range.
    pub fn key(self, position: i32) -> Self {
        self.keys(KeyRange::new(position))
    }

    /// Sets key positions, replacing any previous range. Use no key method for
    /// commands without keys, or GETKEYS_API for dynamic discovery.
    pub fn keys(mut self, keys: KeyRange) -> Self {
        self.keys = Some(keys);
        self
    }

    /// Sets categories that must be accepted by Redis for the module to load.
    pub fn acl_categories(mut self, categories: impl Into<AclCategory>) -> Self {
        self.required_acl = categories.into();
        self
    }

    /// Sets extra categories tried together with the required categories.
    /// Failure logs a warning and retries only the required categories. On
    /// Redis without command ACL support, optional categories are skipped.
    /// Custom names declared by `redis_module!` are also skipped when Redis
    /// cannot register them, after one module-level notice. Built-in and
    /// required categories are unaffected; other optional failures still warn.
    /// An explicitly empty optional category is rejected.
    pub fn optional_acl_categories(mut self, categories: impl Into<AclCategory>) -> Self {
        self.optional_acl = Some(categories.into());
        self
    }

    #[doc(hidden)]
    pub fn __legacy(mut self, flags: &str, first: i32, last: i32, step: i32) -> Self {
        self.legacy_flags = Some(flags.into());
        // Pass legacy metadata through unchanged, including values ignored
        // by Redis when first == 0. Builder ranges get stricter validation.
        self.keys = Some(KeyRange { first, last, step });
        self
    }

    /// Registers during module initialization. `redis_module!` calls this
    /// automatically for builder expressions in its command list. A runtime
    /// [`Context`](crate::Context) cannot supply the required load-time capability.
    ///
    /// A container and its subcommands are checked before anything is
    /// registered, but Redis can still refuse a step after the container
    /// exists, and Redis has no way to remove a command during the load. An
    /// `init` that registers a definition itself should fail the load when
    /// this returns an error, as `redis_module!` does.
    pub fn register(self, ctx: &RegistrationContext<'_>) -> Result<(), RedisError> {
        if self.callback.is_some() {
            if !self.subcommands.is_empty() {
                return Err(RedisError::Str(
                    "ERR only a container, from CommandBuilder::container, can have subcommands",
                ));
            }
            let prepared = self.prepare(ctx)?;
            let name =
                register_base_command(ctx, &self.name, &prepared.flags, self.callback, self.keys)?;
            set_acl_categories(ctx, &name, &prepared.required, prepared.optional.as_deref())?;
            return set_command_info(ctx, &name, self.info);
        }
        self.register_container(ctx)
    }

    fn register_container(self, ctx: &RegistrationContext<'_>) -> Result<(), RedisError> {
        if self.keys.is_some() {
            return Err(RedisError::Str("ERR a command container cannot have keys"));
        }
        // Redis 7.0 accepts these in a command name but cannot then look the
        // container up; 7.2 and later refuse them.
        if self
            .name
            .iter()
            .any(|byte| INVALID_NAME_BYTES.contains(byte))
        {
            return Err(RedisError::Str(
                "ERR a command container name cannot contain spaces, line breaks or any of |@=,",
            ));
        }
        // Redis grants a container's categories to every subcommand.
        if !self.required_acl.to_string().is_empty() || self.optional_acl.is_some() {
            return Err(RedisError::Str(
                "ERR a command container cannot have ACL categories: Redis grants them to every subcommand",
            ));
        }
        // Redis runs a container's handler, which it does not have, for a
        // call its arity accepts without a subcommand, and for any call to a
        // container without subcommands.
        if matches!(self.info.arity, Some(1 | -1)) {
            return Err(RedisError::Str(
                "ERR a command container's arity must require a subcommand (2 or more, or -2 or less)",
            ));
        }
        if self.subcommands.is_empty() {
            return Err(RedisError::Str(
                "ERR a command container needs at least one subcommand",
            ));
        }
        let create = unsafe { raw::RedisModule_CreateSubcommand }
            .ok_or(RedisError::Str(SUBCOMMANDS_NEED_7_0))?;
        let prepared = self.prepare(ctx)?;
        let mut children: Vec<(CommandBuilder, CString, Prepared)> =
            Vec::with_capacity(self.subcommands.len());
        for child in self.subcommands {
            let checked = child.check_subcommand(ctx)?;
            // Redis looks subcommands up case-insensitively.
            if children
                .iter()
                .any(|(_, other, _)| checked.0.as_bytes().eq_ignore_ascii_case(other.as_bytes()))
            {
                return Err(RedisError::String(format!(
                    "ERR duplicate subcommand {}",
                    checked.0.to_string_lossy()
                )));
            }
            children.push((child, checked.0, checked.1));
        }

        let name = register_base_command(ctx, &self.name, &prepared.flags, None, None)?;
        let get = unsafe { crate::__available_api!(RedisModule_GetCommand) };
        let container = unsafe { get(ctx.get_raw(), name.as_ptr()) };
        if container.is_null() {
            return Err(RedisError::Str("ERR registered command was not found"));
        }
        // Create every subcommand before setting anything else, so a failed
        // step leaves the container with the subcommands it has.
        let mut registered: Vec<(CommandBuilder, CString, Prepared)> =
            Vec::with_capacity(children.len());
        for (child, short_name, prepared) in children {
            let flags = CString::new(prepared.flags.as_str())?;
            let callback: raw::RedisModuleCmdFunc = match child.callback {
                Some(callback) => Some(callback),
                None => None,
            };
            let (first, last, step) = child
                .keys
                .map_or((0, 0, 0), |keys| (keys.first, keys.last, keys.step));
            // SAFETY: container is this module's registered container, and
            // the strings outlive the call, which copies them.
            if unsafe {
                create(
                    container,
                    short_name.as_ptr(),
                    callback,
                    flags.as_ptr(),
                    first,
                    last,
                    step,
                )
            } != Status::Ok as i32
            {
                return Err(RedisError::String(format!(
                    "ERR failed to register subcommand {}|{}",
                    name.to_string_lossy(),
                    short_name.to_string_lossy()
                )));
            }
            let full_name = CString::new([name.as_bytes(), b"|", short_name.as_bytes()].concat())?;
            registered.push((child, full_name, prepared));
        }
        for (child, full_name, prepared) in registered {
            set_acl_categories(
                ctx,
                &full_name,
                &prepared.required,
                prepared.optional.as_deref(),
            )?;
            set_command_info(ctx, &full_name, child.info)?;
        }
        set_command_info(ctx, &name, self.info)
    }

    /// Checks a container's child and returns its name and prepared metadata.
    fn check_subcommand(
        &self,
        ctx: &RegistrationContext<'_>,
    ) -> Result<(CString, Prepared), RedisError> {
        if self.callback.is_none() {
            return Err(RedisError::Str("ERR a subcommand cannot be a container"));
        }
        if !self.subcommands.is_empty() {
            return Err(RedisError::Str("ERR a subcommand cannot have subcommands"));
        }
        let child = CString::new(self.name.as_slice())?;
        if child.as_bytes().is_empty()
            || child
                .as_bytes()
                .iter()
                .any(|byte| INVALID_NAME_BYTES.contains(byte))
        {
            return Err(RedisError::Str(
                "ERR a subcommand name must be non-empty and cannot contain spaces, line breaks or any of |@=,",
            ));
        }
        // A subcommand's arguments always include the container's name and
        // its own, so an arity of exactly 1 could never be met.
        if self.info.arity == Some(1) {
            return Err(RedisError::Str(
                "ERR a subcommand's arity counts the container and subcommand names and cannot be 1",
            ));
        }
        Ok((child, self.prepare(ctx)?))
    }

    /// Validates the key range and flags and resolves the ACL categories.
    fn prepare(&self, ctx: &RegistrationContext<'_>) -> Result<Prepared, RedisError> {
        if self.info.arity == Some(0) {
            return Err(RedisError::Str("ERR a command arity must not be 0"));
        }
        if self.legacy_flags.is_none() {
            if let Some(keys) = self.keys {
                keys.validate()?;
            }
        }
        let typed_flags = self.flags.redis_flags()?;
        let mut flags = self.legacy_flags.clone().unwrap_or_default();
        if !typed_flags.is_empty() {
            if !flags.is_empty() {
                flags.push(' ');
            }
            flags.push_str(&typed_flags);
        }
        if !command_flags_valid(&flags) {
            return Err(RedisError::Str("ERR unknown or malformed command flags"));
        }
        // Validate category strings before creating the command.
        let required = CString::new(self.required_acl.to_string())?;
        let optional = self
            .optional_acl
            .as_ref()
            .map(|category| CString::new(category.to_string()))
            .transpose()?;
        if optional
            .as_ref()
            .is_some_and(|category| category.as_bytes().is_empty())
        {
            return Err(RedisError::Str(
                "ERR optional ACL categories must not be empty",
            ));
        }
        let optional = optional
            .map(|category| {
                // The C string was constructed above from a Rust String.
                let categories = ctx.filter_optional_acl_categories(category.to_str().unwrap());
                CString::new(categories)
            })
            .transpose()?
            .filter(|category| !category.as_bytes().is_empty());
        Ok(Prepared {
            flags,
            required,
            optional,
        })
    }
}

/// A definition's flags and ACL categories, checked for registration.
struct Prepared {
    flags: String,
    required: CString,
    optional: Option<CString>,
}

/// Bytes Redis 7.2 and later refuse in a command name (`isCommandNameValid`).
const INVALID_NAME_BYTES: &[u8] = b" \r\n|@=,";

const SUBCOMMANDS_NEED_7_0: &str =
    "ERR command containers and subcommands need Redis 7.0 or later (RedisModule_CreateSubcommand)";

/// Sends `name`'s command info, if any, in one `SetCommandInfo` call.
fn set_command_info(
    ctx: &RegistrationContext<'_>,
    name: &CStr,
    info: Info,
) -> Result<(), RedisError> {
    if info.is_empty() {
        return Ok(());
    }
    let set_info = unsafe { raw::RedisModule_SetCommandInfo }.ok_or(RedisError::Str(
        "ERR command info needs Redis 7.0 or later (RedisModule_SetCommandInfo)",
    ))?;
    if info
        .tips
        .iter()
        .any(|tip| tip.is_empty() || tip.contains(' '))
    {
        return Err(RedisError::Str(
            "ERR a command tip must be non-empty and contain no space",
        ));
    }
    let text = |value: Option<String>| value.map(CString::new).transpose();
    let summary = text(info.summary)?;
    let complexity = text(info.complexity)?;
    let since = text(info.since)?;
    let tips = (!info.tips.is_empty())
        .then(|| CString::new(info.tips.join(" ")))
        .transpose()?;
    let history = info
        .history
        .into_iter()
        .map(|(since, changes)| Ok((CString::new(since)?, CString::new(changes)?)))
        .collect::<Result<Vec<_>, RedisError>>()?;
    let get = unsafe { crate::__available_api!(RedisModule_GetCommand) };
    let command = unsafe { get(ctx.get_raw(), name.as_ptr()) };
    if command.is_null() {
        return Err(RedisError::Str("ERR registered command was not found"));
    }
    let mut history_entries: Vec<raw::RedisModuleCommandHistoryEntry> = history
        .iter()
        .map(|(since, changes)| raw::RedisModuleCommandHistoryEntry {
            since: since.as_ptr(),
            changes: changes.as_ptr(),
        })
        .collect();
    if !history_entries.is_empty() {
        history_entries.push(raw::RedisModuleCommandHistoryEntry {
            since: ptr::null(),
            changes: ptr::null(),
        });
    }
    // The key specs and args leak their strings into the raw structs until
    // they are freed below, after Redis copies them.
    let key_specs = (!info.key_specs.is_empty()).then(|| get_redis_key_spec(info.key_specs));
    let mut args = get_redis_command_args(info.args);
    let pointer = |value: &Option<CString>| value.as_ref().map_or(ptr::null(), |v| v.as_ptr());
    let raw_info = raw::RedisModuleCommandInfo {
        version: &COMMNAD_INFO_VERSION,
        summary: pointer(&summary),
        complexity: pointer(&complexity),
        since: pointer(&since),
        history: if history_entries.is_empty() {
            ptr::null_mut()
        } else {
            history_entries.as_mut_ptr()
        },
        tips: pointer(&tips),
        arity: info.arity.unwrap_or(0),
        key_specs: key_specs
            .as_ref()
            .map_or(ptr::null_mut(), |specs| specs.as_ptr().cast_mut()),
        args: args
            .as_mut()
            .map_or(ptr::null_mut(), |args| args.as_mut_ptr()),
    };
    // SAFETY: command is this module's registered command, and the info and
    // everything it points to outlive the call, which copies them.
    let status = unsafe { set_info(command, &raw_info) };
    if let Some(key_specs) = key_specs {
        free_redis_key_spec(key_specs);
    }
    args.unwrap_or_default().iter().for_each(free_command_arg);
    if status != Status::Ok as i32 {
        return Err(RedisError::String(format!(
            "ERR Redis refused the command info of {} (an invalid key spec or argument, or info already set)",
            name.to_string_lossy()
        )));
    }
    Ok(())
}

pub(super) fn register_base_command(
    ctx: &RegistrationContext<'_>,
    name: &[u8],
    flags: &str,
    callback: Option<CommandCallback>,
    keys: Option<KeyRange>,
) -> Result<CString, RedisError> {
    let name = CString::new(name)?;
    if name.as_bytes().is_empty() {
        return Err(RedisError::Str("ERR command name must not be empty"));
    }
    if !command_flags_valid(flags) {
        return Err(RedisError::Str("ERR unknown or malformed command flags"));
    }
    let flags = CString::new(flags)?;
    let (first, last, step) = keys.map_or((0, 0, 0), |keys| (keys.first, keys.last, keys.step));
    let create = unsafe { crate::__available_api!(RedisModule_CreateCommand) };
    // A container passes no handler.
    let callback: raw::RedisModuleCmdFunc = match callback {
        Some(callback) => Some(callback),
        None => None,
    };
    if unsafe {
        create(
            ctx.get_raw(),
            name.as_ptr(),
            callback,
            flags.as_ptr(),
            first,
            last,
            step,
        )
    } != Status::Ok as i32
    {
        return Err(RedisError::String(format!(
            "ERR failed to register command {}",
            name.to_string_lossy()
        )));
    }
    Ok(name)
}

fn set_acl_categories(
    ctx: &RegistrationContext<'_>,
    name: &std::ffi::CStr,
    required: &std::ffi::CStr,
    optional: Option<&std::ffi::CStr>,
) -> Result<(), RedisError> {
    let has_required = !required.to_bytes().is_empty();
    let Some(set_acl) = (unsafe { raw::RedisModule_SetCommandACLCategories }) else {
        return if has_required {
            Err(RedisError::Str(
                "ERR Redis version does not support ACL categories",
            ))
        } else {
            Ok(())
        };
    };
    if !has_required && optional.is_none() {
        return Ok(());
    }
    let get = unsafe { crate::__available_api!(RedisModule_GetCommand) };
    let command = unsafe { get(ctx.get_raw(), name.as_ptr()) };
    if command.is_null() {
        return Err(RedisError::Str("ERR registered command was not found"));
    }
    if let Some(optional) = optional {
        let categories = if has_required {
            CString::new([required.to_bytes(), b" ", optional.to_bytes()].concat())?
        } else {
            optional.to_owned()
        };
        if unsafe { set_acl(command, categories.as_ptr()) } == Status::Ok as i32 {
            return Ok(());
        }
        ctx.log_warning(&format!(
            "Failed to set optional ACL categories for command {}",
            name.to_string_lossy()
        ));
    }
    if has_required && unsafe { set_acl(command, required.as_ptr()) } != Status::Ok as i32 {
        return Err(RedisError::String(format!(
            "ERR failed to set mandatory ACL categories for command {}",
            name.to_string_lossy()
        )));
    }
    Ok(())
}
