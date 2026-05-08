use axum::response::Html;
use axum::routing::get;
use axum::Router;
use tracing::{error, info};

const DASHBOARD_HTML: &str = include_str!("../../src/dashboard/index.html");

pub(crate) fn start_dashboard(port: u16, host: &str) {
  let host = host.to_string();
  tokio::spawn(async move {
    let app = Router::new().route("/", get(|| async { Html(DASHBOARD_HTML) }));
    let addr = format!("{host}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
      Ok(listener) => listener,
      Err(e) => {
        error!("Dashboard failed to bind on {addr}: {e}");
        return;
      }
    };

    info!("Dashboard listening on {addr}");
    if let Err(e) = axum::serve(listener, app).await {
      error!("Dashboard server error: {e}");
    }
  });
}
