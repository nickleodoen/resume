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

use std::collections::HashMap;

use crate::git;
use crate::models::{self, Provider};
use crate::session::{EventType, Session, SessionEvent};

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
Given a session log, produce a structured briefing with exactly these three sections:\n\n\
Working on\n\
One sentence describing the feature, bug fix, or improvement in plain English. \
Focus on what it does for the user or developer, not implementation details.\n\n\
Progress\n\
Bullet points (2-4) of what was actually accomplished this session. Start each with '- '. \
Write like a changelog: 'Added X', 'Fixed Y', 'Removed Z'. \
Name the capability or behavior, not the function or struct that implements it. \
After each bullet, append the primary filename in parentheses, e.g. '(src/summarize.rs)'. \
If changes spanned more than one file, append the primary file plus a pill like '(src/summarize.rs +2 more)'.\n\n\
Next step\n\
One actionable sentence describing the next task in plain English. \
If it involves a specific file, end with the filename in parentheses, e.g. '(src/tui.rs)'.\n\n\
Rules:\n\
- Output plain text only — no markdown, no asterisks, no bold syntax\n\
- Section headers are bare words on their own line, nothing else\n\
- Keep total output under 140 words\n\
- NEVER mention function names, struct fields, parameter names, or type names\n\
- NEVER say things like 'updated X() to accept Y parameter' or 'removed Z field'\n\
- DO say things like 'Added per-model timeout support', 'Removed notes from briefing context'\n\
- Git diffs are the highest-priority context — use them to understand what changed\n\
- Claude Code messages reveal developer intent — use them to explain the why behind changes";

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
    // Qwen3 and other thinking models generate a chain-of-thought by default,
    // which inflates generation time massively for complex prompts. Disable it.
    think: bool,
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
}

// ── Prompt builder ────────────────────────────────────────────────────────────

const MAX_CURRENT_DIFF_BYTES: usize = 4_000;
const MAX_FILE_BYTES: usize = 2_000;

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "toml", "yaml", "yml",
    "html", "css", "sh", "md",
];

/// Read snippets of the top 3 most-edited source files from FileChange events.
/// Returns (relative_path, content_snippet) pairs, silently skipping unreadable files.
fn collect_file_snippets(events: &[SessionEvent]) -> Vec<(String, String)> {
    // Count edits per path.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for ev in events {
        if matches!(ev.event_type, EventType::FileChange) {
            *counts.entry(ev.content.as_str()).or_insert(0) += 1;
        }
    }

    // Sort by edit frequency, filter to known source extensions.
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    let mut snippets = Vec::new();
    for (path, _) in ranked {
        if snippets.len() >= 3 {
            break;
        }
        // Extension allowlist — skip binaries, lock files, generated assets, etc.
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !SOURCE_EXTENSIONS.contains(&ext) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        let total = bytes.len();
        let snippet = if total <= MAX_FILE_BYTES {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            // Truncate at the nearest newline at or below the byte limit.
            let cut = bytes[..MAX_FILE_BYTES]
                .iter()
                .rposition(|&b| b == b'\n')
                .unwrap_or(MAX_FILE_BYTES);
            format!(
                "{}\n[truncated — {total} bytes total]",
                String::from_utf8_lossy(&bytes[..cut])
            )
        };
        snippets.push((path.to_string(), snippet));
    }
    snippets
}

