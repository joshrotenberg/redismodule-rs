/// Encode a release version as `major * 10000 + minor * 100 + patch`.
///
/// Use this for the integer `version:` in [`redis_module!`](crate::redis_module),
/// reported by `MODULE LIST`. Minor and patch must each be below 100 so they
/// cannot collide with the next component. Valid SemVer build metadata is
/// ignored, so `1.2.3+build.01` and `1.2.3` both encode as `10203`.
/// Prereleases are rejected because this encoding cannot distinguish them
/// from their corresponding release. Zero (`0.0.0`) is allowed.
///
/// This is an optional convention for module versions, independent of the
/// server version and native data-type encoding versions. It needs no server.
/// [`module_version!`](crate::module_version!) applies it to the caller's Cargo
/// package version and always validates at compile time.
///
/// # Panics
///
/// Panics on malformed SemVer, prereleases, minor or patch values above 99,
/// or an encoded value exceeding [`i32::MAX`]. Core components must be decimal
/// integers without leading zeroes. A call in a const expression makes these
/// errors compile-time failures.
///
/// ```
/// use redis_module::encode_module_version;
/// const VERSION: i32 = encode_module_version("1.23.45");
/// assert_eq!(VERSION, 12345);
/// assert_eq!(encode_module_version("1.23.45+build.01"), VERSION);
/// ```
///
/// ```compile_fail,E0080
/// const VERSION: i32 = redis_module::encode_module_version("1.2.3-rc.1");
/// ```
///
/// ```compile_fail,E0080
/// const VERSION: i32 = redis_module::encode_module_version("1.100.0");
/// ```
///
/// ```compile_fail,E0080
/// const VERSION: i32 = redis_module::encode_module_version("214748.36.48");
/// ```
#[must_use]
pub const fn encode_module_version(version: &str) -> i32 {
    let bytes = version.as_bytes();
    let mut position = 0;
    let mut component = 0;
    let mut encoded = 0_i64;
    while component < 3 {
        let start = position;
        let mut value = 0_i32;
        while position < bytes.len() && bytes[position] >= b'0' && bytes[position] <= b'9' {
            let digit = (bytes[position] - b'0') as i32;
            assert!(
                value <= (i32::MAX - digit) / 10,
                "module version exceeds i32::MAX"
            );
            value = value * 10 + digit;
            position += 1;
        }
        assert!(position > start, "module version must be major.minor.patch");
        assert!(
            position - start == 1 || bytes[start] != b'0',
            "module version components cannot have leading zeroes"
        );
        if component > 0 {
            assert!(
                value < 100,
                "module version minor and patch must be below 100"
            );
        }
        encoded = if component == 0 {
            value as i64
        } else {
            encoded * 100 + value as i64
        };
        component += 1;
        if component < 3 {
            assert!(
                position < bytes.len() && bytes[position] == b'.',
                "module version must be major.minor.patch"
            );
            position += 1;
        }
    }
    assert!(
        encoded <= i32::MAX as i64,
        "module version exceeds i32::MAX"
    );
    if position < bytes.len() {
        assert!(
            bytes[position] != b'-',
            "module version cannot encode prereleases"
        );
        assert!(bytes[position] == b'+', "invalid module version suffix");
        position += 1;
        let mut identifier_len = 0;
        while position < bytes.len() {
            let byte = bytes[position];
            if byte == b'.' {
                assert!(
                    identifier_len > 0,
                    "module version build identifiers cannot be empty"
                );
                identifier_len = 0;
            } else {
                assert!(
                    (byte >= b'0' && byte <= b'9')
                        || (byte >= b'A' && byte <= b'Z')
                        || (byte >= b'a' && byte <= b'z')
                        || byte == b'-',
                    "invalid module version build identifier"
                );
                identifier_len += 1;
            }
            position += 1;
        }
        assert!(
            identifier_len > 0,
            "module version build identifiers cannot be empty"
        );
    }
    encoded as i32
}

/// Encode the **calling crate's** `CARGO_PKG_VERSION` for `redis_module!`.
///
/// Expands to a const expression using [`encode_module_version`], so invalid
/// versions fail to compile even when the invocation is in a runtime expression.
/// The same limits and build metadata rules apply. Existing modules can keep
/// explicit integer versions or their own encoding instead.
///
/// ```
/// const VERSION: i32 = redis_module::module_version!();
/// assert_eq!(VERSION, redis_module::encode_module_version(env!("CARGO_PKG_VERSION")));
/// ```
///
/// In a module declaration:
///
/// ```ignore
/// redis_module! {
///     name: "example",
///     version: redis_module::module_version!(),
///     // ... allocator, data_types and commands ...
/// }
/// ```
#[macro_export]
macro_rules! module_version {
    () => {{
        const MODULE_VERSION: i32 =
            $crate::encode_module_version(::core::env!("CARGO_PKG_VERSION"));
        MODULE_VERSION
    }};
}

#[cfg(test)]
mod tests {
    use super::encode_module_version;

    #[test]
    fn release_versions_and_metadata() {
        for (version, expected) in [
            ("0.0.0", 0),
            ("0.0.1", 1),
            ("0.99.99", 9999),
            ("1.0.0", 10000),
            ("1.23.45", 12345),
            ("3.0.0", 30000),
            ("100.0.0", 1000000),
            ("214748.36.47", i32::MAX),
            ("1.23.45+build.01.sha-ABC", 12345),
            ("0.0.0+-", 0),
        ] {
            assert_eq!(encode_module_version(version), expected, "{version}");
        }
    }

    #[test]
    fn rejects_malformed_and_unrepresentable_versions() {
        for version in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            ".1.2",
            "1..2",
            "1.2.",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "+1.2.3",
            "-1.2.3",
            "v1.2.3",
            " 1.2.3",
            "1.2.3 ",
            "1.2.3\n",
            "1.2.3\0",
            "１.2.3",
            "1.2.3-rc.1",
            "1.2.3-rc.1+build",
            "1.2.3-",
            "1.100.0",
            "1.0.100",
            "214748.36.48",
            "214748.37.0",
            "214749.0.0",
            "2147483648.0.0",
            "999999999999999999999.0.0",
            "1.999999999999999999999.0",
            "1.2.3+",
            "1.2.3+.a",
            "1.2.3+a.",
            "1.2.3+a..b",
            "1.2.3+a_b",
            "1.2.3+a+b",
            "1.2.3+café",
            "1.2.3+a b",
            "1.2.3+a\0",
        ] {
            assert!(
                std::panic::catch_unwind(|| encode_module_version(version)).is_err(),
                "{version:?}"
            );
        }
    }

    #[test]
    fn release_order_at_component_boundaries() {
        assert!(encode_module_version("0.99.99") < encode_module_version("1.0.0"));
        assert!(encode_module_version("1.0.99") < encode_module_version("1.1.0"));
        const VERSION: i32 = crate::module_version!();
        assert_eq!(VERSION, encode_module_version(env!("CARGO_PKG_VERSION")));
    }
}
