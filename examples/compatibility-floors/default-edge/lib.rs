//! A dependency edge which intentionally enables the SDK's default 6.0 floor.
use redis_module::{EventContext, Status, apierror::APIResult};

pub fn post_notification(ctx: &EventContext) -> APIResult<Status> {
    ctx.add_post_notification_job(|_| {})
}
