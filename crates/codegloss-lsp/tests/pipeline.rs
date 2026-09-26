//! The background translation pipeline, driven through the message loop.
//!
//! What is under test is the promise of AGENTS.md: a request handler answers
//! from the cache and never waits for the engine. The engine used here refuses
//! to produce anything until the test lets it, which is what makes that promise
//! checkable - a handler that ran the engine inline would sit there forever
//! instead of failing a timing assertion that happens to pass on a fast machine.
//!
//! It also glosses text visibly (`[ja] ...`), which the passthrough engine
//! cannot do: with output equal to input there is no way to tell an answer that
//! came from the cache from one that fell back to the source.

mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use codegloss_core::Segment;
use codegloss_lsp::code_lens::PENDING_TITLE;
use codegloss_lsp::translation::EngineSwitch;
use codegloss_translator::Translator;
use harness::{Batches, Harness, SeenRequests, answers_within, wait_until};
use serde_json::{Value, json};

const DOCUMENT_URI: &str = "file:///tmp/codegloss/main.rs";
const DOCUMENT_TEXT: &str = concat!(
    "// Return the cached user.\n",
    "fn find_user() {}\n",
    "// Fails when the id is unknown.\n",
    "fn fail() {}\n",
);
const FIRST_COMMENT: &str = "Return the cached user.";

/// How long a handler is given to answer while the engine is blocked. The
/// handler does a map lookup, so this is orders of magnitude more than it
/// needs; what it rules out is a handler that waits for the model.
const HANDLER_PATIENCE: Duration = Duration::from_millis(50);

/// Longer than the worker's debounce window, so that "no batch ran" means the
/// queue was empty and not that a batch was still being collected.
const PAST_THE_DEBOUNCE: Duration = Duration::from_millis(400);

/// An engine that blocks until the test releases it, and counts its calls.
struct TestEngine {
    calls: AtomicUsize,
    permits: Mutex<mpsc::Receiver<()>>,
    release: Mutex<mpsc::Sender<()>>,
    /// What `model_version` answers. It is part of every cache key, so two
    /// engines that disagree here cannot read each other's glosses - which is
    /// what the swap test turns into an observation.
    version: String,
    /// The prefix its glosses carry, so that an answer can be traced back to
    /// the engine that produced it.
    tag: String,
}

impl TestEngine {
    fn new() -> Arc<Self> {
        Self::named("test-engine-1", "ja")
    }

