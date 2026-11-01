//! A small TOML subset, enough for service files:
//!
//! ```toml
//! # comments
//! key = "basic string"        # \" \\ \n \t \r \uXXXX escapes
//! key = 'literal string'
//! key = ["arrays", "may span",
//!        "several lines"]
//! key = true
//! key = 42
//! [table]
//! KEY = "value"
//! ```
//!
//! Not supported (rejected with an error, never silently misread): dotted
//! keys, inline tables, `[[array tables]]`, multi-line strings, floats, dates.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Value>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Str(_) => "string",
            Value::Int(_) => "integer",
            Value::Bool(_) => "boolean",
            Value::Array(_) => "array",
        }
    }
}

pub type Table = BTreeMap<String, Value>;

#[derive(Debug, Default, PartialEq)]
pub struct Doc {
    pub root: Table,
    pub tables: BTreeMap<String, Table>,
}

pub fn parse(src: &str) -> Result<Doc, String> {
    let mut p = Parser { c: src.chars().collect(), i: 0, line: 1 };
    let mut doc = Doc::default();
    let mut current: Option<String> = None;
    loop {
        p.skip_blank();
        match p.peek() {
            None => break,
            Some('[') => {
                p.bump();
                if p.peek() == Some('[') {
                    return Err(p.err("array tables `[[...]]` are not supported"));
                }
                p.skip_ws();
                let name = p.key()?;
                p.skip_ws();
                p.expect(']')?;
                p.end_of_line()?;
                if doc.tables.contains_key(&name) || doc.root.contains_key(&name) {
                    return Err(p.err(&format!("duplicate table `[{name}]`")));
                }
                doc.tables.insert(name.clone(), Table::new());
                current = Some(name);
            }
            Some(_) => {
                let line = p.line;
                let key = p.key()?;
                p.skip_ws();
                if p.peek() == Some('.') {
                    return Err(p.err("dotted keys are not supported"));
                }
                p.expect('=')?;
                p.skip_ws();
                let value = p.value()?;
                p.end_of_line()?;
                let table = match &current {
                    None => &mut doc.root,
                    Some(t) => doc.tables.get_mut(t).unwrap(),
                };
                if table.contains_key(&key) {
                    return Err(format!("line {line}: duplicate key `{key}`"));
                }
                table.insert(key, value);
            }
        }
    }
    Ok(doc)
}

struct Parser {
    c: Vec<char>,
    i: usize,
    line: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.i += 1;
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    fn err(&self, msg: &str) -> String {
        format!("line {}: {}", self.line, msg)
    }

    fn describe(ch: Option<char>) -> String {
        match ch {
            None => "end of file".into(),
            Some('\n') => "end of line".into(),
            Some(c) => format!("`{c}`"),
        }
    }

