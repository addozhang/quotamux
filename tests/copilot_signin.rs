use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use quotamux::workspace::Workspace;
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc};
use tower::ServiceExt;

fn config(path: &Path) -> String {
    let value = json!({"config_version":4,"server":{"listen":"127.0.0.1:0","data_dir":path.join("data")},"backends":[{"id":"copilot","adapter":"copilot","credentials":[{"id":"copilot-token","api_key":"placeholder"}],"models":[{"name":"gpt-5","protocols":["openai-chat"]}]}],"models":[{"name":"gpt-5","protocols":["openai-chat"],"layers":[{"name":"copilot","strategy":"random","targets":[{"backend":"copilot","credential":"copilot-token","model":"gpt-5"}]}]}]});
    toml::to_string_pretty(&value).unwrap()
}

async fn device_code_handler(body: String) -> axum::Json<Value> {
    assert!(body.contains("client_id=Iv1.b507a08c87ecfe98"));
    json!({
        "device_code": "device-1",
        "user_code": "ABCD-1234",
        "verification_uri": "https://github.com/login/device",
        "expires_in": 900,
        "interval": 1,
    })
    .into()
}

async fn token_handler(body: String) -> axum::Json<Value> {
    assert!(body.contains("device_code=device-1"));
    json!({"access_token": "gho_route_token", "token_type": "bearer"}).into()
}

async fn user_handler() -> axum::Json<Value> {
    json!({"login": "octocat"}).into()
}

async fn call(
    workspace: &Arc<Workspace>,
    uri: &str,
    body: Option<(&'static str, Value)>,
    origin: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().uri(uri).method("POST");
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    let request = match body {
        Some((key, value)) => builder
            .header("content-type", "application/json")
            .body(Body::from(json!({key: value}).to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = workspace.router().oneshot(request).await.unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    (status, value)
}

#[tokio::test]
async fn copilot_sign_in_routes_drive_the_device_flow() {
    let app = Router::new()
        .route("/login/device/code", post(device_code_handler))
        .route("/login/oauth/access_token", post(token_handler))
        .route("/user", axum::routing::get(user_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    unsafe {
        std::env::set_var(
            "QUOTAMUX_TEST_DEVICE_CODE_URL",
            format!("{base}/login/device/code"),
        );
        std::env::set_var(
            "QUOTAMUX_TEST_TOKEN_URL",
            format!("{base}/login/oauth/access_token"),
        );
        std::env::set_var("QUOTAMUX_TEST_USER_URL", format!("{base}/user"));
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("routing.toml");
    fs::write(&path, config(dir.path())).unwrap();
    let workspace = Workspace::open(&path).await.unwrap();

    let (status, started) = call(&workspace, "/api/copilot/sign-in", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["user_code"], "ABCD-1234");
    assert_eq!(started["device_code"], "device-1");
    assert_eq!(
        started["verification_uri"],
        "https://github.com/login/device"
    );

    let (status, polled) = call(
        &workspace,
        "/api/copilot/sign-in/poll",
        Some(("device_code", started["device_code"].clone())),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(polled["status"], "complete");
    assert_eq!(polled["token"], "gho_route_token");
    assert_eq!(polled["login"], "octocat");

    let (status, body) = call(
        &workspace,
        "/api/copilot/sign-in/poll",
        Some(("device_code", json!(""))),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().is_some_and(|e| !e.is_empty()));

    let (status, _) = call(
        &workspace,
        "/api/copilot/sign-in",
        None,
        Some("https://dashboard.example"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    drop(workspace);
    unsafe {
        std::env::remove_var("QUOTAMUX_TEST_DEVICE_CODE_URL");
        std::env::remove_var("QUOTAMUX_TEST_TOKEN_URL");
        std::env::remove_var("QUOTAMUX_TEST_USER_URL");
    }
}
