// Generates a human-readable briefing from the session log.
// Supports two providers:
//   - Anthropic (cloud) via the Messages API — requires ANTHROPIC_API_KEY
//   - Ollama (local)    via the Chat API     — requires `ollama serve` on :11434
//
// Provider is selected automatically based on the model ID (see models::provider_for).
// Override the model at runtime: RESUME_MODEL=<model-id> resume show

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::models::{self, Provider};
use crate::session::{EventType, Session};

// ── Endpoints ─────────────────────────────────────────────────────────────────

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const OLLAMA_URL: &str = "http://localhost:11434/api/chat";

// ── Model selection ───────────────────────────────────────────────────────────
// Change DEFAULT_MODEL to any ID from src/models.rs to switch permanently.
// Override at runtime without recompiling: RESUME_MODEL=<model-id> resume show
//
// Anthropic:  claude-haiku-4-5-20251001 · claude-sonnet-4-6 · claude-opus-4-6
// Ollama:     qwen3.5 · qwen3-coder:30b · deepseek-coder-v2:16b
pub const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

// ── Shared system prompt ──────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = "\
You are an expert developer assistant helping engineers pick up where they left off.\n\
Given a session log of shell commands, file changes, git diffs, and optional developer notes, \
produce a structured briefing with exactly these four sections:\n\n\
**What I was working on**\n\
One or two sentences naming the feature, bug, or task. Be specific — use actual file names and \
function/component names from the log.\n\n\
**Progress made**\n\
Bullet points (2–4) of concrete things that were completed or changed this session.\n\n\
**Where I left off**\n\
One sentence describing the exact state of things at the end of the session — what was in flight, \
what was broken, or what was about to happen next.\n\n\
**Recommended next step**\n\
One actionable sentence telling the developer exactly what to do first when they sit back down.\n\n\
Rules: be concrete, use real names from the log, skip generic filler, keep total output under 200 words. \
If developer notes are present, treat them as high-priority context — they capture intent and decisions \
that may not be visible in file changes alone.";

// ── Anthropic types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicBlock>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    text: String,
}

// ── Ollama types ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
}

// ── Prompt builder ────────────────────────────────────────────────────────────

fn build_prompt(sess: &Session, notes_text: &str, current_diff: Option<String>) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Project: {}", sess.project));
    lines.push(format!(
        "Session started: {}",
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!("Total events: {}", sess.events.len()));
    lines.push(String::new());

    let commands: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::Command))
        .collect();
    let file_changes: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::FileChange))
        .collect();
    let diffs: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::GitDiff))
        .collect();

    if !commands.is_empty() {
        lines.push("=== Shell commands (chronological) ===".to_string());
        for ev in &commands {
            lines.push(format!("  [{}] {}", ev.timestamp.format("%H:%M:%S"), ev.content));
        }
        lines.push(String::new());
    }

    if !file_changes.is_empty() {
        lines.push("=== Files touched ===".to_string());
        let mut seen = std::collections::HashSet::new();
        for ev in &file_changes {
            if seen.insert(&ev.content) {
                lines.push(format!("  {}", ev.content));
            }
        }
        lines.push(String::new());
    }

    if !diffs.is_empty() {
        lines.push("=== Commits made this session ===".to_string());
        for ev in &diffs {
            lines.push(format!("  {}", ev.content));
        }
        lines.push(String::new());
    }

    if let Some(diff) = current_diff {
        if !diff.is_empty() {
            lines.push("=== Current uncommitted changes (git diff) ===".to_string());
            lines.push(diff);
            lines.push(String::new());
        }
    }

    if !notes_text.trim().is_empty() {
        lines.push(
            "=== Developer notes (manually saved, treat as high-signal context) ==="
                .to_string(),
        );
        lines.push(notes_text.to_string());
        lines.push(String::new());
    }

    lines.join("\n")
}

// ── Provider implementations ──────────────────────────────────────────────────

async fn call_anthropic(model: &str, user_content: &str) -> Result<String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY environment variable not set")?;

    let request = AnthropicRequest {
        model: model.to_string(),
        max_tokens: 2048,
        system: SYSTEM_PROMPT.to_string(),
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: user_content.to_string(),
        }],
    };

    let client = Client::new();
    let resp = client
        .post(ANTHROPIC_URL)
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .context("failed to call Anthropic API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Anthropic API error {status}: {body}");
    }

    let parsed: AnthropicResponse = resp.json().await.context("failed to parse Anthropic response")?;
    Ok(parsed
        .content
        .into_iter()
        .next()
        .map(|b| b.text)
        .unwrap_or_else(|| "(no response)".to_string()))
}

async fn call_ollama(model: &str, user_content: &str) -> Result<String> {
    let request = OllamaRequest {
        model: model.to_string(),
        messages: vec![
            OllamaMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            OllamaMessage {
                role: "user".to_string(),
                content: user_content.to_string(),
            },
        ],
        stream: false,
    };

    let client = Client::new();
    let resp = client
        .post(OLLAMA_URL)
        .json(&request)
        .send()
        .await
        .context("failed to reach Ollama — is `ollama serve` running on localhost:11434?")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Ollama error {status}: {body}");
    }

    let parsed: OllamaResponse = resp.json().await.context("failed to parse Ollama response")?;
    Ok(parsed.message.content)
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Generate a developer briefing for the session using the given model ID.
pub async fn generate(sess: &Session, notes_text: &str, model_id: &str) -> Result<String> {
    if sess.events.is_empty() {
        return Ok(
            "No events recorded yet. Run `resume` to start a session and do some work first."
                .to_string(),
        );
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let current_diff = git::current_diff(&cwd).ok().filter(|d| !d.is_empty());
    let prompt = build_prompt(sess, notes_text, current_diff);
    let user_content = format!("Here is my session log:\n\n{}", prompt);

    match models::provider_for(model_id) {
        Provider::Anthropic => call_anthropic(model_id, &user_content).await,
        Provider::Ollama => call_ollama(model_id, &user_content).await,
    }
}
