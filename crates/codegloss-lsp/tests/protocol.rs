//! Drives `LspService` directly as a tower `Service`.
//!
//! No process is spawned and no stdio is involved: requests go in as
//! `jsonrpc::Request` values and responses come back as `jsonrpc::Response`.
//! The server refuses everything before `initialize` (JSON-RPC -32002), so the
//! order of the calls below is part of what is under test.

use std::str::FromStr;

use codegloss_lsp::Backend;
use codegloss_lsp::backend::HOVER_PLACEHOLDER;
use serde_json::{Value, json};
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::Request;
use tower_lsp_server::ls_types::Uri;

const DOCUMENT_URI: &str = "file:///tmp/codegloss/main.rs";
const DOCUMENT_TEXT: &str = "// Return the cached user.\nfn find_user() {}\n";

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

#[tokio::test(flavor = "current_thread")]
async fn hover_returns_the_placeholder_gloss() {
    let (mut service, _socket) = LspService::new(Backend::new);

    request(&mut service, initialize_request()).await;
    notify(
        &mut service,
        Request::build("initialized").params(json!({})).finish(),
    )
    .await;
    notify(&mut service, did_open_notification()).await;

    let response = request(
        &mut service,
        Request::build("textDocument/hover")
            .params(json!({
                "textDocument": { "uri": DOCUMENT_URI },
                "position": { "line": 0, "character": 3 },
            }))
            .id(2)
            .finish(),
    )
    .await;

    let contents = &response["result"]["contents"];
    assert_eq!(contents["kind"], json!("markdown"));
    assert_eq!(contents["value"], json!(HOVER_PLACEHOLDER));
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
