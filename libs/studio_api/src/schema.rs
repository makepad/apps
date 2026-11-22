//! The JSON-schema subset the tool table is written in, and the checker
//! that holds every call to it.
//!
//! Supported keywords: `type` (one name or a list: `object`, `array`,
//! `string`, `number`, `integer`, `boolean`, `null`), `properties`,
//! `required`, `additionalProperties` (a boolean; unknown fields are refused
//! unless it is `true`), `items`, `enum`, `minimum`, `exclusiveMinimum` (a
//! number: the value must be above it), `maximum`, `minItems`,
//! `minLength`, `anyOf`, and the annotations `description`, `default`,
//! `examples` and `title`. Anything else in a schema is a table bug
//! ([`check_schema`] says so in the tests).
//!
//! Errors name the path of the value (`ops[2].start`), what was expected
//! and what came, in words a model can act on, and every problem is
//! reported, not just the first.

use makepad_strict_json::{self as json, Value};

/// Deep enough for any argument a tool takes (edits nest keys in lists in
/// objects); the table's own schemas are shallower.
pub const MAX_DEPTH: u32 = 64;

pub fn parse(text: &str) -> Result<Value, String> {
    json::parse_depth(text.trim().as_bytes(), MAX_DEPTH).map_err(|error| format!("not valid JSON ({error})"))
}

/// A value's kind in the words the errors use.
pub fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Int(_) | Value::F64(_) => "a number",
        Value::Str(_) => "a string",
        Value::Arr(_) => "an array",
        Value::Obj(_) => "an object",
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(value) => Some(*value as f64),
        Value::F64(value) => Some(*value),
        _ => None,
    }
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => matches!(value, Value::Obj(_)),
        "array" => matches!(value, Value::Arr(_)),
        "string" => matches!(value, Value::Str(_)),
        "number" => matches!(value, Value::Int(_) | Value::F64(_)),
        "integer" => match value {
            Value::Int(_) => true,
            Value::F64(value) => value.fract() == 0.0,
            _ => false,
        },
        "boolean" => matches!(value, Value::Bool(_)),
        "null" => matches!(value, Value::Null),
        _ => false,
    }
}

