//! Turning a source file into the comment blocks CodeGloss will translate.
//!
//! Comments are located with a Tree-sitter query, never with a regular
//! expression: `//` inside a string literal such as `"https://example.com"`
//! belongs to the string node and must not be picked up.

use codegloss_core::{CommentBlock, CommentStyle};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::languages::{CommentSyntax, SupportedLanguage};

/// Why a document could not be scanned for comments.
///
/// Every variant is a bug in this crate rather than a property of the document:
/// the grammars and the queries are compiled in. They are returned instead of
/// panicking so that one broken grammar cannot take the language server down.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractError {
    /// The linked Tree-sitter cannot run this grammar, typically an ABI mismatch.
    #[error("the {language} grammar cannot be loaded: {source}")]
    Grammar {
        language: &'static str,
        #[source]
        source: tree_sitter::LanguageError,
    },
    /// The bundled `.scm` does not compile against its own grammar.
    #[error("the {language} comment query does not compile: {source}")]
    Query {
        language: &'static str,
        #[source]
        source: tree_sitter::QueryError,
    },
    /// Tree-sitter gave up, e.g. because a timeout or cancellation flag was set.
    #[error("tree-sitter produced no tree for a {language} document")]
    NoTree { language: &'static str },
}

/// Extracts every translatable comment from `source`.
///
/// Blocks come back sorted by position. Consecutive line comments that read as
/// one paragraph are merged into a single block, so a sentence spread over
/// several `//` lines is translated as a whole rather than line by line.
/// Comments that carry no words - an empty `//`, a `//////////` rule - are
/// dropped, and dropping them also ends the run they interrupt.
pub fn extract_comment_blocks(
    source: &str,
    language: SupportedLanguage,
) -> Result<Vec<CommentBlock>, ExtractError> {
    let grammar = language.grammar();
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|source| ExtractError::Grammar {
            language: language.as_str(),
            source,
        })?;

    // Re-parsing the whole document on every change is fine at this size; the
    // step up is `parser.parse(source, Some(&old_tree))` with the tree kept
    // alongside the document.
    let tree = parser.parse(source, None).ok_or(ExtractError::NoTree {
        language: language.as_str(),
    })?;

    let query =
        Query::new(&grammar, language.comment_query()).map_err(|source| ExtractError::Query {
            language: language.as_str(),
            source,
        })?;

    let syntax = language.comment_syntax();
    let mut comments = Vec::new();
    let mut cursor = QueryCursor::new();
    // `QueryCursor::matches` returns a `StreamingIterator`, so a plain `for`
    // loop does not compile here.
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            if let Some(comment) = RawComment::from_node(capture.node, source, syntax) {
                comments.push(comment);
            }
        }
    }

    comments.sort_by_key(|comment| comment.start_byte);
    comments.retain(RawComment::is_translatable);
    Ok(merge_runs(comments, source))
}

/// Which marker opened a comment.
///
/// Read off the syntax tree rather than off the text: the grammar exposes the
/// doc-comment markers as their own nodes, so `//////////` is not mistaken for
/// a `///` doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// `//` or `/* */`.
    Plain,
    /// `//!`, the module-level form.
    InnerDoc,
    /// `///` or `/** */`.
    OuterDoc,
}

/// One comment node, before neighbouring ones are merged into a block.
#[derive(Debug)]
struct RawComment {
    style: CommentStyle,
    marker: Marker,
    /// The comment text with its markers stripped.
    body: String,
    start_line: u32,
    end_line: u32,
    start_byte: usize,
    end_byte: usize,
    /// Column the comment starts at. Two comments indented differently belong
    /// to different blocks even when they sit on consecutive lines.
    column: usize,
    /// `false` for a comment that trails code, as in `let x = 1; // note`.
    /// Trailing comments never join a block.
    own_line: bool,
}

