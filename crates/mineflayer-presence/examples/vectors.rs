//! Write codec test vectors: for every protocol, state, direction and packet,
//! `<count>` random values, each as one JSON line with the encoded bytes.
//! Before writing a vector it checks that the bytes decode back to the same
//! value and that decoding consumes every byte. `scripts/presence-diff`
//! feeds the lines to node-minecraft-protocol.
//!
//! ```sh
//! cargo run --release -p mineflayer-presence --example vectors -- 1000 > vectors.jsonl
//! ```

use std::io::{Cursor, Write};

use mineflayer_presence::{
    dialects,
    protodef::{Len, Type, Value},
    table::State,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn text(&mut self) -> String {
        const ALPHABET: &[char] = &['a', 'Z', '0', ' ', '_', '/', 'é', 'ß', '中', '🙂', '\u{a7}', '"', '\\'];
        let n = self.below(12) as usize;
        (0..n).map(|_| ALPHABET[self.below(ALPHABET.len() as u64) as usize]).collect()
    }
}

fn gen_value(t: &Type, rng: &mut Rng, siblings: &[(Box<str>, Value)]) -> Value {
    match t {
        Type::Void => Value::None,
        Type::Bool => Value::Bool(rng.below(2) == 1),
        Type::I8 => Value::Int(i64::from(rng.next() as i8)),
        Type::U8 => Value::Int(i64::from(rng.next() as u8)),
        Type::I16 => Value::Int(i64::from(rng.next() as i16)),
        Type::I32 => Value::Int(i64::from(rng.next() as i32)),
        Type::U32 => Value::Int(i64::from(rng.next() as u32)),
        Type::I64 => Value::Int(rng.next() as i64),
        Type::VarInt => Value::Int(i64::from(match rng.below(3) {
            0 => rng.below(128) as i32,
            1 => rng.next() as i32,
            _ => -(rng.below(1000) as i32),
        })),
        Type::F32 => Value::Float(f64::from((rng.next() as i32) as f32 / 7.0)),
        Type::F64 => Value::Float((rng.next() as i64) as f64 / 13.0),
        Type::Uuid => Value::Uuid(u128::from(rng.next()) << 64 | u128::from(rng.next())),
        Type::String => Value::Str(rng.text()),
        Type::RestBuffer => {
            let n = rng.below(16) as usize;
            Value::Bytes(rng.bytes(n))
        }
        Type::AnonymousNbt => {
            if rng.below(4) == 0 { Value::None } else { Value::Str(rng.text()) }
        }
        Type::Buffer(Len::VarInt | Len::I16) => {
            let n = rng.below(24) as usize;
            Value::Bytes(rng.bytes(n))
        }
        Type::Buffer(Len::Fixed(n)) => Value::Bytes(rng.bytes(*n)),
        Type::Array(len, inner) => {
            let n = match len {
                Len::VarInt | Len::I16 => rng.below(4) as usize,
                Len::Fixed(n) => *n,
            };
            Value::List((0..n).map(|_| gen_value(inner, rng, &[])).collect())
        }
        Type::Option(inner) => {
            if rng.below(2) == 0 {
                Value::None
            } else {
                // an option of a value that encodes as nothing cannot round-trip
                let mut v = gen_value(inner, rng, siblings);
                while v == Value::None {
                    v = gen_value(inner, rng, siblings);
                }
                v
            }
        }
        Type::Container(fields) => {
            let mut out: Vec<(Box<str>, Value)> = Vec::new();
            for (name, ft) in fields {
                let v = gen_value(ft, rng, &out);
                out.push((name.clone(), v));
            }
            Value::Map(out)
        }
        Type::Switch { compare_to, fields, default } => {
            let key = siblings
                .iter()
                .rev()
                .find(|(k, _)| **k == **compare_to)
                .map(|(_, v)| match v {
                    Value::Bool(b) => b.to_string(),
                    Value::Int(i) => i.to_string(),
                    Value::Str(s) => s.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let t = fields.iter().find(|(k, _)| **k == *key).map(|(_, t)| t).unwrap_or(default);
            gen_value(t, rng, siblings)
        }
        Type::Mapper { mappings, .. } => Value::Str(mappings[rng.below(mappings.len() as u64) as usize].1.to_string()),
        // servers set only the named bits
        Type::Bitflags { flags, .. } => Value::Int(rng.below(1 << flags.len()) as i64),
        Type::RegistryEntryHolder { base_name, otherwise_name, otherwise } => {
            if rng.below(2) == 0 {
                Value::Map(vec![(base_name.clone(), Value::Int(rng.below(64) as i64))])
            } else {
                Value::Map(vec![(otherwise_name.clone(), gen_value(otherwise, rng, &[]))])
            }
        }
    }
}

fn to_json(v: &Value) -> serde_json::Value {
    use serde_json::{Value as J, json};
    match v {
        Value::None => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::String(i.to_string()),
        Value::Float(f) => json!(f),
        Value::Str(s) => J::String(s.clone()),
        Value::Bytes(b) => json!({ "bytes": b.iter().map(|x| format!("{x:02x}")).collect::<String>() }),
        Value::Uuid(u) => json!({ "uuid": format!("{u:032x}") }),
        Value::List(items) => J::Array(items.iter().map(to_json).collect()),
        Value::Map(fields) => J::Object(fields.iter().map(|(k, v)| (k.to_string(), to_json(v))).collect()),
    }
}

fn main() {
    let count: usize = std::env::args().nth(1).map_or(1000, |c| c.parse().expect("count"));
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut failures = 0;
    for d in dialects() {
        for (state, to_client, name, id, schema) in d.packets() {
            // read by id only; the table stores no layout for them
            if to_client && matches!(name, "login" | "respawn") && state == State::Play {
                continue;
            }
            for _ in 0..count {
                let v = gen_value(schema, &mut rng, &[]);
                let mut bytes = Vec::new();
                mineflayer_presence::protodef::write_varint(&mut bytes, id);
                if let Err(e) = schema.encode(&v, &mut bytes) {
                    failures += 1;
                    eprintln!("encode {} {} {name}: {e}", d.protocol, state.key());
                    continue;
                }
                let mut cur = Cursor::new(&bytes[..]);
                mineflayer_presence::protodef::read_varint(&mut cur).expect("id");
                match schema.decode(&mut cur) {
                    Ok(back) if back == v && cur.position() as usize == bytes.len() => {}
                    other => {
                        failures += 1;
                        eprintln!("roundtrip {} {} {name}: {other:?} != {v:?}", d.protocol, state.key());
                        continue;
                    }
                }
                let line = serde_json::json!({
                    "protocol": d.protocol,
                    "version": d.names[0],
                    "state": state.key(),
                    "direction": if to_client { "toClient" } else { "toServer" },
                    "name": name,
                    "hex": bytes.iter().map(|x| format!("{x:02x}")).collect::<String>(),
                    "value": to_json(&v),
                });
                writeln!(out, "{line}").expect("stdout");
            }
        }
    }
    out.flush().expect("stdout");
    eprintln!("rust roundtrip failures: {failures}");
    std::process::exit(i32::from(failures > 0));
}
