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
    /// Which line comments speak to the toolchain rather than to a reader.
    pub directives: DirectiveSyntax,
    /// What the shape of a comment means in this language.
    ///
    /// This registry is the one place that knows which language is which, so it
    /// is the one place that may answer this. `codegloss-core` owns the
    /// vocabulary and never learns the list of languages, which is what keeps
    /// adding a grammar a change to this file alone.
    pub rules: CommentRules,
}

/// The shape of a comment line that instructs a tool instead of addressing a
/// reader.
///
/// A property of the language, so it belongs to the registry - and unlike a
/// fence, nothing outside the parser ever needs to ask: the line is dropped
/// before a block is built, so `codegloss-core` never sees one and no second
/// copy of this judgement can grow anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectiveSyntax {
    /// No comment of this language instructs a tool. Rust's attributes are
    /// attributes, and `//go:build linux` in a Rust file is a sentence about
    /// Go.
    None,
    /// Go's `//name:value`. `go/ast`'s `isDirective`, which is what `go/ast`
    /// itself uses to keep these out of a doc comment's text.
    Go,
}

impl DirectiveSyntax {
    /// `body` is one comment line with its `//` taken off and **nothing else
    /// done to it** - the space after the marker, if there was one, still
    /// there. That space is what tells `// export the users table` from
    /// `//export foo`, and it is load-bearing: with it gone, both of them read
    /// as a directive and the sentence loses its gloss. It needs no test of its
    /// own here because it fails every branch below on its own - the prefixes
    /// do not match it and a space is not `[a-z0-9]` - which is also how
    /// `go/ast` gets the same answer with the check in its caller.
    pub(crate) fn matches(self, body: &str) -> bool {
        match self {
            Self::None => false,
            Self::Go => {
                if ["line ", "extern ", "export "]
                    .iter()
                    .any(|word| body.starts_with(word))
                {
                    return true;
                }
                let Some(colon) = body.find(':') else {
                    return false;
                };
                // Lowercase and digits up to the colon, and one byte past it:
                // `go/ast` reads that byte too, so `//go:` on its own and
                // `//TODO:fix` are both prose. `get` rather than a slice - the
                // byte after the colon can be the first of a multibyte
                // character, and that is a no as well.
                let Some(head) = body.get(..colon + 2) else {
                    return false;
                };
                colon > 0
                    && head.bytes().enumerate().all(|(index, byte)| {
                        index == colon || byte.is_ascii_lowercase() || byte.is_ascii_digit()
                    })
            }
        }
    }
}

const C_LIKE_SYNTAX: CommentSyntax = CommentSyntax {
    line: "//",
    block_start: "/*",
    block_end: "*/",
    block_continuation: "*",
    rules: CommentRules::Fenced,
    directives: DirectiveSyntax::None,
};

/// Go writes the same markers as C and reads them differently: a doc comment
/// marks an example by indenting it, and a Markdown fence never appears - not
/// once in the whole of `GOROOT` (`docs/model-runtime-notes.md` §16).
const GO_SYNTAX: CommentSyntax = CommentSyntax {
    rules: CommentRules::Indented,
    directives: DirectiveSyntax::Go,
    ..C_LIKE_SYNTAX
};

impl SupportedLanguage {
    /// Every language this build reads, as the `languageId` an editor sends.
    ///
    /// **CI reads this**, through `examples/languages.rs`: the same list exists
    /// a second time in `editors/zed/extension.toml` as Zed's language names,
    /// in a workspace neither build sees, and adding a language to one side
    /// alone fails silently in both directions - the server parses a language
    /// Zed never attaches it to, or Zed attaches it and
    /// [`Self::from_lsp_language_id`] answers `None` and the document is
    /// treated as having no comments. The `languages` step of `.github/
    /// workflows/ci.yml` compares the two lower-cased.
    ///
    /// A variant missing from here is not quiet: the check above then reports
    /// the language as one `extension.toml` has and this file does not.
    pub const ALL: [Self; 2] = [Self::Rust, Self::Go];

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

    /// What the shape of a comment means in this language.
    ///
    /// The registry is the one place that knows which language is which, so it
    /// is the one place that may answer this - `codegloss-core` owns the
    /// vocabulary and never learns the list of languages. Public because a
    /// corpus is extracted under a language and scored under rules
    /// (`corpus`, and `codegloss-translator`'s harnesses), and the alternative
    /// to asking here is a second copy of the mapping over there.
    pub fn rules(self) -> CommentRules {
        self.comment_syntax().rules
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

    /// `ALL` is what CI compares against `editors/zed/extension.toml`, so an
    /// entry that no `didOpen` could ever produce would make the check pass
    /// while the server stayed silent on that buffer.
    #[test]
    fn every_listed_language_is_one_a_client_can_ask_for() {
        for language in SupportedLanguage::ALL {
            assert_eq!(
                SupportedLanguage::from_lsp_language_id(language.as_str()),
                Some(language),
                "{language:?} is listed in ALL under an id no client can send"
            );
        }
        assert_eq!(
            SupportedLanguage::ALL.len(),
            SupportedLanguage::ALL
                .iter()
                .map(|language| language.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "ALL lists the same language twice"
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
        for language in SupportedLanguage::ALL {
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

    /// `go/ast`'s `isDirective`, line for line. The interesting rows are the
    /// last four: a space after the marker means prose, an uppercase word is
    /// prose, a colon with nothing after it is prose, and Rust has no
    /// directives at all - which is why scoping this to a language costs
    /// nothing to prove.
    #[test]
    fn a_directive_speaks_to_the_toolchain_and_a_comment_does_not() {
        for body in [
            "go:build linux",
            "go:generate go run mkasm.go",
            "line 42",
            "extern foo",
            "export bar",
            "cgo:noescape",
        ] {
            assert!(DirectiveSyntax::Go.matches(body), "in {body:?}");
            assert!(!DirectiveSyntax::None.matches(body), "in {body:?}");
        }

        for body in [
            " go:build linux",
            " note: this is prose",
            // The space is the whole of the difference here: without it these
            // three are `line`, `extern` and `export` directives.
            " line 42 of the file",
            " extern functions are declared elsewhere",
            " export the users table before upgrading",
            "TODO: fix this",
            "Go:build linux",
            "go:",
            ":build",
            "no colon here",
            "",
            "go:あ",
        ] {
            assert!(!DirectiveSyntax::Go.matches(body), "in {body:?}");
        }
    }

    /// The registry is the one place that knows which language reads its
    /// comments which way. Wiring a grammar in without saying this is how a
    /// language gets its examples handed to the engine as prose (Issue #53,
    /// and Issue #30 for Go).
    #[test]
    fn the_registry_says_which_rules_a_language_has() {
        // Through the public accessor: that is what a corpus is extracted and
        // scored with, and pinning only the private field would let the two
        // drift.
        assert_eq!(SupportedLanguage::Rust.rules(), CommentRules::Fenced);
        assert_eq!(SupportedLanguage::Go.rules(), CommentRules::Indented);
        assert_eq!(
            SupportedLanguage::Rust.comment_syntax().directives,
            DirectiveSyntax::None
        );
        assert_eq!(
            SupportedLanguage::Go.comment_syntax().directives,
            DirectiveSyntax::Go
        );
    }
}
