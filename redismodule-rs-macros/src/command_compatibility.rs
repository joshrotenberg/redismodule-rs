//! Regression corpus established against the legacy serde_syn JSONY parser.
use super::*;

fn parse_metadata(source: &str) -> Args {
    syn::parse_str(source).unwrap_or_else(|error| panic!("{source}: {error}"))
}

#[test]
fn braced_typed_and_quoted_command_fields_have_identical_meaning() {
    let canonical =
        parse_metadata(r#"{ name: "compat", flags: [ReadOnly], arity: -2, key_spec: [] }"#);
    for source in [
        r#"Args { name: Some("compat"), flags: [RedisCommandFlags::ReadOnly], arity: -2i64, key_spec: [], }"#,
        r#"{ "name": Option::Some("compat"), "flags": ["ReadOnly"], "arity": -0x2, "key_spec": [] }"#,
        r#"{ name: ::Some("compat"), flags: [::ReadOnly], arity: -0b10, key_spec: [] }"#,
    ] {
        assert_eq!(
            format!("{canonical:?}"),
            format!("{:?}", parse_metadata(source)),
            "{source}"
        );
    }
}

#[test]
fn missing_optional_fields_and_explicit_none_are_equivalent() {
    let canonical = parse_metadata("{ flags: [], arity: 1, key_spec: [] }");
    for source in [
        "{ name: None, flags: [], enterprise_flags: None, summary: None, complexity: None, since: None, tips: None, arity: 1, key_spec: [], args: None, acl_categories: None }",
        "{ name: Option::None, flags: [], arity: 1, key_spec: [], args: ::None }",
    ] {
        assert_eq!(
            format!("{canonical:?}"),
            format!("{:?}", parse_metadata(source)),
            "{source}"
        );
    }
    assert!(canonical.name.is_none());
    assert!(canonical.args.is_none());
    assert!(canonical.enterprise_flags.is_none());
}

#[test]
fn every_command_enterprise_and_key_flag_is_retained() {
    let args = parse_metadata(
        r#"{
        flags: [Write, ReadOnly, Admin, DenyOOM, DenyScript, AllowLoading, PubSub,
            Random, AllowStale, NoMonitor, NoSlowlog, Fast, GetkeysApi, NoCluster,
            NoAuth, MayReplicate, NoMandatoryKeys, Blocking, AllowBusy, GetchannelsApi],
        enterprise_flags: Some([RedisEnterpriseCommandFlags::ProxyFiltered]),
        arity: 1,
        key_spec: [{ flags: [ReadOnly, ReadWrite, Overwrite, Remove, Access, Update,
            Insert, Delete, NotKey, Incomplete, VariableFlags],
            begin_search: Index({index: 1}), find_keys: Range({last_key: -1, steps: 1, limit: 0}) }]
    }"#,
    );
    assert_eq!(
        args.flags
            .iter()
            .map(<&'static str>::from)
            .collect::<Vec<_>>(),
        [
            "write",
            "readonly",
            "admin",
            "deny-oom",
            "deny-script",
            "allow-loading",
            "pubsub",
            "random",
            "allow-stale",
            "no-monitor",
            "no-slowlog",
            "fast",
            "getkeys-api",
            "no-cluster",
            "no-auth",
            "may-replicate",
            "no-mandatory-keys",
            "blocking",
            "allow-busy",
            "getchannels-api"
        ]
    );
    assert_eq!(
        <&'static str>::from(&args.enterprise_flags.unwrap()[0]),
        "_proxy-filtered"
    );
    assert_eq!(
        args.key_spec[0]
            .flags
            .iter()
            .map(<&'static str>::from)
            .collect::<Vec<_>>(),
        [
            "READ_ONLY",
            "READ_WRITE",
            "OVERWRITE",
            "REMOVE",
            "ACCESS",
            "UPDATE",
            "INSERT",
            "DELETE",
            "NOT_KEY",
            "INCOMPLETE",
            "VARIABLE_FLAGS"
        ]
    );
}

#[test]
fn typed_nested_key_specs_and_acl_payloads_preserve_values() {
    let args = parse_metadata(
        r#"Args {
        flags: ["ReadOnly"], arity: -3,
        key_spec: [KeySpecArg {
            notes: Some("search by token"), flags: [RedisCommandKeySpecFlags::ReadOnly, "Access"],
            begin_search: BeginSearch::Keyword(BeginSearchKeyword {keyword: "KEYS", startfrom: -2i32}),
            find_keys: FindKeys::Keynum(FindKeysNum {key_num_idx: 0i32, first_key: 1, key_step: 2})
        }],
        acl_categories: Some([AclCategory::Read, "Fast", Single("custom"), Multi([Write, AclCategory::Admin])])
    }"#,
    );
    let spec = &args.key_spec[0];
    assert_eq!(spec.notes.as_deref(), Some("search by token"));
    let BeginSearch::Keyword(keyword) = &spec.begin_search else {
        panic!("keyword variant")
    };
    assert_eq!((&*keyword.keyword, keyword.startfrom), ("KEYS", -2));
    let FindKeys::Keynum(number) = &spec.find_keys else {
        panic!("keynum variant")
    };
    assert_eq!(
        (number.key_num_idx, number.first_key, number.key_step),
        (0, 1, 2)
    );
    assert_eq!(
        args.acl_categories.unwrap(),
        vec![
            AclCategory::Read,
            AclCategory::Fast,
            AclCategory::Single("custom".to_owned()),
            AclCategory::Multi(vec![AclCategory::Write, AclCategory::Admin])
        ]
    );
}

#[test]
fn command_help_supports_recursive_arguments_and_optional_forms() {
    let args = parse_metadata(
        r##"{
        name: "compat.help", flags: [], arity: -1, key_spec: [],
        summary: r#"help "quoted""#, complexity: "O(N)", since: "3.0", tips: "request_policy:all_shards",
        args: [{name: "choice", arg_type: CommandArgType::OneOf, key_spec_index: Some(4294967295u32),
            token: Some("KEY"), summary: "choose", since: "3.0", flags: [None, Optional, Multiple, MultipleToken],
            deprecated_since: None, display_text: "choice...", subargs: Some([
                CommandArg {name: "group", arg_type: "Block", subargs: [
                    {name: "key", arg_type: Key, key_spec_index: 0},
                    {name: "count", arg_type: Integer}, {name: "value", arg_type: Double},
                    {name: "text", arg_type: String}, {name: "pattern", arg_type: Pattern},
                    {name: "time", arg_type: UnixTime}, {name: "literal", arg_type: PureToken}
                ]}
            ])
        }]
    }"##,
    );
    assert_eq!(args.summary.as_deref(), Some("help \"quoted\""));
    let arg = &args.args.unwrap()[0];
    assert_eq!(arg.arg_type, CommandArgType::OneOf);
    assert_eq!(arg.key_spec_index, Some(u32::MAX));
    assert_eq!(
        arg.flags
            .as_ref()
            .unwrap()
            .iter()
            .map(<&'static str>::from)
            .collect::<Vec<_>>(),
        ["NONE", "OPTIONAL", "MULTIPLE", "MULTIPLE_TOKEN"]
    );
    assert_eq!(arg.display_text.as_deref(), Some("choice..."));
    let group = &arg.subargs.as_ref().unwrap()[0];
    assert_eq!(group.arg_type, CommandArgType::Block);
    assert_eq!(
        group
            .subargs
            .as_ref()
            .unwrap()
            .iter()
            .map(|arg| arg.arg_type)
            .collect::<Vec<_>>(),
        [
            CommandArgType::Key,
            CommandArgType::Integer,
            CommandArgType::Double,
            CommandArgType::String,
            CommandArgType::Pattern,
            CommandArgType::UnixTime,
            CommandArgType::PureToken
        ]
    );
}

#[test]
fn integer_boundaries_bases_and_suffixes_are_preserved() {
    for (literal, expected) in [
        ("9223372036854775807i64", i64::MAX),
        ("-9223372036854775807", -i64::MAX),
        ("0xff", 255),
        ("0o17", 15),
        ("0b1_001", 9),
        ("-0", 0),
    ] {
        let args = parse_metadata(&format!("{{ flags: [], arity: {literal}, key_spec: [] }}"));
        assert_eq!(args.arity, expected, "{literal}");
    }
    let args = parse_metadata(
        "{ flags: [], arity: 1, key_spec: [{flags: [], begin_search: Index({index: 2147483647i32}), find_keys: Range({last_key: -2147483647, steps: 0x2, limit: 0})}] }",
    );
    let BeginSearch::Index(index) = &args.key_spec[0].begin_search else {
        panic!("index variant")
    };
    let FindKeys::Range(range) = &args.key_spec[0].find_keys else {
        panic!("range variant")
    };
    assert_eq!(index.index, i32::MAX);
    assert_eq!(
        (range.last_key, range.steps, range.limit),
        (-i32::MAX, 2, 0)
    );
}

#[test]
fn malformed_or_unsupported_metadata_is_rejected_without_panicking() {
    for source in [
        "flags: [], arity: 1, key_spec: []",
        "({flags: [], arity: 1, key_spec: []})",
        "Other {flags: [], arity: 1, key_spec: []}",
        "{flags: [], arity: 1}",
        "{flags: [], key_spec: []}",
        "{arity: 1, key_spec: []}",
        "{flags: [], arity: 1, key_spec: [], unknown: true}",
        "{flags: [], flags: [], arity: 1, key_spec: []}",
        "{flags: [], arity: 1, key_spec: [], name: None, name: None}",
        "{flags: [UnknownFlag], arity: 1, key_spec: []}",
        "{flags: [Other::ReadOnly], arity: 1, key_spec: []}",
        "{flags: [], arity: 9223372036854775808, key_spec: []}",
        "{flags: [], arity: -9223372036854775808, key_spec: []}",
        "{flags: [], arity: 1i32, key_spec: []}",
        "{flags: [], arity: +1, key_spec: []}",
        "{flags: [], arity: \"1\", key_spec: []}",
        "{flags: [], arity: 1, key_spec: [], summary: null}",
        "{flags: [], arity: 1, key_spec: [], args: [{name: \"x\", arg_type: Key, key_spec_index: -1}]}",
        "{flags: [], arity: 1, key_spec: [], args: [{name: \"x\", arg_type: Key, key_spec_index: 4294967296}]}",
        "{flags: [], arity: 1, key_spec: [{flags: [], begin_search: Index({index: -2147483648}), find_keys: Range({last_key: 0, steps: 1, limit: 0})}]}",
        "{flags = [], arity: 1, key_spec: []}",
        "{flags: [] arity: 1, key_spec: []}",
        "{flags: [], arity: 1, key_spec: []} trailing",
    ] {
        assert!(
            syn::parse_str::<Args>(source).is_err(),
            "unexpectedly accepted: {source}"
        );
    }
}
