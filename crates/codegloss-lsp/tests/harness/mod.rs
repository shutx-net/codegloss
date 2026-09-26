//! Drives the message loop in process.
//!
//! No process is spawned and no stdio is involved: a message goes into
//! [`Dispatcher::handle`] and the response comes back as JSON, which is what
//! the assertions are written against. The connection's other half is a
//! channel this harness reads, so a test can also see what the *server* sent -
//! the `workspace/*/refresh` requests the pipeline raises when a batch lands.
//!
//! IMPORTANT: those refreshes have to be answered. They are requests, not
//! notifications, and the worker waits up to five seconds for each; a harness
//! that left them hanging would make everything after the first batch look
//! broken and slow. [`Harness`] starts a thread that answers them as it
//! records them.

#![allow(dead_code)] // Each test file uses a part of this.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use codegloss_core::GlossCache;
use codegloss_lsp::Backend;
use codegloss_lsp::client::Client;
use codegloss_lsp::server::{Action, Dispatcher};
use codegloss_lsp::translation::{BatchCounter, EngineSwitch, engine_channel};
use codegloss_translator::{PassthroughTranslator, Translator};
use crossbeam_channel::Receiver;
use lsp_server::{Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

/// Long enough that a real stall fails the test, short enough that a hang does
/// not sit in CI until the job times out.
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// The methods the server sent to the client, in order.
#[derive(Clone, Default)]
pub struct SeenRequests(Arc<Mutex<Vec<String>>>);

impl SeenRequests {
    #[must_use]
    pub fn methods(&self) -> Vec<String> {
        self.0.lock().expect("the recorder is not poisoned").clone()
    }

    #[must_use]
    pub fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|seen| *seen == method).count()
    }
}

/// A server, the client side of its connection, and an id counter.
pub struct Harness {
    dispatcher: Dispatcher,
    client: Arc<Client>,
    seen: SeenRequests,
    next_id: i32,
}

impl Harness {
    /// A server on the engine that ships today.
    #[must_use]
    pub fn new() -> Self {
        Self::with_engine(Arc::new(PassthroughTranslator))
    }

    /// A server on an engine of the test's choosing, which can never be
    /// replaced.
    #[must_use]
    pub fn with_engine(engine: Arc<dyn Translator>) -> Self {
        Self::build(|client| Backend::with_engine(client, engine))
    }

    /// A server on an engine the test may replace while it runs.
    ///
    /// The switch comes back rather than being dropped, which is the whole
    /// difference: a server that may still be given another engine keeps its
    /// worker watching for one.
    #[must_use]
    pub fn swappable(engine: Arc<dyn Translator>) -> (Self, EngineSwitch) {
        let (switch, watch) = engine_channel(engine);
        let harness = Self::build(move |client| {
            Backend::with_cache(client, watch, Arc::new(GlossCache::default()))
        });
        (harness, switch)
    }

    fn build(backend: impl FnOnce(Arc<Client>) -> Backend) -> Self {
        let (sender, sent) = crossbeam_channel::unbounded();
        let client = Arc::new(Client::new(sender));
        let seen = answer_the_server(&client, sent);
        let dispatcher = Dispatcher::new(backend(Arc::clone(&client)), Arc::clone(&client));

        Self {
            dispatcher,
            client,
            seen,
            next_id: 0,
        }
    }

    #[must_use]
    pub fn backend(&self) -> &Backend {
        self.dispatcher.backend()
    }

    #[must_use]
    pub fn batches(&self) -> BatchCounter {
        self.backend().glosses().batches_completed()
    }

    #[must_use]
    pub fn seen(&self) -> &SeenRequests {
        &self.seen
    }

