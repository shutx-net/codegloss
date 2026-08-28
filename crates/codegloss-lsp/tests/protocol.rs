//! Drives `LspService` directly as a tower `Service`.
//!
//! No process is spawned and no stdio is involved: requests go in as
//! `jsonrpc::Request` values and responses come back as `jsonrpc::Response`.
//! The server refuses everything before `initialize` (JSON-RPC -32002), so the
//! order of the calls below is part of what is under test.

use std::str::FromStr;

use codegloss_lsp::Backend;
use serde_json::{Value, json};
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::Request;
use tower_lsp_server::ls_types::Uri;

const DOCUMENT_URI: &str = "file:///tmp/codegloss/main.rs";
/// Line 0 is a comment, line 1 is code, and line 2 mixes both after a string
/// literal wide enough that a byte offset and a UTF-16 offset disagree.
const DOCUMENT_TEXT: &str = concat!(
    "// Return the cached user.\n",
    "fn find_user() {}\n",
    "const NAME: &str = \"日本語\"; // Trailing note.\n",
);

/// Sends a request and returns its response as JSON.
async fn request(service: &mut LspService<Backend>, request: Request) -> Value {
    let response = service
        .ready()
        .await
        .expect("service is ready")
        .call(request)
        .await
        .expect("service has not exited")
        .expect("a request produces a response");
    serde_json::to_value(response).expect("response serializes")
}

/// Sends a notification, which by definition has no response.
async fn notify(service: &mut LspService<Backend>, notification: Request) {
    let response = service
        .ready()
        .await
        .expect("service is ready")
        .call(notification)
        .await
        .expect("service has not exited");
    assert!(response.is_none(), "notifications must not be answered");
}

fn initialize_request() -> Request {
    Request::build("initialize")
        .params(json!({ "capabilities": {} }))
        .id(1)
        .finish()
}

fn did_open_notification() -> Request {
    Request::build("textDocument/didOpen")
        .params(json!({
            "textDocument": {
                "uri": DOCUMENT_URI,
                "languageId": "rust",
                "version": 1,
                "text": DOCUMENT_TEXT,
            }
        }))
        .finish()
}

