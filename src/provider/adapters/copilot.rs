use reqwest::{RequestBuilder, header::HeaderMap};

use crate::{config::AdapterKind, types::Protocol};

use super::{EndpointPolicy, ProviderAdapter, ThinkingStyle, bearer_auth, standard_url};

pub(super) static COPILOT: Copilot = Copilot;

pub(super) struct Copilot;

impl ProviderAdapter for Copilot {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Copilot
    }

    fn default_thinking_style(&self) -> Option<ThinkingStyle> {
        Some(ThinkingStyle::EffortLevels)
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
        // GitHub's Anthropic endpoint takes a Bearer token rather than the
        // standard x-api-key header; the version header still applies.
        if protocol == Protocol::AnthropicMessages {
            request = request.header(
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
