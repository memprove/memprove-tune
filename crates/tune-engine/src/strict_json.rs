//! A small strict JSON parser, so that the crate needs no serialization
//! dependency.
//!
//! It rejects duplicate object members, including members nested inside
//! arrays or objects, and input past the limits of [`JsonLimits`].

use std::{collections::BTreeSet, fmt, str};

/// Default maximum number of nested JSON arrays and objects.
pub const DEFAULT_MAX_NESTING_DEPTH: usize = 128;

/// A parser resource governed by [`JsonLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonLimit {
    /// UTF-8 bytes in the complete JSON input.
    InputBytes,
    /// Nested JSON arrays and objects.
    NestingDepth,
    /// UTF-8 bytes in one decoded JSON string or member name.
    StringBytes,
    /// Members in one JSON object.
    ObjectMembers,
    /// Items in one JSON array.
    ArrayItems,
}

/// Configurable strict-JSON resource limits.
///
/// Only nesting has a bounded default. Other limits remain explicitly
/// unlimited until the transport contract freezes concrete values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonLimits {
    /// Requested maximum nested arrays and objects, including the root value.
    ///
    /// Values above [`DEFAULT_MAX_NESTING_DEPTH`] are clamped to that hard
    /// safety ceiling; smaller values remain effective.
    pub max_nesting_depth: usize,
    /// Optional maximum input length in UTF-8 bytes.
    pub max_input_bytes: Option<usize>,
    /// Optional maximum decoded length of one string in UTF-8 bytes.
    pub max_string_bytes: Option<usize>,
    /// Optional maximum members in one object.
    pub max_object_members: Option<usize>,
    /// Optional maximum items in one array.
    pub max_array_items: Option<usize>,
}

impl Default for JsonLimits {
    fn default() -> Self {
        Self {
            max_nesting_depth: DEFAULT_MAX_NESTING_DEPTH,
            max_input_bytes: None,
            max_string_bytes: None,
            max_object_members: None,
            max_array_items: None,
        }
    }
}

/// A JSON value that preserves exact integer text and object insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonValue {
    /// JSON `null`.
    Null,
    /// A JSON boolean.
    Bool(bool),
    /// A JSON number in its original lexical form.
    Number(String),
    /// A JSON string.
    String(String),
    /// A JSON array.
    Array(Vec<Self>),
    /// A JSON object. Duplicate names are rejected by [`parse_strict`].
    Object(Vec<(String, Self)>),
}

impl JsonValue {
    /// Returns this value as an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// Returns this value as an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    /// Returns this value as a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// Returns this value as a boolean.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns this value as an integer when its JSON representation is integral.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(value)
                if !value.contains('.') && !value.contains('e') && !value.contains('E') =>
            {
                value.parse().ok()
            }
            _ => None,
        }
    }

    /// Looks up one member of a JSON object.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Self> {
        self.as_object()?
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value))
    }

    /// Returns a stable type name for diagnostics.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }
}

/// The classified reason strict JSON parsing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonErrorKind {
    /// Input ended before the current value was complete.
    UnexpectedEnd,
    /// The next token could not begin or continue the expected JSON value.
    UnexpectedToken {
        /// Human-readable token category expected by the parser.
        expected: &'static str,
        /// Character observed at the failure offset, when one exists.
        found: Option<char>,
    },
    /// A number did not obey the JSON number grammar.
    InvalidNumber,
    /// A string contained an unsupported JSON escape.
    InvalidEscape,
    /// A string contained an invalid Unicode escape or surrogate pair.
    InvalidUnicode,
    /// Input bytes were not valid UTF-8.
    InvalidUtf8,
    /// An object repeated a member name.
    DuplicateMember(String),
    /// Non-whitespace input followed the first complete JSON value.
    TrailingCharacters,
    /// A configured parser resource limit was exceeded.
    LimitExceeded {
        /// Resource whose configured maximum was exceeded.
        limit: JsonLimit,
        /// Configured inclusive maximum.
        maximum: usize,
    },
}

