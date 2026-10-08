//! Public conversion contracts exercised when updating our derive dependencies.

use std::fmt::Debug;

use num_traits::{FromPrimitive, ToPrimitive};
use redis_module::logging::RedisLogLevel;
use redis_module::raw::{Aux, KeyType, ReplyType, Status, Where};

fn assert_conversions<E: Debug + PartialEq + FromPrimitive + ToPrimitive>(
    variants: &[(E, i32)],
    unknown_negative: i32,
) {
    for (variant, value) in variants {
        assert_eq!(E::from_i32(*value).as_ref(), Some(variant));
        assert_eq!(E::from_i64(i64::from(*value)).as_ref(), Some(variant));
        assert_eq!(variant.to_i32(), Some(*value));
        assert_eq!(variant.to_i64(), Some(i64::from(*value)));

        if let Ok(unsigned) = u64::try_from(*value) {
            assert_eq!(E::from_u64(unsigned).as_ref(), Some(variant));
            assert_eq!(variant.to_u64(), Some(unsigned));
        }
    }

    assert_eq!(E::from_i32(unknown_negative), None);
    assert_eq!(E::from_i64(i64::from(unknown_negative)), None);
    assert_eq!(E::from_i32(99), None);
    assert_eq!(E::from_u64(99), None);
}

#[test]
fn key_type_integer_conversions() {
    assert_conversions(
        &[
            (KeyType::Empty, 0),
            (KeyType::String, 1),
            (KeyType::List, 2),
            (KeyType::Hash, 3),
            (KeyType::Set, 4),
            (KeyType::ZSet, 5),
            (KeyType::Module, 6),
            (KeyType::Stream, 7),
        ],
        -1,
    );
}

#[test]
fn list_position_integer_conversions() {
    assert_conversions(&[(Where::ListHead, 0), (Where::ListTail, 1)], -1);
}

#[test]
fn reply_type_integer_conversions() {
    assert_conversions(
        &[
            (ReplyType::Unknown, -1),
            (ReplyType::String, 0),
            (ReplyType::Error, 1),
            (ReplyType::Integer, 2),
            (ReplyType::Array, 3),
            (ReplyType::Null, 4),
            (ReplyType::Map, 5),
            (ReplyType::Set, 6),
            (ReplyType::Bool, 7),
            (ReplyType::Double, 8),
            (ReplyType::BigNumber, 9),
            (ReplyType::VerbatimString, 10),
        ],
        -2,
    );
}

#[test]
fn auxiliary_phase_integer_conversions() {
    assert_conversions(&[(Aux::Before, 1), (Aux::After, 2)], -1);
    assert_eq!(Aux::from_i32(0), None);
}

#[test]
fn status_integer_conversions() {
    assert_conversions(&[(Status::Ok, 0), (Status::Err, 1)], -1);
}

#[test]
fn log_level_strings() {
    for (level, expected) in [
        (RedisLogLevel::Debug, "debug"),
        (RedisLogLevel::Notice, "notice"),
        (RedisLogLevel::Verbose, "verbose"),
        (RedisLogLevel::Warning, "warning"),
    ] {
        assert_eq!(level.as_ref(), expected);
    }
}
