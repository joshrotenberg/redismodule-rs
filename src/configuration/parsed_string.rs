use super::{ConfigurationContext, ConfigurationValue};
use crate::{RedisError, RedisGILGuard, RedisLockIndicator, RedisString};

struct Value<T> {
    raw: String,
    parsed: T,
}

/// A UTF-8 string configuration with a validated, parsed value.
///
/// The parser runs before publication. Returning an error leaves both the raw
/// text and parsed value unchanged. The accepted text is preserved exactly for
/// `CONFIG GET`, even when the parser normalizes it. Register this value in the
/// existing `string` configuration group or with
/// [`super::register_string_configuration`]. Redis applies registration defaults
/// and configured load values through the same validating setter. The macro
/// also validates module arguments when `module_args_as_configuration: true`.
///
/// `T` must be `Send` because a configuration can be accessed by locked workers;
/// it need not be `Clone` or `Sync`. Reads require a [`RedisLockIndicator`].
/// Parsers should have no side effects: Redis can invoke them repeatedly during
/// loading and configuration rollback. An `on_changed` callback observes an
/// accepted value after storage; validation belongs in the parser.
///
/// ```no_run
/// use redis_module::{Context, RedisError};
/// use redis_module::configuration::ParsedStringConfig;
/// fn parse(text: &str) -> Result<u16, RedisError> {
///     text.trim().parse().map_err(|_| RedisError::Str("expected a u16"))
/// }
/// let config = ParsedStringConfig::new(" 12 ", parse)?;
/// # fn read(config: &ParsedStringConfig<u16>, ctx: &Context) {
/// let (raw, number) = config.with_value(ctx, |raw, number| (raw.to_owned(), *number));
/// # }
/// # Ok::<(), RedisError>(())
/// ```
pub struct ParsedStringConfig<T: Send> {
    value: RedisGILGuard<Value<T>>,
    parse: fn(&str) -> Result<T, RedisError>,
}

impl<T: Send> ParsedStringConfig<T> {
    /// Validate the initial text without using any Redis API.
    ///
    /// This initial value does not replace the default supplied at registration;
    /// Redis later validates and applies the selected default or configured
    /// load value.
    pub fn new(
        raw: impl Into<String>,
        parse: fn(&str) -> Result<T, RedisError>,
    ) -> Result<Self, RedisError> {
        let raw = raw.into();
        let parsed = parse(&raw)?;
        Ok(Self {
            value: RedisGILGuard::new(Value { raw, parsed }),
            parse,
        })
    }

    /// Read the matching raw and parsed values while holding the Redis lock.
    ///
    /// The closure borrows the value. Do not reenter this configuration from
    /// the closure, including through Redis calls that invoke its callbacks.
    /// Return owned results; references cannot escape the lock proof:
    ///
    /// ```compile_fail
    /// use redis_module::{Context, configuration::ParsedStringConfig};
    /// fn escape<'a>(config: &'a ParsedStringConfig<String>, ctx: &Context) -> &'a str {
    ///     config.with_value(ctx, |raw, _| raw)
    /// }
    /// ```
    ///
    /// Parsed references cannot escape either:
    ///
    /// ```compile_fail
    /// use redis_module::{Context, configuration::ParsedStringConfig};
    /// fn escape<'a>(config: &'a ParsedStringConfig<Vec<String>>, ctx: &Context) -> &'a [String] {
    ///     config.with_value(ctx, |_, parsed| parsed.as_slice())
    /// }
    /// ```
    pub fn with_value<G: RedisLockIndicator, R>(
        &self,
        ctx: &G,
        read: impl FnOnce(&str, &T) -> R,
    ) -> R {
        let value = self.value.lock(ctx);
        read(&value.raw, &value.parsed)
    }

    fn update(&self, ctx: &ConfigurationContext, raw: &str) -> Result<(), RedisError> {
        let parsed = (self.parse)(raw)?;
        let replacement = Value {
            raw: raw.to_owned(),
            parsed,
        };
        let old = {
            let mut value = self.value.lock(ctx);
            std::mem::replace(&mut *value, replacement)
        };
        // User destructors may reenter the configuration. The complete new
        // value is already published, and its borrow has been released.
        drop(old);
        Ok(())
    }
}

impl<T: Send> ConfigurationValue<RedisString> for ParsedStringConfig<T> {
    fn get(&self, ctx: &ConfigurationContext) -> RedisString {
        let raw = self.with_value(ctx, |raw, _| raw.to_owned());
        RedisString::create(None, raw.as_str())
    }

    fn set(&self, ctx: &ConfigurationContext, value: RedisString) -> Result<(), RedisError> {
        self.update(ctx, value.try_as_str()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<u16, RedisError> {
        let value = text
            .trim()
            .parse::<u16>()
            .map_err(|_| RedisError::Str("expected 1..=64"))?;
        (1..=64)
            .contains(&value)
            .then_some(value)
            .ok_or(RedisError::Str("expected 1..=64"))
    }

    #[test]
    fn constructor_validates_and_preserves_text() {
        assert!(ParsedStringConfig::new("0", parse).is_err());
        let config = ParsedStringConfig::new(" 008 ", parse).unwrap();
        let ctx = ConfigurationContext::new();
        config.with_value(&ctx, |raw, parsed| {
            assert_eq!(raw, " 008 ");
            assert_eq!(*parsed, 8);
        });
    }

    #[test]
    fn rejected_updates_preserve_the_pair_and_valid_updates_replace_it() {
        let ctx = ConfigurationContext::new();
        let config = ParsedStringConfig::new("2", parse).unwrap();
        for bad in ["", "0", "65", "not a number"] {
            assert!(config.update(&ctx, bad).is_err());
            config.with_value(&ctx, |raw, parsed| assert_eq!((raw, *parsed), ("2", 2)));
        }
        config.update(&ctx, " 004 ").unwrap();
        config.with_value(&ctx, |raw, parsed| assert_eq!((raw, *parsed), (" 004 ", 4)));
    }

    #[test]
    fn parser_panic_does_not_publish_a_partial_value() {
        fn parser(text: &str) -> Result<u16, RedisError> {
            assert_ne!(text, "panic", "parser panic");
            parse(text)
        }
        let ctx = ConfigurationContext::new();
        let config = ParsedStringConfig::new("2", parser).unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                config.update(&ctx, "panic")
            }))
            .is_err()
        );
        config.with_value(&ctx, |raw, parsed| assert_eq!((raw, *parsed), ("2", 2)));
        config.update(&ctx, "4").unwrap();
    }

    #[test]
    fn old_value_is_dropped_after_publication_and_borrow_release() {
        use std::sync::{
            OnceLock,
            atomic::{AtomicUsize, Ordering},
        };
        static CONFIG: OnceLock<ParsedStringConfig<Probe>> = OnceLock::new();
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        // Not Clone: reads do not require cloning the parsed value.
        struct Probe;
        impl Drop for Probe {
            fn drop(&mut self) {
                CONFIG
                    .get()
                    .unwrap()
                    .with_value(&ConfigurationContext::new(), |raw, _| {
                        assert_eq!(raw, "new");
                    });
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        let config = CONFIG.get_or_init(|| ParsedStringConfig::new("old", |_| Ok(Probe)).unwrap());
        config.update(&ConfigurationContext::new(), "new").unwrap();
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }
}
