//! A validator for the JSON Schema subset the agent's tool definitions use.
//!
//! Tool arguments arrive from a language model, so validation is a hot path on every call and its
//! error messages are read by the model rather than by a person. Both of those argue for a small
//! exact implementation over a general one: the messages name the offending path and say what was
//! expected, and defaults are filled in so a handler never sees a half-populated object.

use crate::json::{Json, JsonObject};

/// What validation decided, plus the value with defaults filled in.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether every constraint held.
    pub valid: bool,
    /// What went wrong, phrased for the model that sent the arguments.
    pub errors: Vec<String>,
    /// The input with schema defaults applied.
    pub value: Json,
}

/// Checks `input` against `schema` and fills in defaults.
pub fn validate(schema: &Json, input: &Json) -> ValidationResult {
    let mut errors = Vec::new();
    let value = walk(schema, input, "", &mut errors);
    ValidationResult {
        valid: errors.is_empty(),
        errors,
        value,
    }
}

/// Every JSON Schema keyword this validator enforces.
///
/// Anything outside this list is not checked, so [`crate::bundle`] refuses a bundle that uses one:
/// a constraint that is declared and silently unenforced is worse than one that is absent, because
/// a reader of `core/agent` would believe it holds everywhere.
pub const SUPPORTED_KEYWORDS: &[&str] = &[
    "$comment",
    "additionalProperties",
    "const",
    "default",
    "description",
    "enum",
    "items",
    "maxItems",
    "maxLength",
    "maximum",
    "minItems",
    "minLength",
    "minimum",
    "properties",
    "required",
    "title",
    "type",
];

fn walk(schema: &Json, input: &Json, path: &str, errors: &mut Vec<String>) -> Json {
    let at = if path.is_empty() { "(root)" } else { path };

    if let Some(expected) = schema.get("const")
        && input != expected
    {
        errors.push(format!("{at}: must be {}", expected.serialize()));
        return input.clone();
    }

    if let Some(Json::Array(options)) = schema.get("enum")
        && !options.contains(input)
    {
        errors.push(format!(
            "{at}: must be one of {}",
            options
                .iter()
                .map(Json::serialize)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return input.clone();
    }

    let types = declared_types(schema);
    if !types.is_empty() && !types.iter().any(|kind| matches_type(kind, input)) {
        errors.push(format!(
            "{at}: expected {}, got {}",
            types.join(" or "),
            describe(input)
        ));
        return input.clone();
    }

    match input {
        Json::String(value) => check_string(schema, value, at, errors),
        Json::Number(value) => check_number(schema, *value, at, errors),
        Json::Array(items) => return check_array(schema, items, path, at, errors),
        Json::Object(object) => return check_object(schema, object, path, at, errors),
        _ => {}
    }

    input.clone()
}

fn check_string(schema: &Json, value: &str, at: &str, errors: &mut Vec<String>) {
    // JSON Schema counts code points, so a value containing emoji is measured the way a conforming
    // validator measures it and every binding agrees.
    let length = value.chars().count() as f64;
    if let Some(minimum) = schema.get("minLength").and_then(Json::as_f64)
        && length < minimum
    {
        errors.push(format!(
            "{at}: must be at least {} characters",
            Json::from(minimum).serialize()
        ));
    }
    if let Some(maximum) = schema.get("maxLength").and_then(Json::as_f64)
        && length > maximum
    {
        errors.push(format!(
            "{at}: must be at most {} characters",
            Json::from(maximum).serialize()
        ));
    }
}

fn check_number(schema: &Json, value: f64, at: &str, errors: &mut Vec<String>) {
    if let Some(minimum) = schema.get("minimum").and_then(Json::as_f64)
        && value < minimum
    {
        errors.push(format!(
            "{at}: must be >= {}",
            Json::from(minimum).serialize()
        ));
    }
    if let Some(maximum) = schema.get("maximum").and_then(Json::as_f64)
        && value > maximum
    {
        errors.push(format!(
            "{at}: must be <= {}",
            Json::from(maximum).serialize()
        ));
    }
}

fn check_array(
    schema: &Json,
    items: &[Json],
    path: &str,
    at: &str,
    errors: &mut Vec<String>,
) -> Json {
    if let Some(minimum) = schema.get("minItems").and_then(Json::as_usize)
        && items.len() < minimum
    {
        errors.push(format!(
            "{at}: needs at least {minimum} item{}",
            if minimum == 1 { "" } else { "s" }
        ));
    }
    if let Some(maximum) = schema.get("maxItems").and_then(Json::as_usize)
        && items.len() > maximum
    {
        errors.push(format!("{at}: allows at most {maximum} items"));
    }

    match schema.get("items") {
        Some(item_schema) => Json::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| walk(item_schema, item, &format!("{path}[{index}]"), errors))
                .collect(),
        ),
        None => Json::Array(items.to_vec()),
    }
}

