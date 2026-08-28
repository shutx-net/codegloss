//! The background translation pipeline.
//!
//! IMPORTANT (AGENTS.md): an LSP request handler must never run the engine.
//! Handlers touch two methods here, and neither of them can block:
//! [`TranslationService::lookup`] reads the cache, and
//! [`TranslationService::enqueue`] pushes onto an unbounded channel. Everything
//! slow happens on the worker task, which asks the client to refetch its hints
//! once results are in.
//!
//! The engine in the tree today returns its input unchanged and takes
//! microseconds, so a synchronous handler would look perfectly healthy. That is
//! precisely why the pipeline is built now: with candle in place a synchronous
//! handler freezes the editor for as long as inference takes, and by then the
//! shortcut is load-bearing.
//!
//! Shape of one round trip:
//!
//! ```text
//! didOpen / didChange / hover miss
//!   -> enqueue(uri, texts)                (returns immediately)
//!   -> worker collects jobs for 150 ms    (a burst of keystrokes is one batch)
//!   -> drops cached and duplicate texts
//!   -> spawn_blocking(translate(batch))   (off the async executor)
//!   -> cache.insert(..)
//!   -> workspace/inlayHint/refresh + workspace/codeLens/refresh
//! ```

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codegloss_core::{GlossCache, GlossKey, Segment};
use codegloss_translator::Translator;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, timeout, timeout_at};
use tower_lsp_server::ls_types::Uri;
use tower_lsp_server::{Client, jsonrpc};

/// The only language pair v0.1 handles.
///
/// Both halves go into the cache key, so widening this later (P9) cannot serve
/// an English-to-Japanese translation for another target language.
pub const SOURCE_LANGUAGE: &str = "en";
/// Target language. See [`SOURCE_LANGUAGE`].
pub const TARGET_LANGUAGE: &str = "ja";

/// How long jobs are collected before a batch is run.
///
/// A typing burst produces one `didChange` per keystroke, and each of them
/// re-extracts every comment in the file. Without this window the engine would
/// be handed a batch per keystroke; with it, the user stops typing and one
/// batch runs.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Smallest gap between two rounds of refresh requests.
///
/// `workspace/*/refresh` is a global ask: the editor refetches hints for every
/// visible buffer. Sending one per finished translation would make a large file
/// unusable, so a batch sends at most one round and rounds stay this far apart.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(300);

/// How long to wait for the client to answer a refresh request.
///
/// The worker is a single long-lived task: a client that accepts the request
/// and never answers would otherwise stop every future translation, not just
/// this one.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

/// One document's worth of text to translate.
struct Job {
    uri: Uri,
    /// Every comment of the document, not only the ones that prompted the job.
    /// Jobs for the same document supersede one another inside the debounce
    /// window, which is only correct because each one is complete.
    texts: Vec<String>,
}

/// The handle the LSP handlers hold: a cache to read and a queue to write.
pub struct TranslationService {
    translator: Arc<dyn Translator>,
    cache: Arc<GlossCache>,
    jobs: mpsc::UnboundedSender<Job>,
    batches: watch::Receiver<u64>,
    initialized: Arc<AtomicBool>,
}

impl fmt::Debug for TranslationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranslationService")
            .field("model_version", &self.translator.model_version())
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl TranslationService {
    /// Starts the worker task and returns the handle to talk to it.
    ///
    /// Exactly one worker, on purpose. Translation is serialised so that a real
    /// engine holds one model in memory and runs one inference at a time;
    /// `spawn_blocking`'s pool would otherwise happily start hundreds.
    ///
    /// Must be called from inside a tokio runtime.
    pub fn spawn(client: Client, translator: Arc<dyn Translator>) -> Self {
        let cache = Arc::new(GlossCache::default());
        let initialized = Arc::new(AtomicBool::new(false));
        let (jobs, queue) = mpsc::unbounded_channel();
        let (completed, batches) = watch::channel(0);

        let worker = Worker {
            client,
            translator: Arc::clone(&translator),
            cache: Arc::clone(&cache),
            initialized: Arc::clone(&initialized),
            completed,
            last_refresh: None,
        };
        tokio::spawn(worker.run(queue));

        Self {
            translator,
            cache,
            jobs,
            batches,
            initialized,
        }
    }

    /// Records that the client has sent `initialized`.
    ///
    /// Until it has, the worker keeps its results to itself: a refresh request
    /// sent too early is refused by the client with -32002, and the client is
    /// about to ask for everything anyway.
    pub fn mark_initialized(&self) {
        self.initialized.store(true, Ordering::Release);
    }

    /// The finished translation of `text`, if there is one.
    ///
    /// This is the only thing a request handler may call to obtain a
    /// translation. A miss is a miss: it never waits for one to be produced.
    pub fn lookup(&self, text: &str) -> Option<Arc<str>> {
        self.cache.get(&self.key(text))
    }

    /// Queues a document for translation and returns immediately.
    ///
    /// `texts` should be every comment of the document. Sending the whole
    /// document is what makes it safe for a later job to supersede an earlier
    /// one; sending only the comment under the cursor would drop the rest.
    pub fn enqueue(&self, uri: Uri, texts: Vec<String>) {
        if texts.is_empty() {
            return;
        }

        // Cached texts are filtered by the worker rather than here: this runs
        // inside a request handler, and the worker has to re-check anyway
        // because entries can land between the two points.
        if self.jobs.send(Job { uri, texts }).is_err() {
            tracing::warn!("the translation worker is gone; nothing will be translated");
        }
    }

    /// Number of batches the worker has finished, for anyone that needs to wait
    /// for the pipeline to settle. The counter is bumped once per batch, after
    /// its results are cached and its refresh requests have been answered, so
    /// observing a change means the new translations are readable.
    pub fn batches_completed(&self) -> watch::Receiver<u64> {
        self.batches.clone()
    }

    /// Cache key of `text` under the engine currently loaded.
    pub fn key(&self, text: &str) -> GlossKey {
        key(self.translator.model_version(), text)
    }
}