fn type_names(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::Str(name)) => vec![name.as_str()],
        Some(Value::Arr(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// A short preview of a value for an error line.
fn preview(value: &Value) -> String {
    let text = value.to_json();
    if text.chars().count() > 48 {
        format!("{}…", text.chars().take(47).collect::<String>())
    } else {
        text
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_string() } else { format!("{path}.{key}") }
}

/// Check `value` against `schema`; `Err` holds one line per problem.
pub fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check(schema, value, "", &mut errors);
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn check(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let at = if path.is_empty() { "the arguments".to_string() } else { format!("`{path}`") };
    if let Some(Value::Arr(options)) = schema.get("anyOf") {
        let fits = options.iter().any(|option| {
            let mut scratch = Vec::new();
            check(option, value, path, &mut scratch);
            scratch.is_empty()
        });
        if !fits {
            let kinds: Vec<String> = options.iter().map(|option| {
                option.get("description").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| type_names(option).join(" or "))
            }).collect();
            errors.push(format!("{at}: {} fits none of the accepted forms ({})", preview(value), kinds.join("; ")));
        }
        return;
    }
    let types = type_names(schema);
    if !types.is_empty() && !types.iter().any(|name| type_matches(name, value)) {
        let expected = types.iter().map(|name| match *name {
            "integer" => "a whole number",
            "number" => "a number",
            "string" => "a string",
            "boolean" => "true or false",
            "array" => "an array",
            "object" => "an object",
            _ => "null",
        }).collect::<Vec<_>>().join(" or ");
        let hint = match (types.first().copied(), value) {
            (Some("number" | "integer"), Value::Str(text)) if text.trim().trim_end_matches('s').trim().parse::<f64>().is_ok() => {
                format!(" — write it without quotes: {}", text.trim().trim_end_matches('s').trim())
            }
            (Some("integer"), Value::F64(number)) => format!(" — round it: {}", number.round()),
            (Some("array"), Value::Str(text)) if text.trim_start().starts_with('[') => " — pass the array itself, not a string holding it".to_string(),
            (Some("object"), Value::Str(text)) if text.trim_start().starts_with('{') => " — pass the object itself, not a string holding it".to_string(),
            _ => String::new(),
        };
        errors.push(format!("{at}: expected {expected}, got {} ({}){hint}", kind_of(value), preview(value)));
        return;
    }
    if let Some(Value::Arr(allowed)) = schema.get("enum") {
        if !allowed.iter().any(|option| option == value) {
            let options = allowed.iter().map(Value::to_json).collect::<Vec<_>>().join(", ");
            errors.push(format!("{at}: {} is not one of {options}", preview(value)));
        }
    }
    if let Some(number) = number(value) {
        if let Some(minimum) = schema.get("minimum").and_then(self::number) {
            if number < minimum {
                errors.push(format!("{at}: {number} is below the minimum {minimum}"));
            }
        }
        if let Some(bound) = schema.get("exclusiveMinimum").and_then(self::number) {
            if number <= bound {
                errors.push(format!("{at}: {number} must be above {bound}"));
            }
        }
        if let Some(maximum) = schema.get("maximum").and_then(self::number) {
            if number > maximum {
                errors.push(format!("{at}: {number} is above the maximum {maximum}"));
            }
        }
    }
    match value {
        Value::Str(text) => {
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
                if (text.chars().count() as u64) < min {
                    errors.push(if min == 1 { format!("{at}: must not be empty") } else { format!("{at}: must be at least {min} characters") });
                }
            }
        }
        Value::Arr(items) => {
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min {
                    errors.push(format!("{at}: needs at least {min} item{}", if min == 1 { "" } else { "s" }));
                }
            }
            if let Some(item) = schema.get("items") {
                for (index, value) in items.iter().enumerate() {
                    check(item, value, &format!("{}[{index}]", if path.is_empty() { "" } else { path }), errors);
                }
            }
        }
        Value::Obj(fields) => {
            let properties = match schema.get("properties") {
                Some(Value::Obj(properties)) => properties.as_slice(),
                _ => &[],
            };
            if let Some(Value::Arr(required)) = schema.get("required") {
                for name in required.iter().filter_map(Value::as_str) {
                    if !fields.iter().any(|(key, value)| key == name && !value.is_null()) {
                        let what = properties.iter().find(|(key, _)| key == name)
                            .and_then(|(_, property)| property.get("description").and_then(Value::as_str))
                            .map(|description| format!(" ({description})")).unwrap_or_default();
                        errors.push(format!("`{}` is missing{what}", join(path, name)));
                    }
                }
            }
            let open = schema.get("additionalProperties").and_then(Value::as_bool) == Some(true);
            for (key, value) in fields {
                match properties.iter().find(|(name, _)| name == key) {
                    Some((_, property)) => {
                        // An optional field sent as null counts as absent.
                        if !value.is_null() || type_names(property).contains(&"null") {
                            check(property, value, &join(path, key), errors);
                        }
                    }
                    None if !open => {
                        let known = properties.iter().map(|(name, _)| format!("`{name}`")).collect::<Vec<_>>();
                        let near = properties.iter().map(|(name, _)| name.as_str()).find(|name| close(name, key));
                        let hint = match near {
                            Some(name) => format!(" — did you mean `{name}`?"),
                            None if known.is_empty() => " — this tool takes no arguments".to_string(),
                            None => format!(" — the fields are {}", known.join(", ")),
                        };
                        errors.push(format!("`{}` is not a field here{hint}", join(path, key)));
                    }
                    None => {}
                }
            }
        }
        _ => {}
    }
}

