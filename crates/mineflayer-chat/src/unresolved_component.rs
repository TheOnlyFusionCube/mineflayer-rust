use std::fmt::{self, Display};

use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Map, Value};

use crate::base_component::BaseComponent;

/// The component kinds that only a server, or the client's own settings,
/// can resolve to text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnresolvedKind {
    /// A key binding, such as `key.jump`.
    Keybind,
    /// An entity selector, such as `@p`.
    Selector,
    /// A scoreboard value.
    Score,
    /// A value read from block, entity, or storage NBT.
    Nbt,
    /// A sprite or a player head.
    Object,
}

impl UnresolvedKind {
    /// The key that names the kind, in the order vanilla tests them.
    pub(crate) const ALL: [(&'static str, Self); 5] = [
        ("score", Self::Score),
        ("selector", Self::Selector),
        ("keybind", Self::Keybind),
        ("nbt", Self::Nbt),
        ("object", Self::Object),
    ];

    pub(crate) fn from_fields(fields: &Map<String, Value>) -> Option<Self> {
        if let Some(Value::String(name)) = fields.get("type") {
            return Self::ALL
                .into_iter()
                .find(|(key, _)| key == name)
                .map(|(_, kind)| kind);
        }
        Self::ALL
            .into_iter()
            .find(|(key, _)| fields.contains_key(*key))
            .map(|(_, kind)| kind)
    }
}

/// The keys that belong to the [`BaseComponent`], not to the kind.
pub(crate) const BASE_KEYS: [&str; 13] = [
    "extra",
    "type",
    "color",
    "shadow_color",
    "bold",
    "italic",
    "underlined",
    "strikethrough",
    "obfuscated",
    "click_event",
    "hover_event",
    "insertion",
    "font",
];

/// A keybind, selector, score, nbt, or object component.
///
/// The client cannot resolve these to text by itself, so the component
/// keeps the fields it was sent, and writes them back unchanged. Its text
/// is the fallback that vanilla shows when it cannot resolve the
/// component.
#[derive(Clone, Debug, PartialEq)]
pub struct UnresolvedComponent {
    pub base: BaseComponent,
    pub kind: UnresolvedKind,
    /// The kind's own fields, as received. Style and `extra` are in `base`.
    pub fields: Map<String, Value>,
}

impl UnresolvedComponent {
    /// Keep the fields of `all` that are not base fields.
    pub(crate) fn new(kind: UnresolvedKind, mut all: Map<String, Value>) -> Self {
        all.retain(|key, _| !BASE_KEYS.contains(&key.as_str()));
        Self {
            base: BaseComponent::default(),
            kind,
            fields: all,
        }
    }

    /// The text that vanilla shows for this component when it cannot
    /// resolve it: the key name of a keybind, the pattern of a selector,
    /// the stored value of a score, and nothing for nbt and object.
    pub fn fallback_text(&self) -> &str {
        let value = match self.kind {
            UnresolvedKind::Keybind => self.fields.get("keybind"),
            UnresolvedKind::Selector => self.fields.get("selector"),
            UnresolvedKind::Score => self.fields.get("score").and_then(|s| s.get("value")),
            UnresolvedKind::Nbt | UnresolvedKind::Object => None,
        };
        value.and_then(Value::as_str).unwrap_or_default()
    }
}

impl Display for UnresolvedComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.fallback_text())?;
        for sibling in &self.base.siblings {
            sibling.fmt(f)?;
        }
        Ok(())
    }
}

impl Serialize for UnresolvedComponent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_map(None)?;
        for (key, value) in &self.fields {
            state.serialize_entry(key, value)?;
        }
        self.base.serialize_map::<S>(&mut state)?;
        state.end()
    }
}

#[cfg(feature = "simdnbt")]
impl simdnbt::Serialize for UnresolvedComponent {
    fn to_compound(self) -> simdnbt::owned::NbtCompound {
        let mut compound = simdnbt::owned::NbtCompound::new();
        for (key, value) in self.fields {
            if let Some(tag) = json_to_nbt(value) {
                compound.insert(key, tag);
            }
        }
        compound.extend(self.base.to_compound());
        compound
    }
}

