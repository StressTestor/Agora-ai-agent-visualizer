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
            "opencode" => "https://openrouter.ai/api/v1",
            "deepseek" => "https://api.deepseek.com/v1",
            "moonshot" => "https://api.moonshot.cn/v1",
            "minimax" => "https://api.minimaxi.chat/v1",
            "zai" => "https://api.z.ai/api/paas/v4",
            "zai-coding" => "https://api.z.ai/api/coding/paas/v4",
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
            return Err(ProviderError::RateLimit("rate limited".to_string()));
        }
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(ProviderError::Other(format!("HTTP {status}: {text}")));
        }

        let parsed: OpenAiResponse = resp
            .json()
            .map_err(|e| ProviderError::Other(format!("failed to parse response: {e}")))?;

        parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| ProviderError::Other("no choices in response".to_string()))
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
            return Err(ProviderError::RateLimit("rate limited".to_string()));
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
            .first()
            .map(|c| c.text.clone())
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
// Provider factory
// ---------------------------------------------------------------------------

pub fn build_provider(name: &str, api_key: &str) -> Option<Box<dyn Provider>> {
    match name {
        "anthropic" => Some(Box::new(AnthropicClient::new(api_key))),
        "minimax-coding" => Some(Box::new(AnthropicClient::with_base_url(
            "minimax-coding",
            api_key,
            "https://api.minimax.io/anthropic",
        ))),
        "openai" | "openrouter" | "groq" | "opencode" | "deepseek" | "moonshot" | "minimax" | "zai" | "zai-coding" => {
            OpenAiCompatible::for_provider(name, api_key).map(|p| Box::new(p) as Box<dyn Provider>)
        }
        _ => None,
    }
}
