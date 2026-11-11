//! Writer that reproduces the Tabletop Simulator JSON format.
//!
//! The format is:
//!
//! - pretty-printed with two spaces per level and `": "` after keys;
//! - empty objects and arrays as `{}` and `[]`;
//! - CRLF line endings, no trailing newline, no BOM;
//! - strings escaped like Newtonsoft.Json's default: `\"`, `\\`, `\b`, `\f`,
//!   `\n`, `\r`, `\t`, and `\u` plus four lowercase hex digits for all other
//!   control characters and for U+0085, U+2028 and U+2029; everything else is
//!   written raw;
//! - numbers with their original digits (requires `arbitrary_precision`); the
//!   exponent marker is written as `E` and always carries a sign (`1E-05`,
//!   `1E+20`), as .NET does. serde_json normalizes exponents while parsing, so
//!   an input like `1e5` is written as `1E+5`.
//!
//! `serde_json`'s pretty formatter is not used because it hard-codes `\n`.

use std::io::{self, BufWriter, Write};

use serde_json::Value;

const NL: &[u8] = b"\r\n";
const SPACES: [u8; 64] = [b' '; 64];

/// Serializes any JSON value in Tabletop Simulator format.
pub fn to_tts_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_node(&mut out, value, 0).expect("writing to a Vec cannot fail");
    out
}

/// Writes any JSON value in Tabletop Simulator format to `out`, buffered.
pub fn write_value<W: Write>(value: &Value, out: W) -> io::Result<()> {
    let mut out = BufWriter::new(out);
    write_node(&mut out, value, 0)?;
    out.flush()
}

fn indent<W: Write>(out: &mut W, level: usize) -> io::Result<()> {
    let mut n = level * 2;
    while n > 0 {
        let chunk = n.min(SPACES.len());
        out.write_all(&SPACES[..chunk])?;
        n -= chunk;
    }
    Ok(())
}

fn write_node<W: Write>(out: &mut W, value: &Value, level: usize) -> io::Result<()> {
    match value {
        Value::Null => out.write_all(b"null"),
        Value::Bool(true) => out.write_all(b"true"),
        Value::Bool(false) => out.write_all(b"false"),
        Value::Number(n) => {
            // serde_json with `arbitrary_precision` keeps the digits but
            // normalizes the exponent to `e-05` / `e+20`; .NET writes
            // `E-05` / `E+20`, so only the exponent marker is upper-cased.
            let text = n.to_string();
            match text.find('e') {
                None => out.write_all(text.as_bytes()),
                Some(pos) => {
                    out.write_all(&text.as_bytes()[..pos])?;
                    out.write_all(b"E")?;
                    out.write_all(&text.as_bytes()[pos + 1..])
                }
            }
        }
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                return out.write_all(b"[]");
            }
            out.write_all(b"[")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.write_all(b",")?;
                }
                out.write_all(NL)?;
                indent(out, level + 1)?;
                write_node(out, item, level + 1)?;
            }
            out.write_all(NL)?;
            indent(out, level)?;
            out.write_all(b"]")
        }
        Value::Object(map) => {
            if map.is_empty() {
                return out.write_all(b"{}");
            }
            out.write_all(b"{")?;
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.write_all(b",")?;
                }
                out.write_all(NL)?;
                indent(out, level + 1)?;
                write_string(out, key)?;
                out.write_all(b": ")?;
                write_node(out, item, level + 1)?;
            }
            out.write_all(NL)?;
            indent(out, level)?;
            out.write_all(b"}")
        }
    }
}

