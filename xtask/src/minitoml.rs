//! A small TOML-subset reader for the repo's own data files (`test-media/manifest.toml`,
//! `test-media/stream-sources.toml`), so xtask needs no new crate (SR R-SUP-1).
//!
//! Supported: `#` comments, `key = value` with bare or quoted keys, `[table]` and
//! `[[array.of.tables]]` headers with dotted names, basic strings ("…" with `\" \\ \n \t
//! \uXXXX` escapes), literal strings ('…'), integers (with `_` separators and a sign),
//! floats, booleans, and arrays of those (which may span lines and nest). Anything else
//! (inline tables, dates, multi-line strings, dotted keys) is an error naming the line,
//! so a file that parses here means the same thing to a full TOML parser.

use std::collections::BTreeMap;

/// A parsed value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Table),
}

/// Keys in sorted order (TOML tables are unordered).
pub type Table = BTreeMap<String, Value>;

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
}

/// Parse a document into its root table.
pub fn parse(src: &str) -> Result<Table, String> {
    let mut root = Table::new();
    // Tables defined by a `[header]`, and arrays created by `[[header]]`, keyed by their
    // path with array indices, so a repeat `[t]` or a `[[a]]` onto `a = [...]` fails
    // the way it does in a full TOML parser.
    let mut defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut aot: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Path of the current table; the bool marks "last element of an array of tables".
    let mut current: Vec<(String, bool)> = Vec::new();
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        line: 1,
    };
    loop {
        p.skip_ws_comments_newlines();
        if p.eof() {
            break;
        }
        if p.peek() == Some(b'[') {
            let array = p.s.get(p.i + 1) == Some(&b'[');
            p.i += if array { 2 } else { 1 };
            let path = p.key_path()?;
            p.skip_ws();
            let close: &[u8] = if array { b"]]" } else { b"]" };
            if !p.s[p.i..].starts_with(close) {
                return Err(p.err("expected ']' closing the table header"));
            }
            p.i += close.len();
            p.end_of_line()?;
            if array {
                // The array itself, indexed only through its parents.
                let (last, parents) = path.split_last().ok_or_else(|| p.err("empty header"))?;
                let key = format!("{}.{last}", path_key(&root, parents));
                if root_has(&root, &path) && !aot.contains(&key) {
                    return Err(p.err("[[array]] onto a key that is not an array of tables"));
                }
                aot.insert(key);
            } else if !defined.insert(path_key(&root, &path)) {
                return Err(p.err("table defined twice"));
            }
            open_table(&mut root, &path, array).map_err(|e| p.err(&e))?;
            current = path
                .iter()
                .enumerate()
                .map(|(n, k)| (k.clone(), array && n + 1 == path.len()))
                .collect();
            continue;
        }
        let key = p.key()?;
        p.skip_ws();
        if p.peek() == Some(b'.') {
            return Err(p.err("dotted keys are not supported"));
        }
        if p.peek() != Some(b'=') {
            return Err(p.err("expected '='"));
        }
        p.i += 1;
        p.skip_ws();
        let value = p.value()?;
        p.end_of_line()?;
        let table = resolve(&mut root, &current).map_err(|e| p.err(&e))?;
        if table.insert(key.clone(), value).is_some() {
            return Err(p.err(&format!("duplicate key '{key}'")));
        }
    }
    Ok(root)
}

/// `a.b[3].c`-style key for a header path, indexing arrays of tables at their last
/// element (where the header lands).
fn path_key(root: &Table, path: &[String]) -> String {
    let mut key = String::new();
    let mut t = Some(root);
    for k in path {
        key.push('.');
        key.push_str(k);
        let next = t.and_then(|t| t.get(k));
        t = match next {
            Some(Value::Table(x)) => Some(x),
            Some(Value::Array(a)) => {
                key.push_str(&format!("[{}]", a.len().saturating_sub(1)));
                match a.last() {
                    Some(Value::Table(x)) => Some(x),
                    _ => None,
                }
            }
            _ => None,
        };
    }
    key
}

