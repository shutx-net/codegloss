//! The CodeGloss language server.
//!
//! The binary in `main.rs` is a thin wrapper: everything lives in the library
//! so that the protocol tests can drive the message loop in-process instead of
//! spawning a real server over stdio.
//!
//! The protocol is split in three. [`ls_types`] is the data - the LSP types
//! this server reads and writes, written out rather than taken from a model of
//! the whole specification. [`server`] is the client's half of the
//! conversation, and [`client`] is this server's half. JSON-RPC framing itself
//! comes from `lsp-server`.

#![forbid(unsafe_code)]

pub mod backend;
pub mod client;
pub mod code_lens;
pub mod config;
pub mod documents;
pub mod logging;
pub mod ls_types;
#[cfg(feature = "candle")]
pub mod model_pack;
pub mod server;
pub mod translation;

pub use backend::Backend;
pub use config::ServerConfig;
pub use translation::TranslationService;
