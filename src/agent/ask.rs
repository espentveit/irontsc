//! Asking a vision model about the screen.
//!
//! A screenshot costs an agent real tokens and a round trip to wherever its model lives. When
//! there is a vision model on the near side of the network -- a llama.cpp server on the machine
//! under the desk, say -- a question about the screen can be answered there instead, and the
//! agent gets a sentence back rather than a picture.
//!
//! The endpoint is whatever speaks the OpenAI shape: this posts to `<endpoint>/chat/completions`
//! with the screenshot as a `data:` URL, which is what llama.cpp, vLLM and SGLang all accept.
//! Plain `http://` only, deliberately -- see [`super::client::post_json`].

use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use serde_json::{Value, json};

/// Where to ask, and what to ask it.
#[derive(Debug, Clone, Default)]
pub struct Vision {
    /// An OpenAI-shaped base URL: `http://server:8080/v1` for llama.cpp,
    /// `http://localhost:11434/v1` for Ollama, and the same shape for vLLM or SGLang.
    pub endpoint: String,
    /// The model to ask for. llama.cpp ignores it, Ollama requires it, so an empty one is
    /// looked up from the endpoint the first time it is needed.
    pub model: String,
    /// What that lookup found, remembered so it happens once.
    discovered: std::sync::Arc<std::sync::OnceLock<String>>,
}

impl Vision {
    /// Builds the configuration from whatever the settings and the command line said.
    ///
    /// `None` when there is no endpoint, which is what keeps the tool off the list.
    pub fn from_settings(endpoint: &str, model: &str) -> Option<Self> {
        let endpoint = endpoint.trim().trim_end_matches('/');
        if endpoint.is_empty() {
            return None;
        }
        Some(Self {
            endpoint: endpoint.to_owned(),
            model: model.trim().to_owned(),
            discovered: std::sync::Arc::default(),
        })
    }

    /// The model to name in the request.
    ///
    /// llama.cpp serves one model and ignores the name; Ollama serves many and insists on a
    /// real one, so an unset model is looked up rather than guessed. The lookup is cached for
    /// the life of the session -- it is one request, but not one per screenshot.
    async fn model(&self) -> anyhow::Result<String> {
        if !self.model.is_empty() {
            return Ok(self.model.clone());
        }
        if let Some(discovered) = self.discovered.get() {
            return Ok(discovered.clone());
        }

        let (status, _headers, reply) =
            super::client::get_json(&self.endpoint, &self.models_target()?)
                .await
                .ok()
                .filter(|(status, _, _)| status.is_success())
                .ok_or_else(|| {
                    anyhow!(
                        "no model is configured and {} would not list its own; set the model \
                         name in the settings",
                        self.endpoint
                    )
                })?;
        let _ = status;

        let listed: Value = serde_json::from_str(&reply)?;
        let name = listed
            .pointer("/data/0/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} listed no models", self.endpoint))?
            .to_owned();
        let _ = self.discovered.set(name.clone());
        Ok(name)
    }

    /// Where the endpoint lists what it is serving.
    fn models_target(&self) -> anyhow::Result<String> {
        let uri: hyper::Uri = self
            .endpoint
            .parse()
            .with_context(|| format!("`{}` is not a URL", self.endpoint))?;
        Ok(format!("{}/models", uri.path().trim_end_matches('/')))
    }

    /// The path the request goes to, relative to the endpoint's host.
    fn target(&self) -> anyhow::Result<String> {
        let uri: hyper::Uri = self
            .endpoint
            .parse()
            .with_context(|| format!("`{}` is not a URL", self.endpoint))?;
        if uri.scheme_str() != Some("http") {
            anyhow::bail!(
                "`{}` is not a plain http URL; only an endpoint on the near side of the \
                 network is supported",
                self.endpoint
            );
        }
        let path = uri.path().trim_end_matches('/');
        Ok(format!("{path}/chat/completions"))
    }

    /// Puts one question and one screenshot to the model, and returns what it says.
    pub async fn ask(&self, png: &[u8], question: &str) -> anyhow::Result<String> {
        let image = base64::engine::general_purpose::STANDARD.encode(png);
        let body = json!({
            "model": self.model().await?,
            // Nothing here wants invention: the screen says what it says.
            "temperature": 0,
            "max_tokens": 2048,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image_url",
                      "image_url": { "url": format!("data:image/png;base64,{image}") } },
                    { "type": "text", "text": question },
                ],
            }],
        });

        let (status, _headers, reply) = super::client::post_json(
            &self.endpoint,
            &self.target()?,
            &[],
            body.to_string(),
        )
        .await
        .with_context(|| format!("could not reach the vision model at {}", self.endpoint))?;

        if !status.is_success() {
            let detail = reply.trim();
            let detail: String = detail.chars().take(300).collect();
            return Err(anyhow!("the vision model answered {status}: {detail}"));
        }

        let parsed: Value = serde_json::from_str(&reply)
            .with_context(|| "the vision model's reply was not JSON".to_owned())?;
        parsed
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("the vision model's reply had no content"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_endpoint_means_no_tool() {
        assert!(Vision::from_settings("", "").is_none());
        assert!(Vision::from_settings("   ", "anything").is_none());
    }

    #[test]
    fn trims_the_endpoint_and_leaves_an_unset_model_to_be_looked_up() {
        let vision = Vision::from_settings("http://server:8080/v1/", "").expect("configured");
        assert_eq!(vision.endpoint, "http://server:8080/v1");
        assert!(vision.model.is_empty());
        assert_eq!(vision.models_target().expect("a path"), "/v1/models");
    }

    #[test]
    fn keeps_a_named_model_for_a_server_that_holds_several() {
        let ollama = Vision::from_settings("http://localhost:11434/v1", "glm-ocr:latest")
            .expect("configured");
        assert_eq!(ollama.model, "glm-ocr:latest");
        assert_eq!(ollama.target().expect("a path"), "/v1/chat/completions");
    }

    #[test]
    fn builds_the_completions_path_from_the_endpoint() {
        let vision = Vision::from_settings("http://server:8080/v1", "glm").expect("configured");
        assert_eq!(vision.target().expect("a path"), "/v1/chat/completions");

        let bare = Vision::from_settings("http://server:8080", "glm").expect("configured");
        assert_eq!(bare.target().expect("a path"), "/chat/completions");
    }

    #[test]
    fn refuses_an_endpoint_it_cannot_reach_without_tls() {
        let vision = Vision::from_settings("https://api.example.com/v1", "m").expect("configured");
        assert!(vision.target().is_err());
    }
}
