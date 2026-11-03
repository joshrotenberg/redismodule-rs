pub const MAX_WATCHES: usize = 64;
pub const MAX_DEFINITION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_OWNER_BYTES: usize = 1024;

pub fn definition_bytes(parts: &[usize]) -> Option<usize> {
    parts
        .iter()
        .try_fold(0usize, |total, part| total.checked_add(*part))
}

pub fn definition_fits(current: usize, additional: usize) -> bool {
    current
        .checked_add(additional)
        .is_some_and(|total| total <= MAX_DEFINITION_BYTES)
}
