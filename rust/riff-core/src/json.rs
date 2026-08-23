//! A JSON value of unknown shape, plus the parser and writer the engine needs.
//!
//! Tool arguments arrive from a language model, so they cannot be decoded into a fixed type until
//! they have been validated. The other bindings reach for a platform JSON facility here; Rust's
//! standard library has none, and the engine takes no third-party dependencies, so the small exact
//! implementation lives in the crate.

use std::fmt::Write as _;

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// Any JSON number, held as a double the way every JSON implementation does.
    Number(f64),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, in insertion order.
    Object(JsonObject),
}

/// A JSON object that remembers the order its keys were inserted in.
///
/// Ordering is kept because tool results are serialized and read by a model, and a stable shape is
/// easier to diff against the other bindings than a hash order that changes between runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JsonObject {
    entries: Vec<(String, Json)>,
}

impl JsonObject {
    /// An empty object.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces `key`, keeping the position of an existing key.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        match self.entries.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// Inserts `value` only when it is `Some`, which is how the other bindings spread an optional
    /// field into an object literal.
    pub fn insert_some(&mut self, key: impl Into<String>, value: Option<Json>) {
        if let Some(value) = value {
            self.insert(key, value);
        }
    }

    /// The value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    /// Whether `key` is present, including when its value is `null`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The keys, in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// The entries, in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Json)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// How many keys the object has.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the object has no keys.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(String, Json)> for JsonObject {
    fn from_iter<T: IntoIterator<Item = (String, Json)>>(iter: T) -> Self {
        let mut object = Self::new();
        for (key, value) in iter {
            object.insert(key, value);
        }
        object
    }
}

impl Json {
    /// An empty object, the shape a handler returns when it has nothing to report.
    pub fn object() -> Self {
        Json::Object(JsonObject::new())
    }

    /// Builds a string value without the caller spelling out the conversion.
    pub fn string(value: impl Into<String>) -> Self {
        Json::String(value.into())
    }

    /// The string, when this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    /// The boolean, when this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The number, when this is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// The number rounded to an integer, when this is a number.
    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|value| value.round() as i64)
    }

    /// The number rounded to a `usize`, when this is a non-negative number.
    pub fn as_usize(&self) -> Option<usize> {
        self.as_i64().and_then(|value| usize::try_from(value).ok())
    }

    /// The array, when this is one.
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(value) => Some(value),
            _ => None,
        }
    }

    /// The array, or an empty slice for every other shape.
    pub fn array_or_empty(&self) -> &[Json] {
        self.as_array().unwrap_or(&[])
    }

    /// The object, when this is one.
    pub fn as_object(&self) -> Option<&JsonObject> {
        match self {
            Json::Object(value) => Some(value),
            _ => None,
        }
    }

    /// The value at `key`, when this is an object that has one.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_object().and_then(|object| object.get(key))
    }

    /// The string at `key`, when this is an object that has one.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }

    /// Whether this is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Parses JSON text. An empty string is an empty object, which is how a model spells "no
    /// arguments".
    pub fn parse(text: &str) -> Result<Json, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Json::object());
        }
        let mut parser = Parser {
            bytes: trimmed.as_bytes(),
            at: 0,
            depth: 0,
        };
        parser.skip_whitespace();
        let value = parser.value()?;
        parser.skip_whitespace();
        if parser.at != parser.bytes.len() {
            return Err(format!(
                "unexpected trailing characters at byte {}",
                parser.at
            ));
        }
        Ok(value)
    }

    /// Serializes to compact JSON, the form every provider expects a tool result in.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(value) => write_number(*value, out),
            Json::String(value) => write_string(value, out),
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(object) => {
                out.push('{');
                for (index, (key, value)) in object.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Json::Bool(value)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        Json::Number(value)
    }
}

impl From<usize> for Json {
    fn from(value: usize) -> Self {
        Json::Number(value as f64)
    }
}

impl From<u64> for Json {
    fn from(value: u64) -> Self {
        Json::Number(value as f64)
    }
}