/// JSON to NBT, for the fields of a component that arrived as JSON. A
/// `null` has no NBT form and is dropped.
#[cfg(feature = "simdnbt")]
fn json_to_nbt(value: Value) -> Option<simdnbt::owned::NbtTag> {
    use simdnbt::owned::{NbtCompound, NbtList, NbtTag};

    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => NbtTag::Byte(b.into()),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i32::try_from(i).map_or(NbtTag::Long(i), NbtTag::Int),
            None => NbtTag::Double(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => NbtTag::String(s.into()),
        Value::Array(items) => NbtTag::List(NbtList::from(
            items
                .into_iter()
                .filter_map(json_to_nbt)
                .collect::<Vec<_>>(),
        )),
        Value::Object(map) => {
            let mut compound = NbtCompound::new();
            for (key, value) in map {
                if let Some(tag) = json_to_nbt(value) {
                    compound.insert(key, tag);
                }
            }
            NbtTag::Compound(compound)
        }
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use crate::FormattedText;

    /// Each kind inside the `extra` list of a text component, and the text
    /// vanilla shows for it.
    fn cases() -> Vec<(serde_json::Value, &'static str)> {
        vec![
            (json!({"keybind": "key.jump"}), "key.jump"),
            (json!({"selector": "@p", "separator": {"text": ", "}}), "@p"),
            (
                json!({"score": {"name": "Alex", "objective": "kills", "value": "7"}}),
                "7",
            ),
            (json!({"score": {"name": "Alex", "objective": "kills"}}), ""),
            (
                json!({"nbt": "Items[0]", "block": "1 2 3", "interpret": true}),
                "",
            ),
            (json!({"object": "atlas", "sprite": "item/apple"}), ""),
            (
                json!({"type": "keybind", "keybind": "key.use", "bold": true}),
                "key.use",
            ),
        ]
    }

    fn in_extra(component: &serde_json::Value) -> serde_json::Value {
        json!({"text": "[", "extra": [component, {"text": "]"}]})
    }

    #[test]
    fn json_components_decode_to_their_fallback_text() {
        for (component, text) in cases() {
            let parsed = FormattedText::deserialize(&in_extra(&component))
                .unwrap_or_else(|e| panic!("{component}: {e}"));
            assert_eq!(parsed.to_string(), format!("[{text}]"), "{component}");
        }
    }

    #[cfg(feature = "mineflayer-buf")]
    mod nbt {
        use std::io::Cursor;

        use mineflayer_buf::McBuf;

        use super::*;

        fn nbt_bytes(component: &FormattedText) -> Vec<u8> {
            let mut bytes = Vec::new();
            component.mc_write(&mut bytes).unwrap();
            bytes
        }

        #[test]
        fn nbt_components_decode_to_their_fallback_text() {
            for (component, text) in cases() {
                // the JSON form, written as NBT, is the NBT form a server sends
                let json_form = FormattedText::deserialize(&in_extra(&component)).unwrap();
                let read = FormattedText::mc_read(&mut Cursor::new(&nbt_bytes(&json_form)))
                    .unwrap_or_else(|e| panic!("{component}: {e}"));
                assert_eq!(read.to_string(), format!("[{text}]"), "{component}");
            }
        }

        #[test]
        fn nbt_components_round_trip() {
            for (component, _) in cases() {
                let first = FormattedText::mc_read(&mut Cursor::new(&nbt_bytes(
                    &FormattedText::deserialize(&in_extra(&component)).unwrap(),
                )))
                .unwrap();
                let second = FormattedText::mc_read(&mut Cursor::new(&nbt_bytes(&first))).unwrap();
                assert_eq!(first, second, "{component}");
            }
        }

        #[test]
        fn nbt_that_is_no_component_still_fails() {
            let mut bytes = Vec::new();
            simdnbt::owned::BaseNbt::from(simdnbt::owned::NbtCompound::from_values(vec![(
                "unrelated".into(),
                simdnbt::owned::NbtTag::Int(1),
            )]))
            .write_unnamed(&mut bytes);
            assert!(FormattedText::mc_read(&mut Cursor::new(&bytes)).is_err());
        }
    }
}