    /// Sends a request and returns its response as JSON.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let request = Request::new(
            RequestId::from(self.next_id),
            method.to_owned(),
            params.clone(),
        );
        match self.dispatcher.handle(Message::Request(request)) {
            Action::Reply(response) => {
                serde_json::to_value(response).expect("a response serializes")
            }
            other => panic!("{method} produced {other:?} instead of a response"),
        }
    }

    /// Sends a notification, which by definition has no response.
    pub fn notify(&mut self, method: &str, params: Value) {
        let notification = Notification::new(method.to_owned(), params);
        match self.dispatcher.handle(Message::Notification(notification)) {
            Action::Ignore => {}
            other => panic!("{method} produced {other:?} instead of nothing"),
        }
    }

    /// `initialize` and `initialized`, which every other request needs first.
    pub fn initialize(&mut self) -> Value {
        let response = self.request("initialize", json!({ "capabilities": {} }));
        self.notify("initialized", json!({}));
        response
    }

    /// Opens a document. The language id is what picks the grammar; nothing
    /// looks at the file extension.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        );
    }

    pub fn did_change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
    }

    pub fn did_close(&mut self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    pub fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    }

    pub fn code_lens(&mut self, uri: &str) -> Value {
        self.request(
            "textDocument/codeLens",
            json!({ "textDocument": { "uri": uri } }),
        )
    }

    /// Waits for the pipeline to finish a batch beyond `seen`.
    ///
    /// Panics on a timeout rather than returning: every caller treats it as a
    /// failure, and the panic says which wait it was.
    pub fn settle(&self, seen: u64) -> u64 {
        self.batches()
            .wait_past(seen, SETTLE_TIMEOUT)
            .expect("the pipeline finished a batch")
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// A place in the worker's batch counter, for a test that waits for one batch
/// after another.
///
/// It remembers what it has already waited for, so `next()` means "one more
/// than last time" and a batch that finished while the test was asserting is
/// not missed.
pub struct Batches {
    counter: BatchCounter,
    seen: u64,
}

impl Batches {
    #[must_use]
    pub fn of(server: &Harness) -> Self {
        let counter = server.batches();
        let seen = counter.count();
        Self { counter, seen }
    }

    /// Waits for the worker to finish one more batch.
    pub fn next(&mut self) {
        self.seen = self
            .counter
            .wait_past(self.seen, SETTLE_TIMEOUT)
            .expect("the pipeline finished a batch");
    }

    /// Asserts that no batch runs in `patience`.
    ///
    /// Give it more than the worker's debounce window, or a job that *was*
    /// queued would not have produced a batch yet and this would pass on
    /// nothing.
    pub fn none_within(&self, patience: Duration) {
        assert!(
            self.counter.wait_past(self.seen, patience).is_none(),
            "a batch ran that should not have"
        );
    }
}

/// Waits for something the worker does off to the side, with no counter to
/// watch.
pub fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + SETTLE_TIMEOUT;
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Runs `call` on a thread of its own and fails if it has not answered within
/// `patience`.
///
/// This is how "a handler must never wait for the engine" is checked. The call
/// is synchronous, so a handler that ran the blocked engine would simply not
/// return, and without this the test would hang until CI killed the job
/// instead of failing. The thread is abandoned on a timeout, which is the
/// point: the test has already failed and the process is about to end.
pub fn answers_within<T, R>(
    patience: Duration,
    what: &str,
    subject: T,
    call: impl FnOnce(&mut T) -> R + Send + 'static,
) -> (T, R)
where
    T: Send + 'static,
    R: Send + 'static,
{
    let (done, answered) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        let mut subject = subject;
        let result = call(&mut subject);
        let _ = done.send((subject, result));
    });
    answered
        .recv_timeout(patience)
        .unwrap_or_else(|_| panic!("{what}"))
}

/// Records what the server sends and answers the requests among it.
///
/// The thread holds only a weak reference to the client, so that dropping the
/// harness closes the channel and ends it. Holding a strong one would keep the
/// sender alive and the thread would never see the end of the stream.
fn answer_the_server(client: &Arc<Client>, sent: Receiver<Message>) -> SeenRequests {
    let seen = SeenRequests::default();
    let recorder = seen.clone();
    let answering = Arc::downgrade(client);

    thread::spawn(move || {
        for message in sent {
            let method = match &message {
                Message::Request(request) => request.method.clone(),
                Message::Notification(notification) => notification.method.clone(),
                // Nothing here sends one; the server is the one being tested.
                Message::Response(_) => continue,
            };
            recorder
                .0
                .lock()
                .expect("the recorder is not poisoned")
                .push(method);

            if let Message::Request(request) = message {
                let Some(client) = answering.upgrade() else {
                    break;
                };
                client.settle(Response::new_ok(request.id, Value::Null));
            }
        }
    });

    seen
}