    fn named(version: &str, tag: &str) -> Arc<Self> {
        let (release, permits) = mpsc::channel();
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            permits: Mutex::new(permits),
            release: Mutex::new(release),
            version: version.to_owned(),
            tag: tag.to_owned(),
        })
    }

    /// Lets one batch through.
    fn release_one_batch(&self) {
        self.release
            .lock()
            .expect("the release channel is not poisoned")
            .send(())
            .expect("the engine is still alive");
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Translator for TestEngine {
    fn translate(&self, segments: &[Segment]) -> anyhow::Result<Vec<String>> {
        // Blocking here is the point: it stands in for the seconds a real
        // model takes. It is only safe because the pipeline runs the engine on
        // a worker thread of its own; on the thread that reads the connection
        // it would deadlock the whole server, which is exactly the failure
        // this test is here to catch.
        self.permits
            .lock()
            .expect("the permit channel is not poisoned")
            .recv()
            .expect("the test released the engine before dropping it");

        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(segments
            .iter()
            .map(|segment| format!("[{}] {}", self.tag, segment.text()))
            .collect())
    }

    fn model_version(&self) -> &str {
        &self.version
    }
}

/// Brings up a server on `engine`.
///
/// The harness answers the requests it sends as it records them, which
/// matters: `workspace/*/refresh` is a request, not a notification, and a
/// worker waiting on an answer that never comes would make everything after
/// it look broken.
fn server(engine: Arc<TestEngine>) -> Harness {
    Harness::with_engine(engine as Arc<dyn Translator>)
}

/// The same, on an engine the test can replace while the server is running.
///
/// The switch comes back instead of being dropped, which is the whole
/// difference from [`server`]: a server that may still be given another engine
/// keeps its worker watching for one.
fn swappable_server(engine: Arc<TestEngine>) -> (Harness, EngineSwitch) {
    Harness::swappable(engine as Arc<dyn Translator>)
}

fn initialize(server: &mut Harness, send_initialized: bool) {
    server.request("initialize", json!({ "capabilities": {} }));
    if send_initialized {
        server.notify("initialized", json!({}));
    }
}

fn did_open(server: &mut Harness, text: &str) {
    did_open_as(server, DOCUMENT_URI, "rust", text);
}

/// The `languageId` is how the server learns what a buffer is - no extension is
/// consulted anywhere - and it decides which comment rules the document's
/// blocks carry.
fn did_open_as(server: &mut Harness, uri: &str, language_id: &str, text: &str) {
    server.did_open(uri, language_id, text);
}

fn did_change(server: &mut Harness, version: i32, text: &str) {
    server.did_change(DOCUMENT_URI, version, text);
}

/// The hover answer at a position, as the string the editor would render.
fn hover_value(server: &mut Harness, line: u32, character: u32) -> Value {
    hover_value_in(server, DOCUMENT_URI, line, character)
}

fn hover_value_in(server: &mut Harness, uri: &str, line: u32, character: u32) -> Value {
    server.hover(uri, line, character)["result"]["contents"]["value"].clone()
}

/// The titles of the lenses of the fixture, paired with the lines they sit on.
fn code_lens_titles(server: &mut Harness) -> Vec<(u64, String)> {
    let response = server.code_lens(DOCUMENT_URI);

    response["result"]
        .as_array()
        .expect("the answer is a list of lenses")
        .iter()
        .map(|lens| {
            let line = lens["range"]["start"]["line"]
                .as_u64()
                .expect("a lens points at a line");
            let title = lens["command"]["title"]
                .as_str()
                .expect("a lens carries a title")
                .to_owned();
            (line, title)
        })
        .collect()
}

/// The refresh requests the server sent, in order. Other server-to-client
/// traffic (`window/logMessage`) is not what these tests are about.
fn refresh_requests(seen: &SeenRequests) -> Vec<String> {
    seen.methods()
        .into_iter()
        .filter(|method| method.ends_with("/refresh"))
        .collect()
}

/// The whole contract in one pass: the first hover answers while the engine is
/// still busy, the client is asked to refetch when the batch lands, and the
/// hover after that carries the gloss.
#[test]
fn hover_answers_from_the_cache_and_never_waits_for_the_engine() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let seen = server.seen().clone();
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);

    // The engine is holding: nothing can be in the cache yet. The answer is the
    // English source, and it arrives now rather than when the model is done.
    let (mut server, first) = answers_within(
        HANDLER_PATIENCE,
        "hover must not wait for the engine",
        server,
        |server| hover_value(server, 0, 3),
    );
    assert_eq!(first, json!(FIRST_COMMENT));

    engine.release_one_batch();
    batches.next();

    // Same position, same buffer: only the cache changed.
    let second = hover_value(&mut server, 0, 3);
    assert_eq!(
        second,
        json!("[ja] Return the cached user.\n\n> Return the cached user.")
    );

    // Hover has no refresh of its own, but the hint requests do, and they are
    // what makes a gloss appear without the user asking twice. One round for
    // the whole batch: a refresh per translation would have the editor refetch
    // every visible buffer over and over.
    assert_eq!(
        refresh_requests(&seen),
        vec![
            "workspace/inlayHint/refresh".to_owned(),
            "workspace/codeLens/refresh".to_owned(),
        ]
    );

    // Both comments of the document went in one batch, not one batch each.
    assert_eq!(engine.calls(), 1);
}

