//! `mmlab watch` — print every WebSocket frame the lab sends.
//!
//! The reason this exists: `mm_api::ws` decodes an event from its `data`
//! object alone, and several events put what matters in `broadcast` instead.
//! Which ones, and under which key, is not in the published documentation and
//! differs between "somebody was added" and "you were added". Reading the
//! frames is the only way to find out, and guessing produced a decoder that
//! silently classified them as `Unknown`.
//!
//! Raw on purpose. Nothing here parses; it prints what arrived.

use crate::{Error, Result};

pub async fn watch(username: &str) -> Result<()> {
    let token = crate::seed::token_for(username).await?;
    let client = mm_http::Client::new(crate::stack::URL)
        .map_err(|e| Error(e.to_string()))?
        .with_token(token.clone());
    let url = client
        .websocket_url(&[])
        .map_err(|e| Error(e.to_string()))?;
    let mut socket = mm_http::WsConn::connect(&url, &token)
        .await
        .map_err(|e| Error(e.to_string()))?;

    println!("watching {url} as {username} — ^C to stop");
    while let Some(frame) = socket.next_text().await {
        let text = frame.map_err(|e| Error(e.to_string()))?;
        // Pretty-printed, because these are the payloads being read by eye.
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap_or(text)),
            Err(_) => println!("{text}"),
        }
    }
    Ok(())
}
