# rev-proxy

A small reverse HTTP proxy in Rust, built on **Tokio** (async runtime) and **Hyper 1.x**.

## Features

- Async I/O on the Tokio multi-threaded runtime
- Round-robin load balancing across one or more upstreams
- Pooled, keep-alive upstream connections (Hyper client)
- Streams request and response bodies (no buffering)
- Strips hop-by-hop headers (RFC 9110 §7.6.1), including any listed in `Connection`
- Adds `X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host` and `Via`
- Connect and response timeouts → `502 Bad Gateway` / `504 Gateway Timeout`
- Built-in health check at `/__proxy/health`
- Graceful shutdown on Ctrl+C / SIGTERM (drains in-flight requests, up to 30 s)
- Structured logging via `tracing` (`RUST_LOG=debug` for more detail)

## Build

```bash
cargo build --release
cargo test
```

## Run

```bash
# Two demo backends
cargo run --example upstream -- 9001 &
cargo run --example upstream -- 9002 &

# The proxy
cargo run --release -- --listen 127.0.0.1:8080 \
    --upstream http://127.0.0.1:9001,http://127.0.0.1:9002

# Requests alternate between the two backends
curl -s localhost:8080/hello?x=1
curl -s localhost:8080/hello?x=2
curl -s localhost:8080/__proxy/health
```

## Options

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `-l, --listen` | `PROXY_LISTEN` | `0.0.0.0:8080` | Listen address |
| `-u, --upstream` | `PROXY_UPSTREAMS` | — (required) | Upstream base URL(s); repeat or comma-separate |
| `-t, --timeout` | `PROXY_TIMEOUT` | `30` | Seconds to wait for upstream response headers |
| `-c, --connect-timeout` | | `5` | Seconds to wait for the upstream TCP connect |
| `--preserve-host` | | off | Forward the client's `Host` header instead of the upstream's |

## Layout

```
src/main.rs        listener, accept loop, graceful shutdown
src/proxy.rs       request rewriting, forwarding, header handling
src/config.rs      CLI / env parsing
examples/upstream.rs   echo backend for local testing
```

## Limitations / next steps

- Inbound HTTP/1.1 only; upstreams must be plain `http://` (terminate TLS in front, or add `hyper-rustls`)
- No active health checks: a dead upstream still gets its share of requests (returns 502)
- WebSocket / `Upgrade` requests are not tunnelled
