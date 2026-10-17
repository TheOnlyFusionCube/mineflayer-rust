#[cfg(feature = "mineflayer-buf")]
use std::io::{self, Cursor, Write};
use std::{collections::HashSet, hash::Hash};

#[cfg(feature = "mineflayer-buf")]
use mineflayer_buf::{BufReadError, McBuf, McBufVar};
#[cfg(feature = "mineflayer-buf")]
use mineflayer_chat::FormattedText;

use super::Suggestion;
use crate::context::StringRange;
#[cfg(feature = "mineflayer-buf")]
use crate::suggestion::SuggestionValue;

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Suggestions {
    range: StringRange,
    suggestions: Vec<Suggestion>,
}

impl Suggestions {
    pub fn new(range: StringRange, suggestions: Vec<Suggestion>) -> Self {
        Self { range, suggestions }
    }

    pub fn merge(command: &str, input: &[Suggestions]) -> Self {
        if input.is_empty() {
            return Suggestions::default();
        } else if input.len() == 1 {
            return input[0].clone();
        };

        let mut texts = HashSet::new();
        for suggestions in input {
            texts.extend(suggestions.suggestions.clone());
        }

        Suggestions::create(command, &texts)
    }

    pub fn create(command: &str, suggestions: &HashSet<Suggestion>) -> Self {
        if suggestions.is_empty() {
            return Suggestions::default();
        };
        let mut start = usize::MAX;
        let mut end = usize::MIN;
        for suggestion in suggestions {
            start = suggestion.range.start().min(start);
            end = suggestion.range.end().max(end);
        }
        let range = StringRange::new(start, end);
        let mut texts = HashSet::new();
        for suggestion in suggestions {
            texts.insert(suggestion.expand(command, range));
        }
        let mut sorted = texts.into_iter().collect::<Vec<_>>();

        sorted.sort_by(|a, b| a.value.cmp_ignore_case(&b.value));

        Suggestions {
            range,
            suggestions: sorted,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.suggestions.is_empty()
    }

    pub fn list(&self) -> &[Suggestion] {
        &self.suggestions
    }

    pub fn range(&self) -> StringRange {
        self.range
    }
}

#[cfg(feature = "mineflayer-buf")]
impl McBuf for Suggestions {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        #[derive(McBuf)]
        struct StandaloneSuggestion {
            pub text: String,
            pub tooltip: Option<FormattedText>,
        }

        let start = u32::mc_read_var(buf)? as usize;
        let length = u32::mc_read_var(buf)? as usize;
        let range = StringRange::between(start, start + length);

        // the range of a Suggestion depends on the Suggestions containing it,
        // so we can't just `impl McBuf for Suggestion`
        let mut suggestions = Vec::<StandaloneSuggestion>::mc_read(buf)?
            .into_iter()
            .map(|s| Suggestion {
                value: SuggestionValue::Text(s.text),
                tooltip: s.tooltip.map(|t| t.to_string()),
                range,
            })
            .collect::<Vec<_>>();
        suggestions.sort_by(|a, b| a.value.cmp(&b.value));

        Ok(Suggestions { range, suggestions })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        (self.range.start() as u32).mc_write_var(buf)?;
        (self.range.length() as u32).mc_write_var(buf)?;
        // self.suggestions.mc_write(buf)?;

        self.suggestions
            .iter()
            .map(|s| {
                (
                    s.value.to_string(),
                    s.tooltip.clone().map(FormattedText::from),
                )
            })
            .collect::<Box<_>>()
            .mc_write(buf)?;

        Ok(())
    }
}