/// The task that owns the engine.
struct Worker {
    client: Client,
    translator: Arc<dyn Translator>,
    cache: Arc<GlossCache>,
    initialized: Arc<AtomicBool>,
    completed: watch::Sender<u64>,
    last_refresh: Option<Instant>,
}

impl Worker {
    async fn run(mut self, mut queue: mpsc::UnboundedReceiver<Job>) {
        while let Some(job) = queue.recv().await {
            let mut pending = HashMap::new();
            pending.insert(job.uri, job.texts);

            let still_open = collect_until_quiet(&mut queue, &mut pending).await;
            self.run_batch(pending).await;

            if !still_open {
                break;
            }
        }
        tracing::debug!("translation worker stopped");
    }

    /// Translates whatever of `pending` is not cached yet, then asks the client
    /// to refetch.
    async fn run_batch(&mut self, pending: HashMap<Uri, Vec<String>>) {
        let documents = pending.len();
        let segments = uncached_segments(&self.cache, self.translator.model_version(), pending);

        let stored = if segments.is_empty() {
            0
        } else {
            self.run_engine(segments).await
        };

        tracing::debug!(documents, stored, "translation batch finished");
        if stored > 0 {
            self.refresh().await;
        }

        // Bumped for every batch, including one that had nothing left to do:
        // the counter says "the queue has been drained up to here", which is
        // what a caller waiting for the pipeline to settle needs to know.
        self.completed.send_modify(|count| *count += 1);
    }

    /// Runs the engine off the async executor and caches what comes back.
    /// Returns how many translations were stored.
    async fn run_engine(&self, segments: Vec<Segment>) -> usize {
        let count = segments.len();
        tracing::debug!(segments = count, "running the engine");

        let translator = Arc::clone(&self.translator);
        // IMPORTANT: inference is CPU-bound and blocking. Running it on the
        // executor would stall every other LSP handler on the same thread.
        let finished = tokio::task::spawn_blocking(move || {
            let translations = translator.translate(&segments);
            (segments, translations)
        })
        .await;

        match finished {
            Ok((segments, Ok(translations))) => self.store(segments, translations),
            Ok((_, Err(error))) => {
                // A failed batch is dropped rather than retried: the next
                // didChange or hover queues it again, and an engine that fails
                // on a given text will keep failing on it.
                tracing::warn!(%error, "the engine failed on a batch");
                0
            }
            Err(error) => {
                tracing::error!(%error, "the translation task did not finish");
                0
            }
        }
    }

    fn store(&self, segments: Vec<Segment>, translations: Vec<String>) -> usize {
        if translations.len() != segments.len() {
            // The trait says one output per input. An engine that breaks that
            // would otherwise pair every following comment with the wrong
            // translation, and the cache would keep serving the mix-up.
            tracing::error!(
                inputs = segments.len(),
                outputs = translations.len(),
                "the engine returned the wrong number of translations; dropping the batch"
            );
            return 0;
        }

        let model_version = self.translator.model_version();
        let stored = segments.len();
        for (segment, translation) in segments.into_iter().zip(translations) {
            self.cache
                .insert(key(model_version, segment.text()), Arc::from(translation));
        }
        stored
    }

    /// Asks the client to refetch the hints it is showing.
    ///
    /// Hover has no counterpart to this - the protocol has no
    /// `workspace/hover/refresh` - so a hover that missed the cache stays as it
    /// was until the user hovers again.
    async fn refresh(&mut self) {
        if !self.initialized.load(Ordering::Acquire) {
            tracing::debug!("client has not sent initialized yet; not refreshing");
            return;
        }

        if let Some(last) = self.last_refresh {
            let elapsed = last.elapsed();
            if elapsed < MIN_REFRESH_INTERVAL {
                tokio::time::sleep(MIN_REFRESH_INTERVAL - elapsed).await;
            }
        }
        self.last_refresh = Some(Instant::now());

        send_refresh(
            "workspace/inlayHint/refresh",
            self.client.inlay_hint_refresh(),
        )
        .await;
        send_refresh(
            "workspace/codeLens/refresh",
            self.client.code_lens_refresh(),
        )
        .await;
    }
}

