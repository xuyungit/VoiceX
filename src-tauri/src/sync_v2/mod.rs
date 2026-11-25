//! VoiceX Sync v2 core modules (pure logic, crypto, merge, scope, and future
//! storage/transport/engine integration).

pub mod credentials;
pub mod crypto;
pub mod engine;
pub mod hlc;
pub mod knowledge;
pub mod merge;
pub mod migrate;
pub mod payloads;
pub mod scope;
mod stats_baseline;
pub mod store;
pub mod transport;
