use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use quotamux::{Config, store::Store, types::RequestRecord, workspace::Workspace};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};
use tower::ServiceExt;

fn legacy(path: &Path) -> String {
    let value = json!({"config_version":3,"server":{"listen":"127.0.0.1:0","data_dir":path.join("data")},"backends":[{"id":"mock","adapter":"custom-chat-completions","endpoint":"http://127.0.0.1:1/chat/completions","credentials":[{"id":"first","api_key":"fake-first"},{"id":"second","api_key":"fake-second"}],"models":[{"name":"upstream","protocols":["openai-chat"],"pricing":{"cache_hit_input_usd_per_million":0.2,"cache_miss_input_usd_per_million":1.0,"output_usd_per_million":2.0}}]}],"models":[{"name":"served","aliases":["alias"],"protocols":["openai-chat","openai-responses"],"layers":[{"name":"preferred","strategy":"random","targets":[{"backend":"mock","credential":"first","model":"upstream"}]},{"name":"fallback","strategy":"random","targets":[{"backend":"mock","credential":"second","model":"upstream"}]}]}]});
    toml::to_string_pretty(&value).unwrap()
}
async fn read(workspace: &std::sync::Arc<Workspace>, uri: &str) -> Value {
    let response = workspace
        .router()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(response.status().is_success());
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}
async fn close(workspace: std::sync::Arc<Workspace>) {
    drop(workspace);
    tokio::time::sleep(Duration::from_millis(30)).await;
}

#[tokio::test]
async fn legacy_migration_preserves_semantics_secrets_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("routing.toml");
    let original = legacy(dir.path());
    fs::write(&path, &original).unwrap();
    let old: Config = toml::from_str(&original).unwrap();
    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.config_version, 4);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        original,
        "check must not write"
    );
    assert!(!path.with_extension("toml.v3.bak").exists());
    let workspace = Workspace::open(&path).await.unwrap();
    let public = read(&workspace, "/api/routing").await;
    assert_eq!(
        public["migration"],
        json!({"from":3,"to":4,"persisted":true})
    );
    assert_eq!(
        public["models"][0]["effective_order"],
        json!(["preferred", "fallback"])
    );
    let redacted = serde_json::to_string(&public).unwrap();
    assert!(!redacted.contains("fake-first") && !redacted.contains("fake-second"));
    let normalized = fs::read(&path).unwrap();
    let updated: Config = toml::from_str(std::str::from_utf8(&normalized).unwrap()).unwrap();
    let mut expected = serde_json::to_value(old).unwrap();
    expected["config_version"] = json!(4);
    assert_eq!(serde_json::to_value(&updated).unwrap(), expected);
    assert!(updated.capacity_pools.is_empty());
    assert_eq!(updated.models[0].timezone, "UTC");
    assert!(updated.models[0].schedules.is_empty());
    assert!(
        updated.backends[0]
            .credentials
            .iter()
            .all(|credential| credential.pool.is_none())
    );
    assert_eq!(updated.backends[0].credentials[0].api_key, "fake-first");
    let backup = path.with_extension("toml.v3.bak");
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    close(workspace).await;
    let reopened = Workspace::open(&path).await.unwrap();
    assert!(read(&reopened, "/api/routing").await["migration"].is_null());
    assert_eq!(fs::read(&path).unwrap(), normalized);
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    close(reopened).await;
}

#[tokio::test]
async fn existing_request_records_and_statistics_survive_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("routing.toml");
    fs::write(&path, legacy(dir.path())).unwrap();
    let record: RequestRecord = serde_json::from_value(json!({"id":"old-request","started_at_ms":chrono::Utc::now().timestamp_millis(),"completed_at_ms":chrono::Utc::now().timestamp_millis(),"protocol":"openai-chat","requested_model":"served","streaming":false,"status":200,"backend":"mock","translated":false,"request_bytes":10,"response_bytes":20,"total_ms":1,"usage":{"input_tokens":5,"cache_hit_tokens":0,"cache_miss_tokens":5,"reasoning_tokens":0,"output_tokens":2,"total_tokens":7,"provider_reported":true}})).unwrap();
    {
        let store = Store::open(&dir.path().join("data")).unwrap();
        store.ensure_stats_schema().unwrap();
        store.record_request(&record).unwrap();
    }
    let workspace = Workspace::open(&path).await.unwrap();
    let requests = read(&workspace, "/api/requests?limit=20").await;
    assert!(
        serde_json::to_string(&requests)
            .unwrap()
            .contains("old-request")
    );
    let stats = read(&workspace, "/api/stats").await;
    assert!(stats.is_object());
    close(workspace).await;
    let store = Store::open(&dir.path().join("data")).unwrap();
    let records = store.requests(20).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].usage.total_tokens, 7);
}

#[tokio::test]
async fn failed_persistence_still_serves_migrated_document_without_replacing_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("routing.toml");
    let original = legacy(dir.path());
    fs::write(&path, &original).unwrap();
    // Keep the database writable while the configuration directory is read-only.
    drop(Store::open(&dir.path().join("data")).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    }
    let result = Workspace::open(&path).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let workspace = result.unwrap();
    let public = read(&workspace, "/api/routing").await;
    assert_eq!(
        public["migration"],
        json!({"from":3,"to":4,"persisted":false})
    );
    assert_eq!(public["document"]["config_version"], 4);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    close(workspace).await;
}

#[tokio::test]
async fn unknown_version_and_invalid_reference_never_write_migration() {
    for invalid in ["version", "reference"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routing.toml");
        let mut document: toml::Value = toml::from_str(&legacy(dir.path())).unwrap();
        if invalid == "version" {
            document["config_version"] = toml::Value::Integer(99);
        } else {
            document["models"][0]["layers"][0]["targets"][0]["credential"] =
                toml::Value::String("missing".into());
        }
        let bytes = toml::to_string_pretty(&document).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(Config::load(&path).is_err());
        assert!(Workspace::open(&path).await.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        assert!(!path.with_extension("toml.v3.bak").exists());
    }
}
