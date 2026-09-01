//! Entry point of the CodeGloss language server. Speaks LSP over stdio.

#![forbid(unsafe_code)]

use std::sync::Arc;

use codegloss_lsp::{Backend, ServerConfig, config};
use tower_lsp_server::{LspService, Server};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    init_tracing();

    // Downloading is a thing a person does once, not a thing a language server
    // does while an editor waits for `initialize`. See `model_pack.rs`.
    #[cfg(feature = "candle")]
    if std::env::args().any(|argument| argument == codegloss_lsp::model_pack::FETCH_FLAG) {
        return fetch_model();
    }

    // The engine is chosen once, before the first request: loading a model
    // takes long enough that doing it inside `initialize` would delay the
    // editor's first paint, and the choice cannot change while the server
    // runs.
    let settings = ServerConfig::from_environment();
    let engine = config::engine(&settings);
    tracing::info!(model_version = engine.model_version(), "starting");

    // Built here rather than inside the closure: `LspService::new` may call it
    // more than once, and every call has to answer from the same glosses.
    let cache = Arc::new(config::cache(&settings));

    let (service, socket) = LspService::new(move |client| {
        Backend::with_cache(client, Arc::clone(&engine), Arc::clone(&cache))
    });
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
    std::process::ExitCode::SUCCESS
}

/// Downloads the model pack and exits, instead of serving.
#[cfg(feature = "candle")]
fn fetch_model() -> std::process::ExitCode {
    let settings = ServerConfig::from_environment();
    let Some(cache) = config::cache_directory_for(&settings) else {
        tracing::error!("no cache directory could be found to download the model pack into");
        return std::process::ExitCode::FAILURE;
    };
    match codegloss_lsp::model_pack::fetch(&cache, &codegloss_lsp::model_pack::base_url()) {
        Ok(pack) => {
            tracing::info!(pack = %pack.display(), "done");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!("the model pack could not be downloaded: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Sends every log line to stderr.
///
/// IMPORTANT: stdout carries the JSON-RPC stream. A single stray byte there
/// corrupts the protocol and the editor kills the server, so this crate must
/// never `println!`.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CODEGLOSS_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
        .expect("the fallback filter is a valid directive");

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        // The editor shows this in a plain log pane; escape codes would be noise.
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
}
