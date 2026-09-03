//! Domain types shared by every CodeGloss front end, plus the translation
//! cache every front end answers from.
//!
//! This crate deliberately depends on neither an async runtime, a parser, nor
//! any LSP type: it must keep building for `wasm32-unknown-unknown` so that a
//! future browser extension can reuse the same pre/post-processing.

#![forbid(unsafe_code)]

mod cache;
mod docblock;
mod model;
mod preserve;
mod rules;
mod sentence;
mod store;

pub use cache::{DEFAULT_CAPACITY, GlossCache};
pub use docblock::{
    CommentShape, GlossPlan, opens_or_closes_a_fence, opens_or_closes_a_rendered_fence,
};
pub use model::{CommentBlock, CommentStyle, Gloss, GlossKey, PIPELINE_VERSION, Segment};
pub use preserve::{Masked, Preserved, SpanKind, mask, placeholder};
pub use rules::CommentRules;
pub use sentence::{engine_form, join_sentences, split_sentences};
pub use store::GlossStore;
