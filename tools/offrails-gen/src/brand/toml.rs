//! The TOML subset brand.toml uses: `[section]` headers, `key = value` with basic ("…") or
//! literal ('…') strings, integers, floats and booleans, and `#` comments. Keys come back as
//! `section.key` (top-level keys have no prefix). Anything else is an error naming the line.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

pub fn parse(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut out = BTreeMap::new();
    let mut section = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        let at = n + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest
                .split('#')
                .next()
                .and_then(|s| s.trim().strip_suffix(']'))
            else {
                return Err(format!("line {at}: a section header is [name]"));
            };
            let name = name.trim();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("line {at}: bad section name [{name}]"));
            }
            section = name.to_owned();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {at}: expected key = value"));
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("line {at}: bad key {key:?}"));
        }
        let value = parse_value(value.trim()).map_err(|e| format!("line {at} ({key}): {e}"))?;
        let full = if section.is_empty() {
            key.to_owned()
        } else {
            format!("{section}.{key}")
        };
        if out.insert(full.clone(), value).is_some() {
            return Err(format!("line {at}: {full} is set twice"));
        }
    }
    Ok(out)
}

/// One value and an optional trailing comment.
fn parse_value(s: &str) -> Result<Value, String> {
    let (value, rest) = if let Some(body) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.char_indices();
        let mut end = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    end = Some(i + 1);
                    break;
                }
                '\\' => match chars.next().map(|(_, e)| e) {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    other => return Err(format!("unsupported escape \\{}", other.unwrap_or(' '))),
                },
                c => out.push(c),
            }
        }
        let Some(end) = end else {
            return Err("unterminated string".into());
        };
        (Value::Str(out), &body[end..])
    } else if let Some(body) = s.strip_prefix('\'') {
        let Some(end) = body.find('\'') else {
            return Err("unterminated string".into());
        };
        (Value::Str(body[..end].to_owned()), &body[end + 1..])
    } else {
        let token = s.split('#').next().unwrap_or("").trim();
        let v = match token {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            t => Value::Num(t.replace('_', "").parse::<f64>().map_err(|_| {
                format!("expected a \"string\", a number or true/false, found {t:?}")
            })?),
        };
        return Ok(v);
    };
    let rest = rest.trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(format!("unexpected text after the value: {rest:?}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sections_strings_numbers_and_comments() {
        let t = parse(
            "# brand\nname = \"Book \\\"Shelf\\\"\"  # the name\n[colors]\nprimary = '#4f46e5'\n\n[shape]\nscale = 1.5\nsharp = false\n",
        )
        .unwrap();
        assert_eq!(t["name"], Value::Str("Book \"Shelf\"".into()));
        assert_eq!(t["colors.primary"], Value::Str("#4f46e5".into()));
        assert_eq!(t["shape.scale"], Value::Num(1.5));
        assert_eq!(t["shape.sharp"], Value::Bool(false));
    }

    #[test]
    fn errors_name_the_line() {
        assert!(parse("a = \"open\n").unwrap_err().starts_with("line 1"));
        assert!(
            parse("x\n")
                .unwrap_err()
                .contains("line 1: expected key = value")
        );
        assert!(parse("a = 1\na = 2\n").unwrap_err().contains("set twice"));
        assert!(
            parse("[x]\nb = \"1\" junk\n")
                .unwrap_err()
                .contains("line 2 (b)")
        );
        assert!(
            parse("c = nope\n")
                .unwrap_err()
                .contains("expected a \"string\"")
        );
    }
}
