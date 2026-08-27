//! The `LanguageServer` implementation.
//!
//! IMPORTANT: no LSP request handler may translate synchronously. Handlers
//! answer from the cache only; translation happens on background tasks that
//! ask the client to refetch once results are ready. Nothing here blocks yet
//! because nothing here translates yet.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Hover,
    HoverContents, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, MarkupContent, MarkupKind, MessageType, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use tower_lsp_server::{Client, LanguageServer};

use crate::documents::DocumentStore;

/// Fixed text returned by `textDocument/hover` until real glosses land.
///
/// Hover is the only display mode that works with no user configuration, which
/// makes it the cheapest end-to-end smoke test of the extension in Zed.
pub const HOVER_PLACEHOLDER: &str = "CodeGloss: hello";

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: DocumentStore,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DocumentStore::new(),
        }
    }

    pub fn documents(&self) -> &DocumentStore {
        &self.documents
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
        self.documents.open(
            document.uri,
            document.language_id,
            document.version,
            document.text,
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Under FULL sync the client sends a single change holding the entire
        // document. Take the last one so a client that batches still wins.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        self.documents.update(
            &params.text_document.uri,
            params.text_document.version,
            change.text,
        );
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.close(&params.text_document.uri);
    }

    async fn hover(&self, _params: HoverParams) -> Result<Option<Hover>> {
        // Markdown unconditionally: Zed advertises Markdown as the only hover
        // content format it accepts, so there is nothing to negotiate.
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: HOVER_PLACEHOLDER.to_owned(),
            }),
            range: None,
        }))
    }
}