/// A strict JSON parse failure with a byte offset into the original input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Classified parse failure.
    pub kind: JsonErrorKind,
    /// Zero-based byte offset at which parsing failed.
    pub offset: usize,
}

impl JsonError {
    /// Returns a stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match &self.kind {
            JsonErrorKind::UnexpectedEnd => "unexpected_end",
            JsonErrorKind::UnexpectedToken { .. } => "unexpected_token",
            JsonErrorKind::InvalidNumber => "invalid_number",
            JsonErrorKind::InvalidEscape => "invalid_escape",
            JsonErrorKind::InvalidUnicode => "invalid_unicode",
            JsonErrorKind::InvalidUtf8 => "invalid_utf8",
            JsonErrorKind::DuplicateMember(_) => "duplicate_member",
            JsonErrorKind::TrailingCharacters => "trailing_characters",
            JsonErrorKind::LimitExceeded { .. } => "limit_exceeded",
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "JSON error at byte {}: {:?}",
            self.offset, self.kind
        )
    }
}

impl std::error::Error for JsonError {}

/// Longest request member or query parameter name a Problem `detail` may
/// repeat.
const ECHOED_NAME_MAX_CHARS: usize = 32;

/// A caller-supplied member or parameter name that is safe to repeat in a
/// Problem `detail`: lowercase ASCII letters, digits, and `_`, at most 32
/// characters. Anything else, including a 43-character token secret that
/// was pasted in as a name, is not repeated.
#[must_use]
pub fn echoable_name(name: &str) -> Option<&str> {
    (!name.is_empty()
        && name.len() <= ECHOED_NAME_MAX_CHARS
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(name)
}

impl JsonError {
    /// The `detail` of the `400` Problem for a body that is not strict JSON:
    /// what rule failed and the byte offset, never the offending text.
    #[must_use]
    pub fn detail(&self) -> String {
        let rule = match &self.kind {
            JsonErrorKind::UnexpectedEnd => "the input ends inside a value".to_owned(),
            JsonErrorKind::UnexpectedToken { expected, .. } => format!("expected {expected}"),
            JsonErrorKind::InvalidNumber => "a number is not valid JSON".to_owned(),
            JsonErrorKind::InvalidEscape => "a string has an unsupported escape".to_owned(),
            JsonErrorKind::InvalidUnicode => "a string has an invalid Unicode escape".to_owned(),
            JsonErrorKind::InvalidUtf8 => "the input is not valid UTF-8".to_owned(),
            JsonErrorKind::DuplicateMember(name) => match echoable_name(name) {
                Some(name) => format!("the member {name} appears more than once"),
                None => "a member appears more than once".to_owned(),
            },
            JsonErrorKind::TrailingCharacters => "text follows the first JSON value".to_owned(),
            JsonErrorKind::LimitExceeded { limit, maximum } => {
                format!("the {limit:?} limit of {maximum} is exceeded")
            }
        };
        format!(
            "the request body is not valid JSON: {rule} (byte {})",
            self.offset
        )
    }
}

/// Parses exactly one JSON value and rejects duplicate object members.
///
/// # Errors
///
/// Returns [`JsonError`] for malformed JSON, duplicate members, or trailing
/// non-whitespace input.
pub fn parse_strict(input: &str) -> Result<JsonValue, JsonError> {
    parse_strict_with_limits(input, JsonLimits::default())
}

/// Parses exactly one JSON value with explicit resource limits.
///
/// # Errors
///
/// Returns [`JsonError`] for malformed JSON, duplicate members, trailing
/// non-whitespace input, or a configured resource limit.
pub fn parse_strict_with_limits(input: &str, limits: JsonLimits) -> Result<JsonValue, JsonError> {
    enforce_input_limit(input.len(), limits)?;
    let mut parser = Parser {
        input,
        offset: 0,
        limits,
    };
    parser.skip_whitespace();
    let value = parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.offset != input.len() {
        return Err(parser.error(JsonErrorKind::TrailingCharacters));
    }
    Ok(value)
}

/// Parses exactly one UTF-8 encoded JSON value.
///
/// # Errors
///
/// Returns [`JsonError`] when the bytes are not UTF-8 or the JSON value is not
/// strict and complete.
pub fn parse_strict_bytes(input: &[u8]) -> Result<JsonValue, JsonError> {
    parse_strict_bytes_with_limits(input, JsonLimits::default())
}

/// Parses exactly one UTF-8 encoded JSON value with explicit resource limits.
///
/// # Errors
///
/// Returns [`JsonError`] when the byte input, JSON value, or configured limits
/// are invalid.
pub fn parse_strict_bytes_with_limits(
    input: &[u8],
    limits: JsonLimits,
) -> Result<JsonValue, JsonError> {
    enforce_input_limit(input.len(), limits)?;
    let utf8 = str::from_utf8(input).map_err(|error| JsonError {
        kind: JsonErrorKind::InvalidUtf8,
        offset: error.valid_up_to(),
    })?;
    parse_strict_with_limits(utf8, limits)
}

fn enforce_input_limit(input_len: usize, limits: JsonLimits) -> Result<(), JsonError> {
    if let Some(maximum) = limits.max_input_bytes
        && input_len > maximum
    {
        return Err(JsonError {
            kind: JsonErrorKind::LimitExceeded {
                limit: JsonLimit::InputBytes,
                maximum,
            },
            offset: maximum,
        });
    }
    Ok(())
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
    limits: JsonLimits,
}

impl Parser<'_> {
    fn error(&self, kind: JsonErrorKind) -> JsonError {
        JsonError {
            kind,
            offset: self.offset,
        }
    }

    fn error_at(kind: JsonErrorKind, offset: usize) -> JsonError {
        JsonError { kind, offset }
    }

    fn unexpected_at(expected: &'static str, found: Option<char>, offset: usize) -> JsonError {
        match found {
            Some(found) => Self::error_at(
                JsonErrorKind::UnexpectedToken {
                    expected,
                    found: Some(found),
                },
                offset,
            ),
            None => Self::error_at(JsonErrorKind::UnexpectedEnd, offset),
        }
    }

    fn limit_error(limit: JsonLimit, maximum: usize, offset: usize) -> JsonError {
        Self::error_at(JsonErrorKind::LimitExceeded { limit, maximum }, offset)
    }

    fn nested_depth(&self, depth: usize) -> Result<usize, JsonError> {
        let maximum = self.limits.max_nesting_depth.min(DEFAULT_MAX_NESTING_DEPTH);
        if depth >= maximum {
            Err(Self::limit_error(
                JsonLimit::NestingDepth,
                maximum,
                self.offset,
            ))
        } else {
            Ok(depth + 1)
        }
    }

    fn remaining(&self) -> &str {
        &self.input[self.offset..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn next(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\n' | '\r' | '\t')) {
            self.next();
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        match self.peek() {
            Some('n') => {
                self.consume_literal("null")?;
                Ok(JsonValue::Null)
            }
            Some('t') => {
                self.consume_literal("true")?;
                Ok(JsonValue::Bool(true))
            }
            Some('f') => {
                self.consume_literal("false")?;
                Ok(JsonValue::Bool(false))
            }
            Some('"') => self.parse_string().map(JsonValue::String),
            Some('[') => {
                let nested_depth = self.nested_depth(depth)?;
                self.parse_array(nested_depth)
            }
            Some('{') => {
                let nested_depth = self.nested_depth(depth)?;
                self.parse_object(nested_depth)
            }
            Some('-' | '0'..='9') => self.parse_number().map(JsonValue::Number),
            found => Err(Self::unexpected_at("JSON value", found, self.offset)),
        }
    }

    fn consume_literal(&mut self, literal: &'static str) -> Result<(), JsonError> {
        for expected in literal.chars() {
            let offset = self.offset;
            match self.next() {
                Some(found) if found == expected => {}
                found => return Err(Self::unexpected_at(literal, found, offset)),
            }
        }
        Ok(())
    }

    fn enforce_item_limit(
        &self,
        maximum: Option<usize>,
        current: usize,
        limit: JsonLimit,
    ) -> Result<(), JsonError> {
        if let Some(maximum) = maximum
            && current >= maximum
        {
            return Err(Self::limit_error(limit, maximum, self.offset));
        }
        Ok(())
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.next();
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.peek() == Some(']') {
            self.next();
            return Ok(JsonValue::Array(values));
        }
        loop {
            self.enforce_item_limit(
                self.limits.max_array_items,
                values.len(),
                JsonLimit::ArrayItems,
            )?;
            values.push(self.parse_value(depth)?);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.next();
                    self.skip_whitespace();
                }
                Some(']') => {
                    self.next();
                    return Ok(JsonValue::Array(values));
                }
                found => {
                    return Err(Self::unexpected_at(
                        "comma or closing bracket",
                        found,
                        self.offset,
                    ));
                }
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.next();
        self.skip_whitespace();
        let mut entries: Vec<(String, JsonValue)> = Vec::new();
        let mut member_names = BTreeSet::new();
        if self.peek() == Some('}') {
            self.next();
            return Ok(JsonValue::Object(entries));
        }
        loop {
            let member_offset = self.offset;
            if self.peek() != Some('"') {
                return Err(Self::unexpected_at(
                    "object member name",
                    self.peek(),
                    member_offset,
                ));
            }
            self.enforce_item_limit(
                self.limits.max_object_members,
                entries.len(),
                JsonLimit::ObjectMembers,
            )?;
            let name = self.parse_string()?;
            if !member_names.insert(name.clone()) {
                return Err(Self::error_at(
                    JsonErrorKind::DuplicateMember(name),
                    member_offset,
                ));
            }
            self.skip_whitespace();
            if self.peek() != Some(':') {
                return Err(Self::unexpected_at("colon", self.peek(), self.offset));
            }
            self.next();
            self.skip_whitespace();
            entries.push((name, self.parse_value(depth)?));
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.next();
                    self.skip_whitespace();
                }
                Some('}') => {
                    self.next();
                    return Ok(JsonValue::Object(entries));
                }
                found => {
                    return Err(Self::unexpected_at(
                        "comma or closing brace",
                        found,
                        self.offset,
                    ));
                }
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        if self.peek() != Some('"') {
            return Err(Self::unexpected_at("string", self.peek(), self.offset));
        }
        self.next();
        let mut output = String::new();
        loop {
            let character_offset = self.offset;
            match self.next() {
                Some('"') => return Ok(output),
                Some('\\') => self.parse_escape(&mut output, character_offset)?,
                Some(character) if character <= '\u{001f}' => {
                    return Err(Self::unexpected_at(
                        "unescaped string character",
                        Some(character),
                        character_offset,
                    ));
                }
                Some(character) => {
                    self.push_character(&mut output, character, character_offset)?;
                }
                None => {
                    return Err(Self::error_at(
                        JsonErrorKind::UnexpectedEnd,
                        character_offset,
                    ));
                }
            }
        }
    }

    fn push_character(
        &self,
        output: &mut String,
        character: char,
        source_offset: usize,
    ) -> Result<(), JsonError> {
        if let Some(maximum) = self.limits.max_string_bytes {
            let Some(next_length) = output.len().checked_add(character.len_utf8()) else {
                return Err(Self::limit_error(
                    JsonLimit::StringBytes,
                    maximum,
                    source_offset,
                ));
            };
            if next_length > maximum {
                return Err(Self::limit_error(
                    JsonLimit::StringBytes,
                    maximum,
                    source_offset,
                ));
            }
        }
        output.push(character);
        Ok(())
    }

    fn parse_escape(&mut self, output: &mut String, escape_offset: usize) -> Result<(), JsonError> {
        let designator_offset = self.offset;
        match self.next() {
            Some('"') => self.push_character(output, '"', escape_offset)?,
            Some('\\') => self.push_character(output, '\\', escape_offset)?,
            Some('/') => self.push_character(output, '/', escape_offset)?,
            Some('b') => self.push_character(output, '\u{0008}', escape_offset)?,
            Some('f') => self.push_character(output, '\u{000c}', escape_offset)?,
            Some('n') => self.push_character(output, '\n', escape_offset)?,
            Some('r') => self.push_character(output, '\r', escape_offset)?,
            Some('t') => self.push_character(output, '\t', escape_offset)?,
            Some('u') => self.parse_unicode_escape(output, escape_offset)?,
            Some(_) => {
                return Err(Self::error_at(
                    JsonErrorKind::InvalidEscape,
                    designator_offset,
                ));
            }
            None => {
                return Err(Self::error_at(
                    JsonErrorKind::UnexpectedEnd,
                    designator_offset,
                ));
            }
        }
        Ok(())
    }

    fn parse_unicode_escape(
        &mut self,
        output: &mut String,
        escape_offset: usize,
    ) -> Result<(), JsonError> {
        let first = self.parse_hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            if !self.remaining().starts_with("\\u") {
                return Err(self.error(JsonErrorKind::InvalidUnicode));
            }
            let second_escape_offset = self.offset;
            self.offset += 2;
            let second = self.parse_hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(Self::error_at(
                    JsonErrorKind::InvalidUnicode,
                    second_escape_offset,
                ));
            }
            0x1_0000 + (u32::from(first) - 0xd800) * 0x400 + (u32::from(second) - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(Self::error_at(JsonErrorKind::InvalidUnicode, escape_offset));
        } else {
            u32::from(first)
        };
        let Some(character) = char::from_u32(scalar) else {
            return Err(Self::error_at(JsonErrorKind::InvalidUnicode, escape_offset));
        };
        self.push_character(output, character, escape_offset)
    }

    fn parse_hex_quad(&mut self) -> Result<u16, JsonError> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit_offset = self.offset;
            let Some(character) = self.next() else {
                return Err(Self::error_at(JsonErrorKind::InvalidUnicode, digit_offset));
            };
            let Some(digit) = character.to_digit(16) else {
                return Err(Self::error_at(JsonErrorKind::InvalidUnicode, digit_offset));
            };
            let digit = u16::try_from(digit)
                .map_err(|_| Self::error_at(JsonErrorKind::InvalidUnicode, digit_offset))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<String, JsonError> {
        let start = self.offset;
        if self.peek() == Some('-') {
            self.next();
        }
        match self.peek() {
            Some('0') => {
                self.next();
                if matches!(self.peek(), Some('0'..='9')) {
                    return Err(self.error(JsonErrorKind::InvalidNumber));
                }
            }
            Some('1'..='9') => {
                while matches!(self.peek(), Some('0'..='9')) {
                    self.next();
                }
            }
            _ => return Err(self.error(JsonErrorKind::InvalidNumber)),
        }
        if self.peek() == Some('.') {
            self.next();
            self.consume_digits()?;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.next();
            if matches!(self.peek(), Some('+' | '-')) {
                self.next();
            }
            self.consume_digits()?;
        }
        Ok(self.input[start..self.offset].to_owned())
    }

    fn consume_digits(&mut self) -> Result<(), JsonError> {
        if !matches!(self.peek(), Some('0'..='9')) {
            return Err(self.error(JsonErrorKind::InvalidNumber));
        }
        while matches!(self.peek(), Some('0'..='9')) {
            self.next();
        }
        Ok(())
    }
}
