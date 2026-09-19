use crate::error::ProviderError;
use std::time::Duration;

/// One HTTP GET. Returns the status code and body. Header values may carry
/// credentials; implementations must never log them.
pub trait Transport: Send + Sync {
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<(u16, String), ProviderError>;
    fn post(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &str,
    ) -> Result<(u16, String), ProviderError> {
        Err(ProviderError::Unsupported(
            "HTTP POST is not implemented by this transport".into(),
        ))
    }
}

/// Production transport over ureq/rustls with a hard global timeout.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            // Keep 4xx/5xx as responses: provider error bodies carry the
            // reason, and dropping them makes failures undebuggable.
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for UreqTransport {
    fn post(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &str,
    ) -> Result<(u16, String), ProviderError> {
        let mut request = self.agent.post(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        match request.send(body) {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response
                    .into_body()
                    .read_to_string()
                    .map_err(|e| ProviderError::Network(format!("failed reading body: {e}")))?;
                Ok((status, body))
            }
            Err(ureq::Error::StatusCode(code)) => Ok((code, String::new())),
            Err(e) => Err(ProviderError::Network(e.to_string())),
        }
    }
    fn get(&self, url: &str, headers: &[(String, String)]) -> Result<(u16, String), ProviderError> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        match request.call() {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response
                    .into_body()
                    .read_to_string()
                    .map_err(|e| ProviderError::Network(format!("failed reading body: {e}")))?;
                Ok((status, body))
            }
            Err(ureq::Error::StatusCode(code)) => {
                // 4xx/5xx with no readable body; the status still drives the
                // truthful error state.
                Ok((code, String::new()))
            }
            Err(e) => Err(ProviderError::Network(e.to_string())),
        }
    }
}
