// Registry of every model resume knows about.
//
// This is the single source of truth used for:
//   - Provider dispatch in summarize.rs (Anthropic vs Ollama)
//   - TUI model-switcher (Shift+Tab cycling through available models)
//
// To add a model: append a ModelInfo entry to MODELS.
// To change the default: update DEFAULT_MODEL in summarize.rs.

use std::time::Duration;

// ── Provider ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    /// Anthropic cloud API — requires ANTHROPIC_API_KEY
    Anthropic,
    /// Local Ollama instance — requires `ollama serve` on localhost:11434
    Ollama,
}

// ── Model metadata ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Exact ID passed to the API (e.g. "claude-haiku-4-5-20251001", "qwen3.5")
    pub id: &'static str,
    /// Short human-readable name for the model indicator bar
    pub display_name: &'static str,
    pub provider: Provider,
    /// One-line description shown in the model-switcher (used by future tooltip/help UI)
    #[allow(dead_code)]
    pub description: &'static str,
}

// ── Model list ────────────────────────────────────────────────────────────────

pub const MODELS: &[ModelInfo] = &[
    // ── Anthropic (remote) ────────────────────────────────────────────────────
    ModelInfo {
        id: "claude-haiku-4-5-20251001",
        display_name: "Haiku",
        provider: Provider::Anthropic,
        description: "Fast · cheap · great for quick briefings",
    },
    ModelInfo {
        id: "claude-sonnet-4-6",
        display_name: "Sonnet",
        provider: Provider::Anthropic,
        description: "Balanced quality and cost",
    },
    ModelInfo {
        id: "claude-opus-4-6",
        display_name: "Opus",
        provider: Provider::Anthropic,
        description: "Best quality · higher cost",
    },
    // ── Ollama (local) ────────────────────────────────────────────────────────
    ModelInfo {
        id: "qwen3.5",
        display_name: "Qwen 3.5",
        provider: Provider::Ollama,
        description: "Local · fast general-purpose",
    },
    ModelInfo {
        id: "qwen3-coder:30b",
        display_name: "Qwen Coder 30B",
        provider: Provider::Ollama,
        description: "Local · strong code understanding",
    },
    ModelInfo {
        id: "deepseek-coder-v2:16b",
        display_name: "DeepSeek Coder V2 16B",
        provider: Provider::Ollama,
        description: "Local · code-focused",
    },
];

// ── Provider lookup ───────────────────────────────────────────────────────────

/// Determine which provider to use for a given model ID.
/// Checks the registry first; falls back to a heuristic for models set via RESUME_MODEL.
pub fn provider_for(model_id: &str) -> Provider {
    if let Some(info) = MODELS.iter().find(|m| m.id == model_id) {
        return info.provider;
    }
    if model_id.starts_with("claude-") {
        Provider::Anthropic
    } else {
        Provider::Ollama
    }
}

// ── Availability checks ───────────────────────────────────────────────────────

/// True if ANTHROPIC_API_KEY is set in the environment.
pub fn anthropic_available() -> bool {
    std::env::var("ANTHROPIC_API_KEY").is_ok()
}

/// Fetch the list of model name strings currently pulled in Ollama.
/// Returns an empty vec if Ollama is unreachable — connection refused is near-instant.
pub async fn ollama_pulled_ids() -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Tag {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct TagsResponse {
        models: Vec<Tag>,
    }

    let Ok(resp) = reqwest::Client::new()
        .get("http://localhost:11434/api/tags")
        .timeout(Duration::from_secs(2))
        .send()
        .await
    else {
        return vec![];
    };

    resp.json::<TagsResponse>()
        .await
        .map(|r| r.models.into_iter().map(|t| t.name).collect())
        .unwrap_or_default()
}

/// Return all models that are currently reachable.
/// Anthropic models require ANTHROPIC_API_KEY; Ollama models must be pulled.
/// An Ollama model matches if its id is a prefix of a pulled name
/// (e.g. "qwen3.5" matches "qwen3.5:latest").
pub fn filter_available(ollama_pulled: &[String]) -> Vec<&'static ModelInfo> {
    MODELS
        .iter()
        .filter(|m| match m.provider {
            Provider::Anthropic => anthropic_available(),
            Provider::Ollama => ollama_pulled.iter().any(|pulled| {
                let pulled_base = pulled.split(':').next().unwrap_or(pulled.as_str());
                let model_base = m.id.split(':').next().unwrap_or(m.id);
                pulled_base == model_base || pulled.starts_with(m.id)
            }),
        })
        .collect()
}
