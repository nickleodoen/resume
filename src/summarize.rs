// Calls the Anthropic Messages API to generate a human-readable briefing from
// the current session log. Reads ANTHROPIC_API_KEY from the environment.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::git;
use crate::session::{EventType, Session};

const API_URL: &str = "https://api.anthropic.com/v1/messages";

// ── Model selection ───────────────────────────────────────────────────────────
// Change DEFAULT_MODEL to any Anthropic model ID to switch permanently.
// Override at runtime without recompiling: RESUME_MODEL=<model-id> resume
//
// Cheapest:  claude-haiku-4-5-20251001
// Balanced:  claude-sonnet-4-6
// Best:      claude-opus-4-6
const DEFAULT_MODEL: &str = "claude-haiku-4-5-20251001";

#[derive(Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    system: String,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: String,
}

fn build_prompt(sess: &Session, notes_text: &str, current_diff: Option<String>) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Project: {}", sess.project));
    lines.push(format!(
        "Session started: {}",
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!("Total events: {}", sess.events.len()));
    lines.push(String::new());

    // Group events by type for cleaner signal
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
        // Deduplicate file names for brevity
        let mut seen = std::collections::HashSet::new();
        for ev in &file_changes {
            if seen.insert(&ev.content) {
                lines.push(format!("  {}", ev.content));
            }
        }
        lines.push(String::new());
    }

    // Commits recorded during the session
    if !diffs.is_empty() {
        lines.push("=== Commits made this session ===".to_string());
        for ev in &diffs {
            lines.push(format!("  {}", ev.content));
        }
        lines.push(String::new());
    }

    // Current git diff captured fresh at briefing time (independent of live feed)
    if let Some(diff) = current_diff {
        if !diff.is_empty() {
            lines.push("=== Current uncommitted changes (git diff) ===".to_string());
            lines.push(diff);
            lines.push(String::new());
        }
    }

    // Developer notes — persisted across sessions, highest signal for the briefing
    if !notes_text.trim().is_empty() {
        lines.push("=== Developer notes (manually saved, treat as high-signal context) ===".to_string());
        lines.push(notes_text.to_string());
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Call the Anthropic API and return a developer briefing for the session.
pub async fn generate(sess: &Session, notes_text: &str) -> Result<String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY environment variable not set")?;

    if sess.events.is_empty() {
        return Ok("No events recorded yet. Run `resume` to start a session and do some work first.".to_string());
    }

    // Capture current diff fresh — completely separate from the live feed.
    let cwd = std::env::current_dir().unwrap_or_default();
    let current_diff = git::current_diff(&cwd).ok().filter(|d| !d.is_empty());

    let prompt = build_prompt(sess, notes_text, current_diff);

    let model = std::env::var("RESUME_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    let request = ApiRequest {
        model,
        max_tokens: 2048,
        system: "You are an expert developer assistant helping engineers pick up where they left off.\n\
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
that may not be visible in file changes alone."
            .to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: format!("Here is my session log:\n\n{}", prompt),
        }],
    };

    let client = Client::new();
    let resp = client
        .post(API_URL)
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

    let api_resp: ApiResponse = resp.json().await.context("failed to parse API response")?;
    let text = api_resp
        .content
        .into_iter()
        .next()
        .map(|b| b.text)
        .unwrap_or_else(|| "(no response)".to_string());

    Ok(text)
}
