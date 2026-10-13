//! Request forwarding logic.

use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{
    body::Incoming,
    header::{self, HeaderMap, HeaderName, HeaderValue},
    http::uri::PathAndQuery,
    Request, Response, StatusCode, Uri, Version,
};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use tracing::{info, warn};

use crate::config::{Config, Upstream};

/// Body type returned to clients: either a streamed upstream body or a small
/// locally generated error body.
pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// Internal health-check path answered by the proxy itself.
pub const HEALTH_PATH: &str = "/__proxy/health";

/// Headers that apply to a single connection and must not be forwarded
/// (RFC 9110 §7.6.1).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub struct Proxy {
    client: Client<HttpConnector, Incoming>,
    upstreams: Vec<Upstream>,
    next: AtomicUsize,
    request_timeout: Duration,
    preserve_host: bool,
}

impl Proxy {
    pub fn new(cfg: &Config) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(cfg.connect_timeout));
        connector.set_nodelay(true);
        connector.enforce_http(true);

        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(64)
            .build(connector);

        Self {
            client,
            upstreams: cfg.upstreams.clone(),
            next: AtomicUsize::new(0),
            request_timeout: cfg.request_timeout,
            preserve_host: cfg.preserve_host,
        }
    }

    /// Round-robin upstream selection.
    fn pick(&self) -> &Upstream {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        &self.upstreams[i % self.upstreams.len()]
    }

    /// Handle one inbound request. Never fails: upstream problems become
    /// 502/504 responses.
    pub async fn handle(
        self: Arc<Self>,
        mut req: Request<Incoming>,
        peer: SocketAddr,
    ) -> Result<Response<ProxyBody>, Infallible> {
        let start = Instant::now();
        let method = req.method().clone();
        let path = req.uri().path().to_owned();

        if path == HEALTH_PATH {
            return Ok(text_response(StatusCode::OK, "ok\n"));
        }

        let upstream = self.pick();
        let path_and_query = req
            .uri()
            .path_and_query()
            .cloned()
            .unwrap_or_else(|| PathAndQuery::from_static("/"));

        let uri = match Uri::builder()
            .scheme(upstream.scheme.clone())
            .authority(upstream.authority.clone())
            .path_and_query(path_and_query)
            .build()
        {
            Ok(uri) => uri,
            Err(e) => {
                warn!(%peer, error = %e, "could not build upstream URI");
                return Ok(text_response(StatusCode::BAD_REQUEST, "bad request\n"));
            }
        };

        *req.uri_mut() = uri;
        *req.version_mut() = Version::HTTP_11;

        let original_host = req.headers().get(header::HOST).cloned();
        let headers = req.headers_mut();
        strip_hop_by_hop(headers);
        add_forwarding_headers(headers, peer, original_host.as_ref());
        if !self.preserve_host {
            // The client fills in Host from the upstream URI when absent.
            headers.remove(header::HOST);
        }

        // The timeout covers connecting and receiving response headers; the
        // response body then streams without a deadline.
        let response = match tokio::time::timeout(self.request_timeout, self.client.request(req)).await {
            Ok(Ok(mut resp)) => {
                strip_hop_by_hop(resp.headers_mut());
                resp.map(|body| body.boxed())
            }
            Ok(Err(e)) => {
                warn!(%peer, upstream = %upstream.authority, error = %e, "upstream request failed");
                text_response(StatusCode::BAD_GATEWAY, "bad gateway\n")
            }
            Err(_) => {
                warn!(%peer, upstream = %upstream.authority, "upstream timed out");
                text_response(StatusCode::GATEWAY_TIMEOUT, "gateway timeout\n")
            }
        };

        info!(
            %peer,
            %method,
            path = %path,
            upstream = %upstream.authority,
            status = response.status().as_u16(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "proxied"
        );
        Ok(response)
    }
}

/// Remove hop-by-hop headers, including any named in `Connection`.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let listed: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in listed {
        headers.remove(name);
    }
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
}

/// Add X-Forwarded-* and Via headers.
///
/// Note: an existing X-Forwarded-For from the client is appended to, not
/// trusted. If this proxy faces the internet directly, downstream services
/// should only trust the last entry.
fn add_forwarding_headers(headers: &mut HeaderMap, peer: SocketAddr, original_host: Option<&HeaderValue>) {
    let ip = peer.ip().to_string();
    let xff = match headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        Some(prev) if !prev.trim().is_empty() => format!("{prev}, {ip}"),
        _ => ip,
    };
    if let Ok(v) = HeaderValue::from_str(&xff) {
        headers.insert("x-forwarded-for", v);
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    if let Some(host) = original_host {
        headers.insert("x-forwarded-host", host.clone());
    }
    headers.append(header::VIA, HeaderValue::from_static("1.1 rev-proxy"));
}

fn text_response(status: StatusCode, msg: &'static str) -> Response<ProxyBody> {
    let body = Full::new(Bytes::from_static(msg.as_bytes()))
        .map_err(|never| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .expect("static response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_hop_by_hop_and_connection_listed_headers() {
        let mut h = HeaderMap::new();
        h.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-secret"),
        );
        h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert(header::TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

        strip_hop_by_hop(&mut h);

        assert_eq!(h.len(), 1);
        assert!(h.contains_key(header::CONTENT_TYPE));
    }

    #[test]
    fn appends_x_forwarded_for() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        let host = HeaderValue::from_static("example.com");
        add_forwarding_headers(&mut h, "192.168.1.5:5000".parse().unwrap(), Some(&host));

        assert_eq!(h["x-forwarded-for"].to_str().unwrap(), "10.0.0.1, 192.168.1.5");
        assert_eq!(h["x-forwarded-host"].to_str().unwrap(), "example.com");
        assert_eq!(h["x-forwarded-proto"].to_str().unwrap(), "http");
    }
}
