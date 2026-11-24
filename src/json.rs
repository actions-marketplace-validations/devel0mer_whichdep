//! A zero-dependency RFC 8259 JSON parser and writer used for `--json`,
//! machine-readable API serialization, and `package-lock.json` manipulation.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn opt_str(s: Option<impl Into<String>>) -> Json {
        match s {
            Some(s) => Json::Str(s.into()),
            None => Json::Null,
        }
    }

    pub fn strs<I, S>(items: I) -> Json
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Json::Arr(items.into_iter().map(|s| Json::Str(s.into())).collect())
    }

    pub fn obj<const N: usize>(fields: [(&str, Json); N]) -> Json {
        Json::Obj(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn int(n: usize) -> Json {
        Json::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }

    /// Parse any valid RFC 8259 JSON text.
    pub fn parse(input: &str) -> Result<Json, String> {
        let mut parser = Parser::new(input);
        let value = parser.parse_value(0)?;
        parser.skip_ws();
        if !parser.is_eof() {
            return Err(format!(
                "unexpected trailing JSON content at byte {}",
                parser.pos
            ));
        }
        Ok(value)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Float(f) => Some(*f),
            Json::Int(n) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    pub fn as_arr_mut(&mut self) -> Option<&mut Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o.as_slice()),
            _ => None,
        }
    }

    pub fn as_obj_mut(&mut self) -> Option<&mut Vec<(String, Json)>> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_obj()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        self.as_obj_mut()?
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn set(&mut self, key: impl Into<String>, value: Json) {
        if let Json::Obj(fields) = self {
            let key = key.into();
            if let Some((_, existing)) = fields.iter_mut().find(|(k, _)| *k == key) {
                *existing = value;
            } else {
                fields.push((key, value));
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Json> {
        if let Json::Obj(fields) = self {
            if let Some(pos) = fields.iter().position(|(k, _)| k == key) {
                return Some(fields.remove(pos).1);
            }
        }
        None
    }

    /// Two-space indented output followed by a newline.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    /// Single-line compact JSON output (no trailing newline).
    pub fn compact(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::Float(f) if f.is_finite() => out.push_str(&format!("{f:.3}")),
            Json::Float(_) => out.push_str("null"),
            Json::Str(s) => escape(s, out),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    indent(out, depth + 1);
                    item.write(out, depth + 1);
                    out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push(']');
            }
            Json::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Obj(fields) => {
                out.push_str("{\n");
                for (i, (k, v)) in fields.iter().enumerate() {
                    indent(out, depth + 1);
                    escape(k, out);
                    out.push_str(": ");
                    v.write(out, depth + 1);
                    out.push_str(if i + 1 < fields.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }

    fn write_compact(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::Float(f) if f.is_finite() => out.push_str(&format!("{f:.3}")),
            Json::Float(_) => out.push_str("null"),
            Json::Str(s) => escape(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_compact(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape(k, out);
                    out.push(':');
                    v.write_compact(out);
                }
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

const MAX_DEPTH: usize = 256;

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting depth exceeded".to_string());
        }
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err(format!("unexpected end of JSON at byte {}", self.pos));
        };
        match b {
            b'n' => self.parse_literal(b"null", Json::Null),
            b't' => self.parse_literal(b"true", Json::Bool(true)),
            b'f' => self.parse_literal(b"false", Json::Bool(false)),
            b'"' => self.parse_string().map(Json::Str),
            b'[' => self.parse_array(depth + 1),
            b'{' => self.parse_object(depth + 1),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!(
                "unexpected character '{}' in JSON at byte {}",
                other as char, self.pos
            )),
        }
    }

    fn parse_literal(&mut self, lit: &[u8], value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(value)
        } else {
            Err(format!("invalid JSON token at byte {}", self.pos))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("expected '\"' at byte {}", self.pos));
        }
        self.pos += 1;
        let mut out = String::new();
        while !self.is_eof() {
            let b = self.bytes[self.pos];
            if b == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if b == b'\\' {
                self.pos += 1;
                let Some(esc) = self.peek() else {
                    return Err("unterminated escape sequence in JSON string".to_string());
                };
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = self.parse_hex4()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            if !self.bytes[self.pos..].starts_with(b"\\u") {
                                return Err(
                                    "unpaired UTF-16 high surrogate in JSON string".to_string()
                                );
                            }
                            self.pos += 2;
                            let low = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(
                                    "invalid UTF-16 low surrogate in JSON string".to_string()
                                );
                            }
                            let cp = 0x10000
                                + (((u32::from(code) - 0xD800) << 10) | (u32::from(low) - 0xDC00));
                            let ch = char::from_u32(cp).ok_or_else(|| {
                                "invalid Unicode scalar in JSON string".to_string()
                            })?;
                            out.push(ch);
                        } else {
                            let ch = char::from_u32(u32::from(code)).ok_or_else(|| {
                                "invalid Unicode escape in JSON string".to_string()
                            })?;
                            out.push(ch);
                        }
                    }
                    _ => {
                        return Err(format!(
                            "invalid escape '\\{}' at byte {}",
                            esc as char, self.pos
                        ))
                    }
                }
            } else if b < 0x20 {
                return Err(format!(
                    "unescaped control character in JSON string at byte {}",
                    self.pos
                ));
            } else {
                let rest = &self.src[self.pos..];
                let ch = rest
                    .chars()
                    .next()
                    .ok_or_else(|| "unexpected end of UTF-8 string".to_string())?;
                out.push(ch);
                self.pos += ch.len_utf8();
            }
        }
        Err("unterminated JSON string".to_string())
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        if self.pos + 4 > self.bytes.len() {
            return Err("truncated \\u escape in JSON string".to_string());
        }
        let slice = &self.src[self.pos..self.pos + 4];
        let val = u16::from_str_radix(slice, 16)
            .map_err(|_| format!("invalid hex escape '\\u{slice}'"))?;
        self.pos += 4;
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(format!("invalid JSON number at byte {start}")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("expected digit after '.' at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(format!("expected exponent digits at byte {}", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = &self.src[start..self.pos];
        if !is_float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Json::Int(n));
            }
        }
        let f = text
            .parse::<f64>()
            .map_err(|_| format!("invalid JSON number `{text}`"))?;
        if !f.is_finite() {
            return Err(format!("non-finite JSON number `{text}`"));
        }
        Ok(Json::Float(f))
    }

    fn parse_array(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Arr(items));
                }
                _ => {
                    return Err(format!(
                        "expected ',' or ']' in JSON array at byte {}",
                        self.pos
                    ))
                }
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1; // '{'
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' after JSON key at byte {}", self.pos));
            }
            self.pos += 1;
            let val = self.parse_value(depth)?;
            fields.push((key, val));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Obj(fields));
                }
                _ => {
                    return Err(format!(
                        "expected ',' or '}}' in JSON object at byte {}",
                        self.pos
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_json_requires() {
        let j = Json::str("a\"b\\c\nd\te\u{1}é");
        assert_eq!(j.pretty(), "\"a\\\"b\\\\c\\nd\\te\\u0001é\"\n");
    }

    #[test]
    fn nests_and_indents() {
        let j = Json::obj([
            ("name", Json::str("x")),
            (
                "list",
                Json::Arr(vec![Json::int(1), Json::Bool(true), Json::Null]),
            ),
            ("empty", Json::Arr(vec![])),
            ("t", Json::Float(1.23456)),
            ("o", Json::obj([])),
        ]);
        assert_eq!(
            j.pretty(),
            "{\n  \"name\": \"x\",\n  \"list\": [\n    1,\n    true,\n    null\n  ],\n  \"empty\": [],\n  \"t\": 1.235,\n  \"o\": {}\n}\n"
        );
    }

    #[test]
    fn non_finite_numbers_become_null() {
        assert_eq!(Json::Float(f64::NAN).pretty(), "null\n");
    }

    #[test]
    fn parses_objects_arrays_escapes_and_surrogates() {
        let src = r#"{
            "name": "@scope/pkg",
            "version": 3,
            "ok": true,
            "empty": null,
            "ratio": -1.25e1,
            "emoji": "\uD83D\uDE80",
            "nested": [1, "a\/b\n", {"k": false}]
        }"#;
        let parsed = Json::parse(src).unwrap();
        assert_eq!(
            parsed.get("name").and_then(Json::as_str),
            Some("@scope/pkg")
        );
        assert_eq!(parsed.get("version").and_then(Json::as_i64), Some(3));
        assert_eq!(parsed.get("ok").and_then(Json::as_bool), Some(true));
        assert_eq!(parsed.get("empty"), Some(&Json::Null));
        assert_eq!(parsed.get("ratio").and_then(Json::as_f64), Some(-12.5));
        assert_eq!(parsed.get("emoji").and_then(Json::as_str), Some("🚀"));
        let nested = parsed.get("nested").and_then(Json::as_arr).unwrap();
        assert_eq!(nested[1].as_str(), Some("a/b\n"));
        assert_eq!(nested[2].get("k").and_then(Json::as_bool), Some(false));
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(Json::parse("{\"a\": 1,}").is_err());
        assert!(Json::parse("123 trailing").is_err());
        assert!(Json::parse("\"unterminated").is_err());
        assert!(Json::parse("[1, 2").is_err());
    }
}
