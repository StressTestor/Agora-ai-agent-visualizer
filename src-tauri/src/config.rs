use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
}

/// Known provider names and their corresponding env var names.
const PROVIDER_ENV_VARS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("opencode", "OPENCODE_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
];

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/tmp"));
    PathBuf::from(home)
        .join(".config")
        .join("agora")
        .join("config.json")
}

impl AppConfig {
    /// Load config from file, then overlay env var overrides.
    pub fn load() -> Self {
        let path = config_path();
        let mut config: AppConfig = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Env vars override file values
        for (provider, env_var) in PROVIDER_ENV_VARS {
            if let Ok(key) = std::env::var(env_var) {
                if !key.is_empty() {
                    config.providers.insert(
                        provider.to_string(),
                        ProviderConfig {
                            api_key: key,
                            enabled: true,
                        },
                    );
                }
            }
        }

        config
    }

    /// Save config to file. Creates parent dirs if needed.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create config dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize config: {e}"))?;
        fs::write(&path, &json).map_err(|e| format!("failed to write config: {e}"))?;

        // Set 600 permissions on unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&path, perms).ok();
        }

        Ok(())
    }

    /// Get the API key for a provider (resolved: env var > file).
    pub fn api_key(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .filter(|p| p.enabled && !p.api_key.is_empty())
            .map(|p| p.api_key.clone())
    }
}
