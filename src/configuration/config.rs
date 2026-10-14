//! Typed options for direct configuration registration.
//!
//! Builders borrow names/default text, require static typed storage, and keep no
//! Context. `register` consumes the builder and delegates to the corresponding
//! fallible free function. Existing native OnLoad, allocator and callback-phase
//! obligations still apply; string registration requires the SDK load scope.
//! `redis_module!` supplies that scope around user init. Custom raw entry points
//! must follow [`super::StringConfigurationLoadScope`]'s unsafe lifecycle recipe.
//!
//! ```no_run
//! use redis_module::{Context, RedisError};
//! use redis_module::configuration::config;
//! use std::sync::atomic::AtomicI64;
//! static LIMIT: AtomicI64 = AtomicI64::new(100);
//!
//! fn register(ctx: &Context) -> Result<(), RedisError> {
//!     config::integer("limit", &LIMIT, 100, 1..=10_000)
//!         .on_changed(|_, name, _| { let _ = name; })
//!         .register(ctx)
//! }
//! ```

use super::{
    register_bool_configuration, register_enum_configuration, register_i64_configuration,
    register_string_configuration, ConfigurationContext, ConfigurationFlags, ConfigurationValue,
    EnumConfigurationValue, OnUpdatedCallback,
};
use crate::{Context, RedisError, RedisString};
use std::ops::RangeInclusive;

struct Options<'a, T: 'static> {
    name: &'a str,
    storage: &'static T,
    flags: ConfigurationFlags,
    on_changed: Option<OnUpdatedCallback<T>>,
}

impl<'a, T: 'static> Options<'a, T> {
    fn new(name: &'a str, storage: &'static T) -> Self {
        Self {
            name,
            storage,
            flags: ConfigurationFlags::DEFAULT,
            on_changed: None,
        }
    }
}

// These common modifiers only store options. Native policy lives in the existing
// registration functions; there is no second registration/validation engine.
macro_rules! modifiers {
    ($T:ident) => {
        /// Replace the registration flags (default: DEFAULT).
        pub fn flags(mut self, flags: ConfigurationFlags) -> Self {
            self.options.flags = flags;
            self
        }

        /// Install the existing change callback, boxing it once.
        ///
        /// Captures must be owned/static. No Send/Sync bound is added. This
        /// hides `Some(Box::new(...))` syntax, not the callback allocation.
        pub fn on_changed<F>(mut self, callback: F) -> Self
        where
            F: Fn(&ConfigurationContext, &str, &'static $T) + 'static,
        {
            self.options.on_changed = Some(Box::new(callback));
            self
        }
    };
}

/// An integer registration with a mandatory inclusive range.
#[must_use = "register the configuration or explicitly discard its options"]
pub struct IntegerConfig<'a, T: ConfigurationValue<i64> + 'static> {
    options: Options<'a, T>,
    default: i64,
    range: RangeInclusive<i64>,
}

/// Prepare an integer configuration; no registration occurs until `register`.
pub fn integer<'a, T: ConfigurationValue<i64> + 'static>(
    name: &'a str,
    storage: &'static T,
    default: i64,
    range: RangeInclusive<i64>,
) -> IntegerConfig<'a, T> {
    IntegerConfig {
        options: Options::new(name, storage),
        default,
        range,
    }
}

impl<T: ConfigurationValue<i64> + 'static> IntegerConfig<'_, T> {
    modifiers!(T);

    /// Register during legal OnLoad; propagate errors as OnLoad ERR.
    pub fn register(self, ctx: &Context) -> Result<(), RedisError> {
        register_i64_configuration(
            ctx,
            self.options.name,
            self.options.storage,
            self.default,
            *self.range.start(),
            *self.range.end(),
            self.options.flags,
            self.options.on_changed,
        )
    }
}

/// A boolean registration.
#[must_use = "register the configuration or explicitly discard its options"]
pub struct BooleanConfig<'a, T: ConfigurationValue<bool> + 'static> {
    options: Options<'a, T>,
    default: bool,
}

