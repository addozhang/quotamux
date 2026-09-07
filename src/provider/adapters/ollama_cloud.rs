use reqwest::{RequestBuilder, header::HeaderMap};

use super::{EndpointPolicy, ProviderAdapter, bearer_auth, standard_url};
use crate::{config::AdapterKind, types::Protocol};

pub(super) static OLLAMA_CLOUD: OllamaCloud = OllamaCloud;

pub(super) struct OllamaCloud;

impl ProviderAdapter for OllamaCloud {
    fn kind(&self) -> AdapterKind {
        AdapterKind::OllamaCloud
    }
    fn endpoint_policy(&self) -> EndpointPolicy {
        EndpointPolicy::Official("https://ollama.com/v1")
    }
    fn supports_protocol(&self, protocol: Protocol) -> bool {
        protocol == Protocol::OpenAiChat
    }
    fn build_url(&self, endpoint: &str, protocol: Protocol) -> String {
        standard_url(endpoint, protocol)
    }
    fn apply_auth(
        &self,
        request: RequestBuilder,
        api_key: &str,
        _protocol: Protocol,
        _inbound_headers: &HeaderMap,
    ) -> RequestBuilder {
        bearer_auth(request, api_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::adapters::{ModelProtocolPolicy, adapter_for};

    #[test]
    fn registered_cloud_adapter_uses_official_chat_url_and_bearer_auth() {
        let adapter = adapter_for(AdapterKind::OllamaCloud).expect("registered Ollama adapter");
        assert_eq!(adapter.kind(), AdapterKind::OllamaCloud);
        assert_eq!(
            adapter.endpoint_policy(),
            EndpointPolicy::Official("https://ollama.com/v1")
        );
        assert_eq!(adapter.model_protocol_policy(), ModelProtocolPolicy::Listed);
        assert!(adapter.supports_protocol(Protocol::OpenAiChat));
        assert!(!adapter.supports_protocol(Protocol::OpenAiResponses));
        assert!(!adapter.supports_protocol(Protocol::AnthropicMessages));
        let url = adapter.build_url("https://ollama.com/v1/", Protocol::OpenAiChat);
        assert_eq!(url, "https://ollama.com/v1/chat/completions");
        let request = adapter
            .apply_auth(
                reqwest::Client::new().post(url),
                "fake-test-key",
                Protocol::OpenAiChat,
                &HeaderMap::new(),
            )
            .build()
            .unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer fake-test-key");
    }
}
