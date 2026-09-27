//! Drives the message loop directly.
//!
//! No process is spawned and no stdio is involved: requests go in as
//! `lsp_server::Request` values and responses come back as JSON. The server
//! refuses everything before `initialize` (JSON-RPC -32002), so the order of
//! the calls below is part of what is under test.

mod harness;

use codegloss_lsp::code_lens::{NOOP_COMMAND, PENDING_TITLE};
use codegloss_lsp::ls_types::Uri;
use harness::Harness;
use serde_json::{Value, json};

const DOCUMENT_URI: &str = "file:///tmp/codegloss/main.rs";
/// Line 0 is a comment, line 1 is code, and line 2 mixes both after a string
/// literal wide enough that a byte offset and a UTF-16 offset disagree.
const DOCUMENT_TEXT: &str = concat!(
    "// Return the cached user.\n",
    "fn find_user() {}\n",
    "const NAME: &str = \"日本語\"; // Trailing note.\n",
);

#[test]
fn initialize_advertises_hover_and_full_sync() {
    let mut server = Harness::new();

    let response = server.request("initialize", json!({ "capabilities": {} }));
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

/// Zed takes the text of a lens from `command.title` and draws nothing for a
/// lens without a command, so the no-op command has to be advertised as
/// executable or clicking a gloss reports an unknown command.
#[test]
fn initialize_advertises_code_lenses_and_the_command_they_carry() {
    let mut server = Harness::new();

    let response = server.request("initialize", json!({ "capabilities": {} }));
    let capabilities = &response["result"]["capabilities"];

    assert_eq!(
        capabilities["codeLensProvider"],
        json!({ "resolveProvider": false })
    );
    assert_eq!(
        capabilities["executeCommandProvider"]["commands"],
        json!([NOOP_COMMAND])
    );
}

/// Brings a server up to the point where it has the fixture open.
fn opened_server() -> Harness {
    let mut server = Harness::new();
    server.initialize();
    server.did_open(DOCUMENT_URI, "rust", DOCUMENT_TEXT);
    server
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

#[test]
fn hover_over_a_comment_answers_about_that_comment() {
    let mut server = opened_server();

    let response = server.hover(DOCUMENT_URI, 0, 3);
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

#[test]
fn hover_over_code_returns_nothing() {
    let mut server = opened_server();

    // Inside `find_user` on the function line.
    let response = server.hover(DOCUMENT_URI, 1, 5);
    assert_eq!(response["result"], Value::Null);
    assert!(response.get("error").is_none(), "{response}");
}

/// `character` counts UTF-16 code units. On line 2 the Japanese string literal
/// makes the byte offset run six ahead of the code-unit offset - the comment
/// starts at code unit 26 but at byte 32 - so a server that confuses the two
/// answers on the wrong halves of the line.
#[test]
fn hover_on_a_multibyte_line_lands_on_the_right_half() {
    let mut server = opened_server();

    // Character 22 is inside the string literal.
    assert_eq!(server.hover(DOCUMENT_URI, 2, 22)["result"], Value::Null);

    // Character 30 is inside the trailing comment.
    let response = server.hover(DOCUMENT_URI, 2, 30);
    assert_hover_is_about(&response["result"], "Trailing note.");
    assert_eq!(
        response["result"]["range"]["start"],
        json!({ "line": 2, "character": 26 })
    );
}

#[test]
fn hover_in_a_document_that_was_never_opened_returns_nothing() {
    let mut server = Harness::new();
    server.initialize();

    let response = server.hover(DOCUMENT_URI, 0, 3);
    assert_eq!(response["result"], Value::Null);
}

/// One lens per comment block, on the comment's own line.
///
/// The line matters more than it looks: Zed inserts the lens as a block *above*
/// the line, so the gloss of a comment on line 2 only lands between the code and
/// the comment if the lens says line 2.
#[test]
fn code_lens_answers_one_lens_per_comment_on_the_comment_line() {
    let mut server = opened_server();

    let response = server.code_lens(DOCUMENT_URI);
    let lenses = response["result"]
        .as_array()
        .expect("the answer is a list of lenses");

    // The fixture has a comment on line 0 and a trailing comment on line 2.
    assert_eq!(lenses.len(), 2, "{response}");
    for (lens, line) in lenses.iter().zip([0, 2]) {
        assert_eq!(
            lens["range"]["start"],
            json!({ "line": line, "character": 0 })
        );
        assert_eq!(lens["range"]["end"], lens["range"]["start"]);
        assert_eq!(lens["command"]["command"], json!(NOOP_COMMAND));

        let title = lens["command"]["title"]
            .as_str()
            .expect("a lens carries a title");
        // Which of the two shows up is a race with the background pipeline;
        // `pipeline.rs` pins down each of them with an engine it controls.
        assert!(!title.is_empty(), "an empty title is never drawn");
    }
}

/// A file the client never opened has no answer, as opposed to an empty one.
#[test]
fn code_lens_for_an_unopened_document_returns_nothing() {
    let mut server = opened_server();

    let response = server.code_lens("file:///tmp/codegloss/other.rs");
    assert_eq!(response["result"], Value::Null);
    assert!(response.get("error").is_none(), "{response}");
}

/// A lens is clickable whether or not that was wanted, and the click has to be
/// answered rather than refused.
#[test]
fn executing_the_lens_command_answers_without_an_error() {
    let mut server = opened_server();

    let response = server.request(
        "workspace/executeCommand",
        json!({ "command": NOOP_COMMAND, "arguments": [] }),
    );

    assert_eq!(response["result"], Value::Null);
    assert!(response.get("error").is_none(), "{response}");
}

/// The placeholder is the one piece of UI text a lens can show that hover never
/// does. Hover falls back to the English source instead, because a popup cannot
/// be refreshed once it is on screen and because a lens sits directly above the
/// English it would otherwise be repeating.
#[test]
fn the_lens_placeholder_is_not_what_hover_falls_back_to() {
    let mut server = opened_server();

    let hover = server.hover(DOCUMENT_URI, 0, 3);
    let value = hover["result"]["contents"]["value"]
        .as_str()
        .expect("hover contents carry a string");
    assert!(!value.contains(PENDING_TITLE), "{value:?}");
}

#[test]
fn documents_follow_open_change_and_close() {
    let mut server = Harness::new();
    let uri = Uri::from(DOCUMENT_URI);

    server.request("initialize", json!({ "capabilities": {} }));
    server.did_open(DOCUMENT_URI, "rust", DOCUMENT_TEXT);

    let opened = server
        .backend()
        .documents()
        .snapshot(&uri)
        .expect("document is open");
    assert_eq!(opened.text, DOCUMENT_TEXT);
    assert_eq!(opened.version, 1);
    assert_eq!(opened.blocks.len(), 2);

    server.did_change(DOCUMENT_URI, 2, "// Changed.\n");

    let changed = server
        .backend()
        .documents()
        .snapshot(&uri)
        .expect("document is still open");
    assert_eq!(changed.text, "// Changed.\n");
    assert_eq!(changed.version, 2);
    // The comments are re-extracted from the new buffer, not carried over.
    assert_eq!(changed.blocks.len(), 1);
    assert_eq!(changed.blocks[0].text, "Changed.");

    server.did_close(DOCUMENT_URI);

    assert!(server.backend().documents().is_empty());
}

#[test]
fn requests_before_initialize_are_refused() {
    let mut server = Harness::new();

    let response = server.hover(DOCUMENT_URI, 0, 0);

    // -32002 == ServerNotInitialized.
    assert_eq!(response["error"]["code"], json!(-32002));
}

/// A notification that arrives before `initialize` is dropped rather than
/// answered - the protocol gives a notification nowhere to report an error,
/// and a server that acted on one would be reading a document it was never
/// told the client had opened.
#[test]
fn notifications_before_initialize_are_dropped() {
    let mut server = Harness::new();

    server.did_open(DOCUMENT_URI, "rust", DOCUMENT_TEXT);

    assert!(server.backend().documents().is_empty());
}

/// After `shutdown` the server is on its way out, and the protocol says to
/// refuse what arrives in the meantime rather than serve it.
#[test]
fn requests_after_shutdown_are_refused() {
    let mut server = opened_server();

    let shutdown = server.request("shutdown", Value::Null);
    assert_eq!(shutdown["result"], Value::Null);
    assert!(shutdown.get("error").is_none(), "{shutdown}");

    let response = server.hover(DOCUMENT_URI, 0, 3);
    // -32600 == InvalidRequest.
    assert_eq!(response["error"]["code"], json!(-32600));
}

/// A method this server does not implement is refused by name, not answered
/// with an empty result: a client that asked for something is entitled to know
/// it was not understood.
#[test]
fn an_unimplemented_request_says_so() {
    let mut server = opened_server();

    let response = server.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": DOCUMENT_URI },
            "position": { "line": 0, "character": 0 },
        }),
    );

    // -32601 == MethodNotFound.
    assert_eq!(response["error"]["code"], json!(-32601));
}

/// Parameters that cannot be read are an error on a request, and the request
/// is the only place there is to report one.
#[test]
fn unreadable_request_parameters_are_an_error() {
    let mut server = opened_server();

    let response = server.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": DOCUMENT_URI } }),
    );

    // -32602 == InvalidParams.
    assert_eq!(response["error"]["code"], json!(-32602));
}