/// Every comment of the file is queued when it opens, so the gloss is there
/// before the reader hovers anything.
#[test]
fn opening_a_file_translates_all_of_its_comments() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);
    engine.release_one_batch();
    batches.next();

    assert_eq!(
        hover_value(&mut server, 2, 5),
        json!("[ja] Fails when the id is unknown.\n\n> Fails when the id is unknown.")
    );
}

/// Re-queueing a text that is already cached must not reach the engine: with a
/// real model every duplicate is another second of CPU.
#[test]
fn a_text_that_is_already_cached_is_not_translated_again() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);
    engine.release_one_batch();
    batches.next();
    assert_eq!(engine.calls(), 1);

    // An edit that leaves the comments alone still re-extracts and re-queues
    // them, and a hover over an already glossed comment queues the document
    // again as well.
    did_change(&mut server, 2, DOCUMENT_TEXT);
    batches.next();

    assert_eq!(
        engine.calls(),
        1,
        "the second batch had nothing new and must not have run the engine"
    );
    assert_eq!(
        hover_value(&mut server, 0, 3),
        json!("[ja] Return the cached user.\n\n> Return the cached user.")
    );
}

/// A comment that only appears after an edit is picked up and glossed.
#[test]
fn an_edited_comment_is_translated_again() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);
    engine.release_one_batch();
    batches.next();

    did_change(&mut server, 2, "// Something else entirely.\n");
    engine.release_one_batch();
    batches.next();

    assert_eq!(
        hover_value(&mut server, 0, 3),
        json!("[ja] Something else entirely.\n\n> Something else entirely.")
    );
    assert_eq!(engine.calls(), 2);
}

/// A client that has not sent `initialized` yet would refuse the refresh with
/// -32002, so the pipeline keeps quiet until it has.
#[test]
fn nothing_is_refreshed_before_the_client_is_initialized() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let seen = server.seen().clone();
    let mut batches = Batches::of(&server);

    initialize(&mut server, false);
    did_open(&mut server, DOCUMENT_TEXT);
    engine.release_one_batch();
    batches.next();

    assert!(
        refresh_requests(&seen).is_empty(),
        "a client that has not finished initializing refuses these with -32002"
    );
    // The work still happened; only the notification was held back.
    assert_eq!(engine.calls(), 1);
}

/// The lens half of the contract: an answer arrives while the engine is still
/// busy, and the placeholder in it is replaced once the batch lands.
///
/// The replacement is what makes the placeholder defensible in the first place.
/// Hover cannot do this - there is no `workspace/hover/refresh` - which is why
/// it falls back to the English source instead of saying "translating".
#[test]
fn a_lens_shows_a_placeholder_until_its_gloss_lands() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let seen = server.seen().clone();
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);

    // The engine is holding, so nothing can be cached yet. The answer still
    // comes back now, with one lens per comment on the comment's own line.
    let (mut server, pending) = answers_within(
        HANDLER_PATIENCE,
        "code lens must not wait for the engine",
        server,
        code_lens_titles,
    );
    assert_eq!(
        pending,
        vec![(0, PENDING_TITLE.to_owned()), (2, PENDING_TITLE.to_owned()),]
    );

    engine.release_one_batch();
    batches.next();

    // The client is told to come back for them, which is the only reason a
    // placeholder is acceptable.
    assert!(
        refresh_requests(&seen).contains(&"workspace/codeLens/refresh".to_owned()),
        "{:?}",
        refresh_requests(&seen)
    );

    // Same document, same request: only the cache changed.
    assert_eq!(
        code_lens_titles(&mut server),
        vec![
            (0, "[ja] Return the cached user.".to_owned()),
            (2, "[ja] Fails when the id is unknown.".to_owned()),
        ]
    );
    // The lens request queued the misses it saw, and the retry found them all
    // cached rather than running the engine a second time.
    assert_eq!(engine.calls(), 1);
}

