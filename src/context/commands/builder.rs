use super::{CommandCallback, command_flags_valid};
use crate::RegistrationContext;
use crate::{AclCategory, RedisError, Status, raw};
use bitflags::bitflags;
use std::ffi::CString;

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
#[must_use]
pub struct CommandBuilder {
    name: Vec<u8>,
    callback: CommandCallback,
    flags: CommandFlags,
    legacy_flags: Option<String>,
    keys: Option<KeyRange>,
    required_acl: AclCategory,
    optional_acl: Option<AclCategory>,
}

impl CommandBuilder {
    #[doc(hidden)]
    pub fn __new(name: impl Into<Vec<u8>>, callback: CommandCallback) -> Self {
        Self {
            name: name.into(),
            callback,
            flags: CommandFlags::empty(),
            legacy_flags: None,
            keys: None,
            required_acl: AclCategory::None,
            optional_acl: None,
        }
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
    pub fn register(self, ctx: &RegistrationContext<'_>) -> Result<(), RedisError> {
        if self.legacy_flags.is_none() {
            if let Some(keys) = self.keys {
                keys.validate()?;
            }
        }
        let typed_flags = self.flags.redis_flags()?;
        let mut flags = self.legacy_flags.unwrap_or_default();
        if !typed_flags.is_empty() {
            if !flags.is_empty() {
                flags.push(' ');
            }
            flags.push_str(&typed_flags);
        }
        // Validate category strings before creating the command.
        let required = CString::new(self.required_acl.to_string())?;
        let optional = self
            .optional_acl
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
        let name = register_base_command(ctx, &self.name, &flags, self.callback, self.keys)?;
        set_acl_categories(ctx, &name, &required, optional.as_deref())
    }
}

pub(super) fn register_base_command(
    ctx: &RegistrationContext<'_>,
    name: &[u8],
    flags: &str,
    callback: CommandCallback,
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
    if unsafe {
        create(
            ctx.get_raw(),
            name.as_ptr(),
            Some(callback),
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