fn root_has(root: &Table, path: &[String]) -> bool {
    let mut t = root;
    for (i, k) in path.iter().enumerate() {
        match t.get(k) {
            None => return false,
            Some(_) if i + 1 == path.len() => return true,
            Some(Value::Table(x)) => t = x,
            Some(Value::Array(a)) => match a.last() {
                Some(Value::Table(x)) => t = x,
                _ => return false,
            },
            Some(_) => return false,
        }
    }
    false
}

fn open_table(root: &mut Table, path: &[String], array: bool) -> Result<(), String> {
    let (last, parents) = path.split_last().ok_or("empty table header")?;
    let mut t = root;
    for k in parents {
        t = descend(t, k)?;
    }
    if array {
        let entry = t
            .entry(last.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        match entry {
            Value::Array(a) => a.push(Value::Table(Table::new())),
            _ => return Err(format!("'{last}' is not an array of tables")),
        }
    } else {
        match t.get(last) {
            None => {
                t.insert(last.clone(), Value::Table(Table::new()));
            }
            Some(Value::Table(_)) => {}
            Some(_) => return Err(format!("'{last}' is already a value")),
        }
    }
    Ok(())
}

fn descend<'a>(t: &'a mut Table, k: &str) -> Result<&'a mut Table, String> {
    let v = t
        .entry(k.to_owned())
        .or_insert_with(|| Value::Table(Table::new()));
    match v {
        Value::Table(t) => Ok(t),
        Value::Array(a) => match a.last_mut() {
            Some(Value::Table(t)) => Ok(t),
            _ => Err(format!("'{k}' is not a table")),
        },
        _ => Err(format!("'{k}' is not a table")),
    }
}

fn resolve<'a>(root: &'a mut Table, path: &[(String, bool)]) -> Result<&'a mut Table, String> {
    let mut t = root;
    for (k, _) in path {
        t = descend(t, k)?;
    }
    Ok(t)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    line: usize,
}

