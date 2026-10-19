//! An interpreter for the protodef type language of minecraft-data, limited to
//! the constructs the presence packets use. A schema is compiled once into a
//! [`Type`]; [`Type::decode`] and [`Type::encode`] then run against it.
//!
//! Encoding fills every field the caller leaves out with the zero value of
//! its type (0, false, "", an absent option, an empty array, a zeroed fixed
//! buffer). The session code names the fields it means; the per-version
//! schema decides the order, the width, and which fields exist at all.

use std::io::{Cursor, Read};

use serde_json::Value as Json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("unsupported protodef type: {0}")]
    Unsupported(String),
    #[error("malformed schema: {0}")]
    Malformed(String),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("packet ended early")]
    Eof,
    #[error("varint is too long")]
    VarInt,
    #[error("string is not UTF-8")]
    Utf8,
    #[error("length {0} is out of range")]
    Length(i64),
    #[error("bad NBT tag {0}")]
    Nbt(u8),
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("field {0} has the wrong value type")]
    Mismatch(String),
    #[error("no {0} mapping named {1}")]
    Mapping(String, String),
}

#[derive(Clone, Debug)]
pub enum Len {
    VarInt,
    /// 1.7's length prefix for byte arrays.
    I16,
    Fixed(usize),
}

#[derive(Clone, Debug)]
pub enum Type {
    Void,
    Bool,
    I8,
    U8,
    I16,
    I32,
    U32,
    I64,
    F32,
    F64,
    VarInt,
    Uuid,
    String,
    RestBuffer,
    /// Network NBT (no root name), as sent from 1.20.2.
    AnonymousNbt,
    Buffer(Len),
    Array(Len, Box<Type>),
    Option(Box<Type>),
    Container(Vec<(Box<str>, Type)>),
    Switch {
        compare_to: Box<str>,
        fields: Vec<(Box<str>, Type)>,
        default: Box<Type>,
    },
    Mapper {
        ty: Box<Type>,
        mappings: Vec<(i64, Box<str>)>,
    },
    /// An integer whose low bits are the named flags, in order.
    Bitflags { ty: Box<Type>, flags: Vec<Box<str>> },
    /// A registry id plus one, or 0 followed by an inline value.
    RegistryEntryHolder {
        base_name: Box<str>,
        otherwise_name: Box<str>,
        otherwise: Box<Type>,
    },
}

/// A decoded or to-be-encoded protodef value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// An absent option, a void switch branch.
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Uuid(u128),
    List(Vec<Value>),
    Map(Vec<(Box<str>, Value)>),
}

