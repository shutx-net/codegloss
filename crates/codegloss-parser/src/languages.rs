//! Registry of the languages CodeGloss can extract comments from.
//!
//! Adding a language means adding a variant, its grammar, its query and its
//! comment syntax here, and nothing in [`crate::extract`] is language-specific.
//! **This file is not the whole of it, though.** `codegloss-core`'s `docblock`
//! has the comment markers written into it (`/**`, `///`, `*/`), so a language
//! that marks a comment any other way needs a change there too: Python's `#`
//! and its triple quotes would reach it as prose, and an indented example under
//! a heading that never parsed as one. Issue #61 is about moving those markers
//! out to where this file could carry them; until that is done, budget for
//! both.
//!
//! The list also exists a second time outside this workspace, in
//! `editors/zed/extension.toml`, and CI compares the two
//! ([`SupportedLanguage::ALL`]).

use codegloss_core::CommentRules;
use tree_sitter::Language;

/// A language CodeGloss knows how to read comments out of.
///
/// More variants (Java, Python) follow in a later phase; the grammar crates are
/// already picked, only the wiring is missing. Python needs Issue #61 first -
/// its `#` and its triple quotes are markers `codegloss-core` does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SupportedLanguage {
    Rust,
    Go,
    JavaScript,
    TypeScript,
    /// TypeScript with JSX. A language of its own in Zed and a grammar of its
    /// own in Tree-sitter, because `<T>x` is a cast in one and an element in
    /// the other.
    Tsx,
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
    /// JavaScript's and TypeScript's tool pragmas: `// @ts-ignore`,
    /// `// eslint-disable-next-line no-console`,
    /// `/// <reference types="node" />`.
    ///
    /// Go has one rule and one authority for this; JavaScript has neither. Every
    /// tool invented its own spelling, so this is a list rather than a rule, and
    /// the price of a list is that it goes stale. It is kept short on purpose:
    /// every entry is a form its own tool documents, and a form that reads as a
    /// sentence is left off however common it is. `// global config is loaded
    /// here` is ESLint's `global` directive and an English sentence at the same
    /// time, and the sentence is the one a reader of this tool wants.
    EcmaScript,
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
            // Not Go's rule, and the difference starts at the space. Go reads
            // the byte right after the marker, so `// export ...` is prose and
            // `//export` is a directive. Nothing in JavaScript makes that
            // distinction - TypeScript's own scanner allows any run of
            // whitespace after the marker, and ESLint trims the comment before
            // it looks - so here the space is taken off instead of read.
            //
            // The leading slashes go with it: a triple-slash directive reaches
            // this as `/ <reference ... />`, because the marker that came off
            // was `//`. `///` means nothing else in JavaScript.
            Self::EcmaScript => {
                let body = body.trim().trim_start_matches('/').trim_start();
                ECMASCRIPT_PRAGMAS
                    .iter()
                    .any(|pragma| body.starts_with(pragma))
                    // A triple-slash directive: `<reference path="..." />`,
                    // `<amd-module name="..." />`. Matched by its shape rather
                    // than by its tag, the way TypeScript's own
                    // `tripleSlashXMLCommentStartRegEx` matches it - the tags
                    // are TypeScript's to add to, and a comment that both opens
                    // with `<` and closes with `/>` is not a sentence.
                    || (body.starts_with('<') && body.ends_with("/>"))
            }
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

/// The tool pragmas [`DirectiveSyntax::EcmaScript`] drops, each with the tool
/// that documents it.
///
/// Prefixes, so that the rules and the reason a tool is given after the pragma
/// come off with it: `eslint-disable-next-line no-console -- the CLI prints
/// here` is one directive, and the part after `--` is addressed to a reviewer
/// of the rule and not to a reader of the code.
const ECMASCRIPT_PRAGMAS: [&str; 7] = [
    // TypeScript. The four are the whole set the compiler knows: `@ts-check`,
    // `@ts-nocheck`, `@ts-ignore`, `@ts-expect-error`.
    "@ts-",
    // ESLint: `eslint-disable`, `eslint-disable-line`,
    // `eslint-disable-next-line`, `eslint-enable`, `eslint-env`. The hyphen is
    // part of the prefix - bare `eslint` configures rules inline and is only
    // read from a block comment, while `// eslint is configured in .eslintrc`
    // is a sentence. Its `global`, `globals` and `exported` are left off for
    // the same reason: they are English words first.
    "eslint-",
    "prettier-ignore",
    "biome-ignore",
    // Coverage tools. `istanbul ignore next`, `c8 ignore start`,
    // `v8 ignore next`. The bare tool name is not enough on its own - `c8` and
    // `v8` are words about a runtime as often as they are pragmas - so the
    // verb is part of the prefix.
    "istanbul ignore",
    "c8 ignore",
    "v8 ignore",
];

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

