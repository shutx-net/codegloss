//! The CodeGloss language server.
//!
//! The binary in `main.rs` is a thin wrapper: everything lives in the library
//! so that the protocol tests can drive `LspService` in-process instead of
//! spawning a real server over stdio.

#![forbid(unsafe_code)]

pub mod backend;
pub mod code_lens;
pub mod config;
pub mod documents;
#[cfg(feature = "candle")]
pub mod model_pack;
pub mod translation;

pub use backend::Backend;
pub use config::ServerConfig;
pub use translation::TranslationService;
