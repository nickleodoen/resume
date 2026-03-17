// Calls the Anthropic Messages API to generate a human-readable briefing from
// the current session log. Reads ANTHROPIC_API_KEY from the environment.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::session::{EventType, Session};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-6";

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

fn build_prompt(sess: &Session) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Project: {}", sess.project));
    lines.push(format!(
        "Session started: {}",
        sess.started_at.format("%Y-%m-%d %H:%M:%S UTC")
    ));
    lines.push(format!("Total events: {}", sess.events.len()));
    lines.push(String::new());

    for event in &sess.events {
        let kind = match event.event_type {
            EventType::FileChange => "File changed",
            EventType::GitDiff => "Git diff",
            EventType::Command => "Command run",
        };
        lines.push(format!(
            "[{}] {}: {}",
            event.timestamp.format("%H:%M:%S"),
            kind,
            event.content
        ));
    }

    lines.join("\n")
}

/// Call the Anthropic API and return a developer briefing for the session.
pub async fn generate(sess: &Session) -> Result<String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY environment variable not set")?;

    if sess.events.is_empty() {
        return Ok("No events recorded yet. Start a session with `resume start` and do some work first.".to_string());
    }

    let prompt = build_prompt(sess);

    let request = ApiRequest {
        model: MODEL.to_string(),
        max_tokens: 1024,
        system: "You are a developer assistant. Given a log of file changes and git diffs from a \
coding session, produce a concise briefing (3-6 sentences) that explains: \
(1) what the developer was working on, \
(2) where they left off or got stuck, \
(3) the most likely next step. \
Be concrete and specific. Refer to actual file names and concepts from the log."
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
