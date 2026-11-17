//! "Developer: all commands" (U.14, SPEC §5.15): every registered command as a read-only
//! catalogue for the palette — name, version, what it does, its scope, exec and undo
//! class, where the UI offers it, whether only a human may run it, and its first
//! example. The palette never runs these (its own actions are the app's buttons); the
//! catalogue is for finding the command an agent or `gmclip` would use. Owner commands
//! are not registered (R-OWN-1), so they are not here either.

use gmclip_core::command::{Exec, UiMeta, UndoClass};
use serde_json::{Value, json};

pub fn catalog() -> Value {
    let mut rows: Vec<Value> = gmclip_core::registry::REGISTRY
        .iter()
        .map(|c| {
            let m = c.meta();
            json!({
                "name": m.name,
                "version": m.version,
                "doc": m.doc,
                "scope": m.scope.name(),
                "exec": match m.exec { Exec::Sync => "sync", Exec::Job => "job" },
                "undo": match m.undo {
                    UndoClass::Undoable => "undoable",
                    UndoClass::ReadOnly => "read-only",
                    UndoClass::Destructive => "destructive",
                },
                "ui": match m.ui { UiMeta::Hidden => Value::Null, UiMeta::Action { label, menu } => json!({"label": label, "menu": menu}) },
                "human_only": gmclip_core::registry::human_only(m.name),
                "example": m.examples.first().and_then(|e| serde_json::from_str::<Value>(e).ok()),
            })
        })
        .collect();
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({"commands": rows})
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_registered_command_is_listed_with_its_doc_and_example() {
        let v = super::catalog();
        let rows = v["commands"].as_array().unwrap();
        assert_eq!(rows.len(), gmclip_core::registry::REGISTRY.len());
        let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "sorted by name");
        for r in rows {
            assert!(!r["doc"].as_str().unwrap().is_empty(), "{r}");
            assert!(
                r["example"].is_object(),
                "R-CMD-1: an example per command: {r}"
            );
        }
        assert!(names.contains(&"words_strike"));
    }
}
