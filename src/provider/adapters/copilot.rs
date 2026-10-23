use reqwest::{RequestBuilder, header::HeaderMap};

use crate::{config::AdapterKind, types::Protocol};

use super::{EndpointPolicy, ProviderAdapter, bearer_auth, standard_url};

pub(super) static COPILOT: Copilot = Copilot;

pub(super) struct Copilot;

impl ProviderAdapter for Copilot {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Copilot
    }

    fn endpoint_policy(&self) -> EndpointPolicy {
        EndpointPolicy::Official("https://api.githubcopilot.com")
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
