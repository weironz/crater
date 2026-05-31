//! AI copilot (M4) — natural-language → validated `crater.yaml`.
//!
//! Design principle (requirements §5): **AI is the co-pilot, not the driver.**
//! The model only *proposes* a spec; crater then validates that proposal against
//! the deterministic schema before anything runs. A hallucinated response fails
//! validation and is rejected — it never silently drives a deploy.
//!
//! Provider is OpenAI-*compatible* on purpose: the same code talks to OpenAI,
//! DeepSeek, Qwen/DashScope (compat mode), or an on-prem/intranet model behind a
//! custom endpoint — the common case for offline / 政企 environments.
//!
//! Config via env:
//!   CRATER_AI_ENDPOINT  (e.g. https://api.deepseek.com/v1 or http://intranet:8000/v1)
//!   CRATER_AI_KEY       API key (may be empty for some intranet gateways)
//!   CRATER_AI_MODEL     model name (e.g. deepseek-chat, qwen2.5-72b-instruct)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::spec::CraterSpec;

#[derive(Debug, Clone)]
pub struct AiSettings {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
}

impl AiSettings {
    /// Load from environment. Returns None if no endpoint/model configured.
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("CRATER_AI_ENDPOINT").ok()?;
        let model = std::env::var("CRATER_AI_MODEL").ok()?;
        let api_key = std::env::var("CRATER_AI_KEY").unwrap_or_default();
        Some(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key,
            model,
        })
    }
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> crate::Result<String>;
}

pub struct OpenAiCompatProvider {
    settings: AiSettings,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(settings: AiSettings) -> Self {
        Self {
            settings,
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatRespMessage,
}

#[derive(Deserialize)]
struct ChatRespMessage {
    content: String,
}

#[async_trait]
impl AiProvider for OpenAiCompatProvider {
    async fn complete(&self, system: &str, user: &str) -> crate::Result<String> {
        let url = format!("{}/chat/completions", self.settings.endpoint);
        let body = ChatRequest {
            model: &self.settings.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            temperature: 0.1,
        };
        let mut req = self.client.post(&url).json(&body);
        if !self.settings.api_key.is_empty() {
            req = req.bearer_auth(&self.settings.api_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("AI endpoint {url} -> HTTP {status}: {text}");
        }
        let parsed: ChatResponse = resp.json().await?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("AI response had no choices"))?;
        Ok(content)
    }
}

/// Build the system prompt grounding the model in crater's schema + the
/// components actually available in this install.
pub fn system_prompt(available_components: &[String]) -> String {
    format!(
        r#"You are crater's deployment planner. Convert the user's request into a
crater.yaml spec. Output ONLY the YAML (optionally in a ```yaml code block),
no prose.

Schema:
  inventory:
    hosts:
      - name: <str>
        address: <ip-or-host>
        user: <str, default root>
        port: <int, default 22>
        password: <str, optional>
        roles: [<component names this host runs>]
  components:
    - name: <one of the available components>
      version: <str, optional>
  offline: <bool, optional>

Rules:
- Use ONLY these available components: {components}.
- If the user names hosts/IPs, put them in inventory with matching roles.
- If the user gives no hosts, emit components with an empty inventory.
- Prefer explicit versions only if the user asked; otherwise omit version.
- Never invent components that are not in the available list."#,
        components = available_components.join(", ")
    )
}

/// Extract a YAML document from a model response that may wrap it in a
/// ```yaml ... ``` fence or include stray prose.
pub fn extract_yaml(response: &str) -> String {
    let text = response.trim();
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        let after = match after.find('\n') {
            Some(nl) => &after[nl + 1..],
            None => after,
        };
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
    }
    text.to_string()
}

/// Turn a natural-language request into a validated [`CraterSpec`].
/// The validation step (YAML -> CraterSpec) is the deterministic guard rail.
pub async fn nl_to_spec(
    provider: &dyn AiProvider,
    available_components: &[String],
    request: &str,
) -> crate::Result<(String, CraterSpec)> {
    let system = system_prompt(available_components);
    let raw = provider.complete(&system, request).await?;
    let yaml = extract_yaml(&raw);
    let spec: CraterSpec = serde_yaml::from_str(&yaml)
        .map_err(|e| anyhow::anyhow!("AI produced invalid crater.yaml: {e}\n---\n{yaml}"))?;

    for c in &spec.components {
        if !available_components.contains(&c.name) {
            anyhow::bail!(
                "AI referenced unknown component '{}' (available: {})",
                c.name,
                available_components.join(", ")
            );
        }
    }
    Ok((yaml, spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_yaml() {
        let resp = "Here you go:\n```yaml\ncomponents:\n  - name: docker\n```\nEnjoy!";
        let y = extract_yaml(resp);
        assert!(y.starts_with("components:"));
        assert!(!y.contains("```"));
        assert!(!y.contains("Enjoy"));
    }

    #[test]
    fn extracts_bare_yaml() {
        let resp = "components:\n  - name: docker\n";
        let y = extract_yaml(resp);
        assert!(y.starts_with("components:"));
    }

    #[test]
    fn validates_into_spec() {
        let y = extract_yaml("```yaml\ncomponents:\n  - name: docker\noffline: false\n```");
        let spec: CraterSpec = serde_yaml::from_str(&y).unwrap();
        assert_eq!(spec.components.len(), 1);
        assert_eq!(spec.components[0].name, "docker");
    }

    struct FakeProvider(String);
    #[async_trait]
    impl AiProvider for FakeProvider {
        async fn complete(&self, _s: &str, _u: &str) -> crate::Result<String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn nl_to_spec_validates_components() {
        let avail = vec!["docker".to_string(), "node_exporter".to_string()];
        let p = FakeProvider("```yaml\ncomponents:\n  - name: node_exporter\n```".into());
        let (_y, spec) = nl_to_spec(&p, &avail, "give me a metrics exporter").await.unwrap();
        assert_eq!(spec.components[0].name, "node_exporter");

        let p2 = FakeProvider("```yaml\ncomponents:\n  - name: not_a_component\n```".into());
        assert!(nl_to_spec(&p2, &avail, "x").await.is_err());
    }
}