impl Value {
    pub fn get(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Map(fields) => fields.iter().find(|(k, _)| &**k == name).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn float(&self, name: &str) -> Option<f64> {
        match self.get(name)? {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    fn switch_key(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Str(s) => s.clone(),
            _ => String::new(),
        }
    }
}

/// Build a [`Value::Map`] from `name => value` pairs.
#[macro_export]
macro_rules! fields {
    ($($k:literal => $v:expr),* $(,)?) => {
        $crate::protodef::Value::Map(vec![$((Box::from($k), $crate::protodef::Value::from($v))),*])
    };
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v.into())
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}
impl From<uuid::Uuid> for Value {
    fn from(v: uuid::Uuid) -> Self {
        Value::Uuid(v.as_u128())
    }
}

impl Type {
    pub fn compile(j: &Json) -> Result<Type, SchemaError> {
        let bad = || SchemaError::Malformed(j.to_string());
        match j {
            Json::String(s) => Ok(match s.as_str() {
                "void" => Type::Void,
                "bool" => Type::Bool,
                "i8" => Type::I8,
                "u8" => Type::U8,
                "i16" => Type::I16,
                "i32" => Type::I32,
                "u32" => Type::U32,
                "i64" => Type::I64,
                "f32" => Type::F32,
                "f64" => Type::F64,
                "varint" => Type::VarInt,
                "UUID" => Type::Uuid,
                "restBuffer" => Type::RestBuffer,
                "anonymousNbt" => Type::AnonymousNbt,
                other => return Err(SchemaError::Unsupported(other.to_owned())),
            }),
            Json::Array(a) if a.len() == 2 => {
                let kind = a[0].as_str().ok_or_else(bad)?;
                let o = &a[1];
                Ok(match kind {
                    "pstring" => {
                        if o["countType"] != "varint" {
                            return Err(bad());
                        }
                        Type::String
                    }
                    "buffer" => Type::Buffer(len(o).ok_or_else(bad)?),
                    "array" => Type::Array(len(o).ok_or_else(bad)?, Box::new(Type::compile(&o["type"])?)),
                    "option" => Type::Option(Box::new(Type::compile(o)?)),
                    "container" => Type::Container(
                        o.as_array()
                            .ok_or_else(bad)?
                            .iter()
                            .map(|f| {
                                if f.get("anon").is_some() {
                                    return Err(SchemaError::Unsupported("anon container field".into()));
                                }
                                Ok((field_name(f).ok_or_else(bad)?, Type::compile(&f["type"])?))
                            })
                            .collect::<Result<_, _>>()?,
                    ),
                    "switch" => Type::Switch {
                        compare_to: o["compareTo"].as_str().ok_or_else(bad)?.into(),
                        fields: o["fields"]
                            .as_object()
                            .ok_or_else(bad)?
                            .iter()
                            .map(|(k, t)| Ok((k.as_str().into(), Type::compile(t)?)))
                            .collect::<Result<_, _>>()?,
                        default: Box::new(match o.get("default") {
                            Some(d) => Type::compile(d)?,
                            None => Type::Void,
                        }),
                    },
                    "mapper" => Type::Mapper {
                        ty: Box::new(Type::compile(&o["type"])?),
                        mappings: o["mappings"]
                            .as_object()
                            .ok_or_else(bad)?
                            .iter()
                            .map(|(k, v)| Ok((parse_int(k).ok_or_else(bad)?, v.as_str().ok_or_else(bad)?.into())))
                            .collect::<Result<_, _>>()?,
                    },
                    "bitflags" => Type::Bitflags {
                        ty: Box::new(Type::compile(&o["type"])?),
                        flags: o["flags"]
                            .as_array()
                            .ok_or_else(bad)?
                            .iter()
                            .map(|f| f.as_str().map(Box::from).ok_or_else(bad))
                            .collect::<Result<_, _>>()?,
                    },
                    "registryEntryHolder" => Type::RegistryEntryHolder {
                        base_name: o["baseName"].as_str().ok_or_else(bad)?.into(),
                        otherwise_name: o["otherwise"]["name"].as_str().ok_or_else(bad)?.into(),
                        otherwise: Box::new(Type::compile(&o["otherwise"]["type"])?),
                    },
                    other => return Err(SchemaError::Unsupported(other.to_owned())),
                })
            }
            _ => Err(bad()),
        }
    }

    /// The value an encode writes for this type when the field is left out.
    fn zero(&self) -> Value {
        match self {
            Type::Bool => Value::Bool(false),
            Type::I8 | Type::U8 | Type::I16 | Type::I32 | Type::U32 | Type::I64 | Type::VarInt => Value::Int(0),
            Type::Mapper { ty, .. } | Type::Bitflags { ty, .. } => ty.zero(),
            _ => Value::None,
        }
    }

    pub fn decode(&self, buf: &mut Cursor<&[u8]>) -> Result<Value, DecodeError> {
        self.decode_in(buf, &[])
    }

