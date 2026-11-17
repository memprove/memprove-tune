use tune_engine::strict_json::{
    DEFAULT_MAX_NESTING_DEPTH, JsonErrorKind, JsonLimit, JsonLimits, JsonValue, parse_strict,
    parse_strict_bytes, parse_strict_bytes_with_limits, parse_strict_with_limits,
};

fn assert_limit(error: &tune_engine::strict_json::JsonError, limit: JsonLimit, maximum: usize) {
    assert_eq!(error.kind, JsonErrorKind::LimitExceeded { limit, maximum });
    assert_eq!(error.code(), "limit_exceeded");
}

fn nested_arrays(depth: usize) -> String {
    let opening = "[".repeat(depth);
    let closing = "]".repeat(depth);
    format!("{opening}0{closing}")
}

fn nested_objects(depth: usize) -> String {
    let opening = "{\"value\":".repeat(depth);
    let closing = "}".repeat(depth);
    format!("{opening}0{closing}")
}

#[test]
fn unicode_scalars_escapes_and_surrogate_pairs_decode_strictly() {
    assert_eq!(
        parse_strict(r#""AßΩ𝄞""#).unwrap(),
        JsonValue::String("AßΩ𝄞".to_owned())
    );
    assert_eq!(
        parse_strict(r#""\"\\\/\b\f\n\r\t""#).unwrap(),
        JsonValue::String("\"\\/\u{0008}\u{000c}\n\r\t".to_owned())
    );
    assert_eq!(
        parse_strict(r#""A\u00DF\u03A9\uD834\uDD1E""#).unwrap(),
        JsonValue::String("AßΩ𝄞".to_owned())
    );

    for (input, offset) in [
        (r#""\uD800""#, 7),
        (r#""\uDEAD""#, 1),
        (r#""\uDC00\uD800""#, 1),
        (r#""\uD800\u0041""#, 7),
        (r#""\u12G4""#, 5),
        (r#""\u123""#, 6),
    ] {
        let error = parse_strict(input).unwrap_err();
        assert_eq!(error.kind, JsonErrorKind::InvalidUnicode, "{input}");
        assert_eq!(error.offset, offset, "{input}");
        assert_eq!(error.code(), "invalid_unicode", "{input}");
    }

    let invalid_escape = parse_strict(r#""\x""#).unwrap_err();
    assert_eq!(invalid_escape.kind, JsonErrorKind::InvalidEscape);
    assert_eq!(invalid_escape.offset, 2);
    assert_eq!(invalid_escape.code(), "invalid_escape");
}

#[test]
fn every_unescaped_c0_control_is_rejected_at_its_first_byte() {
    for scalar in 0_u32..=0x1f {
        let control = char::from_u32(scalar).unwrap();
        let input = format!("\"a{control}b\"");
        let error = parse_strict(&input).unwrap_err();
        assert_eq!(
            error.kind,
            JsonErrorKind::UnexpectedToken {
                expected: "unescaped string character",
                found: Some(control),
            },
            "U+{scalar:04X}"
        );
        assert_eq!(error.offset, 2, "U+{scalar:04X}");
        assert_eq!(error.code(), "unexpected_token", "U+{scalar:04X}");
    }
}

#[test]
fn escape_equivalent_duplicate_member_names_are_rejected() {
    let escaped_ascii = parse_strict(r#"{"name":1,"na\u006de":2}"#).unwrap_err();
    assert_eq!(
        escaped_ascii.kind,
        JsonErrorKind::DuplicateMember("name".to_owned())
    );
    assert_eq!(escaped_ascii.offset, 10);
    assert_eq!(escaped_ascii.code(), "duplicate_member");

    let escaped_solidus = parse_strict(r#"{"a/b":1,"a\/b":2}"#).unwrap_err();
    assert_eq!(
        escaped_solidus.kind,
        JsonErrorKind::DuplicateMember("a/b".to_owned())
    );

    let escaped_non_bmp = parse_strict(r#"{"𝄞":1,"\uD834\uDD1E":2}"#).unwrap_err();
    assert_eq!(
        escaped_non_bmp.kind,
        JsonErrorKind::DuplicateMember("𝄞".to_owned())
    );

    let nested_escaped = parse_strict(r#"{"outer":{"name":1,"na\u006de":2}}"#).unwrap_err();
    assert_eq!(
        nested_escaped.kind,
        JsonErrorKind::DuplicateMember("name".to_owned())
    );
    assert_eq!(nested_escaped.offset, 19);

    let canonically_distinct = parse_strict(r#"{"é":1,"e\u0301":2}"#).unwrap();
    assert_eq!(canonically_distinct.as_object().unwrap().len(), 2);
}

#[test]
fn numbers_follow_the_exact_json_grammar_and_preserve_lexemes() {
    for input in [
        "0",
        "-0",
        "1",
        "-1",
        "10",
        "-10",
        "1.0",
        "0.1",
        "0.0",
        "-0.0",
        "1e0",
        "1e10",
        "1E+10",
        "1e-10",
        "-1.2e-3",
        "123456789012345678901234567890",
        "1e400",
    ] {
        assert_eq!(
            parse_strict(input).unwrap(),
            JsonValue::Number(input.to_owned()),
            "{input}"
        );
    }

    for (input, code, offset) in [
        ("-", "invalid_number", 1),
        ("01", "invalid_number", 1),
        ("00", "invalid_number", 1),
        ("-01", "invalid_number", 2),
        ("1.", "invalid_number", 2),
        ("1e", "invalid_number", 2),
        ("1e+", "invalid_number", 3),
        ("1e-", "invalid_number", 3),
        ("-Infinity", "invalid_number", 1),
        ("--1", "invalid_number", 1),
        ("+1", "unexpected_token", 0),
        (".1", "unexpected_token", 0),
        ("NaN", "unexpected_token", 0),
        ("Infinity", "unexpected_token", 0),
        ("0x1", "trailing_characters", 1),
        ("1_0", "trailing_characters", 1),
        ("0١", "trailing_characters", 1),
    ] {
        let error = parse_strict(input).unwrap_err();
        assert_eq!(error.code(), code, "{input}");
        assert_eq!(error.offset, offset, "{input}");
    }

    assert_eq!(parse_strict("-0").unwrap().as_i64(), Some(0));
    assert_eq!(
        parse_strict("9223372036854775807").unwrap().as_i64(),
        Some(i64::MAX)
    );
    assert_eq!(parse_strict("9223372036854775808").unwrap().as_i64(), None);
    assert_eq!(parse_strict("1.0").unwrap().as_i64(), None);
    assert_eq!(parse_strict("1e0").unwrap().as_i64(), None);
}

#[test]
fn exactly_one_top_level_value_and_only_json_whitespace_are_accepted() {
    assert_eq!(parse_strict(" \t\r\nnull\n\r\t ").unwrap(), JsonValue::Null);

    for (input, code, offset) in [
        ("", "unexpected_end", 0),
        (" \t\r\n", "unexpected_end", 4),
        ("null true", "trailing_characters", 5),
        ("{}x", "trailing_characters", 2),
        ("[0,]", "unexpected_token", 3),
        (r#"{"a":0,}"#, "unexpected_token", 7),
        ("\u{feff}null", "unexpected_token", 0),
        ("\u{00a0}null", "unexpected_token", 0),
    ] {
        let error = parse_strict(input).unwrap_err();
        assert_eq!(error.code(), code, "{input:?}");
        assert_eq!(error.offset, offset, "{input:?}");
    }
}

#[test]
fn errors_use_stable_codes_and_the_first_offending_byte() {
    let bad_colon = parse_strict(r#"{"a"x:1}"#).unwrap_err();
    assert!(matches!(
        bad_colon.kind,
        JsonErrorKind::UnexpectedToken {
            expected: "colon",
            found: Some('x')
        }
    ));
    assert_eq!(bad_colon.offset, 4);
    assert_eq!(bad_colon.code(), "unexpected_token");

    let bad_array_separator = parse_strict("[0x]").unwrap_err();
    assert!(matches!(
        bad_array_separator.kind,
        JsonErrorKind::UnexpectedToken {
            expected: "comma or closing bracket",
            found: Some('x')
        }
    ));
    assert_eq!(bad_array_separator.offset, 2);

    let bad_literal = parse_strict("truX").unwrap_err();
    assert!(matches!(
        bad_literal.kind,
        JsonErrorKind::UnexpectedToken {
            expected: "true",
            found: Some('X')
        }
    ));
    assert_eq!(bad_literal.offset, 3);

    for (input, offset) in [("tru", 3), ("[0", 2), (r#"{"a""#, 4), ("[0,", 3)] {
        let error = parse_strict(input).unwrap_err();
        assert_eq!(error.kind, JsonErrorKind::UnexpectedEnd, "{input}");
        assert_eq!(error.offset, offset, "{input}");
        assert_eq!(error.code(), "unexpected_end", "{input}");
    }
}

#[test]
fn nested_malformed_values_preserve_codes_and_absolute_byte_offsets() {
    for (input, code, offset) in [
        (r#"["\x"]"#, "invalid_escape", 3),
        (r#"{"v":"\uDEAD"}"#, "invalid_unicode", 6),
        (r#"{"v":[1e-]}"#, "invalid_number", 9),
        (r#"{"v":[0]}x"#, "trailing_characters", 9),
    ] {
        let error = parse_strict(input).unwrap_err();
        assert_eq!(error.code(), code, "{input}");
        assert_eq!(error.offset, offset, "{input}");
    }
}

#[test]
fn byte_input_rejects_invalid_utf8_at_the_first_invalid_byte() {
    assert_eq!(
        parse_strict_bytes(r#""ßΩ𝄞""#.as_bytes()).unwrap(),
        JsonValue::String("ßΩ𝄞".to_owned())
    );

    for input in [&b"\"\xff\""[..], &b"\"\xe2\x82"[..]] {
        let error = parse_strict_bytes(input).unwrap_err();
        assert_eq!(error.kind, JsonErrorKind::InvalidUtf8);
        assert_eq!(error.offset, 1);
        assert_eq!(error.code(), "invalid_utf8");
    }
}

#[test]
fn the_default_depth_limit_is_safe_and_other_defaults_remain_unlimited() {
    let limits = JsonLimits::default();
    assert_eq!(limits.max_nesting_depth, DEFAULT_MAX_NESTING_DEPTH);
    assert_eq!(DEFAULT_MAX_NESTING_DEPTH, 128);
    assert_eq!(limits.max_input_bytes, None);
    assert_eq!(limits.max_string_bytes, None);
    assert_eq!(limits.max_object_members, None);
    assert_eq!(limits.max_array_items, None);

    parse_strict(&nested_arrays(DEFAULT_MAX_NESTING_DEPTH)).unwrap();
    parse_strict(&nested_objects(DEFAULT_MAX_NESTING_DEPTH)).unwrap();

    let too_deep = parse_strict(&nested_arrays(DEFAULT_MAX_NESTING_DEPTH + 1)).unwrap_err();
    assert_limit(
        &too_deep,
        JsonLimit::NestingDepth,
        DEFAULT_MAX_NESTING_DEPTH,
    );
    assert_eq!(too_deep.offset, DEFAULT_MAX_NESTING_DEPTH);
}

#[test]
fn configured_depth_cannot_exceed_the_hard_safety_ceiling() {
    let attempted_override = JsonLimits {
        max_nesting_depth: usize::MAX,
        ..JsonLimits::default()
    };
    parse_strict_with_limits(
        &nested_arrays(DEFAULT_MAX_NESTING_DEPTH),
        attempted_override,
    )
    .unwrap();
    let error = parse_strict_with_limits(
        &nested_arrays(DEFAULT_MAX_NESTING_DEPTH + 1),
        attempted_override,
    )
    .unwrap_err();
    assert_limit(&error, JsonLimit::NestingDepth, DEFAULT_MAX_NESTING_DEPTH);
    assert_eq!(error.offset, DEFAULT_MAX_NESTING_DEPTH);

    let smaller_limit = JsonLimits {
        max_nesting_depth: 4,
        ..JsonLimits::default()
    };
    parse_strict_with_limits(&nested_objects(4), smaller_limit).unwrap();
    let smaller_error = parse_strict_with_limits(&nested_objects(5), smaller_limit).unwrap_err();
    assert_limit(&smaller_error, JsonLimit::NestingDepth, 4);
    assert_eq!(smaller_error.offset, 36);
}

#[test]
fn additive_limits_reject_the_first_input_string_or_member_over_the_boundary() {
    let input_limit = JsonLimits {
        max_input_bytes: Some(3),
        ..JsonLimits::default()
    };
    let too_many_input_bytes = parse_strict_with_limits("null", input_limit).unwrap_err();
    assert_limit(&too_many_input_bytes, JsonLimit::InputBytes, 3);
    assert_eq!(too_many_input_bytes.offset, 3);

    let string_limit = JsonLimits {
        max_string_bytes: Some(1),
        ..JsonLimits::default()
    };
    let raw_string = parse_strict_with_limits(r#""é""#, string_limit).unwrap_err();
    assert_limit(&raw_string, JsonLimit::StringBytes, 1);
    assert_eq!(raw_string.offset, 1);
    let escaped_string = parse_strict_with_limits(r#""\u00e9""#, string_limit).unwrap_err();
    assert_limit(&escaped_string, JsonLimit::StringBytes, 1);
    assert_eq!(escaped_string.offset, 1);

    let member_limit = JsonLimits {
        max_object_members: Some(1),
        ..JsonLimits::default()
    };
    let too_many_members = parse_strict_with_limits(r#"{"a":0,"b":1}"#, member_limit).unwrap_err();
    assert_limit(&too_many_members, JsonLimit::ObjectMembers, 1);
    assert_eq!(too_many_members.offset, 7);

    let item_limit = JsonLimits {
        max_array_items: Some(1),
        ..JsonLimits::default()
    };
    let too_many_items = parse_strict_with_limits("[0,1]", item_limit).unwrap_err();
    assert_limit(&too_many_items, JsonLimit::ArrayItems, 1);
    assert_eq!(too_many_items.offset, 3);

    let byte_limit = JsonLimits {
        max_input_bytes: Some(3),
        ..JsonLimits::default()
    };
    let limited_bytes = parse_strict_bytes_with_limits(b"null", byte_limit).unwrap_err();
    assert_limit(&limited_bytes, JsonLimit::InputBytes, 3);
    assert_eq!(limited_bytes.offset, 3);
}
