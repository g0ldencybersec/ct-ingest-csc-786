//! Shared HTTP client setup and helpers.
//!
//! The client is built WITHOUT reqwest's automatic decompression, so no
//! `Accept-Encoding` header is sent and response bodies are the exact wire
//! bytes. Static CT data tiles still arrive gzip-compressed (the spec stores
//! them that way) and are decoded with `ctcore::maybe_gunzip`.

use std::time::Duration;

use anyhow::{bail, Context, Result};

pub fn client(contact: Option<&str>) -> Result<reqwest::Client> {
    let ua = match contact {
        Some(c) => format!(
            "ct-ingest-csc786/{} (academic CT measurement; contact: {c})",
            env!("CARGO_PKG_VERSION")
        ),
        None => format!(
            "ct-ingest-csc786/{} (academic CT measurement)",
            env!("CARGO_PKG_VERSION")
        ),
    };
    Ok(reqwest::Client::builder()
        .user_agent(ua)
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .build()?)
}

pub fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// One measured HTTP GET.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    /// What was fetched: "get-entries", "tile", or "issuer".
    pub kind: &'static str,
    /// First index covered (entries/tiles); None for issuers.
    pub index: Option<u64>,
    pub status: u16,
    /// Accept-Encoding header sent ("" = header not sent).
    pub accept_encoding: String,
    /// Content-Encoding header the server actually returned ("" = none).
    pub content_encoding: String,
    /// Response body bytes as received (compressed, if compressed).
    pub wire_bytes: u64,
    /// Body bytes after removing every layer of gzip.
    pub decoded_bytes: u64,
    /// Number of gzip layers found in the body (0, 1, or 2).
    pub gzip_layers: u8,
    /// Milliseconds since trial start: request sent, headers received, body done.
    pub t_start_ms: f64,
    pub t_headers_ms: f64,
    pub t_end_ms: f64,
    pub attempts: u32,
    /// Entries decoded from this response (0 for issuers).
    pub entries: u64,
}

/// GET `url` sending exactly the given `Accept-Encoding` (None = no header),
/// recording timing and the response's actual `Content-Encoding`.
///
/// Returns the raw wire body. Retries (up to 3 attempts) are counted in
/// `attempts`; a trial with retries is flagged by the analysis.
pub async fn get_measured(
    client: &reqwest::Client,
    url: &str,
    accept_encoding: Option<&str>,
    epoch: std::time::Instant,
    kind: &'static str,
    index: Option<u64>,
) -> Result<(Vec<u8>, Event)> {
    let ms = |t: std::time::Instant| t.duration_since(epoch).as_secs_f64() * 1000.0;
    let t_start = std::time::Instant::now();
    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut req = client.get(url);
        if let Some(ae) = accept_encoding {
            req = req.header(reqwest::header::ACCEPT_ENCODING, ae);
        }
        let res = async {
            let resp = req.send().await?;
            let t_headers = std::time::Instant::now();
            let status = resp.status();
            let ce = resp
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = resp.bytes().await?.to_vec();
            Ok::<_, reqwest::Error>((status, ce, body, t_headers))
        }
        .await;
        match res {
            Ok((status, ce, body, t_headers)) if status.is_success() => {
                let ev = Event {
                    kind,
                    index,
                    status: status.as_u16(),
                    accept_encoding: accept_encoding.unwrap_or("").to_string(),
                    content_encoding: ce,
                    wire_bytes: body.len() as u64,
                    decoded_bytes: 0,
                    gzip_layers: 0,
                    t_start_ms: ms(t_start),
                    t_headers_ms: ms(t_headers),
                    t_end_ms: ms(std::time::Instant::now()),
                    attempts,
                    entries: 0,
                };
                return Ok((body, ev));
            }
            Ok((status, _, body, _)) => {
                if attempts >= 3 || (status.as_u16() != 429 && !status.is_server_error()) {
                    bail!("GET {url} -> {status}: {}", String::from_utf8_lossy(&body).trim());
                }
            }
            Err(e) if attempts >= 3 => return Err(e).with_context(|| format!("GET {url}")),
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(500 * attempts as u64)).await;
    }
}

/// Remove up to two layers of gzip (a server may HTTP-compress an
/// already-gzipped tile). Returns the decoded bytes and the layer count.
pub fn strip_gzip(body: Vec<u8>) -> Result<(Vec<u8>, u8)> {
    let mut b = body;
    let mut layers = 0;
    while layers < 2 && b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b {
        b = ctcore::maybe_gunzip(&b)?;
        layers += 1;
    }
    Ok((b, layers))
}

/// GET with a few retries on transport errors, 429, and 5xx.
pub async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut delay = Duration::from_millis(500);
    for attempt in 1..=5 {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp.bytes().await?.to_vec());
                }
                if status.as_u16() != 429 && !status.is_server_error() {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("GET {url} -> {status}: {}", body.trim());
                }
                if attempt == 5 {
                    bail!("GET {url} -> {status} after {attempt} attempts");
                }
            }
            Err(e) if attempt == 5 => {
                return Err(e).with_context(|| format!("GET {url}"));
            }
            Err(_) => {}
        }
        tokio::time::sleep(delay).await;
        delay *= 2;
    }
    unreachable!()
}
