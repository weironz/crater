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

use crate::task::TaskFile;

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

/// Build the system prompt grounding the model in crater's TASK schema.
pub fn system_prompt() -> String {
    r#"You are crater's deployment planner. Convert the user's request into a
crater TASK yaml. Output ONLY the YAML (optionally in a ```yaml block), no prose.

Schema:
  name: <task name>
  hosts: all                      # or an inventory group name
  vars: { <key>: <value> }        # optional; used as {{ key }} (plain substitution)
  materials:                      # optional; things to fetch/pack
    - { name: <id>, kind: binary, url_tmpl: <url, may contain {{version}}> }
  actions:
    - id: <id>
      action: <primitive>
      <params...>
      needs: [<id>...]            # ordering (the engine topo-sorts)
      when_os: [debian|rhel]      # optional closed-enum condition
      phase: install|verify       # optional

Action primitives (use ONLY these):
  pkg_install(packages:{debian:[..],rhel:[..]}), place(material,dest,mode),
  extract(to,from,strip), write_file(dst,content),
  render_template(src,dst), run_cmd(cmd,check), file(path,state,mode),
  copy(src,dest,mode), service(name,state,enabled), lineinfile(path,line,regexp),
  user(name,...), group(name,...), systemd_unit(name,enable,start), module(uses,with).

Rules:
- NO logic in YAML: no when-expressions, loops, or computation. Use `needs` for
  ordering, `when_os` for OS branches, and `{{ var }}` only for plain substitution.
- Prefer `place` (a declared material) for binaries; `pkg_install` for OS packages."#
        .to_string()
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

/// Turn a natural-language request into a validated [`TaskFile`]. The validation
/// (YAML -> TaskFile, which fails on unknown action primitives) is the
/// deterministic guard rail — a hallucinated task is rejected, never run.
pub async fn nl_to_task(
    provider: &dyn AiProvider,
    request: &str,
) -> crate::Result<(String, TaskFile)> {
    let system = system_prompt();
    let raw = provider.complete(&system, request).await?;
    let yaml = extract_yaml(&raw);
    let task: TaskFile = serde_yaml::from_str(&yaml)
        .map_err(|e| anyhow::anyhow!("AI produced invalid task yaml: {e}\n---\n{yaml}"))?;
    Ok((yaml, task))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_yaml() {
        let resp = "Here you go:\n```yaml\nname: t\nactions: []\n```\nEnjoy!";
        let y = extract_yaml(resp);
        assert!(y.starts_with("name:"));
        assert!(!y.contains("```"));
        assert!(!y.contains("Enjoy"));
    }

    #[test]
    fn extracts_bare_yaml() {
        let resp = "name: t\nactions: []\n";
        let y = extract_yaml(resp);
        assert!(y.starts_with("name:"));
    }

    struct FakeProvider(String);
    #[async_trait]
    impl AiProvider for FakeProvider {
        async fn complete(&self, _s: &str, _u: &str) -> crate::Result<String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn nl_to_task_parses_and_rejects_bad() {
        let p = FakeProvider(
            "```yaml\nname: yq\nactions:\n  - {action: run_cmd, cmd: \"yq --version\"}\n```".into(),
        );
        let (_y, task) = nl_to_task(&p, "install yq").await.unwrap();
        assert_eq!(task.name, "yq");
        assert_eq!(task.actions.len(), 1);

        // Unknown primitive → TaskFile parse fails → rejected (guard rail).
        let bad = FakeProvider("```yaml\nname: x\nactions:\n  - {action: nonsense}\n```".into());
        assert!(nl_to_task(&bad, "x").await.is_err());
    }
}
