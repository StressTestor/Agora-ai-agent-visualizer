use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Provider to use for topic enhancement and other single-shot AI calls.
    /// Empty string means auto (falls back to priority list).
    #[serde(default)]
    pub enhance_provider: String,
    /// Model to use for enhancement calls. Empty means use the provider default.
    #[serde(default)]
    pub enhance_model: String,
}

/// Known provider names and their corresponding env var names.
const PROVIDER_ENV_VARS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("opencode", "OPENCODE_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("gemini", "GEMINI_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("moonshot", "MOONSHOT_API_KEY"),
    ("minimax", "MINIMAX_API_KEY"),
    ("minimax-coding", "MINIMAX_CODING_API_KEY"),
    ("zai", "ZAI_API_KEY"),
    ("zai-coding", "ZAI_CODING_API_KEY"),
];

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("/tmp"));
    PathBuf::from(home)
        .join(".config")
        .join("agora")
        .join("config.json")
}

impl AppConfig {
    /// Load config without hiding unreadable or malformed existing files.
    pub fn try_load() -> Result<Self, String> {
        Self::load_from_path(&config_path(), |name| std::env::var(name).ok())
    }

    /// Retain compatibility for CLI/GUI callers while reporting load failures.
    /// Saving later preserves malformed input before replacing it.
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "{error}; using defaults and environment settings until config is repaired"
                );
                let mut config = Self::default();
                config.overlay_env(|name| std::env::var(name).ok());
                config
            }
        }
    }

    fn load_from_path(path: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut config: Self = match fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|error| {
                format!(
                    "failed to parse config {} at line {}, column {} ({:?})",
                    path.display(),
                    error.line(),
                    error.column(),
                    error.classify()
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(format!("failed to read config {}: {error}", path.display())),
        };
        config.overlay_env(env);
        Ok(config)
    }

    fn overlay_env(&mut self, env: impl Fn(&str) -> Option<String>) {
        for (provider, env_var) in PROVIDER_ENV_VARS {
            if let Some(key) = env(env_var).filter(|key| !key.trim().is_empty()) {
                self.providers.insert(
                    provider.to_string(),
                    ProviderConfig {
                        api_key: key.trim().to_string(),
                        enabled: true,
                    },
                );
            }
        }
    }

    /// Save atomically, preserving a malformed previous config for recovery.
    pub fn save(&self) -> Result<(), String> {
        self.save_to_path(&config_path())
    }

    fn save_to_path(&self, path: &Path) -> Result<(), String> {
        match fs::read(path) {
            Ok(raw) if serde_json::from_slice::<Self>(&raw).is_err() => {
                let (backup_path, mut backup) = create_unique_file(path, "corrupt")?;
                if let Err(error) = backup.write_all(&raw).and_then(|_| backup.sync_all()) {
                    drop(backup);
                    remove_temporary(&backup_path);
                    return Err(format!("failed to preserve invalid config: {error}"));
                }
                eprintln!("preserved invalid config at {}", backup_path.display());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "refusing to replace unreadable config {}: {error}",
                    path.display()
                ))
            }
        }
        write_json_atomic(path, self)
    }

    /// Get the API key for a provider (resolved: env var > file).
    pub fn api_key(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .filter(|p| p.enabled && !p.api_key.is_empty())
            .map(|p| p.api_key.clone())
    }
}

static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_unique_file(path: &Path, purpose: &str) -> Result<(PathBuf, File), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| "output path needs a filename".to_string())?;
    for _ in 0..100 {
        let mut filename = name.to_os_string();
        filename.push(format!(
            ".{purpose}-{}-{}",
            std::process::id(),
            FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let candidate = parent.join(filename);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("failed to create {}: {error}", candidate.display())),
        }
    }
    Err(format!(
        "could not allocate a unique file beside {}",
        path.display()
    ))
}

fn remove_temporary(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "could not remove temporary file {}: {error}",
                path.display()
            );
        }
    }
}

/// Replace JSON only after a complete private file has been written and synced.
pub(crate) fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("failed to serialize {}: {error}", path.display()))?;
    let (temporary_path, mut file) = create_unique_file(path, "tmp")?;
    let result = file.write_all(&bytes).and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = result {
        remove_temporary(&temporary_path);
        return Err(format!("failed to write {}: {error}", path.display()));
    }
    if let Err(error) = fs::rename(&temporary_path, path) {
        remove_temporary(&temporary_path);
        return Err(format!("failed to replace {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "agora-config-test-{}-{}",
                std::process::id(),
                FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn config(&self) -> PathBuf {
            self.0.join("config.json")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_json_replaces_complete_files_with_private_permissions() {
        let scratch = Scratch::new();
        let path = scratch.config();
        fs::write(&path, "old").unwrap();
        write_json_atomic(&path, &serde_json::json!({"new": true})).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
            serde_json::json!({"new": true})
        );
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_serialization_and_rename_preserve_existing_data_and_clean_temps() {
        struct Unserializable;
        impl serde::Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("synthetic serialization failure"))
            }
        }
        let scratch = Scratch::new();
        let path = scratch.config();
        fs::write(&path, "preserve").unwrap();
        assert!(write_json_atomic(&path, &Unserializable).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "preserve");
        let directory = scratch.0.join("existing-directory");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("preserve"), "content").unwrap();
        assert!(write_json_atomic(&directory, &true).is_err());
        assert_eq!(
            fs::read_to_string(directory.join("preserve")).unwrap(),
            "content"
        );
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
    }

    #[test]
    fn malformed_config_errors_are_sanitized_and_saved_original_is_recoverable() {
        let scratch = Scratch::new();
        let path = scratch.config();
        let original = br#"{"providers":{"openai":{"api_key":"SYNTHETIC_PRIVATE_VALUE","enabled":"SYNTHETIC_PRIVATE_VALUE"}}}"#;
        fs::write(&path, original).unwrap();
        let error = AppConfig::load_from_path(&path, |_| None).unwrap_err();
        assert!(error.contains("line"));
        assert!(!error.contains("SYNTHETIC_PRIVATE_VALUE"));
        AppConfig::default().save_to_path(&path).unwrap();
        let backup = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".corrupt-")
            })
            .unwrap();
        assert_eq!(fs::read(&backup).unwrap(), original);
        assert!(AppConfig::load_from_path(&path, |_| None).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn missing_config_is_default_but_unreadable_config_is_not_overwritten() {
        let scratch = Scratch::new();
        assert!(AppConfig::load_from_path(&scratch.config(), |_| None)
            .unwrap()
            .providers
            .is_empty());
        fs::create_dir(scratch.config()).unwrap();
        assert!(AppConfig::load_from_path(&scratch.config(), |_| None).is_err());
        assert!(AppConfig::default()
            .save_to_path(&scratch.config())
            .is_err());
        assert!(scratch.config().is_dir());
    }

    #[test]
    fn environment_overlay_includes_gemini_and_ignores_empty_values() {
        let scratch = Scratch::new();
        let mut stored = AppConfig::default();
        stored.providers.insert(
            "openai".into(),
            ProviderConfig {
                api_key: "file-value".into(),
                enabled: true,
            },
        );
        stored.save_to_path(&scratch.config()).unwrap();
        let loaded = AppConfig::load_from_path(&scratch.config(), |name| match name {
            "GEMINI_API_KEY" => Some(" synthetic-gemini ".into()),
            "OPENAI_API_KEY" => Some("  ".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            loaded.api_key("gemini").as_deref(),
            Some("synthetic-gemini")
        );
        assert_eq!(loaded.api_key("openai").as_deref(), Some("file-value"));
    }
}
