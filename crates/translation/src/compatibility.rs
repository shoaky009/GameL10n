//! Exact System.Text.Json default encoding used by the existing identity hashes.

use serde_json::Value;

pub fn legacy_json(value: &Value) -> String {
    let mut output = String::new();
    write_value(value, &mut output);
    output
}

fn write_value(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => write_string(value, output),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_value(value, output);
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_string(key, output);
                output.push(':');
                write_value(value, output);
            }
            output.push('}');
        }
    }
}

fn write_string(value: &str, output: &mut String) {
    use std::fmt::Write;
    output.push('"');
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            ' '..='~' if !matches!(character, '"' | '&' | '\'' | '+' | '<' | '>' | '`') => {
                output.push(character)
            }
            other => {
                for code in other.encode_utf16(&mut [0_u16; 2]).iter() {
                    let _ = write!(output, "\\u{code:04X}");
                }
            }
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_system_text_json_default_encoder() {
        assert_eq!(
            legacy_json(&Value::String("a\"b+c<d>&'` 汉字😀".into())),
            "\"a\\u0022b\\u002Bc\\u003Cd\\u003E\\u0026\\u0027\\u0060 \\u6C49\\u5B57\\uD83D\\uDE00\""
        );
    }
}
