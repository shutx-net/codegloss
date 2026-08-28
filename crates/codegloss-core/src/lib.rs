//! Domain types shared by every CodeGloss front end, plus the translation
//! cache every front end answers from.
//!
//! This crate deliberately depends on neither an async runtime, a parser, nor
//! any LSP type: it must keep building for `wasm32-unknown-unknown` so that a
//! future browser extension can reuse the same pre/post-processing.

#![forbid(unsafe_code)]

mod cache;
mod model;

pub use cache::{DEFAULT_CAPACITY, GlossCache};
pub use model::{CommentBlock, CommentStyle, Gloss, GlossKey, Segment};