    fn decode_in(&self, buf: &mut Cursor<&[u8]>, siblings: &[(Box<str>, Value)]) -> Result<Value, DecodeError> {
        Ok(match self {
            Type::Void => Value::None,
            Type::Bool => Value::Bool(take::<1>(buf)?[0] != 0),
            Type::I8 => Value::Int(i8::from_be_bytes(take(buf)?).into()),
            Type::U8 => Value::Int(u8::from_be_bytes(take(buf)?).into()),
            Type::I16 => Value::Int(i16::from_be_bytes(take(buf)?).into()),
            Type::I32 => Value::Int(i32::from_be_bytes(take(buf)?).into()),
            Type::U32 => Value::Int(u32::from_be_bytes(take(buf)?).into()),
            Type::I64 => Value::Int(i64::from_be_bytes(take(buf)?)),
            Type::F32 => Value::Float(f32::from_be_bytes(take(buf)?).into()),
            Type::F64 => Value::Float(f64::from_be_bytes(take(buf)?)),
            Type::VarInt => Value::Int(read_varint(buf)?.into()),
            Type::Uuid => Value::Uuid(u128::from_be_bytes(take(buf)?)),
            Type::String => {
                let n = read_len(buf, &Len::VarInt)?;
                String::from_utf8(take_vec(buf, n)?).map(Value::Str).map_err(|_| DecodeError::Utf8)?
            }
            Type::RestBuffer => {
                let mut rest = Vec::new();
                buf.read_to_end(&mut rest).map_err(|_| DecodeError::Eof)?;
                Value::Bytes(rest)
            }
            Type::AnonymousNbt => {
                let tag = take::<1>(buf)?[0];
                if tag == 0 { Value::None } else { read_nbt(buf, tag, 0)? }
            }
            Type::Buffer(l) => {
                let n = read_len(buf, l)?;
                Value::Bytes(take_vec(buf, n)?)
            }
            Type::Array(l, t) => {
                let n = read_len(buf, l)?;
                let mut items = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    let before = buf.position();
                    items.push(t.decode_in(buf, &[])?);
                    // an item that reads no bytes would let a forged count
                    // allocate without bound
                    if buf.position() == before && items.len() > 4096 {
                        return Err(DecodeError::Length(n as i64));
                    }
                }
                Value::List(items)
            }
            Type::Option(t) => {
                if take::<1>(buf)?[0] != 0 {
                    t.decode_in(buf, siblings)?
                } else {
                    Value::None
                }
            }
            Type::Container(fields) => {
                let mut out: Vec<(Box<str>, Value)> = Vec::with_capacity(fields.len());
                for (name, t) in fields {
                    let v = t.decode_in(buf, &out)?;
                    out.push((name.clone(), v));
                }
                Value::Map(out)
            }
            Type::Switch { compare_to, fields, default } => {
                let key = sibling(siblings, compare_to).map(Value::switch_key).unwrap_or_default();
                let t = fields.iter().find(|(k, _)| **k == *key).map(|(_, t)| t).unwrap_or(default);
                t.decode_in(buf, siblings)?
            }
            Type::Mapper { ty, mappings } => match ty.decode_in(buf, siblings)? {
                Value::Int(i) => match mappings.iter().find(|(k, _)| *k == i) {
                    Some((_, name)) => Value::Str(name.to_string()),
                    None => Value::Int(i),
                },
                v => v,
            },
            Type::Bitflags { ty, .. } => ty.decode_in(buf, siblings)?,
            Type::RegistryEntryHolder { base_name, otherwise_name, otherwise } => {
                let id = read_varint(buf)?;
                if id == 0 {
                    Value::Map(vec![(otherwise_name.clone(), otherwise.decode_in(buf, &[])?)])
                } else {
                    Value::Map(vec![(base_name.clone(), Value::Int(i64::from(id) - 1))])
                }
            }
        })
    }

    pub fn encode(&self, v: &Value, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        self.encode_in(Some(v), out, &[], "")
    }

