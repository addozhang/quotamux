use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use quotamux::{
    Config,
    config::{BackendModelConfig, ModelPricingConfig},
    pricing::{PriceWindow, PricingSchedule, Tariff},
    workspace::Workspace,
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{sync::Notify, task::JoinHandle};
fn rates(n: f64) -> ModelPricingConfig {
    ModelPricingConfig {
        cache_hit_input_usd_per_million: n,
        cache_miss_input_usd_per_million: n * 2.,
        output_usd_per_million: n * 3.,
    }
}
fn model() -> BackendModelConfig {
    BackendModelConfig {
        name: "upstream".into(),
        pricing: Some(rates(1.)),
        ..Default::default()
    }
}
fn window(name: &str, days: Vec<u8>, start: &str, end: &str, n: f64) -> PriceWindow {
    PriceWindow {
        name: name.into(),
        days,
        start: start.into(),
        end: end.into(),
        pricing: rates(n),
    }
}
fn at(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}
#[test]
fn exact_daily_windows_default_weekend_and_legacy_static_rates() {
    let mut m = model();
    let legacy = Tariff::new(&m).unwrap();
    assert_eq!(
        legacy.at(at("2026-09-07T09:00:00Z")),
        (None, Some(rates(1.)))
    );
    m.pricing_schedule = Some(PricingSchedule {
        timezone: "UTC".into(),
        windows: vec![
            window("morning", vec![1, 2, 3, 4, 5], "09:00:01", "12:00:00", 2.),
            window("afternoon", vec![1, 2, 3, 4, 5], "12:00", "18:00", 3.),
        ],
    });
    let tariff = Tariff::new(&m).unwrap();
    for (time, name, n) in [
        ("2026-09-07T09:00:00.999Z", None, 1.),
        ("2026-09-07T09:00:01Z", Some("morning"), 2.),
        ("2026-09-07T11:59:59.999Z", Some("morning"), 2.),
        ("2026-09-07T12:00:00Z", Some("afternoon"), 3.),
        ("2026-09-07T18:00:00Z", None, 1.),
        ("2026-09-12T10:00:00Z", None, 1.),
    ] {
        assert_eq!(
            tariff.at(at(time)),
            (name.map(str::to_owned), Some(rates(n))),
            "{time}"
        );
    }
}
#[test]
fn overnight_window_uses_start_weekday_and_wraps_sunday() {
    let mut m = model();
    m.pricing_schedule = Some(PricingSchedule {
        timezone: "UTC".into(),
        windows: vec![window("overnight", vec![7], "23:00", "02:00", 4.)],
    });
    let tariff = Tariff::new(&m).unwrap();
    for (time, active) in [
        ("2026-09-13T22:59:59Z", false),
        ("2026-09-13T23:00:00Z", true),
        ("2026-09-14T01:59:59Z", true),
        ("2026-09-14T02:00:00Z", false),
    ] {
        assert_eq!(tariff.at(at(time)).0.is_some(), active);
    }
}
#[test]
fn invalid_tariffs_reject_missing_default_negative_overlap_and_timezone() {
    let mut valid = model();
    valid.pricing_schedule = Some(PricingSchedule {
        timezone: "UTC".into(),
        windows: vec![window("a", vec![1], "09:00", "12:00", 2.)],
    });
    let mut missing = valid.clone();
    missing.pricing = None;
    assert!(Tariff::new(&missing).is_err());
    let mut negative = valid.clone();
    negative.pricing = Some(rates(-1.));
    assert!(Tariff::new(&negative).is_err());
    let mut negative = valid.clone();
    negative.pricing_schedule.as_mut().unwrap().windows[0]
        .pricing
        .output_usd_per_million = -1.;
    assert!(Tariff::new(&negative).is_err());
    let mut overlap = valid.clone();
    overlap
        .pricing_schedule
        .as_mut()
        .unwrap()
        .windows
        .push(window("b", vec![1], "11:59:59", "13:00", 3.));
    assert!(Tariff::new(&overlap).is_err());
    let mut zone = valid;
    zone.pricing_schedule.as_mut().unwrap().timezone = "Not/AZone".into();
    assert!(Tariff::new(&zone).is_err());
}
#[derive(Default)]
struct Mock {
    calls: AtomicUsize,
    release: Notify,
}
fn usage() -> Value {
    json!({"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":40},"completion_tokens":20,"total_tokens":120})
}
async fn upstream(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Response {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    if body["stream"] == true {
        let events = async_stream::stream! {
            yield Ok::<Bytes,Infallible>(Bytes::from_static(b"data: {\"id\":\"s\",\"object\":\"chat.completion.chunk\",\"model\":\"upstream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"start\"},\"finish_reason\":null}]}\n\n"));
            mock.release.notified().await;
            let event=json!({"id":"s","object":"chat.completion.chunk","model":"upstream","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":usage()});
            yield Ok(Bytes::from(format!("data: {event}\n\ndata: [DONE]\n\n")));
        };
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(events),
        )
            .into_response();
    }
    Json(json!({"id":"j","object":"chat.completion","model":"upstream","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":usage()})).into_response()
}
struct Fixture {
    _dir: TempDir,
    url: String,
    client: reqwest::Client,
    mock: Arc<Mock>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let mock = Arc::new(Mock::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = Router::new()
            .route("/v1/chat/completions", post(upstream))
            .with_state(mock.clone());
        let upstream = tokio::spawn(async move {
            axum::serve(listener, server).await.unwrap();
        });
        let document = json!({"config_version":3,"server":{"listen":"127.0.0.1:0","data_dir":dir.path().join("data")},"backends":[{"id":"mock","adapter":"custom-chat-completions","endpoint":endpoint,"credentials":[{"id":"key","api_key":"fake-key"}],"models":[{"name":"upstream","protocols":["openai-chat"],"pricing":rates(1.)}]}],"models":[{"name":"public","protocols":["openai-chat"],"layers":[{"name":"default","strategy":"random","targets":[{"backend":"mock","credential":"key","model":"upstream"}]}]}]});
        let config: Config = serde_json::from_value(document).unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();
        let workspace = Workspace::open(&path).await.unwrap();
        let router = workspace.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let gateway = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            _dir: dir,
            url,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap(),
            mock,
            tasks: vec![upstream, gateway],
        }
    }
    async fn view(&self) -> Value {
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
    async fn save(&self, current: &Value, document: Value) -> Value {
        self.client
            .put(format!("{}/api/routing", self.url))
            .json(&json!({"revision":current["revision"],"document":document}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn call(&self, stream: bool) -> reqwest::Response {
        self.client.post(format!("{}/v1/chat/completions",self.url)).header("x-relay-include-metadata","1").json(&json!({"model":"public","stream":stream,"messages":[{"role":"user","content":"test"}]})).send().await.unwrap().error_for_status().unwrap()
    }
    async fn attempts(&self) -> Vec<Value> {
        let v: Value = self
            .client
            .get(format!("{}/api/attempts", self.url))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        v["attempts"].as_array().unwrap().clone()
    }
}
fn cost(row: &Value, n: f64) {
    let expected = (40. * n + 60. * (2. * n) + 20. * (3. * n)) / 1_000_000.;
    assert!(
        (row["provider_cost_usd"].as_f64().unwrap() - expected).abs() < 1e-12,
        "{row}"
    );
}
#[tokio::test]
async fn pricing_edit_and_preview_preserve_keys_historical_costs_and_source_notes() {
    let f = Fixture::new().await;
    f.call(false).await.bytes().await.unwrap();
    let old = f.attempts().await;
    assert_eq!(old.len(), 1);
    cost(&old[0], 1.);
    let old_id = old[0]["id"].clone();
    let current = f.view().await;
    let mut document = current["document"].clone();
    let m = &mut document["backends"][0]["models"][0];
    m["pricing"] = json!(rates(5.));
    m["pricing_note"] = json!("Published usage estimate");
    m["pricing_source"] = json!("https://example.com/rates");
    m["pricing_schedule"] = json!({"timezone":"UTC","windows":[{"name":"discount","days":[1],"start":"09:00:00","end":"12:00:00","pricing":rates(2.)}]});
    let before = f.mock.calls.load(Ordering::SeqCst);
    let saved = f.save(&current, document).await;
    assert_eq!(
        f.mock.calls.load(Ordering::SeqCst),
        before,
        "pricing edits must not probe unchanged credentials"
    );
    assert_eq!(
        saved["document"]["backends"][0]["models"][0]["pricing_note"],
        "Published usage estimate"
    );
    assert_eq!(
        saved["document"]["backends"][0]["models"][0]["pricing_source"],
        "https://example.com/rates"
    );
    for (time, active, n) in [
        ("2026-09-07T08:59:59Z", None, 5.),
        ("2026-09-07T09:00:00Z", Some("discount"), 2.),
        ("2026-09-07T12:00:00Z", None, 5.),
    ] {
        let preview: Value = f
            .client
            .get(format!("{}/api/routing?at={time}", f.url))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(preview["pricing"][0]["active_window"], json!(active));
        assert_eq!(preview["pricing"][0]["rates"], json!(rates(n)));
    }
    // Use a static rate for the real call, independent of the day this runs.
    let mut document = saved["document"].clone();
    document["backends"][0]["models"][0]["pricing_schedule"] = Value::Null;
    f.save(&saved, document).await;
    f.call(false).await.bytes().await.unwrap();
    let all = f.attempts().await;
    assert_eq!(all.len(), 2);
    cost(all.iter().find(|r| r["id"] == old_id).unwrap(), 1.);
    cost(all.iter().find(|r| r["id"] != old_id).unwrap(), 5.);
}
#[tokio::test]
async fn stream_price_is_frozen_at_admission_across_window_end_and_save() {
    let f = Fixture::new().await;
    let current = f.view().await;
    let start = Utc::now().with_nanosecond(0).unwrap() + chrono::Duration::seconds(2);
    let end = start + chrono::Duration::seconds(3);
    let mut document = current["document"].clone();
    document["backends"][0]["models"][0]["pricing_schedule"] = json!({"timezone":"UTC","windows":[{"name":"brief-discount","days":[start.weekday().number_from_monday()],"start":start.format("%H:%M:%S").to_string(),"end":end.format("%H:%M:%S").to_string(),"pricing":rates(2.)}]});
    let saved = f.save(&current, document).await;
    tokio::time::sleep(
        (start + chrono::Duration::milliseconds(50) - Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
    .await;
    let stream = f.call(true).await;
    let request_id = stream.headers()["x-relay-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        Utc::now() < end,
        "stream must begin inside the short window"
    );
    let mut document = saved["document"].clone();
    document["backends"][0]["models"][0]["pricing"] = json!(rates(7.));
    document["backends"][0]["models"][0]["pricing_schedule"] = Value::Null;
    let calls = f.mock.calls.load(Ordering::SeqCst);
    f.save(&saved, document).await;
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), calls);
    tokio::time::sleep(
        (end + chrono::Duration::milliseconds(50) - Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
    .await;
    f.mock.release.notify_one();
    assert!(stream.text().await.unwrap().contains("[DONE]"));
    let records = f.attempts().await;
    let streamed = records
        .iter()
        .find(|r| r["request_id"] == request_id)
        .unwrap();
    cost(streamed, 2.);
    assert!(streamed["started_at_ms"].as_i64().unwrap() >= start.timestamp_millis());
    assert!(streamed["started_at_ms"].as_i64().unwrap() < end.timestamp_millis());
    f.call(false).await.bytes().await.unwrap();
    let records = f.attempts().await;
    cost(
        records
            .iter()
            .find(|r| r["request_id"] != request_id)
            .unwrap(),
        7.,
    );
}
