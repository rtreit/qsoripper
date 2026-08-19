//! Inspect an existing engine before the launcher reuses its listening port.

use std::time::Duration;

use qsoripper_core::proto::qsoripper::services::{
    cw_service_client::CwServiceClient, GetCwKeyerStatusRequest,
};
use tokio::runtime::Runtime;
use tonic::transport::{Channel, Endpoint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CatHubEndpointInspection {
    Compatible,
    Incompatible { actual: Option<String> },
    Unavailable(String),
}

pub(crate) fn inspect_cathub_endpoint(
    runtime: &Runtime,
    engine_endpoint: &str,
    expected_endpoint: &str,
) -> CatHubEndpointInspection {
    match runtime.block_on(fetch_cathub_endpoint(engine_endpoint)) {
        Ok(actual) if endpoint_matches(actual.as_deref(), expected_endpoint) => {
            CatHubEndpointInspection::Compatible
        }
        Ok(actual) => CatHubEndpointInspection::Incompatible { actual },
        Err(error) => CatHubEndpointInspection::Unavailable(error),
    }
}

fn endpoint_matches(actual: Option<&str>, expected: &str) -> bool {
    actual.is_some_and(|actual| normalize_endpoint(actual) == normalize_endpoint(expected))
}

fn normalize_endpoint(endpoint: &str) -> &str {
    endpoint.trim_end_matches('/')
}

async fn fetch_cathub_endpoint(engine_endpoint: &str) -> Result<Option<String>, String> {
    let channel = connect(engine_endpoint).await?;
    let mut client = CwServiceClient::new(channel);
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        client.get_cw_keyer_status(GetCwKeyerStatusRequest {}),
    )
    .await
    .map_err(|_| "timed out waiting for GetCwKeyerStatus".to_owned())?
    .map_err(|status| format!("GetCwKeyerStatus failed: {}", status.message()))?;
    response
        .into_inner()
        .status
        .map(|status| status.broker_endpoint)
        .ok_or_else(|| "engine returned no CW keyer status".to_owned())
}

async fn connect(endpoint: &str) -> Result<Channel, String> {
    let endpoint = Endpoint::from_shared(endpoint.to_owned())
        .map_err(|error| format!("invalid engine endpoint: {error}"))?
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5));
    endpoint
        .connect()
        .await
        .map_err(|error| format!("engine connection failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_match_accepts_equivalent_trailing_slash() {
        assert!(endpoint_matches(
            Some("http://127.0.0.1:53772/"),
            "http://127.0.0.1:53772"
        ));
    }

    #[test]
    fn endpoint_match_rejects_obsolete_dynamic_port() {
        assert!(!endpoint_matches(
            Some("http://127.0.0.1:50071"),
            "http://127.0.0.1:53772"
        ));
    }

    #[test]
    fn endpoint_match_rejects_missing_broker_endpoint() {
        assert!(!endpoint_matches(None, "http://127.0.0.1:53772"));
    }
}
