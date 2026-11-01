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
