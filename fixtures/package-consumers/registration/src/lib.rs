// Compile/link registration contracts without Redis API initialization or its allocator.
// Callbacks are not invoked, and this fixture is not loaded into a Redis host.
use redis_module::defrag::DefragContext;
use redis_module::server_events::{
    FlushSubevent, LoadingSubevent, ModuleChangeSubevent, ServerRole,
};
use redis_module::{Context, InfoContext, RedisResult, RedisString, RedisValue};
use redis_module_macros::*;

#[command({name: "hygiene.typed", flags: [ReadOnly], arity: 1, key_spec: []})]
fn typed(_: &Context, _: Vec<RedisString>) -> RedisResult {
    Ok(RedisValue::SimpleStringStatic("OK"))
}
#[role_changed_event_handler]
fn role(_: &Context, _: ServerRole) {}
#[loading_event_handler]
fn loading(_: &Context, _: LoadingSubevent) {}
#[flush_event_handler]
fn flush(_: &Context, _: FlushSubevent) {}
#[module_changed_event_handler]
fn module(_: &Context, _: ModuleChangeSubevent) {}
#[config_changed_event_handler]
fn config(_: &Context, _: &[&str]) {}
#[cron_event_handler]
fn cron(_: &Context, _: u64) {}
#[info_command_handler]
fn info(_: &InfoContext, _: bool) -> RedisResult<()> {
    Ok(())
}
#[defrag_function]
fn defrag(_: &DefragContext) {}
#[defrag_start_function]
fn defrag_start(_: &DefragContext) {}
#[defrag_end_function]
fn defrag_end(_: &DefragContext) {}

#[cfg(test)]
mod tests {
    #[test]
    fn every_registration_reaches_its_slice() {
        use redis_module::{commands, defrag, server_events as events};
        assert_eq!(commands::COMMANDS_LIST.len(), 1);
        assert_eq!(events::ROLE_CHANGED_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::LOADING_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::FLUSH_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::MODULE_CHANGED_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::CONFIG_CHANGED_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::CRON_SERVER_EVENTS_LIST.len(), 1);
        assert_eq!(events::INFO_COMMAND_HANDLER_LIST.len(), 1);
        assert_eq!(defrag::DEFRAG_FUNCTIONS_LIST.len(), 1);
        assert_eq!(defrag::DEFRAG_START_FUNCTIONS_LIST.len(), 1);
        assert_eq!(defrag::DEFRAG_END_FUNCTIONS_LIST.len(), 1);
    }
}
