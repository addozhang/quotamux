//! GitHub device-flow login for the `copilot` adapter credential.
//!
//! The Copilot API accepts tokens issued to GitHub's own Copilot editor App
//! (`Iv1.b507a08c87ecfe98`); tokens from other Apps are refused. That App's
//! user access tokens do not expire, so the flow runs once and the resulting
//! token is stored as an ordinary `api_key`. Nothing here refreshes tokens;
//! a revoked or invalidated token surfaces as an upstream `401` and the
//! operator simply logs in again.

use serde_json::Value;

/// GitHub's own Copilot editor App. Copilot exchanges only tokens issued to
/// Apps it recognizes, so a device flow through any other client ID yields a
/// token the Copilot API refuses.
pub const DEVICE_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const SCOPE: &str = "read:user";
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

/// The GitHub endpoints the device flow talks to; tests point them at a fake.
#[derive(Clone, Debug)]
pub struct DeviceEndpoints {
    pub device_code_url: String,
    pub token_url: String,
    pub user_url: String,
}

impl Default for DeviceEndpoints {
    fn default() -> Self {
        Self {
            device_code_url: DEVICE_CODE_URL.to_owned(),
            token_url: TOKEN_URL.to_owned(),
            user_url: USER_URL.to_owned(),
        }
    }
}

/// What the operator needs to finish signing in on github.com.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until `device_code` can no longer be exchanged.
    pub expires_in: u64,
    /// Seconds GitHub asks callers to wait between token polls.
    pub interval: u64,
}

/// The outcome of one token poll. GitHub reports OAuth failures as HTTP 200
/// responses with an `error` field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    /// The operator has not authorized the code yet.
    Pending,
    /// Polling too fast; wait `interval_secs` before the next poll.
    SlowDown { interval_secs: u64 },
    /// The operator authorized the code; `access_token` is the API token.
    Complete { access_token: String },
    /// The device code expired; start the login again.
    Expired,
    /// The operator denied the authorization request.
    Denied,
}

#[derive(Debug)]
pub enum LoginError {
    Transport(String),
    Http { status: u16, body: String },
    Protocol(String),
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "could not reach GitHub: {message}"),
            Self::Http { status, body } => write!(f, "GitHub returned HTTP {status}: {body}"),
            Self::Protocol(message) => write!(f, "unexpected GitHub response: {message}"),
        }
    }
}

impl std::error::Error for LoginError {}

fn truncate(body: String) -> String {
    match body.char_indices().nth(MAX_ERROR_BODY_BYTES) {
        Some((index, _)) => body[..index].to_owned(),
        None => body,
    }
}

fn json_body(
    status: reqwest::StatusCode,
    body: String,
    context: &str,
) -> Result<Value, LoginError> {
    if !status.is_success() {
        return Err(LoginError::Http {
            status: status.as_u16(),
            body: truncate(body),
        });
    }
    serde_json::from_str(&body)
        .map_err(|_| LoginError::Protocol(format!("GitHub returned a non-JSON {context} response")))
}

fn text_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, LoginError> {
    value[name].as_str().ok_or_else(|| {
        LoginError::Protocol(format!("GitHub response is missing string field {name:?}"))
    })
}

fn positive_field(value: &Value, name: &str) -> Result<u64, LoginError> {
    value[name]
        .as_u64()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| {
            LoginError::Protocol(format!(
                "GitHub response is missing positive field {name:?}"
            ))
        })
}

/// Parse the device-code endpoint response.
pub fn parse_device_code(value: &Value) -> Result<DeviceCode, LoginError> {
    Ok(DeviceCode {
        device_code: text_field(value, "device_code")?.to_owned(),
        user_code: text_field(value, "user_code")?.to_owned(),
        verification_uri: text_field(value, "verification_uri")?.to_owned(),
        expires_in: positive_field(value, "expires_in")?,
        interval: value["interval"].as_u64().unwrap_or(5),
    })
}

/// Parse one token-poll response into its outcome.
pub fn parse_token_response(value: &Value) -> Result<PollOutcome, LoginError> {
    if let Some(error) = value["error"].as_str() {
        return Ok(match error {
            "authorization_pending" => PollOutcome::Pending,
            "slow_down" => PollOutcome::SlowDown {
                interval_secs: value["interval"].as_u64().unwrap_or(5),
            },
            "expired_token" => PollOutcome::Expired,
            "access_denied" | "access_denied_device_flow" => PollOutcome::Denied,
            other => {
                return Err(LoginError::Protocol(format!(
                    "GitHub reported OAuth error {other:?}"
                )));
            }
        });
    }
    let access_token = text_field(value, "access_token")?;
    if access_token.is_empty() {
        return Err(LoginError::Protocol(
            "GitHub returned an empty access token".into(),
        ));
    }
    Ok(PollOutcome::Complete {
        access_token: access_token.to_owned(),
    })
}

