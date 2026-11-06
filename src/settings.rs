use crate::discovery::{atomic_write, config_dir};
use crate::models::{resolve_bin_path, ModelOption};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HarnessSettings {
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<ModelOption>,
}
impl Default for HarnessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            models: vec![],
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub harnesses: BTreeMap<String, HarnessSettings>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<ModelOption>,
    pub updated_at: Option<String>,
    pub error: Option<String>,
}
pub fn load() -> Settings {
    std::fs::read(config_dir().join("settings.json"))
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
pub fn catalogs() -> BTreeMap<String, Catalog> {
    std::fs::read(config_dir().join("catalogs.json"))
        .ok()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
pub fn save(settings: &Settings) -> Result<(), String> {
    for (id, config) in &settings.harnesses {
        if !crate::profiles::is_harness(id) {
            return Err("Unsupported harness adapter".into());
        }
        if config.models.len() > 100 {
            return Err("At most 100 custom models per harness".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for m in &config.models {
            if !valid_model_id(&m.id) || !seen.insert(&m.id) || m.id == "default" {
                return Err(
                    "Model IDs must be unique provider IDs (letters, digits, / : . _ -)".into(),
                );
            }
            if m.label.trim().is_empty() || m.label.len() > 160 {
                return Err("Model label is required (max 160 characters)".into());
            }
            if !valid_effort(&m.default_effort) || m.efforts.iter().any(|e| !valid_effort(e)) {
                return Err("Unsupported reasoning effort".into());
            }
            if let Some(url) = m
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
            {
                if !valid_base_url(url) {
                    return Err("Base URL must start with http:// or https://".into());
                }
            }
            if let Some(name) = m
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
            {
                if !valid_env_name(name) {
                    return Err("API key environment variable is not a valid name".into());
                }
            }
        }
    }
    let path = config_dir().join("settings.json");
    if path.exists() {
        std::fs::copy(&path, path.with_extension("json.bak")).map_err(|e| e.to_string())?;
    }
    atomic_write(
        &path,
        &serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/:-._".contains(&b))
}
fn valid_effort(e: &str) -> bool {
    matches!(
        e,
        "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}
fn valid_base_url(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() <= 300
        && !url
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b < 0x20 || b == b'"' || b == b'\'')
}
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
pub fn default_model() -> ModelOption {
    ModelOption {
        id: "default".into(),
        label: "Configured default".into(),
        default_effort: "default".into(),
        efforts: vec!["default".into()],
        base_url: None,
        api_key_env: None,
        thinking: None,
    }
}
/// Model choices MoE Deck knows without asking the harness. Keel is the only
/// harness that needs provider metadata (OpenAI-compatible base URL), so its
/// local ternary and localhost models are listed here. Custom models saved in
/// settings override these by id.
pub fn builtin_models(harness: &str) -> Vec<ModelOption> {
    if harness != "keel" {
        return Vec::new();
    }
    let local = |id: &str, label: &str, port: u16, thinking: bool| ModelOption {
        id: id.into(),
        label: label.into(),
        default_effort: "default".into(),
        efforts: vec!["default".into()],
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        api_key_env: None,
        thinking: Some(thinking),
    };
    vec![
        local("bonsai-9b", "Bonsai 2 9B (Local)", 8080, false),
        local("bonsai-4b", "Bonsai 2 4B (Local)", 8080, false),
        local("bonsai-2b", "Bonsai 2 2B (Local)", 8080, false),
        local("bonsai-27b", "Bonsai 2 27B (Local)", 8080, false),
        local("qwen-27b", "Qwen 3.8 27B (Local)", 8080, true),
        local("kat-coder", "Kat Coder (Local :8085)", 8085, true),
        ModelOption {
            id: "deepseek-chat".into(),
            label: "DeepSeek V3 (API · DEEPSEEK_API_KEY)".into(),
            default_effort: "default".into(),
            efforts: vec!["default".into()],
            base_url: Some("https://api.deepseek.com".into()),
            api_key_env: Some("DEEPSEEK_API_KEY".into()),
            thinking: None,
        },
    ]
}
pub fn native_models(harness: &str) -> Vec<ModelOption> {
    let mut models = Vec::new();
    if harness == "keel" {
        // Put a working local model first so switching to Keel does not
        // silently fall back to a paid API with no key configured.
        models.extend(builtin_models(harness));
    } else {
        models.push(default_model());
    }
    if let Some(c) = catalogs().get(harness) {
        for model in &c.models {
            if !models.iter().any(|m| m.id == model.id) {
                models.push(model.clone());
            }
        }
    }
    if harness == "keel" {
        models.push(default_model());
    }
    models
}
/// The effective model list for a harness: built-ins, discovered catalog and
/// user overrides, in that order (later entries win).
pub fn resolved_models(harness: &str) -> Vec<ModelOption> {
    let mut models = native_models(harness);
    if let Some(config) = load().harnesses.get(harness) {
        for model in &config.models {
            models.retain(|m| m.id != model.id);
            models.push(model.clone());
        }
    }
    models
}
pub fn find_model(harness: &str, id: &str) -> Option<ModelOption> {
    resolved_models(harness).into_iter().find(|m| m.id == id)
}
pub fn apply(harnesses: &mut [crate::models::HarnessInfo]) {
    let settings = load();
    for h in harnesses {
        // Never advertise a guessed installed version.
        h.version.clear();
        if let Some(config) = settings.harnesses.get(&h.id) {
            for model in &config.models {
                h.models.retain(|m| m.id != model.id);
                h.models.push(model.clone());
            }
        }
    }
}

pub fn parse_native(harness: &str, output: &str) -> Vec<ModelOption> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (id, label) = if harness == "agy" {
                line.split_once('\t')?
            } else {
                (line, line)
            };
            if !valid_model_id(id) || (harness == "opencode" && !id.contains('/')) {
                return None;
            }
            Some(ModelOption {
                id: id.into(),
                label: label.into(),
                default_effort: "default".into(),
                efforts: vec!["default".into()],
                base_url: None,
                api_key_env: None,
                thinking: None,
            })
        })
        .collect()
}
/// Explicit native model-list commands only; never run a prompt or install a binary.
pub async fn refresh() -> Result<(), String> {
    static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REFRESH_LOCK.lock().await;
    let mut cache = catalogs();
    for harness in ["agy", "opencode"] {
        let Some(binary) = resolve_bin_path(harness) else {
            continue;
        };
        let mut cmd = tokio::process::Command::new(binary);
        cmd.arg("models")
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null());
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), cmd.output()).await;
        let entry = cache.entry(harness.into()).or_default();
        match result {
            Ok(Ok(output)) if output.status.success() => {
                let models = parse_native(harness, &String::from_utf8_lossy(&output.stdout));
                if models.is_empty() {
                    entry.error = Some(
                        "Native command returned no recognized models; kept last catalog".into(),
                    );
                } else {
                    entry.models = models;
                    entry.updated_at = Some(chrono::Utc::now().to_rfc3339());
                    entry.error = None;
                }
            }
            _ => {
                entry.error =
                    Some("Native model discovery failed or timed out; kept last catalog".into());
            }
        }
    }
    atomic_write(
        &config_dir().join("catalogs.json"),
        &serde_json::to_string(&cache).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keel_models_lead_with_a_working_local_provider() {
        let models = native_models("keel");
        assert_eq!(models[0].id, "bonsai-9b");
        assert_eq!(
            models[0].base_url.as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(models[0].thinking, Some(false));
        assert!(models.iter().any(|m| m.id == "bonsai-2b"));
        assert!(models.iter().any(|m| m.id == "deepseek-chat"));
        // The deliberate "let the harness decide" choice stays available.
        assert_eq!(models.last().map(|m| m.id.as_str()), Some("default"));
    }

    #[test]
    fn rejects_unsafe_provider_urls_and_env_names() {
        assert!(valid_base_url("http://127.0.0.1:8080/v1"));
        assert!(valid_base_url("https://api.example.com/v1"));
        assert!(!valid_base_url("file:///etc/passwd"));
        assert!(!valid_base_url("http://host/v1 --yolo"));
        assert!(valid_env_name("DEEPSEEK_API_KEY"));
        assert!(!valid_env_name("BAD-NAME"));
        assert!(!valid_env_name(""));
    }

    #[test]
    fn preserves_exact_native_model_ids_and_excludes_progress() {
        let models = parse_native("agy", "Fetching available models...\ngemini-4-argon-high\tGemini 4 Argon (High)\nclaude-opus-5-5-medium\tClaude Opus 5.5 (Medium)");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gemini-4-argon-high");
        assert_eq!(
            parse_native("opencode", "opencode-go/glm-5.3\ninvalid heading")[0].id,
            "opencode-go/glm-5.3"
        );
        assert!(!valid_model_id("--model=x;echo bad"));
    }
}
