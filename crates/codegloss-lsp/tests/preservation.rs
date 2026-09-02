//! What Issue #1 asks for, checked through the server rather than through the
//! core crate: a comment goes in, a gloss comes out, and every span that must
//! not be translated is still spelled exactly as it was.
//!
//! The engine here is the real [`PassthroughTranslator`], which returns its
//! input. That is what makes the assertions exact: anything the answers differ
//! in is the pre-processing or the post-processing, because nothing else in the
//! pipeline is allowed to change a character.
//!
//! Swapping the engine for candle re-runs these fixtures first
//! when a gloss then comes out wrong: if they still hold, the model is at fault
//! and not this code.

use std::time::Duration;

use codegloss_lsp::Backend;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::timeout;
use tower::{Service, ServiceExt};
use tower_lsp_server::LspService;
use tower_lsp_server::jsonrpc::{Request, Response};

const DOCUMENT_URI: &str = "file:///tmp/codegloss/preservation.rs";

/// Every pattern Issue #1 lists, in one file:
/// a `TODO:` prefix, identifiers of each shape, inline code, a URL, and a
/// Javadoc block with `@param` / `@return` / `@throws`, plus the comment of
/// more than one sentence Issue #49 was about.
const DOCUMENT_TEXT: &str = concat!(
    "// TODO: cache the result of find_user before UserRepository::load() runs.\n",
    "fn find_user() {}\n",
    "\n",
    "/// Returns `UserDetails` when authentication succeeds.\n",
    "///\n",
    "/// See https://example.com/docs/auth for the protocol.\n",
    "fn authenticate() {}\n",
    "\n",
    "/**\n",
    " * Returns the currently authenticated user.\n",
    " *\n",
    " * @param id the id to look up\n",
    " * @return authenticated user\n",
    " * @throws AuthenticationException if authentication failed\n",
    " */\n",
    "fn current_user() {}\n",
    "\n",
    "/// Returns the cached user. Nothing is written back.\n",
    "fn cached_user() {}\n",
);

const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// A server on the engine that ships today, with the requests it sends back
/// answered so that the worker never waits on a refresh.
fn server() -> LspService<Backend> {
    let (service, socket) = LspService::new(Backend::new);
    let (mut requests, mut responses) = socket.split();
    tokio::spawn(async move {
        while let Some(request) = requests.next().await {
            if let Some(id) = request.id().cloned() {
                let _ = futures::SinkExt::send(&mut responses, Response::from_ok(id, Value::Null))
                    .await;
            }
        }
    });
    service
}

async fn call(service: &mut LspService<Backend>, request: Request) -> Option<Value> {
    let response = service
        .ready()
        .await
        .expect("service is ready")
        .call(request)
        .await
        .expect("service has not exited");
    response.map(|response| serde_json::to_value(response).expect("response serializes"))
}

/// Brings the server up on [`DOCUMENT_TEXT`] and waits for its glosses.
async fn glossed_document() -> LspService<Backend> {
    let mut service = server();
    let mut batches: watch::Receiver<u64> = service.inner().glosses().batches_completed();

    call(
        &mut service,
        Request::build("initialize")
            .params(json!({ "capabilities": {} }))
            .id(1)
            .finish(),
    )
    .await;
    call(
        &mut service,
        Request::build("initialized").params(json!({})).finish(),
    )
    .await;
    call(
        &mut service,
        Request::build("textDocument/didOpen")
            .params(json!({
                "textDocument": {
                    "uri": DOCUMENT_URI,
                    "languageId": "rust",
                    "version": 1,
                    "text": DOCUMENT_TEXT,
                }
            }))
            .finish(),
    )
    .await;

    timeout(SETTLE_TIMEOUT, batches.changed())
        .await
        .expect("the pipeline finished a batch")
        .expect("the pipeline is still running");
    service
}

