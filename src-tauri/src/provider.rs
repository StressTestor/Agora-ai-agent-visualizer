use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
}

#[derive(Debug)]
pub enum ProviderError {
    Network(String),
    Auth(String),
    RateLimit(String),
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Auth(e) => write!(f, "auth error: {e}"),
            Self::RateLimit(e) => write!(f, "rate limit: {e}"),
            Self::Other(e) => write!(f, "provider error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

pub trait Provider: Send + Sync {
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError>;
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;

    /// Stream response chunks by calling `on_chunk` for each partial text fragment.
    /// Returns the complete accumulated text when done.
    /// Default implementation falls back to `chat()` without calling `on_chunk`.
    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        model: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<String, ProviderError> {
        let _ = on_chunk; // fallback: suppress unused warning
        self.chat(messages, model)
    }
}

// ---------------------------------------------------------------------------
// OpenAI-compatible client (covers OpenAI, OpenRouter, Groq, OpenCode)
// ---------------------------------------------------------------------------

pub struct OpenAiCompatible {
    provider_name: String,
    base_url: String,
    api_key: String,
    client: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

// SSE streaming response structs
#[derive(Deserialize)]
struct OpenAiStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

impl OpenAiCompatible {
    pub fn new(provider_name: &str, base_url: &str, api_key: &str) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    /// Apply auth + provider-specific headers to a request builder.
    fn apply_headers(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        let req = req.header("Authorization", format!("Bearer {}", self.api_key));
        if self.provider_name == "openrouter" {
            req.header(
                "HTTP-Referer",
                "https://github.com/StressTestor/Agora-ai-agent-visualizer",
            )
            .header("X-Title", "Agora")
        } else {
            req
        }
    }

    pub fn for_provider(name: &str, api_key: &str) -> Option<Self> {
        let base_url = match name {
            "openai" => "https://api.openai.com/v1",
            "openrouter" => "https://openrouter.ai/api/v1",
            "groq" => "https://api.groq.com/openai/v1",
            "opencode" => "https://opencode.ai/zen/v1",
            "deepseek" => "https://api.deepseek.com/v1",
            "moonshot" => "https://api.moonshot.cn/v1",
            "minimax" => "https://api.minimaxi.chat/v1",
            "zai" => "https://api.z.ai/api/paas/v4",
            "zai-coding" => "https://api.z.ai/api/coding/paas/v4",
            "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai",
            _ => return None,
        };
        Some(Self::new(name, base_url, api_key))
    }
}

impl Provider for OpenAiCompatible {
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = OpenAiRequest {
            model,
            messages,
            stream: None,
        };

        let resp = self
            .apply_headers(self.client.post(&url))
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            return Err(ProviderError::RateLimit(retry_after.to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        let body = resp
            .text()
            .map_err(|e| ProviderError::Other(format!("failed to read response body: {e}")))?;

        let parsed: OpenAiResponse = serde_json::from_str(&body).map_err(|e| {
            ProviderError::Other(format!("failed to parse response: {e} | body: {body}"))
        })?;

        parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| ProviderError::Other(format!("no choices in response | body: {body}")))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .apply_headers(self.client.get(&url))
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "failed to list models: {text}"
            )));
        }

        let parsed: OpenAiModelsResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse models: {e}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                provider: self.provider_name.clone(),
            })
            .collect())
    }

    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        model: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<String, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = OpenAiRequest {
            model,
            messages,
            stream: Some(true),
        };

        let resp = self
            .apply_headers(self.client.post(&url))
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            return Err(ProviderError::RateLimit(retry_after.to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        read_openai_stream(std::io::BufReader::new(resp), on_chunk)
    }
}

// ---------------------------------------------------------------------------
// Anthropic client
// ---------------------------------------------------------------------------

pub struct AnthropicClient {
    api_key: String,
    base_url: String,
    client: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<AnthropicMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct AnthropicModelsResponse {
    data: Vec<AnthropicModelEntry>,
}

#[derive(Deserialize)]
struct AnthropicModelEntry {
    id: String,
}

impl AnthropicClient {
    pub fn new(api_key: &str) -> Self {
        Self::with_base_url(api_key, "https://api.anthropic.com")
    }

    pub fn with_base_url(api_key: &str, base_url: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }
}

impl Provider for AnthropicClient {
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let api_messages: Vec<AnthropicMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| AnthropicMessage {
                role: &m.role,
                content: &m.content,
            })
            .collect();

