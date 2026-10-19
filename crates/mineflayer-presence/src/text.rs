//! Text components to plain text: the JSON strings servers send up to
//! 1.20.2 and the NBT they send from 1.20.3.

use serde_json::Value as Json;

use crate::protodef::Value;

/// Plain text of a component that arrived as a protodef value (a JSON
/// string before 1.20.3, NBT after).
pub fn plain(v: &Value) -> String {
    let mut out = String::new();
    match v {
        Value::Str(s) => match serde_json::from_str::<Json>(s) {
            Ok(j) if !j.is_string() || s.starts_with('"') => json_text(&j, &mut out),
            _ => out.push_str(s),
        },
        other => nbt_text(other, &mut out),
    }
    strip_codes(&out)
}

fn json_text(j: &Json, out: &mut String) {
    match j {
        Json::String(s) => out.push_str(s),
        Json::Array(items) => items.iter().for_each(|i| json_text(i, out)),
        Json::Object(o) => {
            if let Some(t) = o.get("text").and_then(Json::as_str) {
                out.push_str(t);
            }
            if let Some(key) = o.get("translate").and_then(Json::as_str) {
                let args: Vec<String> = o
                    .get("with")
                    .and_then(Json::as_array)
                    .map(|w| {
                        w.iter()
                            .map(|a| {
                                let mut s = String::new();
                                json_text(a, &mut s);
                                s
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push_str(&translate(key, &args));
            }
            if let Some(extra) = o.get("extra") {
                json_text(extra, out);
            }
        }
        Json::Number(n) => out.push_str(&n.to_string()),
        Json::Bool(b) => out.push_str(&b.to_string()),
        Json::Null => {}
    }
}

fn nbt_text(v: &Value, out: &mut String) {
    match v {
        Value::Str(s) => out.push_str(s),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&f.to_string()),
        Value::List(items) => items.iter().for_each(|i| nbt_text(i, out)),
        Value::Map(_) => {
            // NBT has no list of mixed types, so a component's empty-keyed
            // entry carries the text of a list element
            for key in ["", "text"] {
                if let Some(Value::Str(t)) = v.get(key) {
                    out.push_str(t);
                }
            }
            if let Some(Value::Str(key)) = v.get("translate") {
                let args: Vec<String> = match v.get("with") {
                    Some(Value::List(w)) => w
                        .iter()
                        .map(|a| {
                            let mut s = String::new();
                            nbt_text(a, &mut s);
                            s
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                out.push_str(&translate(key, &args));
            }
            if let Some(extra) = v.get("extra") {
                nbt_text(extra, out);
            }
        }
        _ => {}
    }
}

/// A translation key filled with its arguments from Minecraft's en_us
/// strings, which use `%s` in order, `%1$s` by position, and `%%`. A key the
/// table lacks (a server's own) shows as the key and its arguments.
fn translate(key: &str, args: &[String]) -> String {
    let Some(pattern) = mineflayer_language::get(key) else {
        return if args.is_empty() { key.to_owned() } else { format!("{key} {}", args.join(" ")) };
    };
    let mut out = String::with_capacity(pattern.len());
    let mut next = 0;
    let mut rest = pattern;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('%') {
            out.push('%');
            rest = r;
        } else if let Some(r) = rest.strip_prefix('s') {
            out.push_str(args.get(next).map_or("", String::as_str));
            next += 1;
            rest = r;
        } else if let Some((n, r)) = rest.split_once("$s")
            && let Ok(n) = n.parse::<usize>()
        {
            out.push_str(args.get(n.wrapping_sub(1)).map_or("", String::as_str));
            rest = r;
        } else {
            out.push('%');
        }
    }
    out.push_str(rest);
    out
}

/// Text without legacy `§` formatting codes, which servers still put inside
/// component text.
fn strip_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_nbt_components_flatten_the_same() {
        let json = Value::Str(r#"{"translate":"chat.type.text","with":[{"text":"Steve"},"hi"]}"#.into());
        assert_eq!(plain(&json), "<Steve> hi");
        let nbt = Value::Map(vec![
            ("translate".into(), Value::Str("chat.type.text".into())),
            (
                "with".into(),
                Value::List(vec![Value::Map(vec![("text".into(), Value::Str("Steve".into()))]), Value::Str("hi".into())]),
            ),
        ]);
        assert_eq!(plain(&nbt), "<Steve> hi");
        assert_eq!(plain(&Value::Str("plain words".into())), "plain words");
        assert_eq!(plain(&Value::Str(r#"{"text":"a","extra":[{"text":"b"}]}"#.into())), "ab");
    }

    #[test]
    fn positional_arguments_nested_keys_and_legacy_codes() {
        let death = Value::Str(
            r#"{"translate":"death.attack.mob","with":[{"text":"pres001"},{"translate":"entity.minecraft.slime"}]}"#.into(),
        );
        assert_eq!(plain(&death), "pres001 was slain by Slime");
        assert_eq!(plain(&Value::Str("\u{a7}aGreen \u{a7}lbold".into())), "Green bold");
        assert_eq!(plain(&Value::Str(r#"{"translate":"some.plugin.key","with":["x"]}"#.into())), "some.plugin.key x");
    }
}