    fn expect(&mut self, want: char) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.bump();
            Ok(())
        } else {
            Err(self.err(&format!("expected `{want}`, found {}", Self::describe(self.peek()))))
        }
    }

    /// Spaces and tabs only.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.bump();
            }
        }
    }

    /// Whitespace, newlines and comments.
    fn skip_blank(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('#') => self.skip_comment(),
                _ => break,
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_ws();
        self.skip_comment();
        if self.peek() == Some('\r') {
            self.bump();
        }
        match self.peek() {
            None | Some('\n') => Ok(()),
            other => Err(self.err(&format!("unexpected {} after value", Self::describe(other)))),
        }
    }

    fn key(&mut self) -> Result<String, String> {
        match self.peek() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            _ => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                    self.bump();
                }
                if self.i == start {
                    return Err(self.err(&format!("expected a key, found {}", Self::describe(self.peek()))));
                }
                Ok(self.c[start..self.i].iter().collect())
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('"') => self.basic_string().map(Value::Str),
            Some('\'') => self.literal_string().map(Value::Str),
            Some('[') => self.array(),
            Some('t' | 'f') => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
                    self.bump();
                }
                match self.c[start..self.i].iter().collect::<String>().as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    w => Err(self.err(&format!("expected a value, found `{w}` (strings need quotes)"))),
                }
            }
            Some(c) if c.is_ascii_digit() || c == '+' || c == '-' => self.integer(),
            Some(c) if c.is_ascii_alphabetic() => {
                Err(self.err("expected a value; strings need quotes, e.g. \"text\""))
            }
            other => Err(self.err(&format!("expected a value, found {}", Self::describe(other)))),
        }
    }

    fn integer(&mut self) -> Result<Value, String> {
        let mut s = String::new();
        if let Some(c @ ('+' | '-')) = self.peek() {
            s.push(c);
            self.bump();
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
            } else if c != '_' {
                break;
            }
            self.bump();
        }
        if matches!(self.peek(), Some('.' | 'e' | 'E')) {
            return Err(self.err("floats are not supported"));
        }
        s.parse().map(Value::Int).map_err(|_| self.err(&format!("invalid integer `{s}`")))
    }

    fn basic_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        if self.peek() == Some('"') && self.c.get(self.i + 1) == Some(&'"') {
            return Err(self.err("multi-line strings are not supported"));
        }
        let mut s = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.err("unterminated string")),
                Some('"') => return Ok(s),
                Some('\\') => match self.bump() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('u') => {
                        let hex: String = (0..4).filter_map(|_| self.bump()).collect();
                        let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                        s.push(ch.ok_or_else(|| self.err(&format!("invalid escape `\\u{hex}`")))?);
                    }
                    other => return Err(self.err(&format!("invalid escape `\\{}`", other.unwrap_or(' ')))),
                },
                Some(c) => s.push(c),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, String> {
        self.expect('\'')?;
        let mut s = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(self.err("unterminated string")),
                Some('\'') => return Ok(s),
                Some(c) => s.push(c),
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.peek() == Some(']') {
                self.bump();
                break;
            }
            items.push(self.value()?);
            self.skip_blank();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {
                    self.bump();
                    break;
                }
                other => return Err(self.err(&format!("expected `,` or `]` in array, found {}", Self::describe(other)))),
            }
        }
        Ok(Value::Array(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_shaped_doc() {
        let doc = parse(
            r#"
# a comment
description = "Serial login"   # trailing comment
exec = ["/sbin/getty", "-L",
        "115200", 'ttyS0',  # comment inside array
       ]
console = false
stop_timeout = 5

[env]
LANG = "C.UTF-8"
"QUOTED-KEY" = "a\tbé"
"#,
        )
        .unwrap();
        assert_eq!(doc.root["description"], Value::Str("Serial login".into()));
        assert_eq!(
            doc.root["exec"],
            Value::Array(vec![
                Value::Str("/sbin/getty".into()),
                Value::Str("-L".into()),
                Value::Str("115200".into()),
                Value::Str("ttyS0".into()),
            ])
        );
        assert_eq!(doc.root["console"], Value::Bool(false));
        assert_eq!(doc.root["stop_timeout"], Value::Int(5));
        assert_eq!(doc.tables["env"]["LANG"], Value::Str("C.UTF-8".into()));
        assert_eq!(doc.tables["env"]["QUOTED-KEY"], Value::Str("a\tbé".into()));
    }

    #[test]
    fn crlf_and_empty() {
        assert_eq!(parse("").unwrap(), Doc::default());
        assert_eq!(parse("a = 1\r\nb = true\r\n").unwrap().root["b"], Value::Bool(true));
    }

    #[test]
    fn errors_have_line_numbers() {
        let cases = [
            ("a = 1\na = 2", "line 2: duplicate key `a`"),
            ("a = bare", "line 1: expected a value; strings need quotes"),
            ("a = \"open", "line 1: unterminated string"),
            ("a.b = 1", "line 1: dotted keys are not supported"),
            ("[[x]]", "array tables"),
            ("a = 1.5", "floats are not supported"),
            ("a = [1 2]", "expected `,` or `]`"),
            ("a = 1 b = 2", "unexpected `b` after value"),
            ("\n\n= 3", "line 3: expected a key"),
            ("[t]\n[t]", "duplicate table"),
            ("x = \"\"\"multi\"\"\"", "multi-line strings"),
        ];
        for (src, want) in cases {
            let err = parse(src).unwrap_err();
            assert!(err.contains(want), "{src:?}: got {err:?}, want {want:?}");
        }
    }
}