        let body = AnthropicRequest {
            model,
            max_tokens: 4096,
            system,
            messages: api_messages,
            stream: None,
        };

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            return Err(ProviderError::RateLimit(retry_after.to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        let parsed: AnthropicResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse response: {e}")))?;

        parsed
            .content
            .into_iter()
            .find(|c| c.content_type == "text")
            .map(|c| c.text)
            .ok_or_else(|| ProviderError::Other("no content in response".to_string()))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let resp = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            return Err(
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    ProviderError::Auth("the provider rejected the model-list request".to_string())
                } else {
                    ProviderError::Other(format!("failed to list models: HTTP {status}"))
                },
            );
        }

        let parsed: AnthropicModelsResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse models: {e}")))?;

        Ok(parsed
            .data
            .into_iter()
            .map(|m| ModelInfo {
                id: m.id,
                provider: "anthropic".to_string(),
            })
            .collect())
    }

    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        model: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let api_messages: Vec<AnthropicMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| AnthropicMessage {
                role: &m.role,
                content: &m.content,
            })
            .collect();

        let body = AnthropicRequest {
            model,
            max_tokens: 4096,
            system,
            messages: api_messages,
            stream: Some(true),
        };

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ProviderError::Auth("invalid API key".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            return Err(ProviderError::RateLimit(retry_after.to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        read_anthropic_stream(std::io::BufReader::new(resp), on_chunk)
    }
}

// ---------------------------------------------------------------------------
// Claude Code CLI provider (subprocess, uses CC OAuth — no API key needed)
// ---------------------------------------------------------------------------

pub struct ClaudeCodeProvider;

impl Provider for ClaudeCodeProvider {
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let conv: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();

        if conv.is_empty() {
            return Err(ProviderError::Other("no messages to send".to_string()));
        }

        // Flatten conversation history into the prompt. For a single message
        // pass it directly; for multi-turn, label each turn so CC has context.
        let prompt = if conv.len() == 1 {
            conv[0].content.clone()
        } else {
            conv.iter()
                .map(|m| {
                    let label = if m.role == "assistant" {
                        "you"
                    } else {
                        "other"
                    };
                    format!("[{label}]: {}", m.content)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        // GUI apps don't inherit shell PATH — resolve the binary explicitly.
        let claude_bin = [
            "/opt/homebrew/bin/claude",
            "/usr/local/bin/claude",
            "/usr/bin/claude",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
        .unwrap_or("claude");

        let mut cmd = std::process::Command::new(claude_bin);
        cmd.args([
            "-p",
            &prompt,
            "--model",
            model,
            "--output-format",
            "json",
            "--max-turns",
            "1",
            "--tools",
            "",
            "--no-session-persistence",
            "--disable-slash-commands",
        ]);

        if !system.is_empty() {
            cmd.args(["--system-prompt", system]);
        }

        let mut stdout = String::new();
        run_claude_process(cmd, &mut |line| {
            stdout.push_str(line);
            stdout.push('\n');
            Ok(())
        })?;
        let json = serde_json::from_str(&stdout).map_err(|_| {
            ProviderError::Other("failed to parse Claude CLI result JSON".to_string())
        })?;
        claude_result(&json)
    }

    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        model: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let conv: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();

        if conv.is_empty() {
            return Err(ProviderError::Other("no messages to send".to_string()));
        }

        let prompt = if conv.len() == 1 {
            conv[0].content.clone()
        } else {
            conv.iter()
                .map(|m| {
                    let label = if m.role == "assistant" {
                        "you"
                    } else {
                        "other"
                    };
                    format!("[{label}]: {}", m.content)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };

        let claude_bin = [
            "/opt/homebrew/bin/claude",
            "/usr/local/bin/claude",
            "/usr/bin/claude",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
        .unwrap_or("claude");

        let mut cmd = std::process::Command::new(claude_bin);
        cmd.args([
            "-p",
            &prompt,
            "--model",
            model,
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--max-turns",
            "1",
            "--tools",
            "",
            "--no-session-persistence",
            "--disable-slash-commands",
        ]);

        if !system.is_empty() {
            cmd.args(["--system-prompt", system]);
        }

        let mut stream = ClaudeStreamState::default();
        run_claude_process(cmd, &mut |line| stream.on_line(line, on_chunk))?;
        stream.finish()
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![
            ModelInfo {
                id: "haiku".to_string(),
                provider: "claude-code".to_string(),
            },
            ModelInfo {
                id: "sonnet".to_string(),
                provider: "claude-code".to_string(),
            },
            ModelInfo {
                id: "opus".to_string(),
                provider: "claude-code".to_string(),
            },
        ])
    }
}

// ---------------------------------------------------------------------------
// Provider factory
// ---------------------------------------------------------------------------

fn read_sse_events(
    reader: impl BufRead,
    mut on_event: impl FnMut(&str, &str) -> Result<bool, ProviderError>,
) -> Result<(), ProviderError> {
    let mut event = String::new();
    let mut data = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| ProviderError::Network(error.to_string()))?;
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            &line
        };
        if line.is_empty() {
            if !data.is_empty() && on_event(&event, &data.join("\n"))? {
                return Ok(());
            }
            event.clear();
            data.clear();
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = value.to_string(),
            "data" => data.push(value.to_string()),
            _ => {}
        }
    }
    // An unterminated frame at EOF is not a completed SSE event.
    Ok(())
}

fn stream_json(data: &str) -> Result<serde_json::Value, ProviderError> {
    serde_json::from_str(data)
        .map_err(|_| ProviderError::Other("provider sent invalid stream JSON".to_string()))
}

fn stream_error(value: &serde_json::Value) -> ProviderError {
    let error = &value["error"];
    let message = error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .unwrap_or("provider reported an error in the response stream");
    ProviderError::Other(message.to_string())
}

fn complete_stream(text: String, complete: bool) -> Result<String, ProviderError> {
    if !complete {
        return Err(ProviderError::Network(
            "response stream ended before completion".to_string(),
        ));
    }
    if text.is_empty() {
        return Err(ProviderError::Other(
            "stream ended with no content".to_string(),
        ));
    }
    Ok(text)
}

fn read_openai_stream(
    reader: impl BufRead,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String, ProviderError> {
    let mut accumulated = String::new();
    let mut complete = false;
    read_sse_events(reader, |event, data| {
        if data == "[DONE]" {
            complete = true;
            return Ok(true);
        }
        if event == "ping" {
            return Ok(false);
        }
        let value = stream_json(data)?;
        if event == "error" || value.get("error").is_some_and(|error| !error.is_null()) {
            return Err(stream_error(&value));
        }
        let chunk: OpenAiStreamChunk = serde_json::from_value(value).map_err(|_| {
            ProviderError::Other("provider sent an invalid chat stream event".to_string())
        })?;
        if let Some(choice) = chunk.choices.first() {
            if matches!(
                choice.finish_reason.as_deref(),
                Some("length" | "content_filter")
            ) {
                return Err(ProviderError::Other(format!(
                    "response stopped before completion: {}",
                    choice.finish_reason.as_deref().unwrap()
                )));
            }
            if choice.finish_reason.as_deref() == Some("stop") {
                complete = true;
            }
            if let Some(text) = choice
                .delta
                .content
                .as_deref()
                .filter(|text| !text.is_empty())
            {
                on_chunk(text);
                accumulated.push_str(text);
            }
        }
        Ok(false)
    })?;
    complete_stream(accumulated, complete)
}

fn read_anthropic_stream(
    reader: impl BufRead,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String, ProviderError> {
    let mut accumulated = String::new();
    let mut complete = false;
    read_sse_events(reader, |event, data| {
        let value = stream_json(data)?;
        let kind = value["type"]
            .as_str()
            .ok_or_else(|| ProviderError::Other("provider stream event has no type".to_string()))?;
        if event == "error" || kind == "error" {
            return Err(stream_error(&value));
        }
        if kind == "message_stop" {
            complete = true;
            return Ok(true);
        }
        if kind == "message_delta"
            && matches!(
                value["delta"]["stop_reason"].as_str(),
                Some("max_tokens" | "model_context_window_exceeded")
            )
        {
            return Err(ProviderError::Other(
                "response reached its token or context limit before completion".to_string(),
            ));
        }
        let text = if kind == "content_block_delta" && value["delta"]["type"] == "text_delta" {
            value["delta"]["text"].as_str()
        } else if kind == "content_block_start" && value["content_block"]["type"] == "text" {
            value["content_block"]["text"].as_str()
        } else {
            None
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            on_chunk(text);
            accumulated.push_str(text);
        }
        Ok(false)
    })?;
    complete_stream(accumulated, complete)
}

fn claude_result(value: &serde_json::Value) -> Result<String, ProviderError> {
    if value["is_error"] == true
        || value["subtype"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("error"))
    {
        return Err(ProviderError::Other(
            value["result"]
                .as_str()
                .unwrap_or("Claude CLI reported an unsuccessful result")
                .to_string(),
        ));
    }
    if value["type"] != "result" {
        return Err(ProviderError::Other(
            "Claude CLI did not return a final result".to_string(),
        ));
    }
    value["result"]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(String::from)
        .ok_or_else(|| ProviderError::Other("Claude CLI final result contains no text".to_string()))
}

#[derive(Default)]
struct ClaudeStreamState {
    final_result: Option<String>,
}

impl ClaudeStreamState {
    fn on_line(&mut self, line: &str, on_chunk: &mut dyn FnMut(&str)) -> Result<(), ProviderError> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let value = stream_json(line)?;
        if value["type"] == "result" {
            self.final_result = Some(claude_result(&value)?);
        } else if value["type"] == "stream_event" {
            let event = &value["event"];
            if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta" {
                if let Some(text) = event["delta"]["text"].as_str() {
                    on_chunk(text);
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<String, ProviderError> {
        self.final_result.ok_or_else(|| {
            ProviderError::Other(
                "Claude CLI stream ended without a successful final result".to_string(),
            )
        })
    }
}

fn poll_until_exit<T>(
    deadline: std::time::Instant,
    mut poll: impl FnMut() -> std::io::Result<Option<T>>,
) -> std::io::Result<Option<T>> {
    loop {
        if let Some(status) = poll()? {
            return Ok(Some(status));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    if let Err(error) = child.kill() {
        eprintln!("could not terminate Claude CLI: {error}");
    }
    match poll_until_exit(std::time::Instant::now() + Duration::from_secs(1), || {
        child.try_wait()
    }) {
        Ok(Some(_)) => {}
        Ok(None) => eprintln!("Claude CLI did not exit within the cleanup deadline"),
        Err(error) => eprintln!("could not reap Claude CLI: {error}"),
    }
}

fn collect_stderr(mut reader: impl Read) -> std::io::Result<String> {
    let mut retained = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let keep = count.min(16384usize.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
    }
    Ok(String::from_utf8_lossy(&retained).trim().to_string())
}

fn verify_claude_exit(status: std::process::ExitStatus, stderr: &str) -> Result<(), ProviderError> {
    if status.success() {
        return Ok(());
    }
    Err(ProviderError::Other(format!(
        "Claude CLI exited with {status}{}",
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )))
}

fn run_claude_process(
    mut command: std::process::Command,
    on_line: &mut dyn FnMut(&str) -> Result<(), ProviderError>,
) -> Result<(), ProviderError> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| ProviderError::Network(format!("failed to run Claude CLI: {error}")))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    if let Err(error) = std::thread::Builder::new()
        .name("claude-stdout".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let failed = line.is_err();
                if line_tx.send(line).is_err() || failed {
                    break;
                }
            }
        })
    {
        terminate_child(&mut child);
        return Err(ProviderError::Network(format!(
            "failed to start Claude stdout reader: {error}"
        )));
    }
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    if let Err(error) = std::thread::Builder::new()
        .name("claude-stderr".into())
        .spawn(move || {
            let _ = stderr_tx.send(collect_stderr(stderr));
        })
    {
        terminate_child(&mut child);
        return Err(ProviderError::Network(format!(
            "failed to start Claude stderr reader: {error}"
        )));
    }
    let result = (|| {
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(ProviderError::Other(
                    "Claude CLI timed out after 120s".to_string(),
                ));
            }
            match line_rx.recv_timeout(remaining) {
                Ok(Ok(line)) => on_line(&line)?,
                Ok(Err(error)) => {
                    return Err(ProviderError::Network(format!(
                        "failed to read Claude stdout: {error}"
                    )))
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ProviderError::Other(
                        "Claude CLI timed out after 120s".to_string(),
                    ))
                }
            }
        }
        let status = poll_until_exit(deadline, || child.try_wait())
            .map_err(|error| {
                ProviderError::Network(format!("failed to wait for Claude CLI: {error}"))
            })?
            .ok_or_else(|| ProviderError::Other("Claude CLI timed out after 120s".to_string()))?;
        let stderr = match stderr_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(stderr)) => stderr,
            Ok(Err(error)) => {
                return Err(ProviderError::Network(format!(
                    "failed to read Claude stderr: {error}"
                )))
            }
            Err(_) => String::new(), // diagnostics are best effort after the process exited
        };
        verify_claude_exit(status, &stderr)
    })();
    if result.is_err() {
        terminate_child(&mut child);
    }
    result
}

