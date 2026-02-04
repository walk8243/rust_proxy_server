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

use std::time::Duration;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use moka::future::Cache;
use moka::Expiry;
use axum::http::{StatusCode, HeaderMap};
use axum::body::Bytes;
use headers::HeaderMapExt;

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
    cache: Cache<String, CachedResponse>,
}

#[derive(Clone)]
struct CachedResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl IntoResponse for CachedResponse {
    fn into_response(self) -> Response {
        let mut builder = Response::builder().status(self.status);
        *builder.headers_mut().unwrap() = self.headers;
        builder.body(Body::from(self.body)).unwrap()
    }
}

pub struct CacheExpiry;

impl Expiry<String, CachedResponse> for CacheExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &CachedResponse,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        if let Some(cache_control) = value.headers.typed_get::<headers::CacheControl>() {
            if let Some(max_age) = cache_control.max_age() {
                 return Some(max_age);
            }
        }
        // Default to 1 hour if no max-age found
        Some(Duration::from_secs(3600))
    }
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

    let cache = Cache::builder()
        .max_capacity(10000)
        .expire_after(CacheExpiry)
        .build();

    let state = AppState {
        client,
        target_url,
        cache,
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
            cache: Cache::builder().build(),
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
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_caching_behavior() {
        let mock_server = MockServer::start().await;

        // Verify that the server receives only one request for the cached path
        Mock::given(method("GET"))
            .and(path("/data")) // Expect stripped path
            .respond_with(ResponseTemplate::new(200).set_body_string("cached_content"))
            .expect(1) // Should be called only once
            .mount(&mock_server)
            .await;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        
        let state = AppState {
            client: Client::new(),
            target_url: mock_server.uri(),
            cache: Cache::builder().build(),
        };

        let app = create_app(state);
        
        tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/cache/data", port);

        // First request
        let resp1 = client.get(&url).send().await.unwrap();
        assert_eq!(resp1.status(), 200);
        assert_eq!(resp1.text().await.unwrap(), "cached_content");

        // Second request
        let resp2 = client.get(&url).send().await.unwrap();
        assert_eq!(resp2.status(), 200);
        assert_eq!(resp2.text().await.unwrap(), "cached_content");
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_dynamic_ttl_behavior() {
        let mock_server = MockServer::start().await;

        // Response with short TTL (2 seconds)
        Mock::given(method("GET"))
            .and(path("/ttl")) // Expect stripped path
            .respond_with(ResponseTemplate::new(200)
                .insert_header("Cache-Control", "public, max-age=2")
                .set_body_string("content_v1"))
            .expect(1) // First fetch
            .mount(&mock_server)
            .await;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        
        let state = AppState {
            client: Client::new(),
            target_url: mock_server.uri(),
            cache: Cache::builder().expire_after(CacheExpiry).build(),
        };

        let app = create_app(state);
        
        tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/cache/ttl", port);

        // 1. Initial request (Miss -> Cache)
        let resp1 = client.get(&url).send().await.unwrap();
        assert_eq!(resp1.text().await.unwrap(), "content_v1");

        // 2. Immediate subsequent request (Hit)
        let resp2 = client.get(&url).send().await.unwrap();
        assert_eq!(resp2.text().await.unwrap(), "content_v1");

        // 3. Wait for expiration (2s + buffer)
        tokio::time::sleep(Duration::from_millis(2500)).await;
        
        // Should have received exactly 1 request so far (the initial fetch)
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);

        // Reconfigure mock for new content
        mock_server.reset().await;
        Mock::given(method("GET"))
            .and(path("/ttl")) // Expect stripped path
            .respond_with(ResponseTemplate::new(200)
                .insert_header("Cache-Control", "public, max-age=2")
                .set_body_string("content_v2"))
            .expect(1)
            .mount(&mock_server)
            .await;

        // 4. Request after expiration (Miss -> Fetch new content)
        let resp3 = client.get(&url).send().await.unwrap();
        assert_eq!(resp3.text().await.unwrap(), "content_v2");
        
        // Should have received 1 *new* request after reset
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_path_stripping() {
        let mock_server = MockServer::start().await;

        // Verify that the server receives request at /data (stripped)
        Mock::given(method("GET"))
            .and(path("/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string("stripped_success"))
            .expect(1)
            .mount(&mock_server)
            .await;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        
        let state = AppState {
            client: Client::new(),
            target_url: mock_server.uri(),
            cache: Cache::builder().build(),
        };

        let app = create_app(state);
        
        tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        // Request to /cache/data should be forwarded to /data
        let url = format!("http://127.0.0.1:{}/cache/data", port);
        
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "stripped_success");
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_max_age_zero_behavior() {
        let mock_server = MockServer::start().await;

        // Response with max-age=0
        Mock::given(method("GET"))
            .and(path("/no-cache"))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("Cache-Control", "max-age=0")
                .set_body_string("content_zero"))
            .mount(&mock_server)
            .await;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        
        let state = AppState {
            client: Client::new(),
            target_url: mock_server.uri(),
            cache: Cache::builder().expire_after(CacheExpiry).build(),
        };

        let app = create_app(state);
        
        tokio::spawn(async move {
            axum::serve(tokio::net::TcpListener::from_std(listener).unwrap(), app)
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/cache/no-cache", port);

        // 1. First request
        let resp1 = client.get(&url).send().await.unwrap();
        assert_eq!(resp1.status(), 200);
        assert_eq!(resp1.text().await.unwrap(), "content_zero");

        // 2. Second request
        let resp2 = client.get(&url).send().await.unwrap();
        assert_eq!(resp2.status(), 200);
        assert_eq!(resp2.text().await.unwrap(), "content_zero");
        
        // Should have received 2 requests
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
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

    tracing::debug!("Incoming request for: {}", uri);

    // Check for cache
    let should_cache = path.starts_with("/cache");
    let cache_key = if should_cache {
        Some(format!("{} {}", req.method(), uri))
    } else {
        None
    };

    if let Some(key) = &cache_key {
        if let Some(cached) = state.cache.get(key).await {
            tracing::info!("Cache hit for {}", uri);
            return cached.into_response();
        }
    }

    let upstream_path_query = if should_cache {
        path_query.strip_prefix("/cache").unwrap_or(path_query)
    } else {
        path_query
    };

    let uri = format!("{}{}", state.target_url, upstream_path_query);

    tracing::debug!("Forwarding to upstream: {}", uri);
    
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
            if let Some(key) = cache_key {
                let status = res.status();
                let headers = res.headers().clone();
                let body_bytes = res.bytes().await.unwrap_or_default(); // Read full body

                let cached = CachedResponse {
                    status,
                    headers: headers.clone(),
                    body: body_bytes.clone(),
                };
                
                state.cache.insert(key, cached).await;

                let mut response_builder = Response::builder().status(status);
                *response_builder.headers_mut().unwrap() = headers;
                response_builder
                    .body(Body::from(body_bytes))
                    .unwrap()
                    .into_response()
            } else {
                let mut response_builder = Response::builder().status(res.status());
                *response_builder.headers_mut().unwrap() = res.headers().clone();
                response_builder
                    .body(Body::from_stream(res.bytes_stream()))
                    .unwrap()
                    .into_response()
            }
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
