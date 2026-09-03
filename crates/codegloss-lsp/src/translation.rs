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
//!   -> enqueue(uri, sources)              (returns immediately)
//!   -> worker collects jobs for 150 ms    (a burst of keystrokes is one batch)
//!   -> drops cached and duplicate blocks
//!   -> GlossPlan::new                     (pre-processing: mask)
//!   -> spawn_blocking(translate(batch))   (off the async executor)
//!   -> GlossPlan::restore                 (post-processing: unmask, rebuild)
//!   -> cache.insert(..)
//!   -> workspace/inlayHint/refresh + workspace/codeLens/refresh
//! ```
//!
//! IMPORTANT (AGENTS.md): the two steps around `translate` are what keep
//! identifiers, inline code, URLs and doc tags out of the engine's reach, and
//! they live in `codegloss-core` rather than in a [`Translator`]. This worker is
//! the only place they are applied, so an engine swap cannot lose them: what
//! [`Translator::translate`] receives is always a masked segment.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codegloss_core::{CommentRules, GlossCache, GlossKey, GlossPlan, Segment};
use codegloss_translator::Translator;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, timeout, timeout_at};
use tower_lsp_server::ls_types::Uri;
use tower_lsp_server::{Client, jsonrpc};

use crate::documents::CommentSource;

/// The only language pair v0.1 handles.
///
/// Both halves go into the cache key, so widening this later cannot serve
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

/// The end of the engine channel a reader holds.
///
/// The engine is not owned by the pipeline any more, because it can be
/// replaced while the server runs: a server that started without a model pack
/// downloads one in the background and swaps candle in when it arrives
/// (`model_pack::spawn_download`).
///
/// A `watch` rather than a lock, because the worker has to *notice* the swap
/// and not merely see a new value the next time it happens to look: every
/// gloss the old engine produced is now unreachable (the model version is part
/// of the cache key), so the client has to be told to ask again. Readers get
/// the engine of the moment with `borrow()`, which is what the request path
/// needs.
pub type EngineWatch = watch::Receiver<Arc<dyn Translator>>;

/// The other end: whoever may replace the engine holds this one.
///
/// Dropping it is normal and means the engine will never change - which is the
/// case for every build without a downloader, and for the tests.
pub type EngineSwitch = watch::Sender<Arc<dyn Translator>>;

/// A channel carrying the engine, starting out on `initial`.
pub fn engine_channel(initial: Arc<dyn Translator>) -> (EngineSwitch, EngineWatch) {
    watch::channel(initial)
}

/// One document's worth of comments to translate.
struct Job {
    uri: Uri,
    /// Every comment of the document, not only the ones that prompted the job,
    /// each as it is written in the file. Jobs for the same document supersede
    /// one another inside the debounce window, which is only correct because
    /// each one is complete.
    sources: Vec<CommentSource>,
}

/// The handle the LSP handlers hold: a cache to read and a queue to write.
pub struct TranslationService {
    engine: EngineWatch,
    cache: Arc<GlossCache>,
    jobs: mpsc::UnboundedSender<Job>,
    batches: watch::Receiver<u64>,
    initialized: Arc<AtomicBool>,
}

