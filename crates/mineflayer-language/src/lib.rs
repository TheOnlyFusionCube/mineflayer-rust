#![doc = include_str!("../README.md")]

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use compact_str::CompactString;

static STORAGE: LazyLock<Box<[(CompactString, CompactString)]>> = LazyLock::new(|| {
    let json =
        serde_json::from_str::<HashMap<CompactString, CompactString>>(include_str!("en_us.json"))
            .unwrap();
    let mut json = json.into_iter().collect::<Vec<_>>();

    // sort by key to make binary search work
    json.sort_by(|a, b| a.0.cmp(&b.0));

    json.into_boxed_slice()
});

/// Get the string for the given translation ID.
///
/// This just does a lookup in Minecraft's `en_us.json` file.
pub fn get(key: &str) -> Option<&str> {
    let key = CompactString::from(key);
    let storage = &*STORAGE;
    // more memory efficient than a hashmap lookup
    let index = storage.binary_search_by(|(k, _)| k.cmp(&key));
    index.ok().map(|i| storage[i].1.as_str())
}

/// A language: translation ids and their strings, such as the contents of
/// a resource pack's `de_de.json`.
pub type Language = Arc<HashMap<String, String>>;

thread_local! {
    static CURRENT: RefCell<Option<Language>> = const { RefCell::new(None) };
}

/// Run `f` with `language` in front of `en_us` for [`translate`] on this
/// thread. `None` leaves only `en_us`.
pub fn with_language<R>(language: Option<&Language>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Language>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|current| *current.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(CURRENT.with(|current| current.replace(language.cloned())));
    f()
}

/// The string for a translation id: from the language of
/// [`with_language`] when it has the id, else from `en_us`.
pub fn translate(key: &str) -> Option<Cow<'static, str>> {
    let own = CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .and_then(|language| language.get(key).cloned())
    });
    own.map(Cow::Owned)
        .or_else(|| get(key).map(|s| Cow::Owned(s.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_comes_before_en_us_and_only_in_its_scope() {
        let german: Language = Arc::new(HashMap::from([(
            "gui.done".to_owned(),
            "Fertig".to_owned(),
        )]));
        assert_eq!(translate("gui.done").as_deref(), Some("Done"));
        with_language(Some(&german), || {
            assert_eq!(translate("gui.done").as_deref(), Some("Fertig"));
            // ids it lacks fall back to en_us
            assert_eq!(translate("gui.cancel").as_deref(), Some("Cancel"));
        });
        assert_eq!(translate("gui.done").as_deref(), Some("Done"));
    }
}
