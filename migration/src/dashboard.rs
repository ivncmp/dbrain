use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tracing::{error, info};

const DASHBOARD_HTML: &str = include_str!("../../src/dashboard/index.html");
const LOGO_COMPLETE: &[u8] = include_bytes!("../../src/dashboard/logo-complete.png");
const LOGO_IMAGE: &[u8] = include_bytes!("../../src/dashboard/logo-image.png");

// Icons
const ICON_APPLE_TOUCH: &[u8] = include_bytes!("../../src/dashboard/icons/apple-touch-icon.png");
const ICON_FAVICON_96: &[u8] = include_bytes!("../../src/dashboard/icons/favicon-96x96.png");
const ICON_FAVICON_ICO: &[u8] = include_bytes!("../../src/dashboard/icons/favicon.ico");
const ICON_FAVICON_SVG: &[u8] = include_bytes!("../../src/dashboard/icons/favicon.svg");
const ICON_WEBMANIFEST: &[u8] = include_bytes!("../../src/dashboard/icons/site.webmanifest");
const ICON_WEB_192: &[u8] =
  include_bytes!("../../src/dashboard/icons/web-app-manifest-192x192.png");
const ICON_WEB_512: &[u8] =
  include_bytes!("../../src/dashboard/icons/web-app-manifest-512x512.png");

fn static_response(bytes: &'static [u8], content_type: &'static str) -> Response {
  Response::builder()
    .status(StatusCode::OK)
    .header(header::CONTENT_TYPE, content_type)
    .body(Body::from(bytes))
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(crate) fn start_dashboard(port: u16, host: &str) {
  let host = host.to_string();
  tokio::spawn(async move {
    let app = Router::new()
      .route("/", get(|| async { Html(DASHBOARD_HTML) }))
      .route(
        "/logo-complete.png",
        get(|| async { static_response(LOGO_COMPLETE, "image/png") }),
      )
      .route(
        "/logo-image.png",
        get(|| async { static_response(LOGO_IMAGE, "image/png") }),
      )
      .route(
        "/apple-touch-icon.png",
        get(|| async { static_response(ICON_APPLE_TOUCH, "image/png") }),
      )
      .route(
        "/favicon-96x96.png",
        get(|| async { static_response(ICON_FAVICON_96, "image/png") }),
      )
      .route(
        "/favicon.ico",
        get(|| async { static_response(ICON_FAVICON_ICO, "image/x-icon") }),
      )
      .route(
        "/favicon.svg",
        get(|| async { static_response(ICON_FAVICON_SVG, "image/svg+xml") }),
      )
      .route(
        "/site.webmanifest",
        get(|| async { static_response(ICON_WEBMANIFEST, "application/manifest+json") }),
      )
      .route(
        "/web-app-manifest-192x192.png",
        get(|| async { static_response(ICON_WEB_192, "image/png") }),
      )
      .route(
        "/web-app-manifest-512x512.png",
        get(|| async { static_response(ICON_WEB_512, "image/png") }),
      );

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
