use std::fs;
use std::path::Path;

use serde_json::Value;

fn selects(manifest: &str) -> bool {
    serde_json::from_str::<Value>(manifest).ok().is_some_and(|manifest| {
        manifest["plugins"].as_array().is_some_and(|plugins| {
            plugins.iter().filter_map(Value::as_str).any(|plugin| plugin.ends_with("@tj-agents"))
        })
    })
}

/// The repository holding `cwd` selects tj-agents plugins in its `.agents/plugins.json`.
pub(crate) fn selected(cwd: &Path) -> bool {
    for folder in cwd.ancestors() {
        if let Ok(manifest) = fs::read_to_string(folder.join(".agents").join("plugins.json")) {
            return selects(&manifest);
        }
        if folder.join(".git").exists() {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::selects;

    #[test]
    fn only_a_manifest_listing_a_tj_agents_plugin_selects_it() {
        assert!(selects(r#"{"plugins":["base@other","agent@tj-agents"]}"#));
        assert!(!selects(r#"{"plugins":["agent@agents"]}"#));
        assert!(!selects(r#"{"marketplaces":{"tj-agents":{}}}"#));
        assert!(!selects(r#"{"plugins":["agent@tj-agents""#));
    }
}
