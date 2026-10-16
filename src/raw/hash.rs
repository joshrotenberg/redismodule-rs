//! The fixed, single-field HashSet contract. Native counts are not Status.
use super::*;

type HashSet = unsafe extern "C" fn(*mut RedisModuleKey, c_int, ...) -> c_int;

enum Field<'a> {
    Text(&'a CStr),
    String(*mut RedisModuleString),
}

fn text_field(field: &str) -> Result<CString, RedisError> {
    CString::new(field).map_err(|_| RedisError::Str("Hash field contains an interior NUL"))
}

fn inputs(key: usize, field: usize, value: Option<usize>) -> Result<(), RedisError> {
    if key == 0 || field == 0 || value == Some(0) {
        return Err(RedisError::Str("Hash mutation requires non-NULL inputs"));
    }
    if value == Some(REDISMODULE_HASH_DELETE as usize) {
        return Err(RedisError::Str(
            "Hash setter value cannot be the DELETE sentinel",
        ));
    }
    Ok(())
}

fn required<T>(api: Option<T>, message: &'static str) -> Result<T, RedisError> {
    api.ok_or(RedisError::Str(message))
}

fn hash_type(kind: c_int) -> Result<(), RedisError> {
    if kind == REDISMODULE_KEYTYPE_EMPTY as c_int || kind == REDISMODULE_KEYTYPE_HASH as c_int {
        Ok(())
    } else {
        Err(RedisError::Str("WRONGTYPE key is not a hash value"))
    }
}

fn modern(version: Option<c_int>) -> bool {
    version.is_some_and(|version| version >= 0x0006_0200)
}

fn result(count: c_int, modern: bool, errno: Option<c_int>) -> Result<usize, RedisError> {
    match count {
        1 => Ok(1),
        0 if !modern || errno == Some(libc::ENOENT) => Ok(0),
        0 => Err(RedisError::String(format!(
            "Hash mutation failed (native errno {})",
            errno.unwrap_or(0)
        ))),
        _ => Err(RedisError::String(format!(
            "Unexpected single-field HashSet count: {count}"
        ))),
    }
}

// All callers first validate NULL/sentinel inputs. The caller establishes WRITE,
// live pointers, phase, thread/GIL and alias obligations; KeyType cannot infer mode.
unsafe fn mutate(
    key: *mut RedisModuleKey,
    field: Field<'_>,
    value: *const RedisModuleString,
) -> Result<usize, RedisError> {
    let set: HashSet = required(RedisModule_HashSet, "HashSet API is unavailable")?;
    let kind = required(RedisModule_KeyType, "KeyType API is unavailable")?;
    let version = RedisModule_GetServerVersion;
    // Compare the raw integer: newer native types need not exist in KeyType.
    hash_type(kind(key))?;
    let modern = modern(version.map(|get| get()));
    let count = match field {
        Field::Text(field) => set(
            key,
            REDISMODULE_HASH_CFIELDS as c_int,
            field.as_ptr(),
            value,
            ptr::null::<c_char>(),
        ),
        Field::String(field) => set(
            key,
            REDISMODULE_HASH_NONE as c_int,
            field,
            value,
            ptr::null::<RedisModuleString>(),
        ),
    };
    // First operation after HashSet: capture errno for modern zero, before any
    // formatting, input destruction, owner release or another native API call.
    let errno = if modern && count == 0 {
        std::io::Error::last_os_error().raw_os_error()
    } else {
        None
    };
    result(count, modern, errno)
}

/// Set one text-named hash field; returns preexisting fields replaced (0 or 1).
/// Interior NUL is an error. A new field returns 0, even though it was inserted.
///
/// # Safety
/// `key` must be a genuine live handle opened for writing, and `value` must be a
/// live Redis string, not the native DELETE sentinel. Maintain the native phase,
/// thread/GIL and alias requirements throughout the call. NULL is rejected, but
/// stale or otherwise invalid non-NULL pointers are not recoverable errors.
/// Inputs remain borrowed; this does not extend their lifetime or replicate.
pub unsafe fn hash_set(
    key: *mut RedisModuleKey,
    field: &str,
    value: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    let field = text_field(field)?;
    inputs(key as usize, field.as_ptr() as usize, Some(value as usize))?;
    mutate(key, Field::Text(&field), value)
}

