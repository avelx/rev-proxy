//! Tiny echo backend for trying out the proxy locally.
//!
//!   cargo run --example upstream -- 9001
//!
//! Replies with its own port, the request line and the headers it received.

use std::{convert::Infallible, net::SocketAddr};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{server::conn::http1, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(9001);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    println!("upstream listening on http://{addr}");

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| async move {
                let mut out = format!("served by upstream :{port}\n{} {}\n", req.method(), req.uri());
                for (name, value) in req.headers() {
                    out.push_str(&format!("{name}: {}\n", value.to_str().unwrap_or("<binary>")));
                }
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(out))))
            });
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
