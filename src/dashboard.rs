use std::sync::LazyLock;

use axum::{extract::Request, response::Response};
use embedded_spa::{EmbeddedSpa, EmbeddedSpaConfig};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct DashboardAssets;

static DASHBOARD: LazyLock<EmbeddedSpa<DashboardAssets>> = LazyLock::new(|| {
    EmbeddedSpa::new(EmbeddedSpaConfig::default())
        .expect("frontend/dist must contain a valid index.html")
});

pub async fn serve_spa(request: Request) -> Response {
    DASHBOARD.serve(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_entrypoint_references_existing_built_assets() {
        let index = DashboardAssets::get("index.html").expect("embedded entrypoint");
        let html = std::str::from_utf8(&index.data).unwrap();
        let mut references = 0;
        for part in html.split("/assets/").skip(1) {
            let filename = part.split('"').next().unwrap();
            let asset = DashboardAssets::get(&format!("assets/{filename}"))
                .expect("entrypoint asset must exist in embedded bundle");
            assert!(!asset.data.is_empty());
            references += 1;
        }
        assert!(references >= 2, "JavaScript and stylesheet must be bundled");
    }

    #[tokio::test]
    async fn embedded_dashboard_serves_html() {
        let request = axum::http::Request::builder()
            .uri("/")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = serve_spa(request).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
    }
}
