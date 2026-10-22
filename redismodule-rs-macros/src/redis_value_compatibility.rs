//! Regression corpus established against the legacy field-attribute parser.
use super::*;

#[test]
fn field_attributes_preserve_braced_typed_and_quoted_boolean_forms() {
    for source in [
        "{flatten: true}",
        "FieldAttr {flatten: true,}",
        "{\"flatten\": true}",
    ] {
        let field =
            syn::parse_str::<FieldAttr>(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert!(field.flatten, "{source}");
    }
    assert!(
        !syn::parse_str::<FieldAttr>("{flatten: false}")
            .unwrap()
            .flatten
    );
    assert!(!FieldAttr::default().flatten);
}

#[test]
fn field_attributes_require_an_explicit_boolean_and_reject_duplicates_or_unknowns() {
    for source in [
        "{}",
        "{flatten}",
        "flatten: true",
        "(flatten: true)",
        "{flatten: 1}",
        "{flatten: \"true\"}",
        "{flatten: true, flatten: false}",
        "{flatten: true, other: false}",
        "Other {flatten: true}",
        "{flatten = true}",
        "{flatten: true} trailing",
    ] {
        assert!(
            syn::parse_str::<FieldAttr>(source).is_err(),
            "unexpectedly accepted: {source}"
        );
    }
}