    fn encode_in(
        &self,
        v: Option<&Value>,
        out: &mut Vec<u8>,
        siblings: &[(Box<str>, Value)],
        field: &str,
    ) -> Result<(), EncodeError> {
        let mismatch = || EncodeError::Mismatch(field.to_owned());
        let int = |v: Option<&Value>| -> Result<i64, EncodeError> {
            match v {
                None | Some(Value::None) => Ok(0),
                Some(Value::Int(i)) => Ok(*i),
                Some(Value::Bool(b)) => Ok(i64::from(*b)),
                _ => Err(mismatch()),
            }
        };
        let float = |v: Option<&Value>| -> Result<f64, EncodeError> {
            match v {
                None | Some(Value::None) => Ok(0.0),
                Some(Value::Float(f)) => Ok(*f),
                Some(Value::Int(i)) => Ok(*i as f64),
                _ => Err(mismatch()),
            }
        };
        match self {
            Type::Void => {}
            Type::Bool => out.push(u8::from(int(v)? != 0)),
            Type::I8 | Type::U8 => out.push(int(v)? as u8),
            Type::I16 => out.extend((int(v)? as u16).to_be_bytes()),
            Type::I32 | Type::U32 => out.extend((int(v)? as u32).to_be_bytes()),
            Type::I64 => out.extend(int(v)?.to_be_bytes()),
            Type::F32 => out.extend((float(v)? as f32).to_be_bytes()),
            Type::F64 => out.extend(float(v)?.to_be_bytes()),
            Type::VarInt => write_varint(out, int(v)? as i32),
            Type::Uuid => match v {
                None | Some(Value::None) => out.extend(0u128.to_be_bytes()),
                Some(Value::Uuid(u)) => out.extend(u.to_be_bytes()),
                _ => return Err(mismatch()),
            },
            Type::String => {
                let s = match v {
                    None | Some(Value::None) => "",
                    Some(Value::Str(s)) => s,
                    _ => return Err(mismatch()),
                };
                write_varint(out, s.len() as i32);
                out.extend(s.as_bytes());
            }
            Type::RestBuffer => match v {
                None | Some(Value::None) => {}
                Some(Value::Bytes(b)) => out.extend(b),
                _ => return Err(mismatch()),
            },
            Type::AnonymousNbt => match v {
                None | Some(Value::None) => out.push(0),
                // a plain string component is the only NBT the client sends
                Some(Value::Str(s)) => {
                    out.push(8);
                    out.extend((s.len() as u16).to_be_bytes());
                    out.extend(s.as_bytes());
                }
                _ => return Err(mismatch()),
            },
            Type::Buffer(l) => {
                let b: &[u8] = match v {
                    None | Some(Value::None) => &[],
                    Some(Value::Bytes(b)) => b,
                    _ => return Err(mismatch()),
                };
                match l {
                    Len::VarInt => {
                        write_varint(out, b.len() as i32);
                        out.extend(b);
                    }
                    Len::I16 => {
                        out.extend((b.len() as u16).to_be_bytes());
                        out.extend(b);
                    }
                    Len::Fixed(n) => {
                        let mut fixed = vec![0; *n];
                        let k = b.len().min(*n);
                        fixed[..k].copy_from_slice(&b[..k]);
                        out.extend(fixed);
                    }
                }
            }
            Type::Array(l, t) => {
                let items: &[Value] = match v {
                    None | Some(Value::None) => &[],
                    Some(Value::List(items)) => items,
                    _ => return Err(mismatch()),
                };
                match l {
                    Len::VarInt | Len::I16 => {
                        if matches!(l, Len::VarInt) {
                            write_varint(out, items.len() as i32);
                        } else {
                            out.extend((items.len() as u16).to_be_bytes());
                        }
                        for item in items {
                            t.encode_in(Some(item), out, &[], field)?;
                        }
                    }
                    Len::Fixed(n) => {
                        for i in 0..*n {
                            t.encode_in(items.get(i), out, &[], field)?;
                        }
                    }
                }
            }
            Type::Option(t) => match v {
                None | Some(Value::None) => out.push(0),
                Some(inner) => {
                    out.push(1);
                    t.encode_in(Some(inner), out, siblings, field)?;
                }
            },
            Type::Container(fields) => {
                let given: &[(Box<str>, Value)] = match v {
                    None | Some(Value::None) => &[],
                    Some(Value::Map(m)) => m,
                    _ => return Err(mismatch()),
                };
                let mut done: Vec<(Box<str>, Value)> = Vec::with_capacity(fields.len());
                for (name, t) in fields {
                    let fv = given.iter().find(|(k, _)| k == name).map(|(_, v)| v);
                    t.encode_in(fv, out, &done, name)?;
                    done.push((name.clone(), fv.cloned().unwrap_or_else(|| t.zero())));
                }
            }
            Type::Switch { compare_to, fields, default } => {
                let key = sibling(siblings, compare_to).map(Value::switch_key).unwrap_or_default();
                let t = fields.iter().find(|(k, _)| **k == *key).map(|(_, t)| t).unwrap_or(default);
                t.encode_in(v, out, siblings, field)?;
            }
            Type::Mapper { ty, mappings } => {
                let raw = match v {
                    Some(Value::Str(name)) => mappings
                        .iter()
                        .find(|(_, n)| **n == **name)
                        .map(|(k, _)| Value::Int(*k))
                        .ok_or_else(|| EncodeError::Mapping(field.to_owned(), name.clone()))?,
                    other => other.cloned().unwrap_or(Value::Int(0)),
                };
                ty.encode_in(Some(&raw), out, siblings, field)?;
            }
            Type::Bitflags { ty, .. } => ty.encode_in(v, out, siblings, field)?,
            Type::RegistryEntryHolder { base_name, otherwise_name, otherwise } => match v.and_then(|v| v.get(otherwise_name)) {
                Some(inline) => {
                    write_varint(out, 0);
                    otherwise.encode_in(Some(inline), out, &[], otherwise_name)?;
                }
                None => {
                    let id = v.and_then(|v| v.int(base_name)).unwrap_or(0);
                    write_varint(out, (id + 1) as i32);
                }
            },
        }
        Ok(())
    }
}

fn len(o: &Json) -> Option<Len> {
    if o["countType"] == "varint" {
        return Some(Len::VarInt);
    }
    if o["countType"] == "i16" {
        return Some(Len::I16);
    }
    o["count"].as_u64().map(|n| Len::Fixed(n as usize))
}

fn field_name(f: &Json) -> Option<Box<str>> {
    f["name"].as_str().map(Box::from)
}

