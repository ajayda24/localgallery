//! User settings, stored in SQLite. Unknown keys are kept, so newer files load in older apps.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub planner: lg_planner::Settings,
    /// Download missing model files automatically when a job needs them.
    pub auto_download: bool,
    /// Hugging Face access token for gated models (never logged, never sent elsewhere).
    pub hf_token: String,
    /// Extra model folders to use in place (e.g. an existing ComfyUI `models` folder).
    pub extra_model_dirs: Vec<String>,
    /// Group queued jobs by model so models are not reloaded between jobs.
    pub group_by_model: bool,
    /// Live preview while sampling.
    pub previews: bool,
    pub theme: String,
    pub ui_level: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            planner: lg_planner::Settings::default(),
            auto_download: true,
            hf_token: String::new(),
            extra_model_dirs: vec![],
            group_by_model: true,
            previews: true,
            theme: "system".into(),
            ui_level: "simple".into(),
        }
    }
}

impl Settings {
    pub fn load(store: &lg_store::Store) -> Self {
        store
            .setting("settings")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, store: &lg_store::Store) -> anyhow::Result<()> {
        store.set_setting("settings", &serde_json::to_value(self)?)
    }

    /// The settings as the UI sees them: the token is replaced by a flag.
    pub fn redacted(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        v["hf_token"] = serde_json::Value::String(String::new());
        v["hf_token_set"] = serde_json::Value::Bool(!self.hf_token.is_empty());
        v
    }
}
