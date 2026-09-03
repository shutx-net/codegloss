//! Comment extraction for CodeGloss.
//!
//! Reads a source file with Tree-sitter and returns the comments in it as
//! [`codegloss_core::CommentBlock`] values, positions expressed as byte offsets
//! and zero-based line numbers. No LSP type reaches this crate: mapping those
//! offsets onto editor coordinates is the language server's job.

#![forbid(unsafe_code)]

pub mod corpus;
mod extract;
mod languages;

pub use extract::{ExtractError, extract_comment_blocks};
pub use languages::SupportedLanguage;
