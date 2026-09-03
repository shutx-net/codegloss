//! Registry of the languages CodeGloss can extract comments from.
//!
//! Adding a language means adding a variant, its grammar, its query and its
//! comment syntax here; nothing in [`crate::extract`] is language-specific.

use codegloss_core::CommentRules;
use tree_sitter::Language;

/// A language CodeGloss knows how to read comments out of.
///
/// More variants (Java, JavaScript, TypeScript, Tsx, Python) follow in a later
/// phase; the grammar crates are already picked, only the wiring is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SupportedLanguage {
    Rust,
    Go,
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

/// Go writes the same markers as C and reads them differently: a doc comment
/// marks an example by indenting it, and a Markdown fence never appears - not
/// once in the whole of `GOROOT` (`docs/model-runtime-notes.md` §16).
const GO_SYNTAX: CommentSyntax = CommentSyntax {
    rules: CommentRules::Indented,
    ..C_LIKE_SYNTAX
};

impl SupportedLanguage {
    /// Maps the `languageId` a client sends with `textDocument/didOpen` onto a
    /// grammar. Zed reports Rust as `rust` and Go as `go` (its `LanguageName`
    /// lowercased).
    ///
    /// Returns `None` for anything CodeGloss cannot parse yet, which the server
    /// treats as "this document has no comments" rather than as an error.
    pub fn from_lsp_language_id(language_id: &str) -> Option<Self> {
        match language_id {
            "rust" => Some(Self::Rust),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    /// Stable name of the language, for logs and cache keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
        }
    }

    /// The Tree-sitter grammar. The grammar crates expose a
    /// `tree_sitter_language::LanguageFn` that converts into a [`Language`].
    pub(crate) fn grammar(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    /// The query selecting every comment node, with a single `@comment` capture.
    pub(crate) fn comment_query(self) -> &'static str {
        match self {
            Self::Rust => include_str!("queries/rust.scm"),
            Self::Go => include_str!("queries/go.scm"),
        }
    }

    pub(crate) fn comment_syntax(self) -> CommentSyntax {
        match self {
            Self::Rust => C_LIKE_SYNTAX,
            Self::Go => GO_SYNTAX,
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
    fn every_grammar_and_query_loads() {
        for language in [SupportedLanguage::Rust, SupportedLanguage::Go] {
            let grammar = language.grammar();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&grammar)
                .expect("grammar ABI is supported by the linked tree-sitter");
            tree_sitter::Query::new(&grammar, language.comment_query())
                .expect("the bundled query compiles against its own grammar");
        }
    }

    /// Zed sends `LanguageName::lsp_id()`, which is the language's name
    /// lowercased - `Go` becomes `go`. Nothing here looks at a file extension:
    /// the language is whatever the editor says the buffer is.
    #[test]
    fn go_is_recognised_by_its_lsp_language_id() {
        assert_eq!(
            SupportedLanguage::from_lsp_language_id("go"),
            Some(SupportedLanguage::Go)
        );
        assert_eq!(SupportedLanguage::from_lsp_language_id("Go"), None);
        assert_eq!(SupportedLanguage::from_lsp_language_id("golang"), None);
    }

    /// The registry is the one place that knows which language reads its
    /// comments which way. Wiring a grammar in without saying this is how a
    /// language gets its examples handed to the engine as prose (Issue #53,
    /// and Issue #30 for Go).
    #[test]
    fn the_registry_says_which_rules_a_language_has() {
        assert_eq!(
            SupportedLanguage::Rust.comment_syntax().rules,
            CommentRules::Fenced
        );
        assert_eq!(
            SupportedLanguage::Go.comment_syntax().rules,
            CommentRules::Indented
        );
    }
}
