//! Compile-only consumers: no Redis server or global Redis allocator is used.
use redis_module::{EventContext, Status};

// A real API whose return type differs across these floors proves that package
// selection does more than compile an empty consumer. The functions are never run.
#[cfg(all(feature = "redis-6", not(feature = "redis-7-2")))]
pub fn post_notification(ctx: &EventContext) -> redis_module::apierror::APIResult<Status> {
    ctx.add_post_notification_job(|_| {})
}

#[cfg(all(feature = "redis-7-2", not(feature = "redis-6")))]
pub fn post_notification(ctx: &EventContext) -> Status {
    ctx.add_post_notification_job(|_| {})
}
