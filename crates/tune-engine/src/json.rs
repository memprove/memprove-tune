//! A small JSON writer and typed readers over
//! [`crate::strict_json`], so that the crate needs no
//! serialization dependency. Every file the crate writes is an object whose
//! members come in a fixed order, two-space indented, ending with a newline.

use std::{fmt::Write as _, path::Path};

pub use crate::strict_json::JsonValue;
use crate::strict_json::parse_strict;

use crate::EvalError;

/// `value` as a JSON string literal.
#[must_use]
pub fn string(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\t' => encoded.push_str("\\t"),
            '\r' => encoded.push_str("\\r"),
            control if u32::from(control) < 0x20 => {
                write!(encoded, "\\u{:04x}", u32::from(control))
                    .expect("writing to a String cannot fail");
            }
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    encoded
}

/// A JSON value being written: members and items are already encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// Already-encoded JSON text: a number, `true`, `false`, `null`, or a
    /// string literal from [`string`].
    Raw(String),
    /// An array.
    Array(Vec<Out>),
    /// An object, members in this order.
    Object(Vec<(&'static str, Out)>),
}

impl Out {
    /// A string.
    #[must_use]
    pub fn str(value: &str) -> Self {
        Self::Raw(string(value))
    }

    /// An unsigned integer.
    #[must_use]
    pub fn uint(value: u64) -> Self {
        Self::Raw(value.to_string())
    }

    /// A signed integer.
    #[must_use]
    pub fn int(value: i64) -> Self {
        Self::Raw(value.to_string())
    }

    /// A boolean.
    #[must_use]
    pub fn bool(value: bool) -> Self {
        Self::Raw(value.to_string())
    }

    /// `null`.
    #[must_use]
    pub fn null() -> Self {
        Self::Raw("null".to_owned())
    }

    /// The value, or `null`.
    #[must_use]
    pub fn opt<T>(value: Option<T>, into: impl FnOnce(T) -> Self) -> Self {
        value.map_or_else(Self::null, into)
    }

    /// An array of strings.
    #[must_use]
    pub fn strs<S: AsRef<str>>(values: &[S]) -> Self {
        Self::Array(
            values
                .iter()
                .map(|value| Self::str(value.as_ref()))
                .collect(),
        )
    }

    /// The value as pretty-printed JSON text with a final newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::new();
        self.write(&mut text, 0);
        text.push('\n');
        text
    }

    fn write(&self, text: &mut String, depth: usize) {
        match self {
            Self::Raw(raw) => text.push_str(raw),
            Self::Array(items) if items.is_empty() => text.push_str("[]"),
            Self::Object(members) if members.is_empty() => text.push_str("{}"),
            Self::Array(items) => {
                text.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    newline(text, depth + 1);
                    item.write(text, depth + 1);
                }
                newline(text, depth);
                text.push(']');
            }
            Self::Object(members) => {
                text.push('{');
                for (index, (name, value)) in members.iter().enumerate() {
                    if index > 0 {
                        text.push(',');
                    }
                    newline(text, depth + 1);
                    text.push_str(&string(name));
                    text.push_str(": ");
                    value.write(text, depth + 1);
                }
                newline(text, depth);
                text.push('}');
            }
        }
    }
}

fn newline(text: &mut String, depth: usize) {
    text.push('\n');
    for _ in 0..depth {
        text.push_str("  ");
    }
}

/// Parses `text` (from `path`, for messages) as strict JSON.
///
/// # Errors
/// [`EvalError::Invalid`] when it is not strict JSON.
pub fn parse(text: &str, path: Option<&Path>) -> Result<JsonValue, EvalError> {
    parse_strict(text).map_err(|error| EvalError::invalid(path, format!("not valid JSON: {error}")))
}

/// Typed access to the members of one JSON object, with errors that name
/// the member and the file.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    value: &'a JsonValue,
    path: Option<&'a Path>,
    at: &'a str,
}

impl<'a> Reader<'a> {
    /// A reader for `value`, an object at `at` (for example `results[2]`)
    /// in the file `path`.
    ///
    /// # Errors
    /// [`EvalError::Invalid`] when `value` is not an object.
    pub fn new(
        value: &'a JsonValue,
        path: Option<&'a Path>,
        at: &'a str,
    ) -> Result<Self, EvalError> {
        if value.as_object().is_none() {
            return Err(EvalError::invalid(
                path,
                format!("{} must be an object", name(at)),
            ));
        }
        Ok(Self { value, path, at })
    }