/// A document with nothing to translate must not wake the worker at all.
#[test]
fn a_file_without_comments_queues_nothing() {
    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let seen = server.seen().clone();
    let batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, "fn main() {}\n");

    // Give the worker every chance to run a batch it should not have: this
    // waits past the debounce window, so a job that had been queued would
    // have become a batch by now.
    batches.none_within(PAST_THE_DEBOUNCE);
    assert_eq!(engine.calls(), 0);
    assert!(refresh_requests(&seen).is_empty());
}

/// Replacing the engine puts every gloss the old one made out of reach, and the
/// client is told to come back for the new ones.
///
/// This is a server that started in English, downloaded a model pack and swapped
/// candle in a minute later. Nothing re-translates by itself: the glosses of the
/// engine that was replaced are keyed under its model version and simply stop
/// being found, and the refresh is what brings the client back to ask.
#[test]
fn replacing_the_engine_makes_the_client_come_back_for_new_glosses() {
    let english = TestEngine::named("test-engine-1", "en");
    let (mut server, switch) = swappable_server(Arc::clone(&english));
    let seen = server.seen().clone();
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open(&mut server, DOCUMENT_TEXT);
    english.release_one_batch();
    batches.next();

    assert_eq!(
        hover_value(&mut server, 0, 5),
        json!("[en] Return the cached user.\n\n> Return the cached user.")
    );
    let before = refresh_requests(&seen).len();

    // What the downloader does once the model pack is in place.
    let japanese = TestEngine::named("test-engine-2", "ja");
    switch
        .send(Arc::clone(&japanese) as Arc<dyn Translator>)
        .expect("the server is still running");

    // The swap on its own asks the client to refetch. Nothing has been
    // translated at this point: there is no batch, and the new engine has not
    // been called.
    wait_until("the swap asked the client to refetch", || {
        refresh_requests(&seen).len() > before
    });
    assert_eq!(japanese.calls(), 0);

    // The client comes back, and the glosses it had are not there any more.
    assert_eq!(
        code_lens_titles(&mut server),
        vec![(0, PENDING_TITLE.to_owned()), (2, PENDING_TITLE.to_owned())]
    );

    // That request queued them again, this time for the new engine.
    japanese.release_one_batch();
    batches.next();
    assert_eq!(
        hover_value(&mut server, 0, 5),
        json!("[ja] Return the cached user.\n\n> Return the cached user.")
    );

    // The engine that was replaced ran for the first batch and never again.
    assert_eq!(english.calls(), 1);
    assert_eq!(japanese.calls(), 1);
}

/// The chain this whole change is about, end to end: `didOpen` says the buffer
/// is Go, the registry stamps its blocks with Go's rules, the queue carries
/// them, and the worker builds the plan with them. An indented run is then an
/// example, and its gloss is the code itself rather than a translation of it.
///
/// A worker that assumed one language would still pass every other test in this
/// file - `// Return the cached user.` is a comment in both - which is why the
/// two travel together (`CommentSource`).
#[test]
fn a_go_document_is_glossed_under_go_rules() {
    const GO_URI: &str = "file:///tmp/codegloss/main.go";

    let engine = TestEngine::new();
    let mut server = server(Arc::clone(&engine));
    let mut batches = Batches::of(&server);

    initialize(&mut server, true);
    did_open_as(
        &mut server,
        GO_URI,
        "go",
        concat!(
            "// Find returns the user.\n",
            "//\n",
            "//\tuser := Find(id)\n",
            "func Find(id uint64) {}\n",
        ),
    );

    engine.release_one_batch();
    batches.next();

    // The prose was translated.
    assert_eq!(
        hover_value_in(&mut server, GO_URI, 0, 5),
        json!("[ja] Find returns the user.\n\n> Find returns the user.")
    );

    // The example was not. It is copied through with the tab that said it was
    // an example, and the engine never saw it.
    assert_eq!(
        hover_value_in(&mut server, GO_URI, 2, 5),
        json!("\tuser := Find(id)\n\n> user := Find(id)")
    );
    assert_eq!(engine.calls(), 1);
}