#[tokio::test(flavor = "current_thread")]
async fn initialize_advertises_hover_and_full_sync() {
    let (mut service, _socket) = LspService::new(Backend::new);

    let response = request(&mut service, initialize_request()).await;
    let result = &response["result"];

    assert_eq!(result["capabilities"]["hoverProvider"], json!(true));
    // 1 == TextDocumentSyncKind::FULL in the LSP wire format.
    assert_eq!(result["capabilities"]["textDocumentSync"], json!(1));
    assert_eq!(result["serverInfo"]["name"], json!("codegloss-lsp"));
    assert_eq!(
        result["serverInfo"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
}

/// Brings a service up to the point where it has the fixture open.
async fn opened_service() -> LspService<Backend> {
    let (mut service, _socket) = LspService::new(Backend::new);

    request(&mut service, initialize_request()).await;
    notify(
        &mut service,
        Request::build("initialized").params(json!({})).finish(),
    )
    .await;
    notify(&mut service, did_open_notification()).await;
    service
}

/// Asserts that a hover answer is about `source`.
///
/// The value is the English source on its own until the background pipeline has
/// a gloss for it, and the gloss with the source quoted underneath once it has;
/// which of the two a given run sees depends on a background batch, so what is
/// asserted here is what both forms share. `pipeline.rs` pins down each of them
/// exactly, with an engine that only produces a gloss when told to.
fn assert_hover_is_about(result: &Value, source: &str) {
    let value = result["contents"]["value"]
        .as_str()
        .expect("hover contents carry a string");
    assert_eq!(result["contents"]["kind"], json!("markdown"));
    assert!(value.contains(source), "{value:?} is not about {source:?}");
}

async fn hover_at(service: &mut LspService<Backend>, line: u32, character: u32) -> Value {
    request(
        service,
        Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": DOCUMENT_URI },
                "position": { "line": line, "character": character },
            }))
            .id(2)
            .finish(),
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn hover_over_a_comment_answers_about_that_comment() {
    let mut service = opened_service().await;

    let response = hover_at(&mut service, 0, 3).await;
    let result = &response["result"];

    assert_hover_is_about(result, "Return the cached user.");
    // The range covers the comment only, markers included and newline excluded.
    assert_eq!(
        result["range"]["start"],
        json!({ "line": 0, "character": 0 })
    );
    assert_eq!(
        result["range"]["end"],
        json!({ "line": 0, "character": 26 })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn hover_over_code_returns_nothing() {
    let mut service = opened_service().await;

    // Inside `find_user` on the function line.
    let response = hover_at(&mut service, 1, 5).await;
    assert_eq!(response["result"], Value::Null);
    assert!(response.get("error").is_none(), "{response}");
}

/// `character` counts UTF-16 code units. On line 2 the Japanese string literal
/// makes the byte offset run six ahead of the code-unit offset - the comment
/// starts at code unit 26 but at byte 32 - so a server that confuses the two
/// answers on the wrong halves of the line.
#[tokio::test(flavor = "current_thread")]
async fn hover_on_a_multibyte_line_lands_on_the_right_half() {
    let mut service = opened_service().await;

    // Character 22 is inside the string literal.
    assert_eq!(hover_at(&mut service, 2, 22).await["result"], Value::Null);

    // Character 30 is inside the trailing comment.
    let response = hover_at(&mut service, 2, 30).await;
    assert_hover_is_about(&response["result"], "Trailing note.");
    assert_eq!(
        response["result"]["range"]["start"],
        json!({ "line": 2, "character": 26 })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn hover_in_a_document_that_was_never_opened_returns_nothing() {
    let (mut service, _socket) = LspService::new(Backend::new);
    request(&mut service, initialize_request()).await;

    let response = hover_at(&mut service, 0, 3).await;
    assert_eq!(response["result"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn documents_follow_open_change_and_close() {
    let (mut service, _socket) = LspService::new(Backend::new);
    let uri = Uri::from_str(DOCUMENT_URI).expect("valid file uri");

    request(&mut service, initialize_request()).await;
    notify(&mut service, did_open_notification()).await;

    let opened = service
        .inner()
        .documents()
        .snapshot(&uri)
        .expect("document is open");
    assert_eq!(opened.text, DOCUMENT_TEXT);
    assert_eq!(opened.version, 1);
    assert_eq!(opened.blocks.len(), 2);

    notify(
        &mut service,
        Request::build("textDocument/didChange")
            .params(json!({
                "textDocument": { "uri": DOCUMENT_URI, "version": 2 },
                "contentChanges": [{ "text": "// Changed.\n" }],
            }))
            .finish(),
    )
    .await;

    let changed = service
        .inner()
        .documents()
        .snapshot(&uri)
        .expect("document is still open");
    assert_eq!(changed.text, "// Changed.\n");
    assert_eq!(changed.version, 2);
    // The comments are re-extracted from the new buffer, not carried over.
    assert_eq!(changed.blocks.len(), 1);
    assert_eq!(changed.blocks[0].text, "Changed.");

    notify(
        &mut service,
        Request::build("textDocument/didClose")
            .params(json!({ "textDocument": { "uri": DOCUMENT_URI } }))
            .finish(),
    )
    .await;

    assert!(service.inner().documents().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn requests_before_initialize_are_refused() {
    let (mut service, _socket) = LspService::new(Backend::new);

    let response = request(
        &mut service,
        Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": DOCUMENT_URI },
                "position": { "line": 0, "character": 0 },
            }))
            .id(1)
            .finish(),
    )
    .await;

    // -32002 == ServerNotInitialized.
    assert_eq!(response["error"]["code"], json!(-32002));
}