/// Prepare a boolean configuration with default flags and no callback.
pub fn boolean<'a, T: ConfigurationValue<bool> + 'static>(
    name: &'a str,
    storage: &'static T,
    default: bool,
) -> BooleanConfig<'a, T> {
    BooleanConfig {
        options: Options::new(name, storage),
        default,
    }
}

impl<T: ConfigurationValue<bool> + 'static> BooleanConfig<'_, T> {
    modifiers!(T);

    /// Register during legal OnLoad; propagate errors as OnLoad ERR.
    pub fn register(self, ctx: &Context) -> Result<(), RedisError> {
        register_bool_configuration(
            ctx,
            self.options.name,
            self.options.storage,
            self.default,
            self.options.flags,
            self.options.on_changed,
        )
    }
}

/// A string registration borrowing its default until registration.
#[must_use = "register the configuration or explicitly discard its options"]
pub struct StringConfig<'a, T: ConfigurationValue<RedisString> + 'static> {
    options: Options<'a, T>,
    default: &'a str,
}

/// Prepare a string configuration; `register` retains SDK load-scope admission.
pub fn string<'a, T: ConfigurationValue<RedisString> + 'static>(
    name: &'a str,
    storage: &'static T,
    default: &'a str,
) -> StringConfig<'a, T> {
    StringConfig {
        options: Options::new(name, storage),
        default,
    }
}

impl<T: ConfigurationValue<RedisString> + 'static> StringConfig<'_, T> {
    modifiers!(T);

    /// Register in the active SDK load scope; propagate any error as OnLoad ERR.
    pub fn register(self, ctx: &Context) -> Result<(), RedisError> {
        register_string_configuration(
            ctx,
            self.options.name,
            self.options.storage,
            self.default,
            self.options.flags,
            self.options.on_changed,
        )
    }
}

/// An enum registration preserving its existing typed default/storage traits.
#[must_use = "register the configuration or explicitly discard its options"]
pub struct EnumConfig<'a, G: EnumConfigurationValue, T: ConfigurationValue<G> + 'static> {
    options: Options<'a, T>,
    default: G,
}

/// Prepare an enum configuration; infer the enum type from its default/storage.
pub fn enumeration<'a, G: EnumConfigurationValue, T: ConfigurationValue<G> + 'static>(
    name: &'a str,
    storage: &'static T,
    default: G,
) -> EnumConfig<'a, G, T> {
    EnumConfig {
        options: Options::new(name, storage),
        default,
    }
}

impl<G: EnumConfigurationValue, T: ConfigurationValue<G> + 'static> EnumConfig<'_, G, T> {
    modifiers!(T);

    /// Register during legal OnLoad; propagate errors as OnLoad ERR.
    pub fn register(self, ctx: &Context) -> Result<(), RedisError> {
        register_enum_configuration(
            ctx,
            self.options.name,
            self.options.storage,
            self.default,
            self.options.flags,
            self.options.on_changed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc, sync::atomic::AtomicBool};

    static STORAGE: AtomicBool = AtomicBool::new(false);
    struct Capture(Rc<Cell<usize>>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn replacing_and_discarding_unregistered_callbacks_releases_captures_without_calls() {
        let drops = Rc::new(Cell::new(0));
        let calls = Rc::new(Cell::new(0));
        let callback = || {
            let capture = Capture(Rc::clone(&drops));
            let calls = Rc::clone(&calls);
            move |_: &ConfigurationContext, _: &str, _: &'static AtomicBool| {
                let _ = &capture;
                calls.set(calls.get() + 1);
            }
        };
        let builder = boolean("unregistered", &STORAGE, false).on_changed(callback());
        let builder = builder.on_changed(callback());
        assert_eq!(drops.get(), 1);
        drop(builder);
        assert_eq!(drops.get(), 2);
        assert_eq!(calls.get(), 0);
    }
}
