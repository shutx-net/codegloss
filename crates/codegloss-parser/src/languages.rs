//! Registry of the languages CodeGloss can extract comments from.
//!
//! Adding a language means adding a variant, its grammar, its query and its
//! comment syntax here; nothing in [`crate::extract`] is language-specific.

use codegloss_core::CommentRules;
use tree_sitter::Language;

/// A language CodeGloss knows how to read comments out of.
///
/// More variants (Java, JavaScript, TypeScript, Tsx, Python, Go) follow in a
/// later phase; the grammar crates are already picked, only the wiring is
/// missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SupportedLanguage {
    Rust,
}

/// The comment markers of one language.
///
/// Doc-comment markers are deliberately absent: whether a comment is a doc
/// comment is read off the syntax tree (`inner` / `outer` marker fields), which
/// is sturdier than matching a prefix against the raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommentSyntax {
    /// Opener of a line comment, e.g. `//`.
    pub line: &'static str,
    /// Opener of a block comment, e.g. `/*`.
    pub block_start: &'static str,
    /// Closer of a block comment, e.g. `*/`.
    pub block_end: &'static str,
    /// Decoration that continuation lines of a block comment are conventionally
    /// indented with, e.g. the `*` of a Javadoc block.
    pub block_continuation: &'static str,
    /// What the shape of a comment means in this language.
    ///
    /// This registry is the one place that knows which language is which, so it
    /// is the one place that may answer this. `codegloss-core` owns the
    /// vocabulary and never learns the list of languages, which is what keeps
    /// adding a grammar a change to this file alone.
    pub rules: CommentRules,
}

const C_LIKE_SYNTAX: CommentSyntax = CommentSyntax {
    line: "//",
    block_start: "/*",
    block_end: "*/",
    block_continuation: "*",
    rules: CommentRules::Fenced,
};

impl SupportedLanguage {
    /// Maps the `languageId` a client sends with `textDocument/didOpen` onto a
    /// grammar. Zed reports Rust as `rust`.
    ///
    /// Returns `None` for anything CodeGloss cannot parse yet, which the server
    /// treats as "this document has no comments" rather than as an error.
    pub fn from_lsp_language_id(language_id: &str) -> Option<Self> {
        match language_id {
            "rust" => Some(Self::Rust),
            _ => None,
        }
    }

    /// Stable name of the language, for logs and cache keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
        }
    }

    /// The Tree-sitter grammar. The grammar crates expose a
    /// `tree_sitter_language::LanguageFn` that converts into a [`Language`].
    pub(crate) fn grammar(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    /// The query selecting every comment node, with a single `@comment` capture.
    pub(crate) fn comment_query(self) -> &'static str {
        match self {
            Self::Rust => include_str!("queries/rust.scm"),
        }
    }

    pub(crate) fn comment_syntax(self) -> CommentSyntax {
        match self {
            Self::Rust => C_LIKE_SYNTAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_is_recognised_by_its_lsp_language_id() {
        assert_eq!(
            SupportedLanguage::from_lsp_language_id("rust"),
            Some(SupportedLanguage::Rust)
        );
    }

    #[test]
    fn unknown_language_ids_are_rejected() {
        assert_eq!(SupportedLanguage::from_lsp_language_id("plaintext"), None);
        assert_eq!(SupportedLanguage::from_lsp_language_id("Rust"), None);
        assert_eq!(SupportedLanguage::from_lsp_language_id(""), None);
    }

    /// A grammar whose ABI the linked Tree-sitter cannot handle fails here
    /// rather than at the first `didOpen`.
    #[test]
    // Removing this once a second language lands is not optional: an
    // unfulfilled expectation is itself a warning, and CI denies warnings.
    #[expect(
        clippy::single_element_loop,
        reason = "the list grows with every language added to the registry"
    )]
    fn every_grammar_and_query_loads() {
        for language in [SupportedLanguage::Rust] {
            let grammar = language.grammar();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&grammar)
                .expect("grammar ABI is supported by the linked tree-sitter");
            tree_sitter::Query::new(&grammar, language.comment_query())
                .expect("the bundled query compiles against its own grammar");
        }
    }
}
