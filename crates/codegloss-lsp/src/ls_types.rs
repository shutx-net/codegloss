//! The LSP data types this server speaks, written out.
//!
//! These used to come from `tower_lsp_server::ls_types`, a full model of the
//! specification. CodeGloss answers seven methods and reads a handful of
//! fields, so what is here is that handful and nothing else: the shapes are
//! the specification's, but only the members this server actually sends or
//! reads are declared.
//!
//! IMPORTANT: every type here is a wire format. A field's name, its
//! `skip_serializing_if`, and whether it is optional are all observable by the
//! editor, so none of them is a free choice. `tests/wire.rs` pins the JSON of
//! everything this server emits.
//!
//! What is deliberately *not* modelled:
//!
//! - Alternatives this server never picks. Hover's `contents` may be a scalar,
//!   a list, or markup, and the sync capability may be a kind or an options
//!   object; this server always sends the one form, so the type is that form.
//!   An enum with a single inhabited case is how a constant on the wire is
//!   said in the type system.
//! - `workDoneProgress` and `partialResultToken`, which flatten into most
//!   request parameters. Nothing here reports progress, and an unknown field
//!   on the way in is ignored by serde, so leaving them out reads the same
//!   messages.
//! - `InitializeParams`. The client's capabilities are not consulted: hover
//!   content is Markdown because Zed accepts nothing else, and the lens format
//!   has no alternatives to negotiate.

use serde::{Deserialize, Serialize};

/// A document's URI, kept exactly as the client wrote it.
///
/// Opaque on purpose. A URI reaches this server in `didOpen`, is used as a map
/// key, and goes back out unchanged; nothing resolves, joins or compares parts
/// of one. Keeping the client's spelling byte for byte is also what makes the
/// key work: the client asks about the document under the same string it
/// opened it with, and a normalising parse could turn two spellings into one
/// key or one into two.
///
/// This is the one place where dropping the old types loses a check.
/// `ls_types::Uri` parsed with `fluent-uri` and refused a malformed one with
/// an invalid-params error; this accepts it and uses it as a key. The client
/// is the author of these strings, and a client that invents an unparseable
/// one gets answers about a document nobody else can name - which is what a
/// rejected request produced too, minus the error.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Uri(String);

