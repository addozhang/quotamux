use reqwest::{RequestBuilder, StatusCode, header::HeaderMap};

use crate::{
    config::AdapterKind,
    types::{FailureClass, Protocol},
};

use super::{EndpointPolicy, ProviderAdapter, ThinkingStyle, bearer_auth, standard_url};
use crate::provider::{ErrorDetails, classify_status};

pub(super) static COPILOT: Copilot = Copilot;

pub(super) struct Copilot;

impl ProviderAdapter for Copilot {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Copilot
    }

    fn default_thinking_style(&self) -> Option<ThinkingStyle> {
        Some(ThinkingStyle::EffortLevels)
    }

    fn classify_error(&self, status: StatusCode, details: &ErrorDetails) -> FailureClass {
        // GitHub's edge intermittently rejects bursty traffic with a
        // plain-text 403 citing the Terms of Service. That is throttling,
        // not a credential problem: genuine auth failures from the Copilot
        // API arrive as structured JSON error bodies. Match the leading
        // sentence — the message sanitizer collapses the markdown link that
        // carries "Service" into "[url]", so longer phrases never survive.
        // Classify this variant as capacity so the circuit reopens after
        // seconds instead of locking the backend out for up to an hour.
        if status == StatusCode::FORBIDDEN
            && details
                .classification_message
                .to_ascii_lowercase()
                .contains("access to this endpoint is forbidden")
        {
            return FailureClass::ProviderCapacity;
        }
        classify_status(status, &details.classification_message)
    }

    fn endpoint_policy(&self) -> EndpointPolicy {
        EndpointPolicy::Official("https://api.githubcopilot.com")
    }

    fn supports_protocol(&self, protocol: Protocol) -> bool {
        matches!(
            protocol,
            Protocol::OpenAiChat | Protocol::OpenAiResponses | Protocol::AnthropicMessages
        )
    }

    fn build_url(&self, endpoint: &str, protocol: Protocol) -> String {
        let base = endpoint.trim_end_matches('/');
        match protocol {
            Protocol::AnthropicMessages => format!("{base}/v1/messages"),
            other => standard_url(endpoint, other),
        }
    }

    fn apply_auth(
        &self,
        mut request: RequestBuilder,
        api_key: &str,
        protocol: Protocol,
        inbound_headers: &HeaderMap,
    ) -> RequestBuilder {
        // Editor identification. GitHub's edge applies risk heuristics to
        // bearer-only API traffic and intermittently rejects it with a
        // plain-text 403; requests identifying as a Copilot editor
        // integration (what VS Code and LiteLLM send) are treated as
        // first-class. Mirrors LiteLLM's get_copilot_default_headers.
        let intent = match protocol {
            Protocol::AnthropicMessages => "messages-proxy",
            _ => "conversation-panel",
        };
        // VS Code's Copilot extension pins 2026-06-01 across every
        // endpoint (see _mixinHeaders in its extension bundle).
        let api_version = "2026-06-01";
        request = request
            .header("copilot-integration-id", "vscode-chat")
            .header("editor-version", "vscode/1.95.0")
            .header("editor-plugin-version", "copilot-chat/0.26.7")
            .header("user-agent", "GitHubCopilotChat/0.26.7")
            .header("x-vscode-user-agent-library-version", "electron-fetch")
            .header("openai-intent", intent)
            .header("x-github-api-version", api_version)
            .header("x-request-id", uuid::Uuid::now_v7().to_string());
        // GitHub's Anthropic endpoint takes a Bearer token rather than the
        // standard x-api-key header; the version header still applies.
        if protocol == Protocol::AnthropicMessages {
            request = request
                .header("x-interaction-type", "messages-proxy")
                .header(
                    "anthropic-version",
                    inbound_headers
                        .get("anthropic-version")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("2023-06-01"),
                );
            if let Some(beta) = inbound_headers.get("anthropic-beta") {
                request = request.header("anthropic-beta", beta);
            }
        }
        bearer_auth(request, api_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::extract_error_details;

    const EDGE_FORBIDDEN_BODY: &str = "Access to this endpoint is forbidden. Please review our [Terms of Service](https://docs.github.com/en/site-policy/github-terms/github-terms-of-service).";

    #[test]
    fn edge_terms_of_service_403_is_capacity_not_auth() {
        // Run the real body through the sanitizer pipeline: the markdown
        // link collapses to "[url]", swallowing the word "Service".
        let details = extract_error_details(EDGE_FORBIDDEN_BODY.as_bytes());
        assert!(details.classification_message.contains("[url]"));
        let class = COPILOT.classify_error(StatusCode::FORBIDDEN, &details);
        assert!(matches!(class, FailureClass::ProviderCapacity));
    }

    #[test]
    fn structured_or_other_403s_stay_authentication_failures() {
        // JSON-shaped auth errors carry no edge boilerplate.
        let class = COPILOT.classify_error(
            StatusCode::FORBIDDEN,
            &ErrorDetails {
                classification_message: "{\"error\":\"forbidden\"}".into(),
                safe_message: String::new(),
                error_type: None,
                upstream_type: None,
                upstream_code: None,
            },
        );
        assert!(matches!(class, FailureClass::ProviderAuth));
        let class = COPILOT.classify_error(
            StatusCode::UNAUTHORIZED,
            &ErrorDetails {
                classification_message: "Bad credentials".into(),
                safe_message: String::new(),
                error_type: None,
                upstream_type: None,
                upstream_code: None,
            },
        );
        assert!(matches!(class, FailureClass::ProviderAuth));
    }

    #[test]
    fn requests_identify_as_a_copilot_editor_integration() {
        let request = reqwest::Client::new().post("https://api.githubcopilot.com/responses");
        let request = COPILOT.apply_auth(
            request,
            "token",
            Protocol::OpenAiResponses,
            &HeaderMap::new(),
        );
        let headers = request.build().unwrap().headers().clone();
        assert_eq!(
            headers.get("copilot-integration-id").unwrap(),
            "vscode-chat"
        );
        assert_eq!(headers.get("editor-version").unwrap(), "vscode/1.95.0");
        assert_eq!(
            headers.get("user-agent").unwrap(),
            "GitHubCopilotChat/0.26.7"
        );
        assert_eq!(headers.get("openai-intent").unwrap(), "conversation-panel");
        assert_eq!(headers.get("x-github-api-version").unwrap(), "2026-06-01");
        assert!(headers.get("x-request-id").is_some());
        assert_eq!(headers.get("authorization").unwrap(), "Bearer token");

        let request = reqwest::Client::new().post("https://api.githubcopilot.com/v1/messages");
        let request = COPILOT.apply_auth(
            request,
            "token",
            Protocol::AnthropicMessages,
            &HeaderMap::new(),
        );
        let headers = request.build().unwrap().headers().clone();
        assert_eq!(headers.get("x-github-api-version").unwrap(), "2026-06-01");
        assert_eq!(headers.get("x-interaction-type").unwrap(), "messages-proxy");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    }
}
