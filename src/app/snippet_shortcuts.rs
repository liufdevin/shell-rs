use std::collections::BTreeMap;

use gpui_kit::{Global, KeyBinding, Keystroke};
use serde::{Deserialize, Deserializer, Serialize};

use super::InsertSnippet;
use crate::terminal::TERMINAL_KEY_CONTEXT;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetShortcut {
    keys: String,
    #[serde(default)]
    run: bool,
}

impl SnippetShortcut {
    pub fn new(keys: String, run: bool) -> Self {
        Self { keys, run }
    }

    pub fn keys(&self) -> &str {
        &self.keys
    }

    pub fn runs(&self) -> bool {
        self.run
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SnippetShortcuts(BTreeMap<String, SnippetShortcut>);

impl Global for SnippetShortcuts {}

impl<'de> Deserialize<'de> for SnippetShortcuts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        Ok(Self(
            entries
                .into_iter()
                .filter_map(|(id, value)| {
                    let shortcut: SnippetShortcut = serde_json::from_value(value).ok()?;
                    Keystroke::parse(shortcut.keys()).ok()?;
                    Some((id, shortcut))
                })
                .collect(),
        ))
    }
}

impl SnippetShortcuts {
    pub fn get(&self, id: &str) -> Option<&SnippetShortcut> {
        self.0.get(id)
    }

    pub fn set(&mut self, id: String, shortcut: Option<SnippetShortcut>) {
        if let Some(shortcut) = shortcut {
            self.0.insert(id, shortcut);
        } else {
            self.0.remove(&id);
        }
    }

    pub fn uses(&self, keys: &str, except: Option<&str>) -> bool {
        let Ok(pressed) = Keystroke::parse(keys) else {
            return false;
        };
        self.0.iter().any(|(id, shortcut)| {
            Some(id.as_str()) != except
                && Keystroke::parse(shortcut.keys()).is_ok_and(|taken| {
                    pressed.modifiers == taken.modifiers && pressed.key == taken.key
                })
        })
    }

    pub(super) fn bindings(&self) -> Vec<KeyBinding> {
        self.0
            .iter()
            .map(|(id, shortcut)| {
                KeyBinding::new(
                    shortcut.keys(),
                    InsertSnippet(id.clone()),
                    Some(TERMINAL_KEY_CONTEXT),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{SnippetShortcut, SnippetShortcuts};

    #[test]
    fn shortcuts_keep_their_execution_mode_and_ignore_unreadable_entries() {
        let mut shortcuts = SnippetShortcuts::default();
        shortcuts.set(
            "snippet-one".into(),
            Some(SnippetShortcut::new("ctrl-1".into(), true)),
        );
        let mut json = serde_json::to_value(&shortcuts).unwrap();
        json["unreadable"] = serde_json::json!({"keys": 42, "run": false});
        let restored: SnippetShortcuts = serde_json::from_value(json).unwrap();
        assert!(restored.get("snippet-one").unwrap().runs());
        assert!(restored.uses("ctrl-1", None));
        assert!(!restored.uses("ctrl-1", Some("snippet-one")));
        assert!(restored.get("unreadable").is_none());
        assert_eq!(restored, shortcuts);
    }
}
