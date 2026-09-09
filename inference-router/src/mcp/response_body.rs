// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use futures::StreamExt;

pub(super) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub(super) async fn text(response: reqwest::Response) -> Result<String, &'static str> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("MCP response exceeds its byte limit");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "MCP response transport failed")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("MCP response exceeds its byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "MCP response is not UTF-8")
}
