//! Command-line / environment configuration.

use std::{net::SocketAddr, time::Duration};

use hyper::http::uri::{Authority, Scheme};
use hyper::Uri;

pub const USAGE: &str = "\
Usage: rev-proxy [OPTIONS]

Options:
  -l, --listen <ADDR>          Address to listen on        [env: PROXY_LISTEN]   [default: 0.0.0.0:8080]
  -u, --upstream <URL>         Upstream base URL; repeat or comma-separate for
                               round-robin load balancing  [env: PROXY_UPSTREAMS]
  -t, --timeout <SECS>         Upstream response timeout   [env: PROXY_TIMEOUT]  [default: 30]
  -c, --connect-timeout <SECS> Upstream connect timeout    [default: 5]
      --preserve-host          Forward the client's Host header unchanged
  -h, --help                   Print this help

Example:
  rev-proxy --listen 127.0.0.1:8080 --upstream http://127.0.0.1:9001,http://127.0.0.1:9002

Logging is controlled with RUST_LOG (e.g. RUST_LOG=debug).";

#[derive(Debug, Clone)]
pub struct Upstream {
    pub scheme: Scheme,
    pub authority: Authority,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub upstreams: Vec<Upstream>,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub preserve_host: bool,
}

impl Config {
    pub fn from_args() -> Result<Self, String> {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut listen = std::env::var("PROXY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let mut upstream_specs: Vec<String> = std::env::var("PROXY_UPSTREAMS")
            .map(|v| vec![v])
            .unwrap_or_default();
        let mut cli_upstreams: Vec<String> = Vec::new();
        let mut timeout = std::env::var("PROXY_TIMEOUT").unwrap_or_else(|_| "30".into());
        let mut connect_timeout = "5".to_string();
        let mut preserve_host = false;

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| args.next().ok_or_else(|| format!("missing value for {name}"));
            match arg.as_str() {
                "-l" | "--listen" => listen = value(&arg)?,
                "-u" | "--upstream" => cli_upstreams.push(value(&arg)?),
                "-t" | "--timeout" => timeout = value(&arg)?,
                "-c" | "--connect-timeout" => connect_timeout = value(&arg)?,
                "--preserve-host" => preserve_host = true,
                "-h" | "--help" => return Err(String::new()),
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        // Command-line upstreams take precedence over the environment.
        if !cli_upstreams.is_empty() {
            upstream_specs = cli_upstreams;
        }

        let listen: SocketAddr = listen
            .parse()
            .map_err(|e| format!("invalid listen address {listen:?}: {e}"))?;

        let upstreams = upstream_specs
            .iter()
            .flat_map(|s| s.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_upstream)
            .collect::<Result<Vec<_>, _>>()?;
        if upstreams.is_empty() {
            return Err("at least one --upstream is required".into());
        }

        Ok(Self {
            listen,
            upstreams,
            request_timeout: Duration::from_secs(parse_secs("--timeout", &timeout)?),
            connect_timeout: Duration::from_secs(parse_secs("--connect-timeout", &connect_timeout)?),
            preserve_host,
        })
    }
}

fn parse_secs(name: &str, v: &str) -> Result<u64, String> {
    match v.parse::<u64>() {
        Ok(0) | Err(_) => Err(format!("{name} must be a positive integer (seconds), got {v:?}")),
        Ok(n) => Ok(n),
    }
}

fn parse_upstream(s: &str) -> Result<Upstream, String> {
    let uri: Uri = s.parse().map_err(|e| format!("invalid upstream {s:?}: {e}"))?;
    let scheme = uri
        .scheme()
        .cloned()
        .ok_or_else(|| format!("upstream {s:?} needs a scheme, e.g. http://{s}"))?;
    if scheme != Scheme::HTTP {
        return Err(format!(
            "upstream {s:?}: only http:// upstreams are supported (put TLS termination in front)"
        ));
    }
    let authority = uri
        .authority()
        .cloned()
        .ok_or_else(|| format!("upstream {s:?} is missing host:port"))?;
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
        return Err(format!("upstream {s:?} must not contain a path or query"));
    }
    Ok(Upstream { scheme, authority })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_multiple_upstreams() {
        let cfg = Config::parse(args(&[
            "--listen",
            "127.0.0.1:8000",
            "-u",
            "http://a:1,http://b:2",
            "-u",
            "http://c:3/",
        ]))
        .unwrap();
        assert_eq!(cfg.listen.port(), 8000);
        let hosts: Vec<_> = cfg.upstreams.iter().map(|u| u.authority.as_str()).collect();
        assert_eq!(hosts, ["a:1", "b:2", "c:3"]);
    }

    #[test]
    fn rejects_bad_upstreams() {
        assert!(Config::parse(args(&["-u", "https://a:1"])).is_err());
        assert!(Config::parse(args(&["-u", "http://a:1/api"])).is_err());
        assert!(Config::parse(args(&["-u", "a:1"])).is_err());
    }
}
