//! RFC 8785 JSON Canonicalization Scheme over `serde_json::Value`: object keys sorted by
//! UTF-16 code units, no whitespace, strings escaped as ECMAScript `JSON.stringify` does,
//! numbers as IEEE-754 doubles printed by ECMAScript `Number.prototype.toString`
//! (shortest round-trip digits; exponent form below 1e-6 and from 1e21).

use serde_json::Value;

/// A value JCS cannot represent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not canonicalisable: {0}")]
pub struct JcsError(pub &'static str);

/// The canonical bytes of `v`.
pub fn canonicalize(v: &Value) -> Result<Vec<u8>, JcsError> {
    let mut out = String::new();
    write(v, &mut out)?;
    Ok(out.into_bytes())
}

fn write(v: &Value, out: &mut String) -> Result<(), JcsError> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            // JCS numbers are IEEE-754 doubles: integers beyond 2^53 round, as in JS.
            let f = n.as_f64().ok_or(JcsError("number"))?;
            out.push_str(&number(f)?);
        }
        Value::String(s) => string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out)?;
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(k, out);
                out.push(':');
                write(m.get(*k).ok_or(JcsError("key"))?, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number::toString(x)` for a finite double.
pub fn number(x: f64) -> Result<String, JcsError> {
    if !x.is_finite() {
        return Err(JcsError("NaN or infinity"));
    }
    if x == 0.0 {
        return Ok("0".to_owned()); // also −0
    }
    // Rust's `{:e}` gives the shortest round-trip digits: "d.ddde±x".
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').ok_or(JcsError("format"))?;
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let e: i32 = exp.parse().map_err(|_| JcsError("format"))?;
    let k = i32::try_from(digits.len()).map_err(|_| JcsError("format"))?;
    let n = e + 1; // value = 0.digits × 10^n
    let mut s = String::new();
    if x < 0.0 {
        s.push('-');
    }
    if k <= n && n <= 21 {
        s.push_str(&digits);
        s.push_str(&"0".repeat((n - k) as usize));
    } else if 0 < n && n <= 21 {
        let (a, b) = digits.split_at(n as usize);
        s.push_str(a);
        s.push('.');
        s.push_str(b);
    } else if -6 < n && n <= 0 {
        s.push_str("0.");
        s.push_str(&"0".repeat((-n) as usize));
        s.push_str(&digits);
    } else {
        let (a, b) = digits.split_at(1);
        s.push_str(a);
        if !b.is_empty() {
            s.push('.');
            s.push_str(b);
        }
        s.push('e');
        s.push(if n > 0 { '+' } else { '-' });
        s.push_str(&(n - 1).abs().to_string());
    }
    Ok(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_print_as_ecmascript() {
        // RFC 8785 Appendix B samples.
        let cases: [(u64, &str); 13] = [
            (0x0000_0000_0000_0000, "0"),
            (0x8000_0000_0000_0000, "0"),
            (0x0000_0000_0000_0001, "5e-324"),
            (0x8000_0000_0000_0001, "-5e-324"),
            (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
            (0x4340_0000_0000_0000, "9007199254740992"),
            (0x4430_0000_0000_0000, "295147905179352830000"),
            (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
            (0x44b5_2d02_c7e1_4af6, "1e+23"),
            (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
            (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
            (0x41b3_de43_5555_5553, "333333333.3333332"),
            (0x4415_af1d_78b5_8c3f, "99999999999999980000"), // checked with Node
        ];
        for (bits, want) in cases {
            assert_eq!(number(f64::from_bits(bits)).unwrap(), want, "{bits:#x}");
        }
        assert_eq!(number(0.5).unwrap(), "0.5");
        assert_eq!(number(-1.25).unwrap(), "-1.25");
        assert_eq!(number(86.0).unwrap(), "86");
        assert!(number(f64::NAN).is_err());
    }

    #[test]
    fn objects_sort_by_utf16_and_strings_escape() {
        // RFC 8785 §3.2.3's sorting example (UTF-16 order puts U+1F600 before U+FB33).
        let v = json!({ "\u{20ac}": "Euro Sign", "\r": "Carriage Return", "\u{fb33}": "Hebrew Letter Dalet With Dagesh",
                         "1": "One", "\u{1f600}": "Emoji: Grinning Face", "\u{80}": "Control", "\u{f6}": "Latin Small Letter O With Diaeresis" });
        let s = String::from_utf8(canonicalize(&v).unwrap()).unwrap();
        let order: Vec<&str> = [
            "Carriage Return",
            "One",
            "Control",
            "Latin Small",
            "Euro Sign",
            "Emoji",
            "Hebrew",
        ]
        .to_vec();
        let mut at = 0;
        for o in order {
            let p = s[at..]
                .find(o)
                .unwrap_or_else(|| panic!("{o} out of order in {s}"));
            at += p;
        }
        let v =
            json!({ "b": [1, 2.5, "x\u{1}\n\"\\", null, true], "a": { "d": 1e21, "c": 0.000001 } });
        assert_eq!(
            String::from_utf8(canonicalize(&v).unwrap()).unwrap(),
            r#"{"a":{"c":0.000001,"d":1e+21},"b":[1,2.5,"x\u0001\n\"\\",null,true]}"#
        );
    }
}
