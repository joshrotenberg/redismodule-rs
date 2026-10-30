#[path = "../src/json_size.rs"]
mod json_size;

use std::collections::BTreeMap;

fn assert_matches_serializer(fields: &BTreeMap<String, String>) -> usize {
    let actual = serde_json::to_vec(fields).unwrap().len();
    assert_eq!(json_size::serialized_json_bytes(fields), Some(actual));
    actual
}

#[test]
fn canonical_accounting_matches_an_independent_serializer() {
    assert_eq!(assert_matches_serializer(&BTreeMap::new()), 2);
    let controls: String = (0..=31).map(char::from).collect();
    for text in ["", "a\"\\/", "é🍓\u{2028}\u{2029}", &controls] {
        let fields = BTreeMap::from([
            (text.to_owned(), text.to_owned()),
            ("__proto__".to_owned(), "constructor".to_owned()),
        ]);
        assert_matches_serializer(&fields);
    }
}

#[test]
fn exact_json_byte_limit_is_independent_of_engine_execution_time() {
    const LIMIT: usize = 1024 * 1024;
    let escaped = 16;
    // {"a":"..."} has eight structural bytes; each NUL is a six-byte escape.
    let value = "x".repeat(LIMIT - 8 - escaped * 6) + &"\0".repeat(escaped);
    let mut fields = BTreeMap::from([("a".to_owned(), value)]);
    assert_eq!(assert_matches_serializer(&fields), LIMIT);
    fields.get_mut("a").unwrap().push('x');
    assert_eq!(assert_matches_serializer(&fields), LIMIT + 1);
}