impl fmt::Debug for TranslationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranslationService")
            .field("model_version", &self.engine.borrow().model_version())
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
    pub fn spawn(client: Client, engine: EngineWatch, cache: Arc<GlossCache>) -> Self {
        let initialized = Arc::new(AtomicBool::new(false));
        let (jobs, queue) = mpsc::unbounded_channel();
        let (completed, batches) = watch::channel(0);

        let worker = Worker {
            client,
            engine: engine.clone(),
            cache: Arc::clone(&cache),
            initialized: Arc::clone(&initialized),
            completed,
            last_refresh: None,
        };
        tokio::spawn(worker.run(queue));

        Self {
            engine,
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

    /// The finished gloss of the comment written as `source`, if there is one.
    ///
    /// `source` is the comment exactly as it appears in the file
    /// ([`CommentBlock::raw`](codegloss_core::CommentBlock::raw)), which is what
    /// the gloss was built from: the same prose written as `//` and as `/** */`
    /// glosses differently, because the structure of the block is part of the
    /// answer.
    ///
    /// This is the only thing a request handler may call to obtain a gloss. A
    /// miss is a miss: it never waits for one to be produced.
    pub fn lookup(&self, source: &str, rules: CommentRules) -> Option<Arc<str>> {
        self.cache.get(&self.key(source, rules))
    }

    /// Queues a document for translation and returns immediately.
    ///
    /// `sources` should be every comment of the document. Sending the whole
    /// document is what makes it safe for a later job to supersede an earlier
    /// one; sending only the comment under the cursor would drop the rest.
    pub fn enqueue(&self, uri: Uri, sources: Vec<CommentSource>) {
        if sources.is_empty() {
            return;
        }

        // Cached comments are filtered by the worker rather than here: this
        // runs inside a request handler, and the worker has to re-check anyway
        // because entries can land between the two points.
        if self.jobs.send(Job { uri, sources }).is_err() {
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

    /// Cache key of the comment written as `source`, under the engine currently
    /// loaded.
    ///
    /// Reading the engine on every call rather than remembering its version is
    /// the point: after a swap this has to start missing, so that the comments
    /// glossed by the engine before it are translated again.
    pub fn key(&self, source: &str, rules: CommentRules) -> GlossKey {
        key(self.engine.borrow().model_version(), source, rules)
    }
}

/// The task that runs the engine.
struct Worker {
    client: Client,
    engine: EngineWatch,
    cache: Arc<GlossCache>,
    initialized: Arc<AtomicBool>,
    completed: watch::Sender<u64>,
    last_refresh: Option<Instant>,
}

impl Worker {
    async fn run(mut self, mut queue: mpsc::UnboundedReceiver<Job>) {
        // Cleared once the switch is gone, which is how a server that can
        // never be given another engine says so.
        //
        // IMPORTANT: the branch has to be taken out of the running, not merely
        // ignored. `changed()` on a closed watch is ready immediately and stays
        // ready, so leaving it in the select turns this wait into a spin - it
        // went round 286,000 times in 100 ms when measured. It does not stall
        // anything (tokio yields the task when its budget runs out, so every
        // test still passes) and it does not corrupt anything. It just burns a
        // core for as long as the editor is open, which is why the guard is
        // here and not a test.
        let mut swappable = true;

        loop {
            let job = tokio::select! {
                job = queue.recv() => job,
                changed = self.engine.changed(), if swappable => {
                    if changed.is_err() {
                        swappable = false;
                    } else {
                        self.engine_replaced().await;
                    }
                    continue;
                }
            };

            let Some(job) = job else { break };
            let mut pending = HashMap::new();
            pending.insert(job.uri, job.sources);

            let still_open = collect_until_quiet(&mut queue, &mut pending).await;
            self.run_batch(pending).await;

            if !still_open {
                break;
            }
        }
        tracing::debug!("translation worker stopped");
    }

    /// Reacts to a new engine: nothing to re-translate here, only something to
    /// say.
    ///
    /// The glosses the previous engine produced are still in the cache and are
    /// still correct for it, but they are keyed under its model version and so
    /// can no longer be found. Asking the client to refetch is what turns that
    /// into work: it comes back for the lenses it is showing, they miss, and
    /// the handler queues them.
    async fn engine_replaced(&mut self) {
        let model_version = self.engine.borrow_and_update().model_version().to_owned();
        tracing::info!(
            model_version,
            "the engine was replaced; asking for a refetch"
        );
        self.refresh().await;
    }

    /// Translates whatever of `pending` is not cached yet, then asks the client
    /// to refetch.
    async fn run_batch(&mut self, pending: HashMap<Uri, Vec<CommentSource>>) {
        let documents = pending.len();
        // Taken once and used for the whole batch. Reading it again after the
        // engine ran would store what one engine produced under another one's
        // key, and nothing downstream could tell.
        //
        // `borrow`, not `borrow_and_update`: a swap that lands while a batch is
        // being collected would otherwise be marked as seen here and never
        // reach the branch that asks the client to refetch. Leaving it unseen
        // costs at most one extra refresh, which is rate-limited anyway.
        let translator = self.engine.borrow().clone();
        let sources = uncached_sources(&self.cache, translator.model_version(), pending);

        let stored = if sources.is_empty() {
            0
        } else {
            self.run_engine(&translator, sources).await
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

    /// Pre-processes, runs the engine off the async executor, post-processes and
    /// caches what comes back. Returns how many glosses were stored.
    async fn run_engine(
        &self,
        translator: &Arc<dyn Translator>,
        sources: Vec<CommentSource>,
    ) -> usize {
        // Pre-processing (`codegloss-core`): each comment is taken apart into
        // the units a translator should see, with identifiers, inline code,
        // URLs and doc tags replaced by placeholders.
        let plans: Vec<GlossPlan> = sources
            .iter()
            .map(|source| GlossPlan::new(&source.raw, source.rules))
            .collect();
        let Batch { segments, slots } = Batch::of(&plans);
        tracing::debug!(
            blocks = sources.len(),
            segments = segments.len(),
            "running the engine"
        );

        // A block that is pure decoration has nothing to translate. It is still
        // stored, so that the next request finds an answer instead of queueing
        // it again for ever.
        if segments.is_empty() {
            return self.store(translator, &sources, &plans, &slots, &[]);
        }

        let engine = Arc::clone(translator);
        // IMPORTANT: inference is CPU-bound and blocking. Running it on the
        // executor would stall every other LSP handler on the same thread.
        let finished = tokio::task::spawn_blocking(move || {
            let translations = engine.translate(&segments);
            (segments, translations)
        })
        .await;

        match finished {
            Ok((segments, Ok(translations))) if translations.len() == segments.len() => {
                self.store(translator, &sources, &plans, &slots, &translations)
            }
            Ok((segments, Ok(translations))) => {
                // The trait says one output per input. An engine that breaks
                // that would otherwise pair every following comment with the
                // wrong translation, and the cache would keep serving the mix-up.
                tracing::error!(
                    inputs = segments.len(),
                    outputs = translations.len(),
                    "the engine returned the wrong number of translations; dropping the batch"
                );
                0
            }
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

    /// Post-processing (`codegloss-core`): puts the protected spans back into
    /// each unit and rebuilds the structure of the block around them, then
    /// caches the gloss under the comment it was made from.
    fn store(
        &self,
        translator: &Arc<dyn Translator>,
        sources: &[CommentSource],
        plans: &[GlossPlan],
        slots: &[Vec<usize>],
        translations: &[String],
    ) -> usize {
        let model_version = translator.model_version();
        for ((source, plan), slots) in sources.iter().zip(plans).zip(slots) {
            let of_this_block: Vec<String> = slots
                .iter()
                .map(|slot| translations[*slot].clone())
                .collect();
            self.cache.insert(
                key(model_version, &source.raw, source.rules),
                Arc::from(plan.restore(&of_this_block)),
            );
        }
        sources.len()
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
    pending: &mut HashMap<Uri, Vec<CommentSource>>,
) -> bool {
    let deadline = Instant::now() + DEBOUNCE;
    loop {
        match timeout_at(deadline, queue.recv()).await {
            Ok(Some(job)) => {
                pending.insert(job.uri, job.sources);
            }
            Ok(None) => return false,
            Err(_elapsed) => return true,
        }
    }
}

/// Drops the comments that are already glossed and the ones that repeat, so
/// that each distinct comment is prepared exactly once.
///
/// Two files quoting the same sentence, or one file whose other comments
/// survived an edit, must not pay for it twice.
fn uncached_sources(
    cache: &GlossCache,
    model_version: &str,
    pending: HashMap<Uri, Vec<CommentSource>>,
) -> Vec<CommentSource> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();

    for source in pending.into_values().flatten() {
        let key = key(model_version, &source.raw, source.rules);
        if cache.contains(&key) || !seen.insert(key) {
            continue;
        }
        sources.push(source);
    }
    sources
}

/// The engine input of one batch: every unit of every comment, deduplicated,
/// plus where each comment's units ended up.
///
/// The deduplication is a second one, below the one `uncached_sources` does:
/// two comments that differ only in their indentation, or a `@return` line
/// repeated across a file, share a single segment and a single inference.
struct Batch {
    segments: Vec<Segment>,
    /// One entry per comment, holding the index into `segments` of each of its
    /// units, in order.
    slots: Vec<Vec<usize>>,
}

impl Batch {
    fn of(plans: &[GlossPlan]) -> Self {
        let mut segments: Vec<Segment> = Vec::new();
        let mut slots = Vec::with_capacity(plans.len());
        let mut seen: HashMap<String, usize> = HashMap::new();

        for plan in plans {
            let mut of_this_plan = Vec::new();
            for segment in plan.segments() {
                let slot = match seen.get(segment.text()).copied() {
                    Some(slot) => slot,
                    None => {
                        seen.insert(segment.text().to_owned(), segments.len());
                        segments.push(segment);
                        segments.len() - 1
                    }
                };
                of_this_plan.push(slot);
            }
            slots.push(of_this_plan);
        }

        Self { segments, slots }
    }
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

/// Cache key of the comment written as `source`, for the given engine.
///
/// IMPORTANT: what is hashed is the comment as the file has it, before any
/// masking. The masked form would be the wrong key twice over: `Returns \`A\`.`
/// and `Returns \`B\`. ` mask to the same string and would collide, and a
/// lookup would have to re-run the masking to build the key at all - inside a
/// request handler, and only to hand back a value it could not unmask without
/// the table that mask produced. Hashing the source keeps the read path a plain
/// map lookup, and what is stored is the finished, already unmasked gloss.
///
/// Free-standing so that the model version cannot be left out of a key by
/// accident: every path that builds one goes through here.
fn key(model_version: &str, source: &str, rules: CommentRules) -> GlossKey {
    GlossKey::new(
        rules,
        model_version,
        SOURCE_LANGUAGE,
        TARGET_LANGUAGE,
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_version_is_part_of_every_key() {
        assert_ne!(
            key("passthrough-1", "text", CommentRules::Fenced),
            key("fugumt-en-ja@1", "text", CommentRules::Fenced)
        );
        assert_eq!(
            key("passthrough-1", "text", CommentRules::Fenced),
            key("passthrough-1", "text", CommentRules::Fenced)
        );
    }

    fn uri(path: &str) -> Uri {
        path.parse().expect("valid file uri")
    }

    /// A queued comment out of a Rust document.
    fn source(raw: &str) -> CommentSource {
        CommentSource {
            raw: raw.to_owned(),
            rules: CommentRules::Fenced,
        }
    }

    #[test]
    fn cached_and_duplicate_comments_never_reach_the_engine() {
        let cache = GlossCache::default();
        cache.insert(
            key("m", "// already done", CommentRules::Fenced),
            Arc::from("済み"),
        );

        let mut pending = HashMap::new();
        pending.insert(
            uri("file:///a.rs"),
            vec![
                source("// already done"),
                source("// new"),
                source("// new"),
            ],
        );

        assert_eq!(
            uncached_sources(&cache, "m", pending),
            vec![source("// new")]
        );
    }

    /// The same sentence in two files is one unit of work.
    #[test]
    fn a_comment_shared_by_two_documents_is_translated_once() {
        let cache = GlossCache::default();
        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec![source("// shared")]);
        pending.insert(uri("file:///b.rs"), vec![source("// shared")]);

        assert_eq!(
            uncached_sources(&cache, "m", pending),
            vec![source("// shared")]
        );
    }

    /// A batch whose comments are all glossed leaves the engine alone entirely.
    #[test]
    fn an_entirely_cached_batch_is_empty() {
        let cache = GlossCache::default();
        cache.insert(key("m", "// done", CommentRules::Fenced), Arc::from("済み"));

        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec![source("// done")]);

        assert!(uncached_sources(&cache, "m", pending).is_empty());
    }

    /// Entries cached under another engine are not reused, so the same comment
    /// has to be translated again after a model swap.
    #[test]
    fn a_model_swap_makes_cached_comments_uncached_again() {
        let cache = GlossCache::default();
        cache.insert(
            key("passthrough-1", "// text", CommentRules::Fenced),
            Arc::from("text"),
        );

        let mut pending = HashMap::new();
        pending.insert(uri("file:///a.rs"), vec![source("// text")]);

        assert!(uncached_sources(&cache, "fugumt-en-ja@1", pending).len() == 1);
    }

    /// What reaches the engine is the masked prose of each unit, not the
    /// comment: the Javadoc stars, the `@return` and the identifier after it
    /// are all gone by then.
    #[test]
    fn a_batch_hands_the_engine_masked_units() {
        let plans = [GlossPlan::new(
            "/**\n * Returns `UserDetails`.\n *\n * @throws AuthError if it failed\n */",
            CommentRules::Fenced,
        )];
        let batch = Batch::of(&plans);

        assert_eq!(
            batch.segments.iter().map(Segment::text).collect::<Vec<_>>(),
            ["Returns X0Q.", "if it failed"]
        );
        assert_eq!(batch.slots, vec![vec![0, 1]]);
    }

    /// Two comments that share a unit share the inference that produces it.
    #[test]
    fn a_unit_two_comments_have_in_common_is_translated_once() {
        let plans = [
            GlossPlan::new("/// @return the user", CommentRules::Fenced),
            GlossPlan::new("    /// @return the user", CommentRules::Fenced),
        ];
        let batch = Batch::of(&plans);

        assert_eq!(batch.segments.len(), 1);
        assert_eq!(batch.slots, vec![vec![0], vec![0]]);
    }
}