/// Keeps taking jobs until the queue stays quiet for [`DEBOUNCE`].
///
/// A later job for a document replaces the earlier one: it was built from a
/// newer buffer, and it carries every comment of it.
///
/// Returns `false` once the queue is closed, which happens when the server is
/// shutting down.
async fn collect_until_quiet(
    queue: &mut mpsc::UnboundedReceiver<Job>,
    pending: &mut HashMap<Uri, Vec<String>>,
) -> bool {
    let deadline = Instant::now() + DEBOUNCE;
    loop {
        match timeout_at(deadline, queue.recv()).await {
            Ok(Some(job)) => {
                pending.insert(job.uri, job.texts);
            }
            Ok(None) => return false,
            Err(_elapsed) => return true,
        }
    }
}

/// Drops the texts that are already cached and the ones that repeat, so the
/// engine sees each distinct text exactly once.
///
/// Two files quoting the same sentence, or one file whose other comments
/// survived an edit, must not pay for it twice.
fn uncached_segments(
    cache: &GlossCache,
    model_version: &str,
    pending: HashMap<Uri, Vec<String>>,
) -> Vec<Segment> {
    let mut seen = HashSet::new();
    let mut segments = Vec::new();

    for text in pending.into_values().flatten() {
        let key = key(model_version, &text);
        if cache.contains(&key) || !seen.insert(key) {
            continue;
        }
        segments.push(Segment::new(text));
    }
    segments
}

async fn send_refresh(name: &str, request: impl Future<Output = jsonrpc::Result<()>>) {
    match timeout(REFRESH_TIMEOUT, request).await {
        Ok(Ok(())) => tracing::debug!(request = name, "the client accepted the refresh"),
        // Not every client supports these requests, and one that does not says
        // so per request. It is not an error worth bothering the user with.
        Ok(Err(error)) => tracing::debug!(request = name, %error, "the client refused the refresh"),
        Err(_elapsed) => tracing::warn!(request = name, "the client did not answer the refresh"),
    }
}

/// Cache key of `text` for the given engine.
///
/// Free-standing so that the model version cannot be left out of a key by
/// accident: every path that builds one goes through here.
fn key(model_version: &str, text: &str) -> GlossKey {
    GlossKey::new(model_version, SOURCE_LANGUAGE, TARGET_LANGUAGE, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_version_is_part_of_every_key() {
        assert_ne!(key("passthrough-1", "text"), key("fugumt-en-ja@1", "text"));
        assert_eq!(key("passthrough-1", "text"), key("passthrough-1", "text"));
    }

    fn uri(path: &str) -> Uri {
        path.parse().expect("valid file uri")
    }

    #[test]
    fn cached_and_duplicate_texts_never_reach_the_engine() {
        let cache = GlossCache::default();
        cache.insert(key("m", "already done"), Arc::from("済み"));

        let mut pending = HashMap::new();
        pending.insert(
            uri("file:///a.rs"),
            vec![
                "already done".to_owned(),
                "new".to_owned(),
                "new".to_owned(),
            ],
        );

        let segments = uncached_segments(&cache, "m", pending);
        assert_eq!(segments, vec![Segment::new("new")]);
    }

    /// The same sentence in two files is one unit of work.
    #[test]
    fn a_text_shared_by_two_documents_is_translated_once() {
        let cache = GlossCache::default();
        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec!["shared".to_owned()]);
        pending.insert(uri("file:///b.rs"), vec!["shared".to_owned()]);

        assert_eq!(
            uncached_segments(&cache, "m", pending),
            vec![Segment::new("shared")]
        );
    }

    /// A batch whose texts are all cached leaves the engine alone entirely.
    #[test]
    fn an_entirely_cached_batch_is_empty() {
        let cache = GlossCache::default();
        cache.insert(key("m", "done"), Arc::from("済み"));

        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec!["done".to_owned()]);

        assert!(uncached_segments(&cache, "m", pending).is_empty());
    }

    /// Entries cached under another engine are not reused, so the same text
    /// has to be translated again after a model swap.
    #[test]
    fn a_model_swap_makes_cached_texts_uncached_again() {
        let cache = GlossCache::default();
        cache.insert(key("passthrough-1", "text"), Arc::from("text"));

        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec!["text".to_owned()]);

        assert!(uncached_segments(&cache, "fugumt-en-ja@1", pending).len() == 1);
    }
}