/// Two names a model may confuse: same letters but one, or one contains the
/// other.
fn close(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    // Two neighbours swapped (`strat` for `start`).
    if a.len() == b.len() {
        let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        if differ.len() == 2 && differ[1] == differ[0] + 1 && a[differ[0]] == b[differ[1]] && a[differ[1]] == b[differ[0]] {
            return true;
        }
    }
    // Levenshtein distance ≤ 1, in one pass.
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

const KEYWORDS: &[&str] = &[
    "type", "properties", "required", "additionalProperties", "items", "enum", "minimum", "exclusiveMinimum", "maximum", "minItems", "minLength", "anyOf",
    "description", "default", "examples", "title",
];

/// A schema of the table is well formed: known keywords and types only,
/// `required` names declared properties, nested schemas are schemas.
pub fn check_schema(schema: &Value) -> Result<(), String> {
    let Value::Obj(fields) = schema else { return Err("a schema must be an object".into()) };
    for (key, value) in fields {
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(format!("unknown keyword `{key}`"));
        }
        match key.as_str() {
            "type" => {
                let names = type_names(schema);
                if names.is_empty() || names.iter().any(|name| !["object", "array", "string", "number", "integer", "boolean", "null"].contains(name)) {
                    return Err(format!("bad type {}", value.to_json()));
                }
            }
            "properties" => {
                let Value::Obj(properties) = value else { return Err("`properties` must be an object".into()) };
                for (name, property) in properties {
                    check_schema(property).map_err(|error| format!("{name}: {error}"))?;
                    if property.get("description").is_none() {
                        return Err(format!("{name}: every property needs a description for the model"));
                    }
                }
            }
            "required" => {
                let Value::Arr(names) = value else { return Err("`required` must be an array".into()) };
                for name in names {
                    let name = name.as_str().ok_or("`required` holds names")?;
                    if schema.get("properties").and_then(|properties| properties.get(name)).is_none() {
                        return Err(format!("`{name}` is required but not a property"));
                    }
                }
            }
            "items" => check_schema(value).map_err(|error| format!("items: {error}"))?,
            // The draft-6 form (a number), never draft-4's boolean beside `minimum`.
            "exclusiveMinimum" => {
                if number(value).is_none() {
                    return Err(format!("`exclusiveMinimum` must be a number, not {}", value.to_json()));
                }
            }
            "anyOf" => {
                let Value::Arr(options) = value else { return Err("`anyOf` must be an array".into()) };
                for option in options {
                    check_schema(option)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Arguments that came as text pairs (a text tool protocol writes every
/// value as raw text): strings stay as they are, and a value whose schema
/// wants a number, boolean, array or object is read as JSON (a number may
/// carry an `s` for seconds). What does not read stays a string, so the
/// checker reports it with the field's name.
pub fn from_text_pairs(schema: &Value, pairs: &[(String, String)]) -> Value {
    let properties = match schema.get("properties") {
        Some(Value::Obj(properties)) => properties.as_slice(),
        _ => &[],
    };
    Value::Obj(pairs.iter().map(|(key, text)| {
        let wants = properties.iter().find(|(name, _)| name == key).map(|(_, property)| type_names(property)).unwrap_or_default();
        let value = if wants.is_empty() || wants.contains(&"string") {
            Value::Str(text.clone())
        } else {
            let trimmed = text.trim();
            let numeric = trimmed.strip_suffix('s').map(str::trim).filter(|_| wants.contains(&"number")).unwrap_or(trimmed);
            parse(numeric).unwrap_or_else(|_| Value::Str(text.clone()))
        };
        (key.clone(), value)
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        parse(r#"{"type": "object", "properties": {
            "start": {"type": "number", "minimum": 0, "description": "seconds"},
            "length": {"type": "number", "exclusiveMinimum": 0, "description": "seconds"},
            "track": {"type": "integer", "description": "track"},
            "kind": {"type": "string", "enum": ["video", "audio"], "description": "kind"},
            "keys": {"type": "array", "minItems": 1, "items": {"type": "array", "items": {"type": ["number", "string"], "description": "t, v or ease"}, "description": "a key"}, "description": "keys"},
            "name": {"type": "string", "minLength": 1, "description": "name"}
        }, "required": ["start", "kind"]}"#).unwrap()
    }

    #[test]
    fn a_good_call_passes_and_the_schema_is_well_formed() {
        check_schema(&schema()).unwrap();
        validate(&schema(), &parse(r#"{"start": 1.5, "kind": "video", "track": 2, "keys": [[0, 1, "ease_out"]]}"#).unwrap()).unwrap();
        // An optional field sent as null is absent.
        validate(&schema(), &parse(r#"{"start": 0, "kind": "audio", "track": null}"#).unwrap()).unwrap();
    }

    #[test]
    fn every_problem_is_named_with_its_path_and_a_fix() {
        let errors = validate(&schema(), &parse(r#"{"start": "2.5s", "track": 1.5, "kind": "vid", "keys": [[0, true]], "strat": 1, "name": ""}"#).unwrap()).unwrap_err();
        let text = errors.join("\n");
        assert!(text.contains("`start`: expected a number, got a string") && text.contains("without quotes: 2.5"), "{text}");
        assert!(text.contains("`track`: expected a whole number") && text.contains("round it: 2"), "{text}");
        assert!(text.contains("`kind`: \"vid\" is not one of \"video\", \"audio\""), "{text}");
        assert!(text.contains("`keys[0][1]`"), "{text}");
        assert!(text.contains("`strat` is not a field here — did you mean `start`?"), "{text}");
        assert!(text.contains("`name`: must not be empty"), "{text}");
        let missing = validate(&schema(), &parse("{}").unwrap()).unwrap_err().join("\n");
        assert!(missing.contains("`start` is missing (seconds)") && missing.contains("`kind` is missing"), "{missing}");
        assert!(validate(&schema(), &parse(r#"{"start": -1, "kind": "video"}"#).unwrap()).unwrap_err()[0].contains("below the minimum 0"));
        assert!(validate(&schema(), &parse(r#"{"start": 0, "kind": "video", "length": 0}"#).unwrap()).unwrap_err()[0].contains("`length`: 0 must be above 0"));
        validate(&schema(), &parse(r#"{"start": 0, "kind": "video", "length": 0.5}"#).unwrap()).unwrap();
        assert!(check_schema(&parse(r#"{"type": "number", "exclusiveMinimum": true}"#).unwrap()).unwrap_err().contains("must be a number"));
    }

    #[test]
    fn a_stringified_array_is_called_out() {
        let errors = validate(&schema(), &parse(r#"{"start": 0, "kind": "video", "keys": "[[0, 1]]"}"#).unwrap()).unwrap_err();
        assert!(errors[0].contains("not a string holding it"), "{errors:?}");
    }

    #[test]
    fn bad_schemas_are_caught() {
        assert!(check_schema(&parse(r#"{"type": "object", "properties": {"a": {"type": "string"}}}"#).unwrap()).unwrap_err().contains("description"));
        assert!(check_schema(&parse(r#"{"type": "thing"}"#).unwrap()).is_err());
        assert!(check_schema(&parse(r#"{"type": "object", "required": ["x"]}"#).unwrap()).unwrap_err().contains("not a property"));
        assert!(check_schema(&parse(r#"{"format": "uri"}"#).unwrap()).unwrap_err().contains("format"));
    }

    #[test]
    fn text_pairs_read_by_the_schema() {
        let pairs = vec![("start".to_string(), "2.5s".to_string()), ("kind".to_string(), "video".to_string()), ("keys".to_string(), "[[0, 1]]".to_string()), ("name".to_string(), "12".to_string())];
        let value = from_text_pairs(&schema(), &pairs);
        assert_eq!(value.get("start"), Some(&Value::F64(2.5)));
        assert_eq!(value.get("name"), Some(&Value::Str("12".into())));
        validate(&schema(), &value).unwrap();
    }

    #[test]
    fn any_of_takes_either_form() {
        let schema = parse(r#"{"anyOf": [{"type": "number", "description": "seconds"}, {"type": "object", "properties": {"marker": {"type": "string", "description": "m"}}, "required": ["marker"], "description": "a marker"}]}"#).unwrap();
        validate(&schema, &Value::Int(3)).unwrap();
        validate(&schema, &parse(r#"{"marker": "saved"}"#).unwrap()).unwrap();
        let error = validate(&schema, &Value::Str("x".into())).unwrap_err().join("");
        assert!(error.contains("seconds; a marker"), "{error}");
    }
}