enum Escape {
    Short(&'static [u8]),
    Unicode(u16),
}

/// Returns the escape for the sequence starting at `bytes[i]` and its length
/// in input bytes, if it needs escaping. The input is valid UTF-8, so lead
/// bytes are unambiguous.
fn escape_at(bytes: &[u8], i: usize) -> Option<(Escape, usize)> {
    let b = bytes[i];
    match b {
        b'"' => Some((Escape::Short(b"\\\""), 1)),
        b'\\' => Some((Escape::Short(b"\\\\"), 1)),
        0x08 => Some((Escape::Short(b"\\b"), 1)),
        0x0C => Some((Escape::Short(b"\\f"), 1)),
        b'\n' => Some((Escape::Short(b"\\n"), 1)),
        b'\r' => Some((Escape::Short(b"\\r"), 1)),
        b'\t' => Some((Escape::Short(b"\\t"), 1)),
        0..0x20 => Some((Escape::Unicode(u16::from(b)), 1)),
        // U+0085 is C2 85.
        0xC2 if bytes.get(i + 1) == Some(&0x85) => Some((Escape::Unicode(0x85), 2)),
        // U+2028 and U+2029 are E2 80 A8 and E2 80 A9.
        0xE2 if bytes.get(i + 1) == Some(&0x80) => match bytes.get(i + 2) {
            Some(0xA8) => Some((Escape::Unicode(0x2028), 3)),
            Some(0xA9) => Some((Escape::Unicode(0x2029), 3)),
            _ => None,
        },
        _ => None,
    }
}

fn write_string<W: Write>(out: &mut W, s: &str) -> io::Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = s.as_bytes();
    out.write_all(b"\"")?;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        // Cheap pre-check: only these bytes can start an escape.
        let b = bytes[i];
        if b >= 0x20 && b != b'"' && b != b'\\' && b != 0xC2 && b != 0xE2 {
            i += 1;
            continue;
        }
        match escape_at(bytes, i) {
            None => i += 1,
            Some((esc, len)) => {
                out.write_all(&bytes[start..i])?;
                match esc {
                    Escape::Short(text) => out.write_all(text)?,
                    Escape::Unicode(c) => out.write_all(&[
                        b'\\',
                        b'u',
                        HEX[usize::from(c >> 12)],
                        HEX[usize::from((c >> 8) & 0xF)],
                        HEX[usize::from((c >> 4) & 0xF)],
                        HEX[usize::from(c & 0xF)],
                    ])?,
                }
                i += len;
                start = i;
            }
        }
    }
    out.write_all(&bytes[start..])?;
    out.write_all(b"\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::{Save, SaveError};

    /// Structural equality that also compares object key order.
    fn same_structure(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && same_structure(v1, v2))
            }
            (Value::Array(x), Value::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_structure(p, q))
            }
            _ => a == b,
        }
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn out_str(v: &Value) -> String {
        String::from_utf8(to_tts_vec(v)).unwrap()
    }

    fn written_string(s: &str) -> String {
        out_str(&Value::String(s.to_owned()))
    }

    #[test]
    fn short_escapes() {
        assert_eq!(
            written_string("\"\\\u{8}\u{c}\n\r\t"),
            r#""\"\\\b\f\n\r\t""#
        );
    }

    #[test]
    fn control_chars_use_lowercase_unicode_escapes() {
        assert_eq!(
            written_string("\u{1}\u{1f}\u{b}"),
            r#""\u0001\u001f\u000b""#
        );
    }

    #[test]
    fn line_separators_and_nel_are_escaped() {
        assert_eq!(
            written_string("a\u{85}b\u{2028}c\u{2029}d"),
            r#""a\u0085b\u2028c\u2029d""#
        );
    }

    #[test]
    fn everything_else_is_raw() {
        let s = "ö日本語🎲/<>&\u{7f}\u{2027}\u{86}\u{2030}'";
        assert_eq!(written_string(s), format!("\"{s}\""));
    }

    #[test]
    fn keys_are_escaped_too() {
        let v = parse(r#"{"a\"b ":1}"#);
        assert_eq!(out_str(&v), "{\r\n  \"a\\\"b\\u2028\": 1\r\n}");
    }

    #[test]
    fn empty_containers() {
        assert_eq!(out_str(&parse("{}")), "{}");
        assert_eq!(out_str(&parse("[]")), "[]");
        assert_eq!(
            out_str(&parse(r#"{"a":{},"b":[]}"#)),
            "{\r\n  \"a\": {},\r\n  \"b\": []\r\n}"
        );
    }

    #[test]
    fn nested_indentation() {
        let v = parse(r#"{"a":[1,{"b":[true,null]}],"c":"x"}"#);
        let expected = "{\r\n  \"a\": [\r\n    1,\r\n    {\r\n      \"b\": [\r\n        true,\r\n        null\r\n      ]\r\n    }\r\n  ],\r\n  \"c\": \"x\"\r\n}";
        assert_eq!(out_str(&v), expected);
    }

    #[test]
    fn deep_indentation_exceeding_chunk() {
        let mut v = Value::Bool(false);
        for _ in 0..40 {
            v = Value::Array(vec![v]);
        }
        let s = out_str(&v);
        assert!(s.contains(&format!("\r\n{}false\r\n", " ".repeat(80))));
        assert!(same_structure(&parse(&s), &v));
    }

    #[test]
    fn number_text_is_preserved() {
        let numbers = [
            "1.0",
            "1E-05",
            "-0.0",
            "-5.039668E-05",
            "12345678901234567890123",
            "0",
            "-7",
            "2.50",
        ];
        let v = parse(&format!("[{}]", numbers.join(",")));
        let s = out_str(&v);
        for n in numbers {
            assert!(s.contains(&format!("\r\n  {n}")), "missing {n} in {s}");
        }
    }

    #[test]
    fn exponent_is_normalized_to_dotnet_style() {
        let v = parse("[1e5,2E+7,3e-2,1.5E10]");
        let s = out_str(&v);
        for n in ["1E+5", "2E+7", "3E-2", "1.5E+10"] {
            assert!(s.contains(&format!("\r\n  {n}")), "missing {n} in {s}");
        }
    }

    #[test]
    fn top_level_array_is_supported() {
        assert_eq!(out_str(&parse("[1,2]")), "[\r\n  1,\r\n  2\r\n]");
    }

    #[test]
    fn write_value_matches_to_tts_vec() {
        let v = parse(r#"{"a":[1,"x"]}"#);
        let mut buf = Vec::new();
        write_value(&v, &mut buf).unwrap();
        assert_eq!(buf, to_tts_vec(&v));
    }

    #[test]
    fn third_party_input_is_normalized_and_idempotent() {
        let src = "{\n  \"z\": \"\\u003cb\\u003e \\u00f6 \\ud83c\\udfb2\",\n  \"a\": 1E-05,\n  \"m\": {\"y\": [1.0, -0.0], \"b\": null}\n}\n";
        let first = Save::from_slice(src.as_bytes()).unwrap();
        let out = first.to_vec();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(text.contains("\"<b> ö 🎲\""));
        assert!(text.contains("1E-05"));
        assert!(text.contains("-0.0"));
        assert!(text.contains("\r\n"));
        assert!(!text.ends_with('\n'));

        let second = Save::from_slice(&out).unwrap();
        assert!(same_structure(first.value(), second.value()));
        let keys: Vec<_> = second.value().as_object().unwrap().keys().collect();
        assert_eq!(keys, ["z", "a", "m"]);
        assert_eq!(second.to_vec(), out);
    }

    #[test]
    fn bom_is_accepted_and_dropped() {
        let mut src = b"\xEF\xBB\xBF".to_vec();
        src.extend_from_slice(b"{\"a\": 1}");
        let save = Save::from_slice(&src).unwrap();
        assert_eq!(save.to_vec(), b"{\r\n  \"a\": 1\r\n}");
    }

    #[test]
    fn non_object_top_level_is_rejected() {
        assert!(matches!(
            Save::from_slice(b"[1]"),
            Err(SaveError::NotAnObject)
        ));
        assert!(matches!("3".parse::<Save>(), Err(SaveError::NotAnObject)));
        assert!(matches!(
            Save::try_from(Value::Null),
            Err(SaveError::NotAnObject)
        ));
        assert!(matches!(Save::from_slice(b"{"), Err(SaveError::Json(_))));
    }

    #[test]
    fn write_to_matches_to_vec() {
        let save: Save = r#"{"a":[1]}"#.parse().unwrap();
        let mut buf = Vec::new();
        save.write_to(&mut buf).unwrap();
        assert_eq!(buf, save.to_vec());
    }
}