    /// The member names that are not in `known`.
    #[must_use]
    pub fn unknown(&self, known: &[&str]) -> Vec<String> {
        self.value
            .as_object()
            .unwrap_or_default()
            .iter()
            .filter(|(key, _)| !known.contains(&key.as_str()))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Refuses members that are not in `known`.
    ///
    /// # Errors
    /// [`EvalError::Invalid`] naming the first unknown member.
    pub fn deny_unknown(&self, known: &[&str]) -> Result<(), EvalError> {
        match self.unknown(known).first() {
            None => Ok(()),
            Some(key) => Err(self.error(key, "is not a known member")),
        }
    }

    /// An [`EvalError::Invalid`] about `member`.
    #[must_use]
    pub fn error(&self, member: &str, detail: &str) -> EvalError {
        EvalError::invalid(self.path, format!("{} {detail}", self.member_name(member)))
    }

    fn member_name(&self, member: &str) -> String {
        if self.at.is_empty() {
            format!("`{member}`")
        } else {
            format!("`{}.{member}`", self.at)
        }
    }

    /// The raw member, absent or `null` as `None`.
    #[must_use]
    pub fn get(&self, member: &str) -> Option<&'a JsonValue> {
        self.value
            .get(member)
            .filter(|value| !matches!(value, JsonValue::Null))
    }

    /// A required string.
    ///
    /// # Errors
    /// When it is absent or not a string.
    pub fn str(&self, member: &str) -> Result<&'a str, EvalError> {
        self.opt_str(member)?
            .ok_or_else(|| self.error(member, "is required"))
    }

    /// An optional string.
    ///
    /// # Errors
    /// When it is present and not a string.
    pub fn opt_str(&self, member: &str) -> Result<Option<&'a str>, EvalError> {
        self.get(member)
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| self.error(member, "must be a string"))
            })
            .transpose()
    }

    /// An optional non-negative integer.
    ///
    /// # Errors
    /// When it is present and not a non-negative integer.
    pub fn opt_u64(&self, member: &str) -> Result<Option<u64>, EvalError> {
        self.get(member)
            .map(|value| {
                value
                    .as_i64()
                    .and_then(|number| u64::try_from(number).ok())
                    .ok_or_else(|| self.error(member, "must be a non-negative integer"))
            })
            .transpose()
    }

    /// A required non-negative integer.
    ///
    /// # Errors
    /// When it is absent or not a non-negative integer.
    pub fn u64(&self, member: &str) -> Result<u64, EvalError> {
        self.opt_u64(member)?
            .ok_or_else(|| self.error(member, "is required"))
    }

    /// An optional integer.
    ///
    /// # Errors
    /// When it is present and not an integer.
    pub fn opt_i64(&self, member: &str) -> Result<Option<i64>, EvalError> {
        self.get(member)
            .map(|value| {
                value
                    .as_i64()
                    .ok_or_else(|| self.error(member, "must be an integer"))
            })
            .transpose()
    }

    /// An optional boolean.
    ///
    /// # Errors
    /// When it is present and not a boolean.
    pub fn opt_bool(&self, member: &str) -> Result<Option<bool>, EvalError> {
        self.get(member)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| self.error(member, "must be true or false"))
            })
            .transpose()
    }

    /// An optional array of strings; absent is empty.
    ///
    /// # Errors
    /// When it is present and not an array of strings.
    pub fn strs(&self, member: &str) -> Result<Vec<String>, EvalError> {
        let Some(value) = self.get(member) else {
            return Ok(Vec::new());
        };
        let items = value
            .as_array()
            .ok_or_else(|| self.error(member, "must be an array of strings"))?;
        items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| self.error(member, "must be an array of strings"))
            })
            .collect()
    }

    /// An optional array; absent is empty.
    ///
    /// # Errors
    /// When it is present and not an array.
    pub fn array(&self, member: &str) -> Result<&'a [JsonValue], EvalError> {
        let Some(value) = self.get(member) else {
            return Ok(&[]);
        };
        value
            .as_array()
            .ok_or_else(|| self.error(member, "must be an array"))
    }

    /// The file this reader reads, for nested readers.
    #[must_use]
    pub const fn path(&self) -> Option<&'a Path> {
        self.path
    }
}

fn name(at: &str) -> String {
    if at.is_empty() {
        "the document".to_owned()
    } else {
        format!("`{at}`")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nested_values_in_member_order() {
        let value = Out::Object(vec![
            ("b", Out::uint(1)),
            ("a", Out::Array(vec![Out::str("x\n\"y\""), Out::null()])),
            ("c", Out::Object(vec![])),
        ]);
        assert_eq!(
            value.render(),
            "{\n  \"b\": 1,\n  \"a\": [\n    \"x\\n\\\"y\\\"\",\n    null\n  ],\n  \"c\": {}\n}\n"
        );
        let parsed = parse(&value.render(), None).expect("round trip");
        assert_eq!(parsed.get("b").and_then(JsonValue::as_i64), Some(1));
    }

    #[test]
    fn reader_names_the_member_and_file() {
        let value = parse(r#"{"n": "x", "extra": 1}"#, None).expect("json");
        let path = Path::new("evals/a.json");
        let reader = Reader::new(&value, Some(path), "task").expect("object");
        let error = reader.u64("n").expect_err("not a number").to_string();
        assert_eq!(
            error,
            "evals/a.json: `task.n` must be a non-negative integer"
        );
        let error = reader.deny_unknown(&["n"]).expect_err("extra").to_string();
        assert_eq!(error, "evals/a.json: `task.extra` is not a known member");
    }
}