impl RawComment {
    fn from_node(node: Node<'_>, source: &str, syntax: CommentSyntax) -> Option<Self> {
        let start_byte = node.start_byte();
        // Doc line comments carry their terminating newline inside the node,
        // plain ones do not. Trimming evens that out, so `end_line` names the
        // last line the comment actually occupies.
        let text = source.get(start_byte..node.end_byte())?.trim_end();
        let end_byte = start_byte + text.len();

        let is_block = text.starts_with(syntax.block_start);
        let doc_marker = node
            .child_by_field_name("inner")
            .map(|node| (Marker::InnerDoc, node))
            .or_else(|| {
                node.child_by_field_name("outer")
                    .map(|node| (Marker::OuterDoc, node))
            });
        let marker = doc_marker.map_or(Marker::Plain, |(marker, _)| marker);

        let opener = if is_block {
            syntax.block_start
        } else {
            syntax.line
        };
        let content_start =
            doc_marker.map_or(start_byte + opener.len(), |(_, node)| node.end_byte());
        let content_end = if is_block && text.ends_with(syntax.block_end) {
            end_byte
                .saturating_sub(syntax.block_end.len())
                .max(content_start)
        } else {
            end_byte
        };
        let content = source.get(content_start..content_end)?;

        let body = if is_block {
            join_block_lines(content, syntax)
        } else {
            content.trim().to_owned()
        };

        let start_line = node.start_position().row as u32;
        let line_start = source[..start_byte]
            .rfind('\n')
            .map_or(0, |index| index + 1);

        Some(Self {
            style: match (is_block, marker) {
                (false, Marker::Plain) => CommentStyle::Line,
                (false, _) => CommentStyle::DocLine,
                (true, Marker::Plain) => CommentStyle::Block,
                (true, _) => CommentStyle::DocBlock,
            },
            marker,
            body,
            start_line,
            end_line: start_line + text.matches('\n').count() as u32,
            start_byte,
            end_byte,
            column: node.start_position().column,
            own_line: source[line_start..start_byte]
                .chars()
                .all(char::is_whitespace),
        })
    }

    /// Whether the comment carries words worth translating.
    ///
    /// An empty `//`, a `//////////` rule and a `// ====` banner all reduce to
    /// punctuation and are skipped.
    fn is_translatable(&self) -> bool {
        self.body.chars().any(char::is_alphanumeric)
    }

    fn is_line_comment(&self) -> bool {
        matches!(self.style, CommentStyle::Line | CommentStyle::DocLine)
    }

    /// Whether `next` continues the same paragraph as `self`.
    fn continues_into(&self, next: &Self) -> bool {
        self.is_line_comment()
            && next.is_line_comment()
            // `///` and `//!` say different things about what they document.
            && self.marker == next.marker
            && self.own_line
            && next.own_line
            && self.column == next.column
            && self.end_line + 1 == next.start_line
    }

    fn into_block(self, source: &str) -> CommentBlock {
        CommentBlock {
            style: self.style,
            text: self.body,
            raw: source[self.start_byte..self.end_byte].to_owned(),
            start_line: self.start_line,
            end_line: self.end_line,
            start_byte: self.start_byte,
            end_byte: self.end_byte,
        }
    }
}

/// Strips the leading `*` decoration from the continuation lines of a block
/// comment and joins what is left into one line.
fn join_block_lines(content: &str, syntax: CommentSyntax) -> String {
    let mut parts = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        // Only continuation lines are decorated; on the opening line a `*`
        // would be part of the sentence.
        let line = if index == 0 {
            line
        } else {
            line.strip_prefix(syntax.block_continuation).unwrap_or(line)
        };
        let line = line.trim();
        if !line.is_empty() {
            parts.push(line);
        }
    }
    parts.join(" ")
}

