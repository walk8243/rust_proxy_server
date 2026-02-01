use axum::{
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use clap::Parser;
use reqwest::Client;
use std::net::SocketAddr;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Clone)]
struct Args {
    /// The target URL to forward requests to (e.g., https://httpbin.org)
    #[clap(short, long)]
    target: String,

    /// Port to listen on
    #[clap(short, long, default_value_t = 3000)]
    port: u16,
}

#[derive(Clone)]
struct AppState {
    client: Client,
    target_url: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rust_proxy_server=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    // Ensure target URL doesn't have a trailing slash for consistent joining
    let target_url = args.target.trim_end_matches('/').to_string();

    let client = Client::new();

    let state = AppState {
        client,
        target_url,
    };

    let app = create_app(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    tracing::info!("Listening on {}", addr);
    tracing::info!("Forwarding to {}", args.target);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/*path", any(proxy_handler))
        .route("/", any(proxy_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use std::net::TcpListener;

    #[tokio::test]
    async fn test_proxy_forwards_request() {
        // Start a mock server
        let mock_server = MockServer::start().await;

        // Configure mock server to respond to /get?foo=bar
        Mock::given(method("GET"))
            .and(path("/get"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "args": { "foo": "bar" }
            })))
            .mount(&mock_server)
            .await;

        // Setup proxy server
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target_url = mock_server.uri();
        
        let state = AppState {
            client: Client::new(),
            target_url: target_url.clone(),
        };

        let app = create_app(state);

        // Spawn proxy server in background
        tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                .await
                .unwrap();
        });

        // Send request to proxy
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://127.0.0.1:{}/get?foo=bar", port))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["args"]["foo"], "bar");
    }
}

async fn proxy_handler(State(state): State<AppState>, mut req: Request) -> Response {
    let path = req.uri().path();
    let path_query = req
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or(path);

    let uri = format!("{}{}", state.target_url, path_query);

    tracing::debug!("Proxying request to: {}", uri);

    *req.uri_mut() = uri.parse().unwrap();
    
    // We need to remove the host header so reqwest calculates the correct one for the target
    req.headers_mut().remove(axum::http::header::HOST);

    let res = state
        .client
        .request(req.method().clone(), &uri)
        .headers(req.headers().clone())
        .body(reqwest::Body::wrap_stream(req.into_body().into_data_stream()))
        .send()
        .await;

    match res {
        Ok(res) => {
            let mut response_builder = Response::builder().status(res.status());
            *response_builder.headers_mut().unwrap() = res.headers().clone();
            response_builder
                .body(Body::from_stream(res.bytes_stream()))
                .unwrap()
                .into_response()
        }
        Err(err) => {
            tracing::error!("Proxy error: {}", err);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Proxy error: {}", err),
            )
                .into_response()
        }
    }
}