fn parse_int(s: &str) -> Option<i64> {
    match s.strip_prefix("0x") {
        Some(hex) => i64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

fn sibling<'a>(siblings: &'a [(Box<str>, Value)], name: &str) -> Option<&'a Value> {
    siblings.iter().rev().find(|(k, _)| &**k == name).map(|(_, v)| v)
}

fn take<const N: usize>(buf: &mut Cursor<&[u8]>) -> Result<[u8; N], DecodeError> {
    let mut b = [0; N];
    buf.read_exact(&mut b).map_err(|_| DecodeError::Eof)?;
    Ok(b)
}

fn take_vec(buf: &mut Cursor<&[u8]>, n: usize) -> Result<Vec<u8>, DecodeError> {
    let remaining = buf.get_ref().len() - buf.position() as usize;
    if n > remaining {
        return Err(DecodeError::Eof);
    }
    let mut v = vec![0; n];
    buf.read_exact(&mut v).map_err(|_| DecodeError::Eof)?;
    Ok(v)
}

fn read_len(buf: &mut Cursor<&[u8]>, l: &Len) -> Result<usize, DecodeError> {
    match l {
        Len::Fixed(n) => Ok(*n),
        Len::I16 => {
            let n = i16::from_be_bytes(take(buf)?);
            usize::try_from(n).map_err(|_| DecodeError::Length(n.into()))
        }
        Len::VarInt => {
            let n = read_varint(buf)?;
            usize::try_from(n).map_err(|_| DecodeError::Length(n.into()))
        }
    }
}

pub fn read_varint(buf: &mut Cursor<&[u8]>) -> Result<i32, DecodeError> {
    let mut value: u32 = 0;
    for i in 0..5 {
        let b = take::<1>(buf)?[0];
        value |= u32::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(DecodeError::VarInt)
}

pub fn write_varint(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        if v & !0x7f == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
}

/// Reads one NBT payload of tag type `tag` into a [`Value`]: compounds become
/// maps, lists and arrays become lists, numbers become ints and floats.
fn read_nbt(buf: &mut Cursor<&[u8]>, tag: u8, depth: u8) -> Result<Value, DecodeError> {
    if depth > 32 {
        return Err(DecodeError::Nbt(tag));
    }
    let nbt_string = |buf: &mut Cursor<&[u8]>| -> Result<String, DecodeError> {
        let n = u16::from_be_bytes(take(buf)?) as usize;
        // modified UTF-8; the text NBT servers send is plain UTF-8 in practice
        Ok(String::from_utf8_lossy(&take_vec(buf, n)?).into_owned())
    };
    let count = |buf: &mut Cursor<&[u8]>| -> Result<usize, DecodeError> {
        let n = i32::from_be_bytes(take(buf)?);
        usize::try_from(n).map_err(|_| DecodeError::Length(n.into()))
    };
    Ok(match tag {
        1 => Value::Int(i8::from_be_bytes(take(buf)?).into()),
        2 => Value::Int(i16::from_be_bytes(take(buf)?).into()),
        3 => Value::Int(i32::from_be_bytes(take(buf)?).into()),
        4 => Value::Int(i64::from_be_bytes(take(buf)?)),
        5 => Value::Float(f32::from_be_bytes(take(buf)?).into()),
        6 => Value::Float(f64::from_be_bytes(take(buf)?)),
        7 => {
            let n = count(buf)?;
            Value::Bytes(take_vec(buf, n)?)
        }
        8 => Value::Str(nbt_string(buf)?),
        9 => {
            let inner = take::<1>(buf)?[0];
            let n = count(buf)?;
            let mut items = Vec::with_capacity(n.min(256));
            for _ in 0..n {
                items.push(read_nbt(buf, inner, depth + 1)?);
            }
            Value::List(items)
        }
        10 => {
            let mut fields = Vec::new();
            loop {
                let t = take::<1>(buf)?[0];
                if t == 0 {
                    break;
                }
                let name = nbt_string(buf)?;
                fields.push((name.into_boxed_str(), read_nbt(buf, t, depth + 1)?));
            }
            Value::Map(fields)
        }
        11 | 12 => {
            let n = count(buf)?;
            let mut items = Vec::with_capacity(n.min(256));
            for _ in 0..n {
                items.push(if tag == 11 {
                    Value::Int(i32::from_be_bytes(take(buf)?).into())
                } else {
                    Value::Int(i64::from_be_bytes(take(buf)?))
                });
            }
            Value::List(items)
        }
        t => return Err(DecodeError::Nbt(t)),
    })
}
