//! The translation engine boundary.
//!
//! Everything CodeGloss knows about how text turns into other text sits behind
//! [`Translator`]. Swapping candle for ct2rs, ort, bergamot or a remote
//! endpoint has to be an exchange of one implementation for another and
//! nothing else, which is why this crate holds no state of its own.
//!
//! IMPORTANT (AGENTS.md): pre-processing, post-processing and caching do not
//! belong here. An implementation that hides them inside itself loses them the
//! day the engine is replaced; they live in `codegloss-core`, on both sides of
//! this trait.

#![forbid(unsafe_code)]

mod passthrough;

use codegloss_core::Segment;

pub use passthrough::{PASSTHROUGH_MODEL_VERSION, PassthroughTranslator};

/// An engine that turns source-language text into target-language text.
///
/// Text in, text out. Implementations are shared across threads behind an
/// `Arc<dyn Translator>`, hence `Send + Sync`.
pub trait Translator: Send + Sync {
    /// Translates a whole batch at once.
    ///
    /// The result has one entry per input segment, in the same order. Taking a
    /// batch rather than a single segment is what lets an NMT engine run its
    /// inputs through the model together later on, without every caller having
    /// to change.
    ///
    /// This is deliberately a **blocking** call: NMT inference is CPU-bound
    /// work, and an `async fn` here would only wrap something that still has to
    /// be handed to a blocking thread pool. The caller is responsible for
    /// keeping it off the async executor - `codegloss-lsp` runs it inside
    /// `tokio::task::spawn_blocking`.
    fn translate(&self, segments: &[Segment]) -> anyhow::Result<Vec<String>>;

    /// Identifies the engine together with the weights it is running.
    ///
    /// It goes into every cache key, so changing the model or the engine has to
    /// change this string; otherwise translations produced by the old one keep
    /// being served after the swap.
    fn model_version(&self) -> &str;
}