/// Ask GitHub for a device code the operator authorizes at `verification_uri`.
pub async fn request_device_code(
    client: &reqwest::Client,
    endpoints: &DeviceEndpoints,
) -> Result<DeviceCode, LoginError> {
    let response = client
        .post(&endpoints.device_code_url)
        .header("Accept", "application/json")
        .form(&[("client_id", DEVICE_CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    parse_device_code(&json_body(status, body, "device-code")?)
}

/// Check once whether the operator finished authorizing on github.com.
pub async fn poll_token(
    client: &reqwest::Client,
    endpoints: &DeviceEndpoints,
    device_code: &str,
) -> Result<PollOutcome, LoginError> {
    let response = client
        .post(&endpoints.token_url)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", DEVICE_CLIENT_ID),
            ("device_code", device_code),
            ("grant_type", DEVICE_GRANT),
        ])
        .send()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| LoginError::Transport(error.to_string()))?;
    parse_token_response(&json_body(status, body, "token")?)
}

/// Best-effort lookup of the GitHub login a token belongs to. It only labels
/// a completed sign-in, so callers treat `None` as harmless.
pub async fn github_login(
    client: &reqwest::Client,
    endpoints: &DeviceEndpoints,
    access_token: &str,
) -> Option<String> {
    let value: Value = client
        .get(&endpoints.user_url)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    value["login"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{DeviceEndpoints, LoginError, PollOutcome};
    use axum::{Router, routing::post};
    use serde_json::json;

    #[test]
    fn parses_device_code_fields() {
        let code = super::parse_device_code(&json!({
            "device_code": "dc",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5,
        }))
        .unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        assert_eq!(code.interval, 5);
        assert_eq!(code.expires_in, 900);
        assert!(super::parse_device_code(&json!({"device_code": "dc"})).is_err());
    }

    #[test]
    fn maps_oauth_error_values_to_outcomes() {
        for (error, expected) in [
            ("authorization_pending", PollOutcome::Pending),
            ("slow_down", PollOutcome::SlowDown { interval_secs: 7 }),
            ("expired_token", PollOutcome::Expired),
            ("access_denied", PollOutcome::Denied),
        ] {
            let value = json!({"error": error, "error_description": "x", "interval": 7});
            assert_eq!(
                super::parse_token_response(&value).unwrap(),
                expected,
                "{error}"
            );
        }
        assert!(matches!(
            super::parse_token_response(&json!({"error": "server_error"})),
            Err(LoginError::Protocol(_))
        ));
    }

    async fn device_code_handler(body: String) -> axum::Json<serde_json::Value> {
        assert!(body.contains("client_id=Iv1.b507a08c87ecfe98"));
        assert!(body.contains("scope=read%3Auser"));
        json!({
            "device_code": "device-1",
            "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 1,
        })
        .into()
    }

    async fn token_handler(
        headers: axum::http::HeaderMap,
        body: String,
    ) -> axum::Json<serde_json::Value> {
        assert_eq!(
            headers.get("accept").and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
        assert!(body.contains("device_code=device-1"));
        json!({"access_token": "gho_token", "scope": "read:user", "token_type": "bearer"}).into()
    }

    #[tokio::test]
    async fn completes_device_flow_against_mock_github() {
        let app = Router::new()
            .route("/login/device/code", post(device_code_handler))
            .route("/login/oauth/access_token", post(token_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        let endpoints = DeviceEndpoints {
            device_code_url: format!("{base}/login/device/code"),
            token_url: format!("{base}/login/oauth/access_token"),
            user_url: format!("{base}/user"),
        };
        let client = reqwest::Client::new();

        let code = super::request_device_code(&client, &endpoints)
            .await
            .unwrap();
        assert_eq!(code.device_code, "device-1");
        assert_eq!(
            super::poll_token(&client, &endpoints, &code.device_code)
                .await
                .unwrap(),
            PollOutcome::Complete {
                access_token: "gho_token".into()
            }
        );
    }
}
