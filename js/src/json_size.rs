use std::collections::BTreeMap;

fn escaped_json_string_bytes(value: &str) -> Option<usize> {
    value.chars().try_fold(2usize, |size, character| {
        let escaped = match character {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => character.len_utf8(),
        };
        size.checked_add(escaped)
    })
}

pub(crate) fn serialized_json_bytes(fields: &BTreeMap<String, String>) -> Option<usize> {
    fields.iter().try_fold(2usize, |size, (name, value)| {
        size.checked_add(usize::from(size > 2))?
            .checked_add(escaped_json_string_bytes(name)?)?
            .checked_add(1)?
            .checked_add(escaped_json_string_bytes(value)?)
    })
}