/// JavaScript, TypeScript and TSX. The markers are C's, and JSDoc writes an
/// example with a Markdown fence the way Rustdoc does - indentation on its own
/// says nothing, so these are [`CommentRules::Fenced`].
const ECMASCRIPT_SYNTAX: CommentSyntax = CommentSyntax {
    directives: DirectiveSyntax::EcmaScript,
    ..C_LIKE_SYNTAX
};

impl SupportedLanguage {
    /// Every language this build reads.
    ///
    /// A language, not a `languageId`: one language answers to several ids,
    /// which is what [`Self::lsp_language_ids`] carries. The two coincide for
    /// Zed, and that is what makes the comparison below possible at all.
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
    pub const ALL: [Self; 5] = [
        Self::Rust,
        Self::Go,
        Self::JavaScript,
        Self::TypeScript,
        Self::Tsx,
    ];

    /// Every `languageId` a client may send for this language.
    ///
    /// One language, several spellings. An editor names its own languages and
    /// the names do not agree: Zed sends its `LanguageName` lowercased, so TSX
    /// arrives as `tsx`, while VS Code calls that same language
    /// `typescriptreact` - and splits `.jsx` out into a `javascriptreact` of
    /// its own where Zed folds it into JavaScript (`.jsx` is one of
    /// JavaScript's `path_suffixes` there). The grammar is JavaScript's either
    /// way, because `tree-sitter-javascript` parses JSX.
    ///
    /// Read off zed main's `crates/grammars/src/*/config.toml` (through
    /// `LanguageName::lsp_id`) and `microsoft/vscode`'s
    /// `extensions/*/package.json`. Nothing here is a guess at a name: an id
    /// belongs in this table once some client is known to send it.
    ///
    /// [`Self::from_lsp_language_id`] reads this and nothing else, so an id
    /// added here is recognised in the same commit - there is no second list to
    /// keep in step. [`Self::as_str`] is always among a language's ids, which
    /// is what lets CI compare [`Self::ALL`] against Zed's `languages` without
    /// knowing anything about VS Code's names.
    pub fn lsp_language_ids(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["rust"],
            Self::Go => &["go"],
            Self::JavaScript => &["javascript", "javascriptreact"],
            Self::TypeScript => &["typescript"],
            Self::Tsx => &["tsx", "typescriptreact"],
        }
    }

    /// Maps the `languageId` a client sends with `textDocument/didOpen` onto a
    /// grammar, over [`Self::lsp_language_ids`].
    ///
    /// Returns `None` for anything CodeGloss cannot parse yet, which the server
    /// treats as "this document has no comments" rather than as an error.
    pub fn from_lsp_language_id(language_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.lsp_language_ids().contains(&language_id))
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
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
        }
    }

    /// The Tree-sitter grammar. The grammar crates expose a
    /// `tree_sitter_language::LanguageFn` that converts into a [`Language`].
    pub(crate) fn grammar(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    /// The query selecting every comment node, with a single `@comment` capture.
    pub(crate) fn comment_query(self) -> &'static str {
        match self {
            Self::Rust => include_str!("queries/rust.scm"),
            Self::Go => include_str!("queries/go.scm"),
            // One query for the three: the TypeScript and TSX grammars are
            // generated from JavaScript's, so the node is the same node.
            Self::JavaScript | Self::TypeScript | Self::Tsx => {
                include_str!("queries/ecmascript.scm")
            }
        }
    }

    pub(crate) fn comment_syntax(self) -> CommentSyntax {
        match self {
            Self::Rust => C_LIKE_SYNTAX,
            Self::Go => GO_SYNTAX,
            Self::JavaScript | Self::TypeScript | Self::Tsx => ECMASCRIPT_SYNTAX,
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

    /// Zed reports each of the three by its `LanguageName` lowercased, which is
    /// what `crates/language_core/src/language_name.rs::lsp_id` does to the
    /// `name` in `crates/grammars/src/{javascript,typescript,tsx}/config.toml`:
    /// `"JavaScript"`, `"TypeScript"`, `"TSX"`. All three are built into Zed,
    /// so no other extension has to be installed for them to arrive.
    ///
    /// VS Code names two of them differently, and that is the whole reason
    /// [`SupportedLanguage::lsp_language_ids`] is a list rather than one name:
    /// `.tsx` is `typescriptreact` there, and `.jsx` is a `javascriptreact` of
    /// its own instead of one of JavaScript's suffixes. Read off
    /// `microsoft/vscode`'s `extensions/javascript/package.json` and
    /// `extensions/typescript-basics/package.json`.
    #[test]
    fn the_ecmascript_family_is_recognised_by_its_lsp_language_ids() {
        for (id, language) in [
            ("javascript", SupportedLanguage::JavaScript),
            ("javascriptreact", SupportedLanguage::JavaScript),
            ("typescript", SupportedLanguage::TypeScript),
            ("tsx", SupportedLanguage::Tsx),
            ("typescriptreact", SupportedLanguage::Tsx),
        ] {
            assert_eq!(
                SupportedLanguage::from_lsp_language_id(id),
                Some(language),
                "in {id:?}"
            );
        }
        // An id is a name some client is known to send, not a guess at one.
        // The first two are Zed's display names rather than its `lsp_id`, the
        // next three are file suffixes, and the last is nobody's spelling.
        for id in ["JavaScript", "TSX", "js", "ts", "jsx", "TypeScriptReact"] {
            assert_eq!(
                SupportedLanguage::from_lsp_language_id(id),
                None,
                "in {id:?}"
            );
        }
    }

    /// The table is the only list of ids there is, so the two properties that
    /// make it usable have to hold inside it rather than at each call site.
    ///
    /// An id names exactly one language: two languages claiming one id would
    /// be resolved by the order of `ALL`, which is not a rule anyone reading
    /// either list would guess. Two languages sharing an `as_str` fail here for
    /// the same reason, since that name is always among a language's ids.
    ///
    /// And a language answers to its own [`SupportedLanguage::as_str`]. `ALL`
    /// is what CI compares against `editors/zed/extension.toml`, so an entry
    /// under a name no `didOpen` could produce would leave that check passing
    /// while the server stayed silent on the buffer - and it is also what lets
    /// the comparison stay in Zed's names while knowing nothing about VS
    /// Code's.
    #[test]
    fn every_language_id_names_exactly_one_language() {
        let mut claimed: Vec<&str> = Vec::new();
        for language in SupportedLanguage::ALL {
            let ids = language.lsp_language_ids();
            assert!(
                ids.contains(&language.as_str()),
                "{language:?} does not answer to its own name {:?}",
                language.as_str()
            );
            for id in ids {
                assert_eq!(
                    SupportedLanguage::from_lsp_language_id(id),
                    Some(language),
                    "in {id:?}"
                );
                assert!(!claimed.contains(id), "{id:?} is claimed twice");
                claimed.push(id);
            }
        }
    }

    /// JSDoc marks an example with a Markdown fence, or with `@example` - never
    /// with indentation, which is Go's alone. Reading these as `Indented` would
    /// copy every wrapped line of prose through untranslated.
    #[test]
    fn the_ecmascript_family_reads_its_comments_as_fenced() {
        for language in [
            SupportedLanguage::JavaScript,
            SupportedLanguage::TypeScript,
            SupportedLanguage::Tsx,
        ] {
            assert_eq!(language.rules(), CommentRules::Fenced, "in {language:?}");
            assert_eq!(
                language.comment_syntax().directives,
                DirectiveSyntax::EcmaScript,
                "in {language:?}"
            );
        }
    }

    /// The tool pragmas, and the sentences they are one keystroke away from.
    ///
    /// The last group is the point: this is a list and not a rule, so the way
    /// it fails is by swallowing prose, and the rows below are the prose it
    /// would swallow if the prefixes were any shorter.
    #[test]
    fn an_ecmascript_pragma_speaks_to_a_tool_and_a_comment_does_not() {
        for body in [
            " @ts-ignore",
            "@ts-expect-error",
            " @ts-expect-error the call is checked at runtime",
            " @ts-nocheck",
            " eslint-disable-next-line no-console",
            " eslint-disable-line ban/ban",
            " eslint-enable",
            " prettier-ignore",
            " biome-ignore lint:",
            " istanbul ignore next",
            " c8 ignore start",
            " v8 ignore next",
            // A triple-slash directive reaches this with its third slash still
            // on, because the marker that came off was `//`.
            "/ <reference types=\"node\" />",
            "/ <reference no-default-lib=\"true\"/>",
            // And inside commented-out code, where the pragma is nested behind
            // a second marker.
            "   // @ts-expect-error",
        ] {
            assert!(DirectiveSyntax::EcmaScript.matches(body), "in {body:?}");
            // Scoped to the languages that name it: the same line in a Rust
            // file is a sentence about JavaScript.
            assert!(!DirectiveSyntax::None.matches(body), "in {body:?}");
        }

        for body in [
            " eslint is configured in .eslintrc",
            " eslintrc lives at the repository root",
            " c8 is the coverage tool we use",
            " v8 optimises this shape",
            " istanbul is no longer maintained",
            " Compare with <T> and the cast it implies",
            " prettier ignores this file",
            " TODO: drop the @ts-ignore below",
            "",
            " ",
            "/",
        ] {
            assert!(!DirectiveSyntax::EcmaScript.matches(body), "in {body:?}");
        }
    }
}