/// Folds runs of consecutive line comments into single blocks.
///
/// Bodies are joined with a space rather than a newline: the result is fed to a
/// machine translator, which wants one sentence, not a column of fragments.
fn merge_runs(comments: Vec<RawComment>, source: &str) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    let mut open: Option<RawComment> = None;

    for comment in comments {
        match open.take() {
            Some(mut previous) if previous.continues_into(&comment) => {
                previous.body.push(' ');
                previous.body.push_str(&comment.body);
                previous.end_line = comment.end_line;
                previous.end_byte = comment.end_byte;
                open = Some(previous);
            }
            Some(previous) => {
                blocks.push(previous.into_block(source));
                open = Some(comment);
            }
            None => open = Some(comment),
        }
    }

    if let Some(previous) = open {
        blocks.push(previous.into_block(source));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(source: &str) -> Vec<CommentBlock> {
        extract_comment_blocks(source, SupportedLanguage::Rust).expect("rust source parses")
    }

    fn texts(source: &str) -> Vec<String> {
        blocks(source).into_iter().map(|block| block.text).collect()
    }

    #[test]
    fn an_empty_comment_ends_the_paragraph() {
        assert_eq!(
            texts("// One.\n//\n// Two.\n"),
            ["One.".to_owned(), "Two.".to_owned()]
        );
    }

    #[test]
    fn a_blank_line_ends_the_paragraph() {
        assert_eq!(
            texts("// One.\n\n// Two.\n"),
            ["One.".to_owned(), "Two.".to_owned()]
        );
    }

    #[test]
    fn a_change_of_indentation_ends_the_paragraph() {
        let source = "fn f() {\n    // One.\n        // Two.\n}\n";
        assert_eq!(texts(source), ["One.".to_owned(), "Two.".to_owned()]);
    }

    #[test]
    fn inner_and_outer_doc_comments_do_not_merge() {
        let source = "//! Module.\n/// Item.\nfn f() {}\n";
        assert_eq!(texts(source), ["Module.".to_owned(), "Item.".to_owned()]);
    }

    #[test]
    fn a_doc_comment_does_not_merge_with_a_plain_one() {
        let source = "// Plain.\n/// Doc.\nfn f() {}\n";
        let blocks = blocks(source);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].style, CommentStyle::Line);
        assert_eq!(blocks[1].style, CommentStyle::DocLine);
    }

    #[test]
    fn a_trailing_comment_stays_on_its_own() {
        // The trailing comment sits on the line right after the run, so only
        // the "is it at the start of its line" test keeps them apart.
        let source = "fn f() {\n    // Note.\n    let x = 1; // Why.\n}\n";
        assert_eq!(texts(source), ["Note.".to_owned(), "Why.".to_owned()]);
    }

    #[test]
    fn two_trailing_comments_do_not_merge() {
        let source = "fn f() {\n    let a = 1; // First.\n    let b = 2; // Second.\n}\n";
        assert_eq!(texts(source), ["First.".to_owned(), "Second.".to_owned()]);
    }

    #[test]
    fn block_comments_are_never_merged() {
        let source = "/* One. */\n/* Two. */\nfn f() {}\n";
        assert_eq!(texts(source), ["One.".to_owned(), "Two.".to_owned()]);
    }

    #[test]
    fn decoration_only_comments_are_dropped() {
        for source in [
            "//////////\nfn f() {}\n",
            "//\nfn f() {}\n",
            "// ====\nfn f() {}\n",
            "/* --- */\nfn f() {}\n",
        ] {
            assert!(texts(source).is_empty(), "not dropped: {source:?}");
        }
    }

    #[test]
    fn a_string_literal_holding_a_url_yields_no_comment() {
        let source = "fn f() {\n    let u = \"https://example.com\";\n    let _ = u;\n}\n";
        assert!(texts(source).is_empty());
    }

    #[test]
    fn a_url_after_a_real_comment_marker_is_kept() {
        let source = "// See https://example.com for details.\nfn f() {}\n";
        assert_eq!(
            texts(source),
            ["See https://example.com for details.".to_owned()]
        );
    }

    #[test]
    fn the_javadoc_star_of_continuation_lines_is_stripped() {
        let source = "/**\n * Loads the user.\n * Returns none on a miss.\n */\nfn f() {}\n";
        let blocks = blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].style, CommentStyle::DocBlock);
        assert_eq!(blocks[0].text, "Loads the user. Returns none on a miss.");
        // The stripped decoration survives in `raw`, which post-processing
        // needs to rebuild the block's shape.
        assert!(blocks[0].raw.contains("\n * Loads the user."));
    }

    #[test]
    fn a_doc_line_comment_does_not_swallow_its_newline() {
        let source = "/// Doc.\nfn f() {}\n";
        let blocks = blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].start_line, 0);
        assert_eq!(blocks[0].end_line, 0);
        assert_eq!(blocks[0].raw, "/// Doc.");
    }

    #[test]
    fn offsets_are_byte_offsets_and_survive_multibyte_text() {
        let source = "let s = \"日本語\"; // 日本語のコメント。\n";
        let blocks = blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "日本語のコメント。");
        assert_eq!(
            &source[blocks[0].start_byte..blocks[0].end_byte],
            "// 日本語のコメント。"
        );
    }

    #[test]
    fn a_document_without_comments_yields_nothing() {
        assert!(texts("fn main() {}\n").is_empty());
        assert!(texts("").is_empty());
    }

    #[test]
    fn an_unterminated_block_comment_does_not_panic() {
        assert_eq!(texts("/* dangling\n"), ["dangling".to_owned()]);
        assert!(texts("/*").is_empty());
        assert!(texts("/*/").is_empty());
    }
}
