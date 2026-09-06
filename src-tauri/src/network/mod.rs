//! Shared transports. WebSockets need explicit proxy resolution: unlike reqwest,
//! tokio-tungstenite's connect_async opens a direct TCP connection.
#[cfg(target_os = "macos")]
mod mac_pac;
mod proxy;
mod websocket;

pub(crate) use websocket::{connect_async, WsStream};
