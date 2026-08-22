//! Minimal server-sent-events pump shared by both adapters.

use anyhow::{Context, Result};
use futures_util::StreamExt;

/// Feed each `data:` payload to `on_data`. Return `Ok(false)` from the
/// callback to stop early (e.g. on `[DONE]`).
pub(crate) async fn pump<S, F>(mut stream: S, mut on_data: F) -> Result<()>
where
    S: futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin,
    F: FnMut(&str) -> Result<bool>,
{
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("network error while streaming")?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                if !on_data(data.trim())? {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}
