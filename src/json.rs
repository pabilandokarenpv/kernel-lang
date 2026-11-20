// Minimal hand-rolled JSON -- matches this project's own "hand-roll the
// pipeline, no parser-generator/external crates" identity (see ARD-006),
// extended here to the LSP server's wire format (see lsp.rs). NOT a
// general-purpose JSON library: just enough to parse/build the specific
// message shapes LSP's JSON-RPC framing actually uses (objects, arrays,
// strings, numbers, bool, null) with standard string-escape handling.
// Object key order is preserved (Vec of pairs, not a sorted map) since
// some LSP clients are sensitive to it for readability/debugging, even
// though JSON itself doesn't require order.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn parse(s: &str) -> Option<Json> {
        let chars: Vec<char> = s.chars().collect();
        let mut pos = 0;
        skip_ws(&chars, &mut pos);
        let v = parse_value(&chars, &mut pos)?;
        Some(v)
    }

    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out);
        out
    }
}

fn skip_ws(chars: &[char], pos: &mut usize) {
    while let Some(&c) = chars.get(*pos) {
        if c.is_whitespace() {
            *pos += 1;
        } else {
            break;
        }
    }
}

fn parse_value(chars: &[char], pos: &mut usize) -> Option<Json> {
    skip_ws(chars, pos);
    match chars.get(*pos)? {
        '{' => parse_object(chars, pos),
        '[' => parse_array(chars, pos),
        '"' => parse_string(chars, pos).map(Json::String),
        't' => {
            if chars[*pos..].starts_with(&['t', 'r', 'u', 'e']) {
                *pos += 4;
                Some(Json::Bool(true))
            } else {
                None
            }
        }
        'f' => {
            if chars[*pos..].starts_with(&['f', 'a', 'l', 's', 'e']) {
                *pos += 5;
                Some(Json::Bool(false))
            } else {
                None
            }
        }
        'n' => {
            if chars[*pos..].starts_with(&['n', 'u', 'l', 'l']) {
                *pos += 4;
                Some(Json::Null)
            } else {
                None
            }
        }
        '-' | '0'..='9' => parse_number(chars, pos),
        _ => None,
    }
}

fn parse_string(chars: &[char], pos: &mut usize) -> Option<String> {
    if chars.get(*pos) != Some(&'"') {
        return None;
    }
    *pos += 1;
    let mut s = String::new();
    loop {
        let c = *chars.get(*pos)?;
        *pos += 1;
        match c {
            '"' => return Some(s),
            '\\' => {
                let esc = *chars.get(*pos)?;
                *pos += 1;
                match esc {
                    '"' => s.push('"'),
                    '\\' => s.push('\\'),
                    '/' => s.push('/'),
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    'b' => s.push('\u{8}'),
                    'f' => s.push('\u{c}'),
                    'u' => {
                        let hex: String = chars.get(*pos..*pos + 4)?.iter().collect();
                        *pos += 4;
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    other => s.push(other),
                }
            }
            other => s.push(other),
        }
    }
}

fn parse_number(chars: &[char], pos: &mut usize) -> Option<Json> {
    let start = *pos;
    if chars.get(*pos) == Some(&'-') {
        *pos += 1;
    }
    while matches!(chars.get(*pos), Some(c) if c.is_ascii_digit()) {
        *pos += 1;
    }
    if chars.get(*pos) == Some(&'.') {
        *pos += 1;
        while matches!(chars.get(*pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
    }
    if matches!(chars.get(*pos), Some('e') | Some('E')) {
        *pos += 1;
        if matches!(chars.get(*pos), Some('+') | Some('-')) {
            *pos += 1;
        }
        while matches!(chars.get(*pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
    }
    let s: String = chars[start..*pos].iter().collect();
    s.parse::<f64>().ok().map(Json::Number)
}

fn parse_array(chars: &[char], pos: &mut usize) -> Option<Json> {
    *pos += 1; // consume '['
    let mut items = Vec::new();
    skip_ws(chars, pos);
    if chars.get(*pos) == Some(&']') {
        *pos += 1;
        return Some(Json::Array(items));
    }
    loop {
        let v = parse_value(chars, pos)?;
        items.push(v);
        skip_ws(chars, pos);
        match chars.get(*pos)? {
            ',' => {
                *pos += 1;
            }
            ']' => {
                *pos += 1;
                break;
            }
            _ => return None,
        }
    }
    Some(Json::Array(items))
}

fn parse_object(chars: &[char], pos: &mut usize) -> Option<Json> {
    *pos += 1; // consume '{'
    let mut pairs = Vec::new();
    skip_ws(chars, pos);
    if chars.get(*pos) == Some(&'}') {
        *pos += 1;
        return Some(Json::Object(pairs));
    }
    loop {
        skip_ws(chars, pos);
        let key = parse_string(chars, pos)?;
        skip_ws(chars, pos);
        if chars.get(*pos) != Some(&':') {
            return None;
        }
        *pos += 1;
        let value = parse_value(chars, pos)?;
        pairs.push((key, value));
        skip_ws(chars, pos);
        match chars.get(*pos)? {
            ',' => {
                *pos += 1;
            }
            '}' => {
                *pos += 1;
                break;
            }
            _ => return None,
        }
    }
    Some(Json::Object(pairs))
}

fn write_json(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Json::String(s) => write_json_string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Object(pairs) => {
            out.push('{');
            for (i, (k, val)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_json(val, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
