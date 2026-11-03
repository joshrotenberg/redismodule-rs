#[allow(dead_code)]
#[path = "../src/watch_limits.rs"]
mod watch_limits;

use watch_limits::{MAX_DEFINITION_BYTES, definition_bytes, definition_fits};

#[test]
fn retained_definition_accounting_has_exact_checked_boundaries() {
    assert_eq!(
        definition_bytes(&[128, 1024, 1024, 64 * 1024, 1024]),
        Some(68_736)
    );
    assert!(definition_fits(MAX_DEFINITION_BYTES - 1, 1));
    assert!(!definition_fits(MAX_DEFINITION_BYTES, 1));
    assert!(!definition_fits(usize::MAX, 1));
    assert_eq!(definition_bytes(&[usize::MAX, 1]), None);
}