impl Uri {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Uri {
    fn from(uri: String) -> Self {
        Self(uri)
    }
}

impl From<&str> for Uri {
    fn from(uri: &str) -> Self {
        Self(uri.to_owned())
    }
}

impl std::fmt::Display for Uri {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A place in a document, counted in UTF-16 code units on the `character`
/// axis (the default position encoding, which this server does not negotiate
/// away).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// A span of a document. `end` is exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

// ---------------------------------------------------------------- initialize

/// The answer to `initialize`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

/// What this server can do. Everything it cannot is left out rather than sent
/// as `null`, which is how the protocol says "not provided".
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_document_sync: Option<TextDocumentSyncKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_lens_provider: Option<CodeLensOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_command_provider: Option<ExecuteCommandOptions>,
}

/// How the client should send changes. A number on the wire.
///
/// Only [`TextDocumentSyncKind::FULL`] is declared: the protocol also has 0
/// (none) and 2 (incremental), and this server asks for neither. See the
/// capability in `backend.rs` for why full sync is the one worth having.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TextDocumentSyncKind(i32);

impl TextDocumentSyncKind {
    /// The client resends the whole buffer on every change.
    pub const FULL: Self = Self(1);
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve_provider: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandOptions {
    pub commands: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ------------------------------------------------------- text document sync

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidOpenTextDocumentParams {
    pub text_document: TextDocumentItem,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentItem {
    pub uri: Uri,
    pub language_id: String,
    pub version: i32,
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidChangeTextDocumentParams {
    pub text_document: VersionedTextDocumentIdentifier,
    pub content_changes: Vec<TextDocumentContentChangeEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionedTextDocumentIdentifier {
    pub uri: Uri,
    pub version: i32,
}

/// One change. Under full sync the client sends no `range`, so only the
/// replacement text is read; a client that sends a range anyway is sending
/// something this server did not ask for, and the text is still the whole
/// document.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentContentChangeEvent {
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DidCloseTextDocumentParams {
    pub text_document: TextDocumentIdentifier,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentIdentifier {
    pub uri: Uri,
}

// ------------------------------------------------------------------- hover

/// `textDocument/hover` parameters.
///
/// The document and the position are flattened into the request object by the
/// specification - `TextDocumentPositionParams` is mixed in, not nested - so
/// they are declared here directly rather than behind a field.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverParams {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

#[derive(Debug, Serialize)]
pub struct Hover {
    /// Always markup. The protocol also allows a scalar or a list of marked
    /// strings, both deprecated, and this server sends neither.
    pub contents: MarkupContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

#[derive(Debug, Serialize)]
pub struct MarkupContent {
    pub kind: MarkupKind,
    pub value: String,
}

/// The one content format this server emits.
///
/// Zed advertises Markdown as the only hover format it accepts, so there is
/// nothing to negotiate; `plaintext` is left undeclared so that it cannot be
/// sent by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkupKind {
    Markdown,
}

// --------------------------------------------------------------- code lens

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensParams {
    pub text_document: TextDocumentIdentifier,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CodeLens {
    pub range: Range,
    /// Never `None` here, but optional on the wire: Zed draws nothing for a
    /// lens without a command, and `code_lens.rs` is what makes sure every
    /// lens carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Command {
    pub title: String,
    pub command: String,
}

// ---------------------------------------------------------- execute command

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandParams {
    pub command: String,
}

// ------------------------------------------------------------------ window

/// The severity of a `window/logMessage`. A number on the wire.
///
/// Only [`MessageType::INFO`] is declared. The protocol also has 1 (error),
/// 2 (warning) and 4 (log); anything this server has to say about itself goes
/// to its own log (`logging.rs`), and the one message sent to the client says
/// that it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MessageType(i32);

impl MessageType {
    pub const INFO: Self = Self(3);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogMessageParams {
    #[serde(rename = "type")]
    pub message_type: MessageType,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The numbers are the protocol's, and an editor reads them as the
    /// protocol defines them: full sync is 1 and an informational message is
    /// 3. Getting either wrong is silent - the client simply behaves as
    /// something else was asked for.
    #[test]
    fn the_enumerations_carry_the_numbers_the_protocol_gives_them() {
        assert_eq!(
            serde_json::to_value(TextDocumentSyncKind::FULL).unwrap(),
            json!(1)
        );
        assert_eq!(serde_json::to_value(MessageType::INFO).unwrap(), json!(3));
        assert_eq!(
            serde_json::to_value(MarkupKind::Markdown).unwrap(),
            json!("markdown")
        );
    }

    /// A capability that is not provided is left out, not sent as `null`. A
    /// client is entitled to read an explicit `null` as "provided, and empty".
    #[test]
    fn an_absent_capability_is_absent_from_the_json() {
        let capabilities = ServerCapabilities {
            hover_provider: Some(true),
            ..ServerCapabilities::default()
        };
        assert_eq!(
            serde_json::to_value(capabilities).unwrap(),
            json!({ "hoverProvider": true })
        );
    }

    /// Hover parameters arrive flat, not under a `textDocumentPositionParams`
    /// key. Nesting them would make every hover fail to parse.
    #[test]
    fn hover_parameters_are_read_flat() {
        let params: HoverParams = serde_json::from_value(json!({
            "textDocument": { "uri": "file:///a.rs" },
            "position": { "line": 2, "character": 7 },
            // Sent by clients that report progress, and ignored here.
            "workDoneToken": "token-1",
        }))
        .expect("hover parameters parse");

        assert_eq!(params.text_document.uri.as_str(), "file:///a.rs");
        assert_eq!(params.position, Position::new(2, 7));
    }

    /// A URI goes out exactly as it came in. It is a key, not a parsed value.
    #[test]
    fn a_uri_is_carried_through_unchanged() {
        let written = "file:///tmp/a%20b/../main.rs?v=1#L3";
        let uri: Uri = serde_json::from_value(json!(written)).expect("a uri is a string");
        assert_eq!(uri.as_str(), written);
        assert_eq!(serde_json::to_value(&uri).unwrap(), json!(written));
    }

    /// Under full sync a change carries only text, but a client is free to
    /// send the optional members anyway; reading one must not fail.
    #[test]
    fn a_change_event_ignores_what_full_sync_does_not_use() {
        let change: TextDocumentContentChangeEvent = serde_json::from_value(json!({
            "range": { "start": { "line": 0, "character": 0 },
                       "end": { "line": 0, "character": 1 } },
            "rangeLength": 1,
            "text": "// Changed.\n",
        }))
        .expect("a change event parses");
        assert_eq!(change.text, "// Changed.\n");
    }
}