fn build_prompt(
    sess: &Session,
    current_diff: Option<String>,
    commits: Vec<(String, String, String)>, // (short_hash, HH:MM, subject)
    file_snippets: Vec<(String, String)>,   // (path, snippet)
    claude_messages: Vec<(i64, String)>,    // (timestamp_ms, display_text)
) -> String {
    let mut lines = Vec::new();

    // ── Metadata ──────────────────────────────────────────────────────────────
    lines.push(format!("Project: {}", sess.project));
    lines.push(format!(
        "Session started: {}",
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!("Total events: {}", sess.events.len()));
    lines.push(String::new());

    // ── Current uncommitted diff (highest priority) ───────────────────────────
    if let Some(diff) = current_diff
        && !diff.is_empty() {
            let capped = if diff.len() > MAX_CURRENT_DIFF_BYTES {
                let cut = diff[..MAX_CURRENT_DIFF_BYTES]
                    .rfind('\n')
                    .unwrap_or(MAX_CURRENT_DIFF_BYTES);
                format!(
                    "{}\n[diff truncated — {} bytes total]",
                    &diff[..cut],
                    diff.len()
                )
            } else {
                diff
            };
            lines.push("=== Current uncommitted changes (git diff — highest priority) ===".to_string());
            lines.push(capped);
            lines.push(String::new());
        }

    // ── Commits this session ──────────────────────────────────────────────────
    // Primary source: git_log_since() — authoritative at show time.
    // Fallback: EventType::GitDiff events (commit messages captured reactively by watcher).
    let git_commit_events: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::GitDiff))
        .collect();

    if !commits.is_empty() {
        lines.push("=== Commits this session ===".to_string());
        for (hash, time, subject) in &commits {
            lines.push(format!("  [{time}] {hash} {subject}"));
        }
        lines.push(String::new());
    } else if !git_commit_events.is_empty() {
        // No git repo accessible — fall back to reactively captured messages.
        lines.push("=== Commits this session ===".to_string());
        for ev in &git_commit_events {
            lines.push(format!("  [{}] {}", ev.timestamp.format("%H:%M"), ev.content));
        }
        lines.push(String::new());
    }

    // ── Claude Code messages ──────────────────────────────────────────────────
    if !claude_messages.is_empty() {
        lines.push("=== Claude Code conversation (what the developer was asking) ===".to_string());
        for (ts_ms, text) in &claude_messages {
            // Convert ms timestamp to HH:MM
            let secs = ts_ms / 1000;
            let dt = chrono::DateTime::from_timestamp(secs, 0)
                .unwrap_or_default()
                .format("%H:%M")
                .to_string();
            lines.push(format!("  [{dt}] {text}"));
        }
        lines.push(String::new());
    }

    // ── Key file contents ─────────────────────────────────────────────────────
    if !file_snippets.is_empty() {
        lines.push("=== Key file contents (most-edited, truncated) ===".to_string());
        for (path, snippet) in &file_snippets {
            lines.push(format!("--- {path} ---"));
            lines.push(snippet.clone());
            lines.push(String::new());
        }
    }

    // ── Shell commands ────────────────────────────────────────────────────────
    let commands: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::Command))
        .collect();
    if !commands.is_empty() {
        lines.push("=== Shell commands (chronological) ===".to_string());
        for ev in &commands {
            lines.push(format!("  [{}] {}", ev.timestamp.format("%H:%M:%S"), ev.content));
        }
        lines.push(String::new());
    }

    // ── Files touched ─────────────────────────────────────────────────────────
    // Deduplicated quick reference; skip files already in the snippets section.
    let snippet_paths: std::collections::HashSet<&str> =
        file_snippets.iter().map(|(p, _)| p.as_str()).collect();
    let file_changes: Vec<_> = sess
        .events
        .iter()
        .filter(|e| matches!(e.event_type, EventType::FileChange))
        .collect();
    if !file_changes.is_empty() {
        let mut seen = std::collections::HashSet::new();
        let remaining: Vec<_> = file_changes
            .iter()
            .filter(|ev| seen.insert(ev.content.as_str()) && !snippet_paths.contains(ev.content.as_str()))
            .collect();
        if !remaining.is_empty() {
            lines.push("=== Files touched ===".to_string());
            for ev in remaining {
                lines.push(format!("  {}", ev.content));
            }
            lines.push(String::new());
        }
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
        think: false,
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

const MAX_CLAUDE_MESSAGES: usize = 20;
const MAX_CLAUDE_MESSAGE_CHARS: usize = 300;

/// Read recent Claude Code user messages for the current project from ~/.claude/history.jsonl.
/// Returns (timestamp_ms, display_text) pairs, filtered to the session timeframe.
fn collect_claude_code_messages(
    project_path: &str,
    since: &chrono::DateTime<chrono::Utc>,
) -> Vec<(i64, String)> {
    let home = dirs::home_dir().unwrap_or_default();
    let history_path = home.join(".claude").join("history.jsonl");
    let Ok(contents) = std::fs::read_to_string(&history_path) else {
        return vec![];
    };

    let since_ms = since.timestamp_millis();

    let mut messages: Vec<(i64, String)> = contents
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            let proj = v.get("project")?.as_str()?;
            if proj != project_path {
                return None;
            }
            let ts = v.get("timestamp")?.as_i64()?;
            if ts < since_ms {
                return None;
            }
            let display = v.get("display")?.as_str()?;
            // Skip slash commands and very short messages
            if display.starts_with('/') || display.len() < 10 {
                return None;
            }
            // Truncate long messages
            let text = if display.len() > MAX_CLAUDE_MESSAGE_CHARS {
                format!("{}…", &display[..MAX_CLAUDE_MESSAGE_CHARS])
            } else {
                display.to_string()
            };
            Some((ts, text))
        })
        .collect();

    messages.sort_by_key(|(ts, _)| *ts);
    // Take the most recent N
    let len = messages.len();
    if len > MAX_CLAUDE_MESSAGES {
        messages = messages.into_iter().skip(len - MAX_CLAUDE_MESSAGES).collect();
    }
    messages
}

/// Generate a developer briefing for the session using the given model ID.
pub async fn generate(sess: &Session, model_id: &str) -> Result<String> {
    if sess.events.is_empty() {
        return Ok(
            "No events recorded yet. Run `resume` to start a session and do some work first."
                .to_string(),
        );
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let current_diff = git::current_diff(&cwd).ok().filter(|d| !d.is_empty());
    let commits = git::git_log_since(&cwd, &sess.started_at);
    let file_snippets = collect_file_snippets(&sess.events);
    let claude_messages = collect_claude_code_messages(
        cwd.to_str().unwrap_or_default(),
        &sess.started_at,
    );
    let prompt = build_prompt(sess, current_diff, commits, file_snippets, claude_messages);
    let user_content = format!("Here is my session log:\n\n{}", prompt);

    let raw = match models::provider_for(model_id) {
        Provider::Anthropic => call_anthropic(model_id, &user_content).await,
        Provider::Ollama => call_ollama(model_id, &user_content).await,
    }?;
    Ok(normalize_briefing(&raw))
}

/// Normalize briefing text so section headers are always immediately followed by
/// their content, with no intervening blank line. Different models emit different
/// amounts of whitespace; this makes the output look the same regardless of model.
fn normalize_briefing(text: &str) -> String {
    const HEADERS: &[&str] = &["Working on", "Progress", "Next step"];
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line);
        // If this line is a section header, skip any immediately following blank lines
        if HEADERS.iter().any(|h| line.trim() == *h) {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out.join("\n")
}
