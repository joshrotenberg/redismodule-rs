//! Exercises rejection of a custom ACL category with invalid characters.
use redis_module::{AclCategory, redis_module};

redis_module! {
    name: "acl_invalid_category",
    version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    acl_categories: [AclCategory::from("invalid category"),],
}
