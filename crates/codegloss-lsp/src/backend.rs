//! The `LanguageServer` implementation.
//!
//! IMPORTANT: no request handler here may produce a gloss on its own. Handlers
//! read what the background pipeline has already cached and queue what is
//! missing; the engine runs in [`crate::translation`], never on the path of a
//! request. That no call into the engine can be found anywhere in this file is
//! not an accident, it is the invariant.

use std::sync::Arc;

use codegloss_translator::{PassthroughTranslator, Translator};
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Hover,
    HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, MarkupContent, MarkupKind, MessageType, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{Client, LanguageServer};

use crate::documents::DocumentStore;
use crate::translation::TranslationService;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: DocumentStore,
    glosses: TranslationService,
}

impl Backend {
    /// The server as it runs for real, on the engine of the day.
    ///
    /// Must be called from inside a tokio runtime: it starts the background
    /// worker.
    pub fn new(client: Client) -> Self {
        Self::with_engine(client, Arc::new(PassthroughTranslator))
    }

    /// The same server on an engine of the caller's choosing.
    ///
    /// The tests use it to count what reaches the engine and to tell a gloss
    /// apart from the source text, which the passthrough engine cannot do
    /// because its output is its input. P7 will use it to pick candle when a
    /// model pack is installed and to fall back when it is not.
    pub fn with_engine(client: Client, engine: Arc<dyn Translator>) -> Self {
        Self {
            documents: DocumentStore::new(),
            glosses: TranslationService::spawn(client.clone(), engine),
            client,
        }
    }

    pub fn documents(&self) -> &DocumentStore {
        &self.documents
    }

    pub fn glosses(&self) -> &TranslationService {
        &self.glosses
    }

    /// Queues every comment of a document. Returns at once; the work happens on
    /// the pipeline's worker.
    fn request_glosses(&self, uri: &Uri) {
        self.glosses
            .enqueue(uri.clone(), self.documents.comment_texts(uri));
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                // FULL sync: the client resends the whole buffer on every
                // change, so there is no incremental patching to get wrong.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "codegloss-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        tracing::info!("codegloss-lsp {} initialized", env!("CARGO_PKG_VERSION"));
        // Only now may the pipeline ask the client to refetch anything: a
        // `workspace/*/refresh` sent earlier is refused with -32002.
        self.glosses.mark_initialized();
        self.client
            .log_message(MessageType::INFO, "CodeGloss language server initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        tracing::debug!(
            uri = document.uri.as_str(),
            language_id = document.language_id.as_str(),
            "did_open"
        );

        let uri = document.uri.clone();
        self.documents.open(
            document.uri,
            document.language_id,
            document.version,
            document.text,
        );
        // Queued as the file opens rather than when the user first hovers, so
        // that reading a file finds the glosses already there.
        self.request_glosses(&uri);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Under FULL sync the client sends a single change holding the entire
        // document. Take the last one so a client that batches still wins.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        let uri = params.text_document.uri;
        self.documents
            .update(&uri, params.text_document.version, change.text);
        // One job per keystroke reaches the queue; the pipeline collapses the
        // burst into a single batch.
        self.request_glosses(&uri);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.close(&params.text_document.uri);
    }

    /// Answers with the comment under the cursor, and with nothing at all
    /// anywhere else.
    ///
    /// Returning `None` over code is what keeps CodeGloss out of the way of the
    /// other language servers Zed runs alongside it: hovers from every server
    /// are merged, so an unconditional answer here would pad every rust-analyzer
    /// popup with a CodeGloss section.
    ///
    /// A comment with no gloss yet answers with the source text as it stands.
    /// The protocol has no `workspace/hover/refresh`, so an answer already on
    /// screen cannot be replaced: showing the English is more use than showing
    /// a placeholder, and the next hover over the same comment shows the
    /// Japanese.
    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let Some(hit) = self.documents.comment_block_at(&uri, position.position) else {
            return Ok(None);
        };

        let value = match self.glosses.lookup(&hit.block.text) {
            Some(gloss) => gloss_markup(&gloss, &hit.block.text),
            None => {
                self.request_glosses(&uri);
                hit.block.text
            }
        };

        // Markdown unconditionally: Zed advertises Markdown as the only hover
        // content format it accepts, so there is nothing to negotiate.
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(hit.range),
        }))
    }
}

/// Renders a finished gloss: the Japanese as a paragraph, the English quoted
/// underneath it.
///
/// The source stays visible because a machine translation of a comment is worth
/// checking against the original, and because the quote is what tells the two
/// apart while the engine is still a passthrough.
///
/// `source` is one line by construction - the parser joins a block's lines with
/// a single space - so a single `> ` prefix quotes all of it.
fn gloss_markup(gloss: &str, source: &str) -> String {
    format!("{gloss}\n\n> {source}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gloss_is_shown_above_the_quoted_source() {
        assert_eq!(
            gloss_markup(
                "キャッシュされたユーザーを返す。",
                "Return the cached user."
            ),
            "キャッシュされたユーザーを返す。\n\n> Return the cached user."
        );
    }
}