fn check_object(
    schema: &Json,
    object: &JsonObject,
    path: &str,
    at: &str,
    errors: &mut Vec<String>,
) -> Json {
    let empty = JsonObject::new();
    let properties = schema
        .get("properties")
        .and_then(Json::as_object)
        .unwrap_or(&empty);
    let mut output = JsonObject::new();

    for key in schema
        .get("required")
        .map(Json::array_or_empty)
        .unwrap_or(&[])
    {
        if let Some(key) = key.as_str()
            && !object.contains_key(key)
        {
            errors.push(format!("{at}: missing required property \"{key}\""));
        }
    }

    let additional_allowed = schema.get("additionalProperties") != Some(&Json::Bool(false));
    for (key, raw) in object.iter() {
        match properties.get(key) {
            Some(child) => {
                let child_path = if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                };
                output.insert(key, walk(child, raw, &child_path, errors));
            }
            None if additional_allowed => output.insert(key, raw.clone()),
            None => {
                let known: Vec<&str> = properties.keys().collect();
                errors.push(format!(
                    "{at}: unexpected property \"{key}\"{}",
                    if known.is_empty() {
                        String::new()
                    } else {
                        format!("; allowed: {}", known.join(", "))
                    }
                ));
            }
        }
    }

    for (key, child) in properties.iter() {
        if !output.contains_key(key)
            && let Some(default) = child.get("default")
        {
            output.insert(key, default.clone());
        }
    }

    Json::Object(output)
}

fn declared_types(schema: &Json) -> Vec<&str> {
    match schema.get("type") {
        Some(Json::String(kind)) => vec![kind.as_str()],
        Some(Json::Array(kinds)) => kinds.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    }
}

fn matches_type(kind: &str, value: &Json) -> bool {
    match kind {
        "string" => matches!(value, Json::String(_)),
        "number" => matches!(value, Json::Number(number) if number.is_finite()),
        "integer" => {
            matches!(value, Json::Number(number) if number.is_finite() && number.fract() == 0.0)
        }
        "boolean" => matches!(value, Json::Bool(_)),
        "array" => matches!(value, Json::Array(_)),
        "object" => matches!(value, Json::Object(_)),
        "null" => matches!(value, Json::Null),
        _ => true,
    }
}

fn describe(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_object;

    #[test]
    fn measures_string_length_in_code_points_as_json_schema_specifies() {
        // 31 emoji: 31 code points, but 62 UTF-16 units. Every binding must agree with a conforming
        // validator, so all of them count scalars.
        let value = Json::from("🎧".repeat(31));
        let schema = Json::Object(json_object! { "type" => "string", "maxLength" => 60.0 });
        assert!(validate(&schema, &value).valid);
    }

    #[test]
    fn still_enforces_the_limit_on_ordinary_text() {
        let too_long = Json::Object(json_object! { "type" => "string", "maxLength" => 3.0 });
        assert!(!validate(&too_long, &Json::from("abcd")).valid);

        let too_short = Json::Object(json_object! { "type" => "string", "minLength" => 2.0 });
        assert!(!validate(&too_short, &Json::from("a")).valid);
    }

    #[test]
    fn fills_in_defaults_so_a_handler_never_sees_a_half_populated_object() {
        let schema = Json::Object(json_object! {
            "type" => "object",
            "properties" => Json::Object(json_object! {
                "limit" => Json::Object(json_object! { "type" => "integer", "default" => 5.0 }),
            }),
        });
        let result = validate(&schema, &Json::object());
        assert!(result.valid);
        assert_eq!(result.value.get("limit").and_then(Json::as_i64), Some(5));
    }

    #[test]
    fn names_the_offending_path() {
        let schema = Json::Object(json_object! {
            "type" => "object",
            "additionalProperties" => false,
            "required" => vec!["operations"],
            "properties" => Json::Object(json_object! {
                "operations" => Json::Object(json_object! { "type" => "array" }),
            }),
        });
        let result = validate(&schema, &Json::Object(json_object! { "nope" => 1.0 }));
        assert!(!result.valid);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("\"operations\""))
        );
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("unexpected property \"nope\""))
        );
    }
}
