//! The `LanguageServer` implementation.
//!
//! IMPORTANT: no request handler here may produce a gloss on its own. Handlers
//! read what the background pipeline has already cached and queue what is
//! missing; the engine runs in [`crate::translation`], never on the path of a
//! request. That no call into the engine can be found anywhere in this file is
//! not an accident, it is the invariant.

use std::sync::Arc;

use codegloss_core::GlossCache;
use codegloss_translator::{PassthroughTranslator, Translator};
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::{
    CodeLens, CodeLensOptions, CodeLensParams, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, ExecuteCommandOptions,
    ExecuteCommandParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, LSPAny, MarkupContent, MarkupKind,
    MessageType, ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri,
};
use tower_lsp_server::{Client, LanguageServer};

use crate::code_lens;
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
        Self::with_cache(client, engine, Arc::new(GlossCache::default()))
    }

    /// The same server again, over a cache the caller built.
    ///
    /// `main.rs` uses it to hand in a cache backed by a directory, so that a
    /// restart does not re-translate what the last run already translated.
    pub fn with_cache(client: Client, engine: Arc<dyn Translator>, cache: Arc<GlossCache>) -> Self {
        Self {
            documents: DocumentStore::new(),
            glosses: TranslationService::spawn(client.clone(), engine, cache),
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
            .enqueue(uri.clone(), self.documents.comment_sources(uri));
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
                code_lens_provider: Some(CodeLensOptions {
                    // A lens leaves here complete. Resolving one is for a
                    // client that wants to defer expensive work per lens, and
                    // there is none: a title is a cache lookup.
                    resolve_provider: Some(false),
                }),
                // Advertised only so that the lenses can be clicked without
                // the client reporting an unknown command. See
                // [`code_lens::NOOP_COMMAND`].
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![code_lens::NOOP_COMMAND.to_owned()],
                    ..ExecuteCommandOptions::default()
                }),
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

        // Keyed on the comment as written, which is what the gloss was built
        // from; the flattened `text` is only what the popup quotes underneath.
        let value = match self.glosses.lookup(&hit.block.raw) {
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

    /// One lens per comment block, each showing the gloss of that comment.
    ///
    /// Zed draws a lens as a line of its own above the line it points at, which
    /// puts the Japanese directly above the English comment - the display mode
    /// the project is aiming at. See [`crate::code_lens`] for the constraints
    /// that shape a lens, and for why one without a gloss yet says "翻訳中"
    /// where hover shows the source instead.
    ///
    /// Nothing here waits for the engine: a block with no gloss is queued and
    /// gets a placeholder, and the `workspace/codeLens/refresh` the pipeline
    /// sends when the batch lands brings the client back for the real title.
    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;

        // `None` for a document that was never opened, an empty list for one
        // that has no comments. The first is a question this server cannot
        // answer; the second is an answer.
        let Some((lenses, missing)) = self.documents.with_blocks(&uri, |blocks| {
            let mut lenses = Vec::with_capacity(blocks.len());
            let mut missing = false;
            for block in blocks {
                match self.glosses.lookup(&block.raw) {
                    Some(gloss) => lenses.push(code_lens::glossed(block, &gloss)),
                    None => {
                        missing = true;
                        lenses.push(code_lens::pending(block));
                    }
                }
            }
            (lenses, missing)
        }) else {
            return Ok(None);
        };

        if missing {
            // Outside the closure on purpose: it reads the document store
            // again, and the closure ran with the document locked.
            self.request_glosses(&uri);
        }
        Ok(Some(lenses))
    }

    /// Accepts [`code_lens::NOOP_COMMAND`] and does nothing with it.
    ///
    /// Every lens carries a command because Zed draws no lens that lacks one,
    /// and a drawn lens is clickable whether or not anything should happen.
    /// Answering `null` is what keeps a click from raising an error in the
    /// editor.
    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<LSPAny>> {
        if params.command != code_lens::NOOP_COMMAND {
            // Not worth failing the request over: an unknown command here is
            // the client's mistake, and an error would surface as a popup.
            tracing::debug!(command = %params.command, "ignoring an unknown command");
        }
        Ok(None)
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
    format!("{}\n\n> {source}", with_hard_breaks(gloss))
}

/// Keeps the line structure of a gloss visible once Markdown renders it.
///
/// The gloss of a doc comment is several lines - a paragraph, then a `@return`
/// line, then a `@throws` line - and Markdown runs consecutive lines of a
/// paragraph together. Two trailing spaces are CommonMark's hard line break,
/// which is what keeps each tag on a line of its own.
///
/// Blank lines already separate paragraphs and need nothing, and a fenced code
/// block is verbatim: adding spaces inside one would add them to the code.
fn with_hard_breaks(gloss: &str) -> String {
    let lines: Vec<&str> = gloss.lines().collect();
    let mut rendered = String::with_capacity(gloss.len());
    let mut fenced = false;

    for (index, line) in lines.iter().enumerate() {
        rendered.push_str(line);
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }

        let Some(next) = lines.get(index + 1) else {
            break;
        };
        if !fenced && !line.is_empty() && !next.is_empty() {
            rendered.push_str("  ");
        }
        rendered.push('\n');
    }
    rendered
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

    /// A one-line gloss is left exactly as it is: the hard break is only for
    /// the lines a doc comment's structure produces.
    #[test]
    fn a_single_line_gloss_gains_nothing() {
        assert_eq!(with_hard_breaks("一行だけ。"), "一行だけ。");
    }

    #[test]
    fn the_lines_of_a_doc_comment_gloss_stay_apart() {
        assert_eq!(
            with_hard_breaks("本文。\n\n@return ユーザー\n@throws AuthError 失敗した場合"),
            "本文。\n\n@return ユーザー  \n@throws AuthError 失敗した場合"
        );
    }

    /// Inside a fence the text is code, and two spaces are two spaces.
    #[test]
    fn a_fenced_example_is_left_alone() {
        let gloss = "例:\n\n```\nlet user = find_user(id);\nlet name = user.name;\n```";
        assert_eq!(with_hard_breaks(gloss), gloss);
    }
}