pub fn build_provider(name: &str, api_key: &str) -> Option<Box<dyn Provider>> {
    match name {
        "anthropic" => Some(Box::new(AnthropicClient::new(api_key))),
        "claude-code" => Some(Box::new(ClaudeCodeProvider)),
        "minimax-coding" => Some(Box::new(AnthropicClient::with_base_url(
            api_key,
            "https://api.minimax.io/anthropic",
        ))),
        "openai" | "openrouter" | "groq" | "opencode" | "deepseek" | "moonshot" | "minimax"
        | "zai" | "zai-coding" | "gemini" => {
            OpenAiCompatible::for_provider(name, api_key).map(|p| Box::new(p) as Box<dyn Provider>)
        }
        _ => None,
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    fn messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".into(),
                content: "Follow the topic.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "A quote: \"hello\"\nNext line: λ".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "A response.".into(),
            },
        ]
    }

    #[test]
    fn borrowed_openai_payload_preserves_messages_and_stream_option() {
        let messages = messages();
        for stream in [None, Some(true)] {
            let body = OpenAiRequest {
                model: "test-model",
                messages: &messages,
                stream,
            };
            let mut expected = serde_json::json!({ "model": "test-model", "messages": messages });
            if let Some(stream) = stream {
                expected["stream"] = stream.into();
            }
            assert_eq!(serde_json::to_value(body).unwrap(), expected);
        }
    }

    #[test]
    fn borrowed_anthropic_payload_preserves_system_and_conversation() {
        let messages = messages();
        for stream in [None, Some(true)] {
            let body = AnthropicRequest {
                model: "test-model",
                max_tokens: 4096,
                system: Some(&messages[0].content),
                messages: messages[1..]
                    .iter()
                    .map(|message| AnthropicMessage {
                        role: &message.role,
                        content: &message.content,
                    })
                    .collect(),
                stream,
            };
            let mut expected = serde_json::json!({
                "model": "test-model", "max_tokens": 4096,
                "system": messages[0].content, "messages": &messages[1..],
            });
            if let Some(stream) = stream {
                expected["stream"] = stream.into();
            }
            assert_eq!(serde_json::to_value(body).unwrap(), expected);
        }
        let empty = AnthropicRequest {
            model: "test-model",
            max_tokens: 4096,
            system: None,
            messages: vec![],
            stream: None,
        };
        assert_eq!(
            serde_json::to_value(empty).unwrap(),
            serde_json::json!({
                "model": "test-model", "max_tokens": 4096, "messages": [],
            })
        );
    }

    #[test]
    #[ignore = "manual synthetic request preparation benchmark; no provider calls"]
    fn benchmark_request_preparation() {
        let messages: Vec<_> = (0..1000)
            .map(|_| ChatMessage {
                role: "user".into(),
                content: "x".repeat(4096),
            })
            .collect();
        let iterations = 100;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(messages.to_vec());
        }
        let owned = started.elapsed();
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(OpenAiRequest {
                model: "test-model",
                messages: &messages,
                stream: Some(true),
            });
        }
        let borrowed = started.elapsed();
        eprintln!("request preparation only: {iterations} iterations, 1000 messages, 4096000 content bytes; owned clone {owned:?}; borrowed {borrowed:?}; excludes JSON serialization and network");
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    const OPENAI_TEXT: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";
    const ANTHROPIC_TEXT: &str = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n";

    #[test]
    fn openai_accepts_complete_streams_with_sse_framing_variants() {
        for text in [
            format!("{OPENAI_TEXT}data: [DONE]\n\n"),
            "data:{\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\r\n\r\ndata:[DONE]\r\n\r\n".into(),
            "\u{feff}: comment\nevent: message\ndata: {\"choices\":\ndata: [{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n".into(),
            format!("event: ping\ndata: heartbeat\n\ndata: {{\"choices\":[{{\"delta\":{{\"content\":null}}}}]}}\n\n{OPENAI_TEXT}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n"),
        ] {
            let mut chunks = String::new();
            assert_eq!(read_openai_stream(std::io::Cursor::new(text), &mut |chunk| chunks.push_str(chunk)).unwrap(), "hello");
            assert_eq!(chunks, "hello");
        }
    }

    #[test]
    fn openai_rejects_incomplete_error_and_token_limited_responses() {
        for suffix in [
            "",
            "data: [DONE]\n", // missing frame terminator
            "data: {\"error\":{\"message\":\"synthetic failure\"}}\n\n",
            "data: {not json}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        ] {
            assert!(read_openai_stream(
                std::io::Cursor::new(format!("{OPENAI_TEXT}{suffix}")),
                &mut |_| {}
            )
            .is_err());
        }
    }

    #[test]
    fn anthropic_requires_message_stop_and_propagates_stream_errors() {
        let completed = format!("{ANTHROPIC_TEXT}data: {{\"type\":\"message_stop\"}}\n\n");
        assert_eq!(
            read_anthropic_stream(std::io::Cursor::new(completed), &mut |_| {}).unwrap(),
            "hello"
        );
        for suffix in [
            "",
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"synthetic overload\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "data: {invalid}\n\n",
        ] {
            assert!(read_anthropic_stream(std::io::Cursor::new(format!("{ANTHROPIC_TEXT}{suffix}")), &mut |_| {}).is_err());
        }
    }

    #[test]
    fn transport_read_errors_remain_errors() {
        struct BrokenReader;
        impl Read for BrokenReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic read failure"))
            }
        }
        impl BufRead for BrokenReader {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                Err(std::io::Error::other("synthetic read failure"))
            }
            fn consume(&mut self, _: usize) {}
        }
        assert!(matches!(
            read_openai_stream(BrokenReader, &mut |_| {}),
            Err(ProviderError::Network(_))
        ));
        assert!(matches!(
            read_anthropic_stream(BrokenReader, &mut |_| {}),
            Err(ProviderError::Network(_))
        ));
    }

    #[test]
    fn claude_requires_successful_final_result_after_partial_output() {
        let partial = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"partial"}}}"#;
        let mut stream = ClaudeStreamState::default();
        let mut chunks = String::new();
        stream
            .on_line(partial, &mut |chunk| chunks.push_str(chunk))
            .unwrap();
        assert_eq!(chunks, "partial");
        assert!(stream.finish().is_err());
        for line in [
            r#"{"type":"result","is_error":true,"result":"synthetic failure"}"#,
            r#"{"type":"result","subtype":"error_max_turns","result":"synthetic failure"}"#,
            "not json",
        ] {
            assert!(ClaudeStreamState::default()
                .on_line(line, &mut |_| {})
                .is_err());
        }
        let mut stream = ClaudeStreamState::default();
        stream
            .on_line(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"complete"}"#,
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(stream.finish().unwrap(), "complete");
    }

    #[test]
    fn process_exit_polling_handles_timeout_success_and_io_error_without_processes() {
        let deadline = std::time::Instant::now();
        assert!(poll_until_exit::<()>(deadline, || Ok(None))
            .unwrap()
            .is_none());
        assert_eq!(poll_until_exit(deadline, || Ok(Some(7))).unwrap(), Some(7));
        assert!(
            poll_until_exit::<()>(deadline, || Err(std::io::Error::other(
                "synthetic wait error"
            )))
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsuccessful_cli_exit_is_rejected_with_diagnostics() {
        use std::os::unix::process::ExitStatusExt;
        assert!(verify_claude_exit(std::process::ExitStatus::from_raw(0), "").is_ok());
        let error =
            verify_claude_exit(std::process::ExitStatus::from_raw(256), "synthetic failure")
                .unwrap_err()
                .to_string();
        assert!(error.contains("synthetic failure"));
    }

    #[test]
    fn stderr_is_fully_drained_while_diagnostics_are_bounded() {
        let mut reader = std::io::Cursor::new(vec![b'x'; 50000]);
        let retained = collect_stderr(&mut reader).unwrap();
        assert_eq!(retained.len(), 16384);
        assert_eq!(reader.position(), 50000);
    }
}
