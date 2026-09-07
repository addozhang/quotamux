//! HTTP acceptance for the unified routing workspace.
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{Datelike, Timelike, Utc};
use quotamux::{Config, workspace::Workspace};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    sync::{Mutex, Notify},
    task::JoinHandle,
};

#[derive(Default)]
struct Mock {
    keys: Mutex<Vec<String>>,
    fail_held: AtomicBool,
    entered: Notify,
    release: Notify,
}
async fn respond(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    mock.keys.lock().await.push(key.clone());
    if key == "Bearer invalid-key" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"invalid credential","type":"authentication_error"}})),
        )
            .into_response();
    }
    if body["messages"][0]["content"] == "wait-for-revocation"
        && mock.fail_held.load(Ordering::SeqCst)
    {
        mock.entered.notify_one();
        mock.release.notified().await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"message":"revoked old key","type":"authentication_error"}})),
        )
            .into_response();
    }
    if body["stream"] == true {
        let hold = body["messages"][0]["content"] == "hold";
        let events = async_stream::stream! {
            yield Ok::<Bytes,Infallible>(Bytes::from_static(b"data: {\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"OK\"},\"finish_reason\":null}]}\n\n"));
            if hold { std::future::pending::<()>().await; }
            yield Ok(Bytes::from_static(b"data: {\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"));
        };
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(events),
        )
            .into_response();
    }
    Json(json!({"id":"test","object":"chat.completion","model":"upstream","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})).into_response()
}
struct App {
    _directory: TempDir,
    path: std::path::PathBuf,
    url: String,
    client: reqwest::Client,
    mocks: Vec<Arc<Mock>>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for App {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl App {
    async fn new(edit: impl FnOnce(&mut Value)) -> Self {
        let directory = TempDir::new().unwrap();
        let mut tasks = Vec::new();
        let mut mocks = Vec::new();
        let mut backends = Vec::new();
        for id in ["primary", "secondary", "peer"] {
            let mock = Arc::new(Mock::default());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!(
                "http://{}/v1/chat/completions",
                listener.local_addr().unwrap()
            );
            let app = Router::new()
                .route("/v1/chat/completions", post(respond))
                .with_state(mock.clone());
            tasks.push(tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            }));
            backends.push(json!({"id":id,"adapter":"custom-chat-completions","endpoint":endpoint,"credentials":[{"id":"key","api_key":"original-key","pool":id}],"models":[{"name":"upstream","protocols":["openai-chat"]}]}));
            mocks.push(mock);
        }
        let mut document = json!({"config_version":3,"server":{"listen":"127.0.0.1:0","data_dir":directory.path().join("data")},"backends":backends,"capacity_pools":[{"id":"primary","max_in_flight":1},{"id":"secondary","max_in_flight":1},{"id":"peer","max_in_flight":1}],"models":[{"name":"public","protocols":["openai-chat"],"layers":[{"name":"first","strategy":"random","targets":[{"backend":"primary","credential":"key","model":"upstream"}]},{"name":"next","strategy":"random","targets":[{"backend":"secondary","credential":"key","model":"upstream"}]}]}]});
        edit(&mut document);
        let config: Config = serde_json::from_value(document).unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
        let workspace = Workspace::open(&path).await.unwrap();
        let router = workspace.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tasks.push(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        Self {
            _directory: directory,
            path,
            url,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
            mocks,
            tasks,
        }
    }
    async fn get(&self) -> Value {
        self.client
            .get(format!("{}/api/routing", self.url))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn save(&self, revision: &Value, document: Value) -> reqwest::Response {
        self.client
            .put(format!("{}/api/routing", self.url))
            .json(&json!({"revision":revision,"document":document}))
            .send()
            .await
            .unwrap()
    }
    async fn call(&self, model: &str, stream: bool) -> reqwest::Response {
        self.client.post(format!("{}/v1/chat/completions",self.url)).header("x-relay-include-metadata","1").json(&json!({"model":model,"stream":stream,"messages":[{"role":"user","content":if stream {"hold"} else {"hello"}}]})).send().await.unwrap()
    }
    async fn route(&self, model: &str, backend: &str) {
        let response = self.call(model, false).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-relay-backend"], backend);
        assert_eq!(
            response.json::<Value>().await.unwrap()["choices"][0]["message"]["content"],
            "OK"
        );
    }
    async fn occupancy(&self, id: &str, expected: u64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let current = self.get().await;
                let pool = current["pools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|p| p["id"] == id)
                    .unwrap();
                if pool["in_flight"] == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("capacity should reach expected occupancy");
    }
}

#[tokio::test]
async fn workspace_redacts_keys_and_rejects_foreign_origin() {
    let app = App::new(|_| {}).await;
    let state = app.get().await;
    assert_eq!(
        state["document"]["backends"][0]["credentials"][0]["api_key"],
        ""
    );
    assert!(!state.to_string().contains("original-key"));
    let payload = json!({"revision":state["revision"],"document":state["document"]});
    let foreign = app
        .client
        .put(format!("{}/api/routing", app.url))
        .header("origin", "https://foreign.example")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn valid_key_save_activates_and_persists_while_blank_preserves_other_keys() {
    let app = App::new(|_| {}).await;
    let before = app.get().await;
    let mut document = before["document"].clone();
    document["backends"][0]["credentials"][0]["api_key"] = json!("replacement-key");
    let response = app.save(&before["revision"], document).await;
    assert_eq!(response.status(), StatusCode::OK);
    let after: Value = response.json().await.unwrap();
    assert_ne!(after["revision"], before["revision"]);
    app.route("public", "primary").await;
    assert_eq!(
        app.mocks[0].keys.lock().await.last().unwrap(),
        "Bearer replacement-key"
    );
    let persisted = Config::load(&app.path).unwrap();
    assert_eq!(
        persisted.backends[0].credentials[0].api_key,
        "replacement-key"
    );
    assert_eq!(persisted.backends[1].credentials[0].api_key, "original-key");
    assert_eq!(
        after["document"]["backends"][0]["credentials"][0]["api_key"],
        ""
    );
}

#[tokio::test]
async fn invalid_key_never_changes_document_or_current_revision() {
    let app = App::new(|_| {}).await;
    let before = app.get().await;
    let bytes = std::fs::read(&app.path).unwrap();
    let mut document = before["document"].clone();
    document["backends"][0]["credentials"][0]["api_key"] = json!("invalid-key");
    let response = app.save(&before["revision"], document).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(response.json::<Value>().await.unwrap()["error"].is_string());
    assert_eq!(app.get().await["revision"], before["revision"]);
    assert_eq!(std::fs::read(&app.path).unwrap(), bytes);
    assert!(
        app.mocks[1].keys.lock().await.is_empty(),
        "validation must not fall back"
    );
    app.route("public", "primary").await;
}

#[tokio::test]
async fn stale_writer_and_external_file_edit_are_conflicts() {
    let app = App::new(|_| {}).await;
    let old = app.get().await;
    let mut document = old["document"].clone();
    document["models"][0]["aliases"] = json!(["alias"]);
    assert_eq!(
        app.save(&old["revision"], document).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        app.save(&old["revision"], old["document"].clone())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let current = app.get().await;
    let mut bytes = std::fs::read(&app.path).unwrap();
    bytes.extend_from_slice(b"\n# external edit\n");
    std::fs::write(&app.path, &bytes).unwrap();
    assert_eq!(
        app.save(&current["revision"], current["document"].clone())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(std::fs::read(&app.path).unwrap(), bytes);
    assert_eq!(app.get().await["revision"], current["revision"]);
}

#[tokio::test]
async fn held_streams_fall_back_all_busy_returns_retry_after_and_drop_releases() {
    let app = App::new(|_| {}).await;
    let first = app.call("public", true).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-relay-backend"], "primary");
    let second = app.call("public", true).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(second.headers()["x-relay-backend"], "secondary");
    let full = tokio::time::timeout(Duration::from_millis(500), app.call("public", false))
        .await
        .expect("full pools should fail promptly");
    assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        full.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );
    drop(first);
    app.occupancy("primary", 0).await;
    app.route("public", "primary").await;
    drop(second);
    app.occupancy("secondary", 0).await;
}

#[tokio::test]
async fn same_layer_capacity_alternative_is_used_before_lower_layer() {
    let app = App::new(|d| {
        d["models"][0]["layers"][0]["targets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"backend":"peer","credential":"key","model":"upstream"}));
    })
    .await;
    let held = app.call("public", true).await;
    assert_eq!(held.status(), StatusCode::OK);
    let used = held.headers()["x-relay-backend"].to_str().unwrap();
    assert!(used == "primary" || used == "peer");
    app.route("public", if used == "primary" { "peer" } else { "primary" })
        .await;
    assert!(app.mocks[1].keys.lock().await.is_empty());
    drop(held);
}

#[tokio::test]
async fn credential_pool_is_shared_across_public_models() {
    let app = App::new(|d| {
        let mut other = d["models"][0].clone();
        other["name"] = json!("other");
        d["models"].as_array_mut().unwrap().push(other);
    })
    .await;
    let held = app.call("public", true).await;
    assert_eq!(held.headers()["x-relay-backend"], "primary");
    app.route("other", "secondary").await;
    drop(held);
    app.occupancy("primary", 0).await;
    app.route("other", "primary").await;
}

#[tokio::test]
async fn shrinking_pool_during_live_streams_preserves_all_occupied_slots() {
    let app = App::new(|d| {
        d["capacity_pools"][0]["max_in_flight"] = json!(2);
    })
    .await;
    let first = app.call("public", true).await;
    let second = app.call("public", true).await;
    assert_eq!(first.headers()["x-relay-backend"], "primary");
    assert_eq!(second.headers()["x-relay-backend"], "primary");
    app.occupancy("primary", 2).await;
    let current = app.get().await;
    let mut document = current["document"].clone();
    document["capacity_pools"][0]["max_in_flight"] = json!(1);
    assert_eq!(
        app.save(&current["revision"], document).await.status(),
        StatusCode::OK
    );
    app.occupancy("primary", 2).await;
    app.route("public", "secondary").await;
    drop(first);
    app.occupancy("primary", 1).await;
    app.route("public", "secondary").await;
    drop(second);
    app.occupancy("primary", 0).await;
    app.route("public", "primary").await;
}

#[tokio::test]
async fn elapsed_clock_transitions_default_a_b_default_without_edit_or_restart() {
    let app = App::new(|_| {}).await;
    let current = app.get().await;
    let first = Utc::now().with_nanosecond(0).unwrap() + chrono::Duration::seconds(4);
    let second = first + chrono::Duration::seconds(2);
    let end = second + chrono::Duration::seconds(2);
    let mut document = current["document"].clone();
    document["models"][0]["timezone"] = json!("UTC");
    document["models"][0]["schedules"] = json!([
        {"name":"A","days":[first.weekday().number_from_monday()],"start":first.format("%H:%M:%S").to_string(),"end":second.format("%H:%M:%S").to_string(),"order":["next","first"]},
        {"name":"B","days":[second.weekday().number_from_monday()],"start":second.format("%H:%M:%S").to_string(),"end":end.format("%H:%M:%S").to_string(),"order":["first","next"]}
    ]);
    let saved = app.save(&current["revision"], document).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: Value = saved.json().await.unwrap();
    let file = std::fs::read(&app.path).unwrap();
    for (at, active, backend) in [
        (None, None, "primary"),
        (Some(first), Some("A"), "secondary"),
        (Some(second), Some("B"), "primary"),
        (Some(end), None, "primary"),
    ] {
        if let Some(at) = at {
            let wait = (at + chrono::Duration::milliseconds(50) - Utc::now())
                .to_std()
                .unwrap_or_default();
            tokio::time::sleep(wait).await;
        }
        let observed = app.get().await;
        assert_eq!(observed["models"][0]["active_schedule"], json!(active));
        assert_eq!(observed["revision"], saved["revision"]);
        app.route("public", backend).await;
    }
    assert_eq!(std::fs::read(&app.path).unwrap(), file);
}

#[tokio::test]
async fn old_attempt_failure_does_not_poison_saved_credential_generation() {
    let app = App::new(|d| {
        d["capacity_pools"][0]["max_in_flight"] = json!(2);
    })
    .await;
    app.mocks[0].fail_held.store(true, Ordering::SeqCst);
    let before = app.get().await;
    let client = app.client.clone();
    let url = app.url.clone();
    let held = tokio::spawn(async move {
        client.post(format!("{url}/v1/chat/completions")).header("x-relay-include-metadata","1").json(&json!({"model":"public","messages":[{"role":"user","content":"wait-for-revocation"}]})).send().await.unwrap()
    });
    tokio::time::timeout(Duration::from_secs(2), app.mocks[0].entered.notified())
        .await
        .unwrap();
    app.occupancy("primary", 1).await;
    let mut document = before["document"].clone();
    document["backends"][0]["credentials"][0]["api_key"] = json!("new-generation-key");
    let saved = app.save(&before["revision"], document).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: Value = saved.json().await.unwrap();
    app.occupancy("primary", 1).await;
    app.mocks[0].release.notify_one();
    let old = held.await.unwrap();
    assert_eq!(old.status(), StatusCode::OK);
    assert_eq!(old.headers()["x-relay-backend"], "secondary");
    assert_eq!(
        old.headers()["x-quotamux-revision"],
        before["revision"].as_str().unwrap()
    );
    old.bytes().await.unwrap();
    app.occupancy("primary", 0).await;
    let view = app.get().await;
    let primary = &view["models"][0]["layers"][0]["targets"][0];
    assert_eq!(primary["circuit"]["mode"], "closed");
    assert_eq!(primary["circuit"]["consecutive_failures"], 0);
    assert!(primary["circuit"]["reason"].is_null());
    let new = app.call("public", false).await;
    assert_eq!(new.status(), StatusCode::OK);
    assert_eq!(new.headers()["x-relay-backend"], "primary");
    assert_eq!(
        new.headers()["x-quotamux-revision"],
        saved["revision"].as_str().unwrap()
    );
    new.bytes().await.unwrap();
    assert_eq!(
        app.mocks[0].keys.lock().await.last().unwrap(),
        "Bearer new-generation-key"
    );
}

#[tokio::test]
async fn assigning_new_pool_accounts_for_previously_unlimited_live_request() {
    let app = App::new(|d| {
        d["backends"][0]["credentials"][0]
            .as_object_mut()
            .unwrap()
            .remove("pool");
    })
    .await;
    let held = app.call("public", true).await;
    assert_eq!(held.status(), StatusCode::OK);
    assert_eq!(held.headers()["x-relay-backend"], "primary");
    let current = app.get().await;
    let mut document = current["document"].clone();
    document["capacity_pools"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"new-account-pool","max_in_flight":1}));
    document["backends"][0]["credentials"][0]["pool"] = json!("new-account-pool");
    assert_eq!(
        app.save(&current["revision"], document).await.status(),
        StatusCode::OK
    );
    app.occupancy("new-account-pool", 1).await;
    app.route("public", "secondary").await;
    drop(held);
    app.occupancy("new-account-pool", 0).await;
    app.route("public", "primary").await;
}

#[tokio::test]
async fn renamed_pool_keeps_occupancy_of_existing_request() {
    let app = App::new(|_| {}).await;
    let held = app.call("public", true).await;
    assert_eq!(held.headers()["x-relay-backend"], "primary");
    app.occupancy("primary", 1).await;
    let current = app.get().await;
    let mut document = current["document"].clone();
    document["capacity_pools"][0]["id"] = json!("renamed-account");
    document["backends"][0]["credentials"][0]["pool"] = json!("renamed-account");
    assert_eq!(
        app.save(&current["revision"], document).await.status(),
        StatusCode::OK
    );
    app.occupancy("renamed-account", 1).await;
    app.route("public", "secondary").await;
    drop(held);
    app.occupancy("renamed-account", 0).await;
    app.route("public", "primary").await;
}

#[cfg(unix)]
#[tokio::test]
async fn unwritable_configuration_directory_preserves_revision_and_document() {
    use std::os::unix::fs::PermissionsExt;
    struct RestoreMode {
        path: std::path::PathBuf,
        mode: u32,
    }
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode))
                .unwrap();
        }
    }
    let app = App::new(|_| {}).await;
    let current = app.get().await;
    let original = std::fs::read(&app.path).unwrap();
    let directory = app.path.parent().unwrap().to_owned();
    let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
    let restore = RestoreMode {
        path: directory.clone(),
        mode,
    };
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(directory.join("write-permission-check"))
            .is_err(),
        "this test requires a nonroot Unix process"
    );
    let mut document = current["document"].clone();
    document["backends"][0]["credentials"][0]["api_key"] = json!("validated-not-persisted");
    let result = app.save(&current["revision"], document).await;
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(result.json::<Value>().await.unwrap()["error"].is_string());
    drop(restore);
    assert!(
        app.mocks[0]
            .keys
            .lock()
            .await
            .iter()
            .any(|key| key == "Bearer validated-not-persisted"),
        "candidate must have passed real validation before persistence failure"
    );
    assert_eq!(app.get().await["revision"], current["revision"]);
    assert_eq!(std::fs::read(&app.path).unwrap(), original);
    app.route("public", "primary").await;
    assert_eq!(
        app.mocks[0].keys.lock().await.last().unwrap(),
        "Bearer original-key"
    );
}

#[tokio::test]
async fn changed_body_limit_applies_immediately_and_startup_fields_are_rejected() {
    let app = App::new(|_| {}).await;
    let current = app.get().await;
    let payload = json!({"model":"public","messages":[{"role":"user","content":"x".repeat(400)}]});
    let response = app
        .client
        .post(format!("{}/v1/chat/completions", app.url))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let mut document = current["document"].clone();
    document["server"]["max_inference_body_bytes"] = json!(256);
    assert_eq!(
        app.save(&current["revision"], document).await.status(),
        StatusCode::OK
    );
    let current = app.get().await;
    let too_big = app
        .client
        .post(format!("{}/v1/chat/completions", app.url))
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(too_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
    app.route("public", "primary").await;
    for (field, value) in [
        ("listen", json!("127.0.0.1:12345")),
        (
            "data_dir",
            json!(app._directory.path().join("different-data")),
        ),
    ] {
        let original = std::fs::read(&app.path).unwrap();
        let mut document = current["document"].clone();
        document["server"][field] = value;
        assert_eq!(
            app.save(&current["revision"], document).await.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}"
        );
        assert_eq!(app.get().await["revision"], current["revision"]);
        assert_eq!(std::fs::read(&app.path).unwrap(), original);
    }
}

#[tokio::test]
async fn sensitive_endpoint_is_redacted_preserved_and_explicitly_replaceable() {
    const QUERY_SECRET: &str = "private-query-token-for-redaction";
    let app = App::new(|document| {
        let endpoint = document["backends"][0]["endpoint"].as_str().unwrap();
        document["backends"][0]["endpoint"] = json!(format!("{endpoint}?token={QUERY_SECRET}"));
    })
    .await;
    let private_endpoint = Config::load(&app.path).unwrap().backends[0]
        .endpoint
        .clone()
        .unwrap();
    let current = app.get().await;
    assert!(current["document"]["backends"][0]["endpoint"].is_null());
    assert!(!current.to_string().contains(QUERY_SECRET));
    assert_eq!(
        current["document"]["backends"][0]["credentials"][0]["api_key"],
        ""
    );

    let mut document = current["document"].clone();
    document["models"][0]["aliases"] = json!(["public-alias"]);
    let saved = app.save(&current["revision"], document).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: Value = saved.json().await.unwrap();
    assert!(!saved.to_string().contains(QUERY_SECRET));
    assert!(saved["document"]["backends"][0]["endpoint"].is_null());
    let persisted = Config::load(&app.path).unwrap();
    assert_eq!(
        persisted.backends[0].endpoint.as_deref(),
        Some(private_endpoint.as_str())
    );
    assert_eq!(persisted.backends[0].credentials[0].api_key, "original-key");
    app.route("public-alias", "primary").await;

    // Use the other mock's clean URL so its calls prove that the explicit
    // replacement was both validated and actually published.
    let clean_endpoint = persisted.backends[1].endpoint.clone().unwrap();
    let old_calls = app.mocks[0].keys.lock().await.len();
    let new_calls = app.mocks[1].keys.lock().await.len();
    let mut document = saved["document"].clone();
    document["backends"][0]["endpoint"] = json!(clean_endpoint);
    let replacement = app.save(&saved["revision"], document).await;
    assert_eq!(replacement.status(), StatusCode::OK);
    let replacement: Value = replacement.json().await.unwrap();
    assert_eq!(
        replacement["document"]["backends"][0]["endpoint"],
        clean_endpoint
    );
    assert!(!replacement.to_string().contains(QUERY_SECRET));
    assert!(
        app.mocks[1].keys.lock().await.len() > new_calls,
        "changed endpoint must receive a real validation call"
    );
    let calls_after_validation = app.mocks[1].keys.lock().await.len();
    app.route("public", "primary").await;
    assert_eq!(app.mocks[0].keys.lock().await.len(), old_calls);
    assert_eq!(
        app.mocks[1].keys.lock().await.len(),
        calls_after_validation + 1
    );
    let persisted = Config::load(&app.path).unwrap();
    assert_eq!(
        persisted.backends[0].endpoint.as_deref(),
        Some(clean_endpoint.as_str())
    );
    assert_eq!(persisted.backends[0].credentials[0].api_key, "original-key");
}

#[tokio::test]
async fn shared_upstream_models_have_separate_history_pages_and_statistics() {
    let app = App::new(|document| {
        document["models"][0]["aliases"] = json!(["public-alias"]);
        let mut second = document["models"][0].clone();
        second["name"] = json!("other");
        second["aliases"] = json!([]);
        document["models"].as_array_mut().unwrap().push(second);
    })
    .await;
    let mut expected = Vec::new();
    for model in [
        "public",
        "other",
        "public-alias",
        "other",
        "other",
        "public",
        "other",
        "public-alias",
        "other",
        "public",
    ] {
        let response = app.call(model, false).await;
        assert_eq!(response.status(), StatusCode::OK);
        let id = response.headers()["x-relay-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        response.json::<Value>().await.unwrap();
        if model != "other" {
            expected.push(id);
        }
    }
    expected.reverse();
    for selector in ["public", "public-alias"] {
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![("model", selector.to_owned()), ("limit", "2".into())];
            if let Some(cursor) = &cursor {
                query.push(("before", cursor.clone()));
            }
            let mut url = reqwest::Url::parse(&format!("{}/api/requests", app.url)).unwrap();
            url.query_pairs_mut().extend_pairs(&query);
            let page: Value = app
                .client
                .get(url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let rows = page["requests"].as_array().unwrap();
            assert!(rows.len() <= 2);
            for row in rows {
                assert_eq!(row["served_model"], "public");
                seen.push(row["id"].as_str().unwrap().to_owned());
            }
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(seen.len() <= expected.len(), "pagination repeated rows");
        }
        assert_eq!(seen, expected);
    }
    let all: Value = app
        .client
        .get(format!("{}/api/requests?limit=20", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all["requests"].as_array().unwrap().len(), 10);
    assert_eq!(
        all["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["served_model"] == "other")
            .count(),
        5
    );
    assert_eq!(
        app.client
            .get(format!("{}/api/requests?model=unknown", app.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    for model in ["public", "public-alias", "other"] {
        let stats: Value = app
            .client
            .get(format!(
                "{}/api/routing/stats?model={model}&window=all",
                app.url
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(stats["totals"]["calls"], 5);
    }
    let stats: Value = app
        .client
        .get(format!("{}/api/stats", app.url))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["requests"]["total"], 10);
}

#[tokio::test]
async fn global_statistics_accept_windows_and_preserve_omitted_lifetime_behavior() {
    let app = App::new(|_| {}).await;
    app.route("public", "primary").await;
    let omitted: Value = app
        .client
        .get(format!("{}/api/stats", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let explicit: Value = app
        .client
        .get(format!("{}/api/stats?window=all", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(omitted, explicit);
    assert_eq!(omitted["requests"]["total"], 1);
    assert_eq!(
        omitted["window"],
        json!({"id":"All","from_ms":null,"to_ms":null})
    );
    for (window, duration) in [
        ("1h", 60),
        ("1d", 24 * 60),
        ("1w", 7 * 24 * 60),
        ("1m", 30 * 24 * 60),
    ] {
        let response = app
            .client
            .get(format!("{}/api/stats?window={window}", app.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let stats: Value = response.json().await.unwrap();
        let from = stats["window"]["from_ms"].as_i64().unwrap();
        let to = stats["window"]["to_ms"].as_i64().unwrap();
        assert_eq!(to % 60_000, 0);
        assert_eq!(to - from, duration * 60_000);
        assert_eq!(stats["window"]["id"], window.to_uppercase());
        let model: Value = app
            .client
            .get(format!(
                "{}/api/routing/stats?model=public&window={window}",
                app.url
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        // Both endpoints use the same time partition and completed-minute policy.
        if model["window"] == stats["window"] {
            assert_eq!(model["totals"]["calls"], stats["requests"]["total"]);
        }
    }
    for invalid in ["invalid", "", "2d"] {
        let response = app
            .client
            .get(format!("{}/api/stats?window={invalid}", app.url))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
