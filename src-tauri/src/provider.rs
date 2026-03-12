use serde::{Deserialize, Serialize};

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
    fn name(&self) -> &str;
    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError>;
    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
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
struct OpenAiRequest {
    model: String,
    messages: Vec<ChatMessage>,
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
            client: reqwest::blocking::Client::new(),
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
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = OpenAiRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
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

        let parsed: OpenAiResponse = serde_json::from_str(&body)
            .map_err(|e| ProviderError::Other(format!("failed to parse response: {e} | body: {body}")))?;

        parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| ProviderError::Other(format!("no choices in response | body: {body}")))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
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
}

// ---------------------------------------------------------------------------
// Anthropic client
// ---------------------------------------------------------------------------

pub struct AnthropicClient {
    provider_name: String,
    api_key: String,
    base_url: String,
    client: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
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
        Self::with_base_url("anthropic", api_key, "https://api.anthropic.com")
    }

    pub fn with_base_url(provider_name: &str, api_key: &str, base_url: &str) -> Self {
        Self {
            provider_name: provider_name.to_string(),
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl Provider for AnthropicClient {
    fn name(&self) -> &str {
        &self.provider_name
    }

    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone());

        let api_messages: Vec<AnthropicMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let body = AnthropicRequest {
            model: model.to_string(),
            max_tokens: 4096,
            system,
            messages: api_messages,
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
            // Coding plan fallback
            if self.base_url.contains("minimax") {
                return Ok(vec![
                    ModelInfo { id: "MiniMax-M2.5".to_string(), provider: "minimax-coding".to_string() },
                ]);
            }
            return Ok(vec![
                ModelInfo { id: "claude-opus-4-6".to_string(), provider: "anthropic".to_string() },
                ModelInfo { id: "claude-sonnet-4-6".to_string(), provider: "anthropic".to_string() },
                ModelInfo { id: "claude-haiku-4-5-20251001".to_string(), provider: "anthropic".to_string() },
            ]);
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
}

// ---------------------------------------------------------------------------
// Claude Code CLI provider (subprocess, uses CC OAuth — no API key needed)
// ---------------------------------------------------------------------------

pub struct ClaudeCodeProvider;

impl Provider for ClaudeCodeProvider {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn chat(&self, messages: &[ChatMessage], model: &str) -> Result<String, ProviderError> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let conv: Vec<&ChatMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

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
                    let label = if m.role == "assistant" { "you" } else { "other" };
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
            "-p", &prompt,
            "--model", model,
            "--output-format", "json",
            "--max-turns", "1",
            "--tools", "",
            "--no-session-persistence",
        ]);

        if !system.is_empty() {
            cmd.args(["--system-prompt", system]);
        }

        let output = cmd
            .output()
            .map_err(|e| ProviderError::Network(format!("failed to run claude CLI: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProviderError::Other(format!("claude CLI error: {stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| ProviderError::Other(format!("failed to parse claude output: {e}")))?;

        json["result"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| ProviderError::Other(format!("no result field in output: {stdout}")))
    }

    fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![
            ModelInfo { id: "haiku".to_string(), provider: "claude-code".to_string() },
            ModelInfo { id: "sonnet".to_string(), provider: "claude-code".to_string() },
            ModelInfo { id: "opus".to_string(), provider: "claude-code".to_string() },
        ])
    }
}

// ---------------------------------------------------------------------------
// Provider factory
// ---------------------------------------------------------------------------

pub fn build_provider(name: &str, api_key: &str) -> Option<Box<dyn Provider>> {
    match name {
        "anthropic" => Some(Box::new(AnthropicClient::new(api_key))),
        "claude-code" => Some(Box::new(ClaudeCodeProvider)),
        "minimax-coding" => Some(Box::new(AnthropicClient::with_base_url(
            "minimax-coding",
            api_key,
            "https://api.minimax.io/anthropic",
        ))),
        "openai" | "openrouter" | "groq" | "opencode" | "deepseek" | "moonshot" | "minimax" | "zai" | "zai-coding" | "gemini" => {
            OpenAiCompatible::for_provider(name, api_key).map(|p| Box::new(p) as Box<dyn Provider>)
        }
        _ => None,
    }
}