impl Parser<'_> {
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn err(&self, msg: &str) -> String {
        format!("line {}: {msg}", self.line)
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }
    fn skip_comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }
    fn skip_ws_comments_newlines(&mut self) {
        loop {
            self.skip_ws();
            self.skip_comment();
            match self.peek() {
                Some(b'\n') => {
                    self.i += 1;
                    self.line += 1;
                }
                Some(b'\r') => self.i += 1,
                _ => break,
            }
        }
    }
    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_ws();
        self.skip_comment();
        match self.peek() {
            None | Some(b'\n') | Some(b'\r') => Ok(()),
            _ => Err(self.err("unexpected text after value")),
        }
    }
    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut path = Vec::new();
        loop {
            self.skip_ws();
            path.push(self.key()?);
            self.skip_ws();
            if self.peek() == Some(b'.') {
                self.i += 1;
            } else {
                return Ok(path);
            }
        }
    }
    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some(b'"') => self.basic_string(),
            Some(b'\'') => self.literal_string(),
            _ => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                {
                    self.i += 1;
                }
                if start == self.i {
                    return Err(self.err("expected a key"));
                }
                Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
            }
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(b'"') => {
                if self.s[self.i..].starts_with(b"\"\"\"") {
                    return Err(self.err("multi-line strings are not supported"));
                }
                self.basic_string().map(Value::Str)
            }
            Some(b'\'') => {
                if self.s[self.i..].starts_with(b"'''") {
                    return Err(self.err("multi-line strings are not supported"));
                }
                self.literal_string().map(Value::Str)
            }
            Some(b'[') => self.array(),
            Some(b'{') => Err(self.err("inline tables are not supported")),
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Value::Bool(false))
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' => self.number(),
            _ => Err(self.err("expected a value")),
        }
    }
    fn array(&mut self) -> Result<Value, String> {
        self.i += 1; // '['
        let mut items = Vec::new();
        loop {
            self.skip_ws_comments_newlines();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(Value::Array(items));
            }
            items.push(self.value()?);
            self.skip_ws_comments_newlines();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {}
                _ => return Err(self.err("expected ',' or ']' in array")),
            }
        }
    }
    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+' | b'.'))
        {
            self.i += 1;
        }
        let raw = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        if !number_syntax(&raw) {
            return Err(self.err(&format!("bad number '{raw}'")));
        }
        let text: String = raw.chars().filter(|&c| c != '_').collect();
        if text.contains(['.', 'e', 'E']) && !text.starts_with("0x") {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| self.err(&format!("bad float '{text}'")))
        } else {
            text.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| self.err(&format!("bad integer '{text}' (hex/dates unsupported)")))
        }
    }
    fn literal_string(&mut self) -> Result<String, String> {
        self.i += 1;
        let start = self.i;
        while !matches!(self.peek(), None | Some(b'\'') | Some(b'\n')) {
            self.i += 1;
        }
        if self.peek() != Some(b'\'') {
            return Err(self.err("unterminated literal string"));
        }
        let s = std::str::from_utf8(&self.s[start..self.i])
            .map_err(|_| self.err("invalid UTF-8"))?
            .to_owned();
        self.i += 1;
        Ok(s)
    }
    fn basic_string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => {
                    let esc = self.s.get(self.i + 1).copied();
                    self.i += 2;
                    match esc {
                        Some(b'"') => out.push(b'"'),
                        Some(b'\\') => out.push(b'\\'),
                        Some(b'n') => out.push(b'\n'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b'u') => {
                            let hex = self
                                .s
                                .get(self.i..self.i + 4)
                                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .and_then(char::from_u32)
                                .ok_or_else(|| self.err("bad \\u escape"))?;
                            self.i += 4;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(hex.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.err("unsupported escape")),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"))
    }
}

/// TOML integer/float syntax: digits with single `_` between digits, no leading zero
/// (except `0` itself), a fraction needs digits on both sides, optional exponent.
fn number_syntax(t: &str) -> bool {
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    let digits = |s: &str, leading_zero_ok: bool| {
        !s.is_empty()
            && !s.starts_with('_')
            && !s.ends_with('_')
            && !s.contains("__")
            && s.chars().all(|c| c.is_ascii_digit() || c == '_')
            && (leading_zero_ok || s == "0" || !s.starts_with('0'))
    };
    let (mantissa, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['+', '-']).unwrap_or(e))),
        None => (body, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((i, f)) => digits(i, false) && digits(f, true),
        None => digits(mantissa, false),
    };
    mantissa_ok && exp.is_none_or(|e| digits(e, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_subset() {
        let doc = r#"
# comment
schema = "x/0"   # trailing
n = -1_642
f = 0.118
yes = true
list = [1, 2,
  3, # c
]
[corpus]
ffmpeg = '8.1.2'
[[file]]
id = "a"
"quoted key" = "éé\"q\""
[[file]]
id = "b"
[file.extra]
k = [[1, 2], ["x"]]
"#;
        let t = parse(doc).unwrap();
        assert_eq!(t["schema"].as_str(), Some("x/0"));
        assert_eq!(t["n"].as_int(), Some(-1642));
        assert_eq!(t["f"].as_float(), Some(0.118));
        assert_eq!(t["yes"], Value::Bool(true));
        assert_eq!(t["list"].as_array().unwrap().len(), 3);
        assert_eq!(
            t["corpus"].as_table().unwrap()["ffmpeg"].as_str(),
            Some("8.1.2")
        );
        let files = t["file"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].as_table().unwrap()["quoted key"].as_str(),
            Some("éé\"q\"")
        );
        let extra = files[1].as_table().unwrap()["extra"].as_table().unwrap();
        assert_eq!(extra["k"].as_array().unwrap().len(), 2);
        // the same sub-table under two array elements is fine
        assert!(parse("[[f]]\n[f.x]\na = 1\n[[f]]\n[f.x]\na = 2").is_ok());
        assert!(parse("a = 1_000\nb = 0.5\nc = -1e3\nd = 0").is_ok());
    }

    #[test]
    fn rejects_what_it_does_not_support() {
        for bad in [
            "a = {x = 1}",
            "a.b = 1",
            "a = \"\"\"x\"\"\"",
            "a = 1979-05-27",
            "a = 1\na = 2",
            "a = \"x\" junk",
            "a = \"open",
            "a = 01",
            "a = 1.",
            "a = -.5",
            "a = 1__0",
            "a = \"\\u+041\"",
            "[t]\nx = 1\n[t]\ny = 2",
            "a = [1]\n[[a]]",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
