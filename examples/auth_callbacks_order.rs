//! Second static hook for native newest-registration-first integration checks.
use redis_module::{
    AuthContext, AuthDecision, AuthRequest, Context, PublicAuthError, RedisResult, RedisString,
    redis_module,
};
use std::sync::atomic::{AtomicI64, Ordering};
static CALLS: AtomicI64 = AtomicI64::new(0);
fn authenticate(_: &AuthContext<'_>, request: AuthRequest<'_>) -> AuthDecision {
    CALLS.fetch_add(1, Ordering::Relaxed);
    if request.username() == b"stub" {
        AuthDecision::Deny(PublicAuthError::new("Later module denied authentication"))
    } else {
        AuthDecision::NotHandled
    }
}
fn calls(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(CALLS.load(Ordering::Relaxed).into())
}
redis_module! {
    name: "auth_callbacks_order", version: 1,
    allocator: (redis_module::alloc::RedisAlloc, redis_module::alloc::RedisAlloc),
    data_types: [],
    auth: authenticate,
    commands: [["auth_callbacks_order.calls", calls, "readonly", 0, 0, 0, ""]],
}