/// The gloss a hover shows, without the quoted English underneath it.
async fn gloss_at(service: &mut LspService<Backend>, line: u32, character: u32) -> String {
    let response = call(
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
    .expect("a hover request is answered");

    let value = response["result"]["contents"]["value"]
        .as_str()
        .expect("a hover carries markdown")
        .to_owned();
    let (gloss, quoted) = value
        .split_once("\n\n> ")
        .expect("a finished gloss quotes its source");
    assert!(!quoted.is_empty(), "the English is quoted underneath");
    gloss.to_owned()
}

async fn lens_titles(service: &mut LspService<Backend>) -> Vec<(u64, String)> {
    let response = call(
        service,
        Request::build("textDocument/codeLens")
            .params(json!({ "textDocument": { "uri": DOCUMENT_URI } }))
            .id(3)
            .finish(),
    )
    .await
    .expect("a code lens request is answered");

    response["result"]
        .as_array()
        .expect("the answer is a list of lenses")
        .iter()
        .map(|lens| {
            (
                lens["range"]["start"]["line"]
                    .as_u64()
                    .expect("a lens points at a line"),
                lens["command"]["title"]
                    .as_str()
                    .expect("a lens carries a title")
                    .to_owned(),
            )
        })
        .collect()
}

/// A `//` comment: the `TODO:` prefix and all three shapes of identifier come
/// back byte for byte.
#[tokio::test(flavor = "current_thread")]
async fn a_line_comment_keeps_its_prefix_and_its_identifiers() {
    let mut service = glossed_document().await;

    assert_eq!(
        gloss_at(&mut service, 0, 5).await,
        "TODO: cache the result of find_user before UserRepository::load() runs."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_doc_comment_keeps_its_inline_code() {
    let mut service = glossed_document().await;

    assert_eq!(
        gloss_at(&mut service, 3, 10).await,
        "Returns `UserDetails` when authentication succeeds."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_doc_comment_keeps_its_url() {
    let mut service = glossed_document().await;

    assert_eq!(
        gloss_at(&mut service, 5, 10).await,
        "See https://example.com/docs/auth for the protocol."
    );
}

/// The Javadoc block of Issue #1: the paragraph, the blank line and one line
/// per tag are all still there, and the tags and the exception type are
/// untouched.
///
/// The two trailing spaces are Markdown's hard line break. Without them the
/// editor would run the three tag lines together into one paragraph.
#[tokio::test(flavor = "current_thread")]
async fn a_javadoc_block_keeps_its_line_structure_and_its_tags() {
    let mut service = glossed_document().await;

    assert_eq!(
        gloss_at(&mut service, 9, 10).await,
        concat!(
            "Returns the currently authenticated user.\n",
            "\n",
            "@param id the id to look up  \n",
            "@return authenticated user  \n",
            "@throws AuthenticationException if authentication failed",
        )
    );
}

/// Two sentences in one comment come back with the space between them.
///
/// The defect of Issue #49, at the level the reader meets it: `join_sentences`
/// suppressed the space after an ASCII full stop as well as after a `。`, so a
/// comment of two sentences was shown as `...user.Nothing...`. The engine here
/// is the one that produces it in production - a server with no model pack
/// answers with its input, which is English.
#[tokio::test(flavor = "current_thread")]
async fn a_comment_of_two_sentences_keeps_the_space_between_them() {
    let mut service = glossed_document().await;

    assert_eq!(
        gloss_at(&mut service, 17, 10).await,
        "Returns the cached user. Nothing is written back."
    );
}

/// The same glosses reach the other display mode. A lens is one line high, so
/// the Javadoc block is folded and cut - that is the code lens doing it, and everything
/// that fits is still exact.
#[tokio::test(flavor = "current_thread")]
async fn the_lenses_carry_the_same_glosses() {
    let mut service = glossed_document().await;
    let titles = lens_titles(&mut service).await;

    assert_eq!(
        titles[..3],
        [
            (
                0,
                "TODO: cache the result of find_user before UserRepository::load() runs."
                    .to_owned()
            ),
            (
                3,
                "Returns `UserDetails` when authentication succeeds.".to_owned()
            ),
            (
                5,
                "See https://example.com/docs/auth for the protocol.".to_owned()
            ),
        ]
    );

    let (line, javadoc) = &titles[3];
    assert_eq!(*line, 8, "the lens sits on the comment's own first line");
    assert!(
        javadoc.starts_with(
            "Returns the currently authenticated user. @param id the id to look up @return authenticated user @throws "
        ),
        "{javadoc:?}"
    );
}

/// Nothing in the document was left without a gloss: a pattern the
/// pre-processing choked on would show up as a lens still saying "translating".
#[tokio::test(flavor = "current_thread")]
async fn every_comment_of_the_document_is_glossed() {
    let mut service = glossed_document().await;
    let titles = lens_titles(&mut service).await;

    assert_eq!(titles.len(), 5);
    for (line, title) in titles {
        assert!(
            title != codegloss_lsp::code_lens::PENDING_TITLE,
            "the comment on line {line} was never glossed"
        );
    }
}
