//! Entry point of the CodeGloss language server. Speaks LSP over stdio.

#![forbid(unsafe_code)]

use codegloss_lsp::Backend;
use tower_lsp_server::{LspService, Server};

#[tokio::main]
async fn main() {
    init_tracing();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
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
