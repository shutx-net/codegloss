//! Entry point of the CodeGloss language server. Speaks LSP over stdio.

#![forbid(unsafe_code)]

use std::sync::Arc;

use codegloss_lsp::client::Client;
use codegloss_lsp::server::{Dispatcher, serve};
use codegloss_lsp::{Backend, ServerConfig, config, translation};
use lsp_server::Connection;

fn main() -> std::process::ExitCode {
    codegloss_lsp::logging::init();

    // Downloading is a thing a person does once, not a thing a language server
    // does while an editor waits for `initialize`. See `model_pack.rs`.
    #[cfg(feature = "candle")]
    if std::env::args().any(|argument| argument == codegloss_lsp::model_pack::FETCH_FLAG) {
        return fetch_model();
    }

    // The engine the server starts on: candle when a model pack is already
    // there, the passthrough otherwise. Which one it is has to be settled
    // before the first request, because it names the cache keys. The weights
    // are not read here - candle opens its pack and reads it the first time
    // something is actually translated, so a session that answers everything
    // from the gloss cache never pays for the model at all.
    let settings = ServerConfig::from_environment();
    let engine = config::engine(&settings);
    tracing::info!(model_version = engine.model_version(), "starting");

    // ...and the engine it may finish on. Starting in English and swapping
    // candle in once the pack has been fetched is what keeps a first run from
    // being "install this, then go and read the README": see `model_pack`.
    let (switch, engine) = translation::engine_channel(engine);
    #[cfg(feature = "candle")]
    codegloss_lsp::model_pack::spawn_download(&settings, switch);
    // Without an engine to load a pack into there is nothing to fetch, and
    // dropping the switch is what tells the pipeline the engine is final.
    #[cfg(not(feature = "candle"))]
    drop(switch);

    // IMPORTANT: `stdio` takes stdout for the protocol. Nothing in this
    // workspace may print to it; the log goes to stderr (`logging.rs`).
    let (connection, io) = Connection::stdio();
    let client = Arc::new(Client::new(connection.sender.clone()));
    let backend = Backend::with_cache(
        Arc::clone(&client),
        engine,
        Arc::new(config::cache(&settings)),
    );

    serve(&connection, Dispatcher::new(backend, client));

    // IMPORTANT: the io threads are dropped, not joined.
    //
    // `IoThreads::join` waits for the writer, and the writer ends only once
    // every clone of the sender is gone - including the one the translation
    // worker holds. A worker in the middle of a batch would make the server
    // linger for as long as inference takes, and one waiting out a refresh
    // that nobody will answer now for another five seconds. An editor that
    // asked the server to exit and watched it sit there would call that a
    // hang, and it would be right.
    //
    // Nothing is lost by leaving: every message is flushed as it is written
    // (`lsp_server::Message::write`), and the last thing sent on this path is
    // the answer to `shutdown`, which the client had to receive before it
    // could send the `exit` that got us here. What a batch in flight had
    // translated is dropped instead of being cached, and the next session
    // translates it again.
    drop(io);
    drop(connection);
    std::process::ExitCode::SUCCESS
}

/// Downloads the model pack and exits, instead of serving.
#[cfg(feature = "candle")]
fn fetch_model() -> std::process::ExitCode {
    let settings = ServerConfig::from_environment();
    let Some(cache) = config::cache_root(&settings) else {
        tracing::error!("no cache directory could be found to download the model pack into");
        return std::process::ExitCode::FAILURE;
    };
    match codegloss_lsp::model_pack::obtain(&cache, &codegloss_lsp::model_pack::base_url()) {
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