/// Delete one text-named hash field; returns preexisting fields deleted (0 or 1).
/// Interior NUL is an error; a missing field returns 0.
///
/// # Safety
/// `key` must be a genuine live writable handle. Maintain native phase,
/// thread/GIL and alias requirements. NULL is rejected; invalid non-NULL pointers
/// are outside this contract. This call does not replicate automatically.
pub unsafe fn hash_del(key: *mut RedisModuleKey, field: &str) -> Result<usize, RedisError> {
    let field = text_field(field)?;
    inputs(key as usize, field.as_ptr() as usize, None)?;
    mutate(key, Field::Text(&field), REDISMODULE_HASH_DELETE)
}

/// Set one existing-string field, preserving all field/value bytes.
/// Returns preexisting fields replaced, including identical-value replacement.
/// Counts describe native physical storage, not logical presence under expiry.
///
/// # Safety
/// `key` must be a genuine live writable handle; `field` and `value` must be live
/// Redis strings. `value` must not be DELETE. Maintain native phase, thread/GIL
/// and alias requirements. NULL is rejected; invalid non-NULL pointers are not
/// recoverable errors. Inputs are borrowed; no retention or replication occurs.
pub unsafe fn hash_set_by_string(
    key: *mut RedisModuleKey,
    field: *mut RedisModuleString,
    value: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    inputs(key as usize, field as usize, Some(value as usize))?;
    mutate(key, Field::String(field), value)
}

/// Delete one existing-string field, preserving its bytes. Returns 0 or 1.
///
/// # Safety
/// `key` must be a genuine live writable handle and `field` a live Redis string.
/// Maintain native phase, thread/GIL and alias requirements. NULL is rejected;
/// invalid non-NULL pointers are outside this contract. The field is borrowed
/// and this call does not replicate automatically.
pub unsafe fn hash_del_by_string(
    key: *mut RedisModuleKey,
    field: *mut RedisModuleString,
) -> Result<usize, RedisError> {
    inputs(key as usize, field as usize, None)?;
    mutate(key, Field::String(field), REDISMODULE_HASH_DELETE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_native_delete_abi() {
        assert_eq!(REDISMODULE_HASH_DELETE as usize, 1);
    }

    #[test]
    fn scalar_admission_and_text_policy() {
        // Scalar facts only: these integers never become pointers or API calls.
        for (key, field, value) in [
            (0, 2, Some(3)),
            (2, 0, Some(3)),
            (2, 3, Some(0)),
            (2, 3, Some(1)),
        ] {
            assert!(inputs(key, field, value).is_err());
        }
        assert!(inputs(2, 3, Some(4)).is_ok());
        assert!(inputs(2, 3, None).is_ok());
        assert!(text_field("a\0b").is_err());
        for text in ["", "field", "λ"] {
            assert_eq!(text_field(text).unwrap().to_bytes(), text.as_bytes());
        }
        for kind in [REDISMODULE_KEYTYPE_EMPTY, REDISMODULE_KEYTYPE_HASH] {
            assert!(hash_type(kind as c_int).is_ok());
        }
        for kind in [-1, REDISMODULE_KEYTYPE_STRING, 8, 99] {
            assert!(hash_type(kind as c_int).is_err());
        }
        assert!(required::<u8>(None, "HashSet API is unavailable").is_err());
        assert!(required::<u8>(None, "KeyType API is unavailable").is_err());
        assert_eq!(required(Some(7), "unused").unwrap(), 7);
    }

    #[test]
    fn version_selects_restricted_policy() {
        for version in [
            None,
            Some(0x0005_0014),
            Some(0x0006_0014),
            Some(0x0006_01ff),
        ] {
            assert!(!modern(version));
        }
        for version in [Some(0x0006_0200), Some(0x0007_0208), Some(0x0008_0a02)] {
            assert!(modern(version));
        }
    }

    #[test]
    fn native_counts_are_not_status() {
        for errno in [
            None,
            Some(0),
            Some(libc::ENOENT),
            Some(libc::EBADF),
            Some(libc::ENOTSUP),
            Some(libc::EINVAL),
        ] {
            assert_eq!(result(0, false, errno).unwrap(), 0);
            assert_eq!(result(1, false, errno).unwrap(), 1);
            assert_eq!(result(1, true, errno).unwrap(), 1);
            assert_eq!(result(0, true, errno).is_ok(), errno == Some(libc::ENOENT));
            for count in [-1, 2, c_int::MAX] {
                assert!(result(count, false, errno).is_err());
                assert!(result(count, true, errno).is_err());
            }
        }
    }
}