impl From<i64> for Json {
    fn from(value: i64) -> Self {
        Json::Number(value as f64)
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Json::String(value)
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Json::String(value.to_owned())
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(value: Vec<T>) -> Self {
        Json::Array(value.into_iter().map(Into::into).collect())
    }
}

impl From<JsonObject> for Json {
    fn from(value: JsonObject) -> Self {
        Json::Object(value)
    }
}

/// Builds a JSON object from `key => value` pairs, in the order they are written.
#[macro_export]
macro_rules! json_object {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut object = $crate::JsonObject::new();
        $(object.insert($key, $crate::Json::from($value));)*
        object
    }};
}

/// Whole numbers are written without a trailing `.0` so payloads match the other bindings.
fn write_number(value: f64, out: &mut String) {
    if !value.is_finite() {
        out.push_str("null");
    } else if value == value.trunc() && value.abs() < 1e15 {
        let _ = write!(out, "{}", value as i64);
    } else {
        let _ = write!(out, "{value}");
    }
}

fn write_string(value: &str, out: &mut String) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

/// A model can emit deeply nested arguments; the limit keeps a malformed payload from exhausting
/// the stack rather than being reported as bad arguments.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    depth: usize,
}

impl Parser<'_> {
    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expect(&mut self, literal: &str) -> Result<(), String> {
        if self.bytes[self.at..].starts_with(literal.as_bytes()) {
            self.at += literal.len();
            Ok(())
        } else {
            Err(format!("expected \"{literal}\" at byte {}", self.at))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("JSON is nested too deeply".to_owned());
        }
        let value = match self.peek() {
            Some(b'n') => {
                self.expect("null")?;
                Json::Null
            }
            Some(b't') => {
                self.expect("true")?;
                Json::Bool(true)
            }
            Some(b'f') => {
                self.expect("false")?;
                Json::Bool(false)
            }
            Some(b'"') => Json::String(self.string()?),
            Some(b'[') => self.array()?,
            Some(b'{') => self.object()?,
            Some(_) => self.number()?,
            None => return Err("unexpected end of JSON".to_owned()),
        };
        self.depth -= 1;
        Ok(value)
    }

    fn array(&mut self) -> Result<Json, String> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(format!("expected \",\" or \"]\" at byte {}", self.at)),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.at += 1;
        let mut object = JsonObject::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(object));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(format!("expected a key at byte {}", self.at));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(format!("expected \":\" at byte {}", self.at));
            }
            self.at += 1;
            self.skip_whitespace();
            let value = self.value()?;
            object.insert(key, value);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(object));
                }
                _ => return Err(format!("expected \",\" or \"}}\" at byte {}", self.at)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| "unterminated string".to_owned())?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| "unterminated escape".to_owned())?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        other => return Err(format!("unknown escape \"\\{}\"", other as char)),
                    }
                }
                _ => {
                    // The input is a `&str`, so a multi-byte sequence is already valid UTF-8 and can
                    // be copied whole rather than decoded byte by byte.
                    let start = self.at;
                    while let Some(byte) = self.peek() {
                        if byte == b'"' || byte == b'\\' {
                            break;
                        }
                        // JSON requires a control character to be escaped, and both other bindings
                        // parse with a library that enforces it. Copying one through would make the
                        // same malformed tool call succeed here and fail there.
                        if byte < 0x20 {
                            return Err(format!(
                                "unescaped control character U+{byte:04X} in a string at byte {}",
                                self.at
                            ));
                        }
                        self.at += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..self.at])
                            .map_err(|_| "string is not valid UTF-8".to_owned())?,
                    );
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        // A character outside the basic plane is written as a surrogate pair, which has to be
        // recombined rather than rejected.
        if (0xD800..0xDC00).contains(&first) {
            if self.peek() == Some(b'\\') && self.bytes.get(self.at + 1) == Some(&b'u') {
                self.at += 2;
                let second = self.hex4()?;
                if (0xDC00..0xE000).contains(&second) {
                    let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                    return char::from_u32(combined)
                        .ok_or_else(|| "invalid surrogate pair".to_owned());
                }
            }
            return Err("unpaired surrogate in \\u escape".to_owned());
        }
        char::from_u32(first).ok_or_else(|| "invalid \\u escape".to_owned())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.at + 4;
        if end > self.bytes.len() {
            return Err("truncated \\u escape".to_owned());
        }
        let text = std::str::from_utf8(&self.bytes[self.at..end])
            .map_err(|_| "invalid \\u escape".to_owned())?;
        let value = u32::from_str_radix(text, 16).map_err(|_| "invalid \\u escape".to_owned())?;
        self.at = end;
        Ok(value)
    }

    /// Scans a JSON number, which is a narrower grammar than `f64::from_str` accepts.
    ///
    /// `+1`, `01`, `.5`, and `1.` all parse as floats and none of them is JSON. Both other bindings
    /// decode with a library that rejects them, so accepting them would let the same tool call
    /// succeed here and fail there.
    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        let invalid = |at: usize| Err(format!("invalid number at byte {at}"));

        if self.peek() == Some(b'-') {
            self.at += 1;
        }

        match self.peek() {
            // A leading zero may not be followed by more digits: `01` is two tokens, not a number.
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at += 1;
                }
            }
            _ => return invalid(self.at),
        }

        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return invalid(self.at);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return invalid(self.at);
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
        }

        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| "invalid number".to_owned())?;
        let value: f64 = text
            .parse()
            .map_err(|_| format!("invalid number \"{text}\""))?;
        if !value.is_finite() {
            // A literal too large to represent. Carrying an infinity forward would fail schema
            // validation later with a message about the wrong thing entirely.
            return Err(format!("number \"{text}\" is out of range"));
        }
        Ok(Json::Number(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_the_shapes_a_model_sends() {
        let text = r#"{"a":1,"b":[true,null,"x"],"c":{"d":-2.5}}"#;
        assert_eq!(Json::parse(text).unwrap().serialize(), text);
    }

    #[test]
    fn treats_empty_arguments_as_an_empty_object() {
        assert_eq!(Json::parse("   ").unwrap(), Json::object());
    }

    #[test]
    fn keeps_astral_characters_whole() {
        let parsed = Json::parse(r#""\ud83c\udfa7 \u00e9""#).unwrap();
        assert_eq!(parsed.as_str(), Some("🎧 é"));
        assert_eq!(parsed.serialize(), "\"🎧 é\"");
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(Json::from("a\nb\u{1}").serialize(), r#""a\nb\u0001""#);
    }

    #[test]
    fn writes_whole_numbers_without_a_fraction() {
        assert_eq!(Json::from(3.0).serialize(), "3");
        assert_eq!(Json::from(3.5).serialize(), "3.5");
    }

    #[test]
    fn rejects_malformed_arguments() {
        assert!(Json::parse("{\"a\":").is_err());
        assert!(Json::parse("{}{}").is_err());
    }

    #[test]
    fn rejects_number_spellings_that_are_not_json() {
        for text in ["+1", "01", ".5", "1.", "1.e3", "1e", "1e+", "-", "0x1"] {
            assert!(
                Json::parse(text).is_err(),
                "\"{text}\" is not a JSON number"
            );
        }
        for text in ["0", "-0", "1", "-1.5", "1e3", "1E+3", "1.5e-3", "0.5"] {
            assert!(Json::parse(text).is_ok(), "\"{text}\" is a JSON number");
        }
        // Out of range rather than silently infinite.
        assert!(Json::parse("1e999").is_err());
    }

    #[test]
    fn rejects_a_control_character_a_string_should_have_escaped() {
        // Both other bindings parse with a library that enforces this, so accepting it would make
        // the same malformed tool call succeed here and fail there.
        assert!(Json::parse("\"a\nb\"").is_err());
        assert!(Json::parse("{\"k\":\"v\u{1}\"}").is_err());
        assert_eq!(Json::parse(r#""a\nb""#).unwrap().as_str(), Some("a\nb"));
    }
}
