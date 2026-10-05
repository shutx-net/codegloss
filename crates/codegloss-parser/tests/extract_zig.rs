//! End-to-end extraction over a fixture that holds one of each Zig comment
//! shape.
//!
//! The fixture is stored as `.zig.txt`: neither cargo nor `zig build` should
//! try to compile it, and `tests/` is where cargo looks for test targets. It is
//! Zig all the same - `zig ast-check` and `zig fmt --check` of 0.17.0 take it
//! as it stands - so every shape in it is one Zig itself writes.
//!
//! Three things set Zig apart, and the blocks this file checks carry all of
//! them. Its grammar has one `(comment)` token for every comment and hands no
//! doc marker over, so `///` and `//!` stay in the text and are read off it in
//! the registry's spelling. Its doc comments are autodoc's Markdown, with
//! backtick fences and no block tags, which is `CommentRules::FencedUntagged`.
//! And `// zig fmt: off` / `// zig fmt: on` speak to the formatter rather than
//! to a reader (`docs/model-runtime-notes.md` §19).

use codegloss_core::{CommentBlock, CommentRules, CommentShape, CommentStyle};
use codegloss_parser::{SupportedLanguage, extract_comment_blocks};

const FIXTURE: &str = include_str!("fixtures/sample.zig.txt");

/// The parts of a block this test cares about, in a shape that fails readably.
#[derive(Debug, PartialEq, Eq)]
struct Summary {
    style: CommentStyle,
    rules: CommentRules,
    start_line: u32,
    end_line: u32,
    text: &'static str,
}

fn expected() -> Vec<Summary> {
    // Four lines that read as comments are not here. Line 19 is a line of a
    // multiline string, `//` and all; line 22 is a `////////////////` rule,
    // decoration with no word in it; lines 26 and 29 are `// zig fmt: off`
    // and `// zig fmt: on`, which address the formatter and are dropped
    // before a block is built.
    vec![
        Summary {
            style: CommentStyle::DocLine,
            rules: CommentRules::FencedUntagged,
            start_line: 0,
            end_line: 1,
            text: "Fixture holding one of every comment shape Zig writes. It is stored as .zig.txt so that neither cargo nor zig build picks it up.",
        },
        // The empty `///` on line 7 is decoration outside a fence: it is
        // dropped, and the paragraph ends there.
        Summary {
            style: CommentStyle::DocLine,
            rules: CommentRules::FencedUntagged,
            start_line: 5,
            end_line: 6,
            text: "Looks the user up in the cache. Falls back to the database on a miss.",
        },
        // Issue #53 in Zig: one block from the opening fence to the closing
        // one. The empty `///` on line 10 is a line of the example, so the run
        // goes on through it, and it adds nothing to the text.
        Summary {
            style: CommentStyle::DocLine,
            rules: CommentRules::FencedUntagged,
            start_line: 8,
            end_line: 13,
            text: "```zig const user = try find(42); // Seed the cache first. _ = user; ```",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::FencedUntagged,
            start_line: 15,
            end_line: 16,
            text: "The // in the URL below is not a comment. It sits inside a string literal, and Tree-sitter leaves it there.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::FencedUntagged,
            start_line: 17,
            end_line: 17,
            text: "Trailing note.",
        },
        // Not a doc comment, so the text is what follows the `//`, the other
        // two slashes included: see
        // `four_slashes_are_a_plain_comment_and_keep_a_slash`.
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::FencedUntagged,
            start_line: 24,
            end_line: 24,
            text: "// Four slashes make a plain comment, not a doc comment.",
        },
        Summary {
            style: CommentStyle::DocLine,
            rules: CommentRules::FencedUntagged,
            start_line: 35,
            end_line: 35,
            text: "The name shown in the UI.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::FencedUntagged,
            start_line: 39,
            end_line: 39,
            text: "日本語のコメントもそのまま抜き出す。",
        },
    ]
}

fn extract() -> Vec<CommentBlock> {
    extract_comment_blocks(FIXTURE, SupportedLanguage::Zig).expect("the fixture parses")
}

fn block_at(start_line: u32) -> CommentBlock {
    extract()
        .into_iter()
        .find(|block| block.start_line == start_line)
        .unwrap_or_else(|| panic!("a block starts at line {start_line}"))
}

/// The shape of a block as its own rules read it, which is what the LSP worker
/// builds a gloss from.
fn shape(block: &CommentBlock) -> CommentShape {
    CommentShape::parse(&block.raw, block.rules)
}

#[test]
fn the_fixture_yields_exactly_the_expected_blocks() {
    let format = |style: &CommentStyle, rules: &CommentRules, start, end, text: &str| {
        format!("{style:?} {rules:?} {start}-{end} {text}")
    };
    let actual: Vec<_> = extract()
        .iter()
        .map(|block| {
            format(
                &block.style,
                &block.rules,
                block.start_line,
                block.end_line,
                &block.text,
            )
        })
        .collect();
    let expected: Vec<_> = expected()
        .iter()
        .map(|summary| {
            format(
                &summary.style,
                &summary.rules,
                summary.start_line,
                summary.end_line,
                summary.text,
            )
        })
        .collect();

    assert_eq!(actual, expected);
}

/// The reason CodeGloss parses with Tree-sitter instead of matching `//` with a
/// regular expression. A URL inside a string literal is not a comment.
#[test]
fn a_url_in_a_string_literal_is_not_a_zig_comment() {
    assert!(
        FIXTURE.contains("\"https://example.com/users\""),
        "the fixture must keep the URL this test is about"
    );

    for block in extract() {
        assert!(
            !block.text.contains("example.com"),
            "a URL inside a string literal leaked into a comment block: {block:?}"
        );
    }
}

/// Zig's other string. A line of a multiline string literal opens with `\\`,
/// and everything after that to the end of the line is the string's - a `//`
/// included, even one at the very start of it. Read off the text, this line
/// holds a comment; in the syntax tree it is part of the string node, and the
/// query never sees it.
#[test]
fn a_multiline_string_line_is_not_a_comment() {
    assert!(
        FIXTURE.contains("\\\\// Not a comment either"),
        "the fixture must keep the multiline string this test is about"
    );

    for block in extract() {
        assert!(
            !block.raw.contains("Not a comment either"),
            "a line of a multiline string leaked into a comment block: {block:?}"
        );
    }
}

/// Zig's grammar hands no doc marker over - `//`, `///`, `//!` and `////` are
/// one `(comment)` token - so the marker is read off the text, in the
/// registry's spelling. Read the way a grammar with no markers is read, `//`
/// and then whatever follows, every doc comment's text would open with the
/// last character of its marker: `/ Looks the user up ...`,
/// `! Fixture holding ...`. The text is what a hover shows before the gloss
/// lands and what it quotes underneath the gloss after.
///
/// The one block that keeps a slash is the `////` line, which is not a doc
/// comment at all: see `four_slashes_are_a_plain_comment_and_keep_a_slash`.
#[test]
fn the_doc_markers_are_read_off_the_text() {
    let blocks = extract();
    // Both kinds are in the fixture, and both are read as doc comments.
    for marker in ["//! ", "/// "] {
        let doc = blocks
            .iter()
            .find(|block| block.raw.starts_with(marker))
            .unwrap_or_else(|| panic!("the fixture must keep a {marker:?} comment"));
        assert_eq!(doc.style, CommentStyle::DocLine, "{doc:?}");
    }

    for block in blocks.iter().filter(|block| !block.raw.starts_with("////")) {
        assert!(
            !block.text.starts_with(['/', '!']),
            "a doc marker was left in the text: {block:?}"
        );
    }
}

/// Issue #53 through a real Zig file: the example arrives whole, so
/// `CommentShape` sees the fences and copies the code through instead of
/// handing it to the engine as prose. The blank `///` inside it is a line of
/// the example rather than the end of a paragraph.
///
/// It takes the registry's markers to see the fence at all. Read as `//` and
/// then a slash, `/// ```zig` is no fence to the parser, the blank line ends
/// the run, and `_ = user;` reaches the engine as prose, run into the comment
/// above it.
#[test]
fn the_fenced_example_reaches_the_shape_in_one_piece() {
    let example = block_at(8);

    assert_eq!(example.end_line, 13);
    assert!(example.raw.starts_with("/// ```zig\n"));
    assert!(example.raw.ends_with("\n/// ```"));

    let shape = shape(&example);
    // The comment inside the example is prose and is the one thing glossed;
    // the code around it is not (Issue #70).
    assert_eq!(shape.units(), ["Seed the cache first."]);
    // Not just "nothing else to translate" but "nothing else changed": the
    // example comes back out as it was written, fences and blank line
    // included.
    assert_eq!(
        shape.source(),
        concat!(
            "```zig\n",
            "const user = try find(42);\n",
            "\n",
            "// Seed the cache first.\n",
            "_ = user;\n",
            "```",
        )
    );
}

/// `// zig fmt: off` and `// zig fmt: on` switch the formatter off and back on
/// around a table aligned by hand, and they say nothing to a reader. Glossing
/// them would put a lens on each with a translation of a formatter command.
///
/// The judgement belongs to the language, and this is what that buys: the same
/// string read as Rust keeps both lines, because Rust has no directives.
#[test]
fn a_zig_fmt_directive_is_not_prose() {
    for directive in ["// zig fmt: off", "// zig fmt: on"] {
        assert!(
            FIXTURE.lines().any(|line| line.trim() == directive),
            "the fixture must keep {directive:?}"
        );
    }

    for block in extract() {
        assert!(
            !block.raw.contains("zig fmt"),
            "a zig fmt directive was glossed: {block:?}"
        );
    }

    let as_rust: Vec<String> = extract_comment_blocks(FIXTURE, SupportedLanguage::Rust)
        .expect("the fixture parses as Rust too")
        .into_iter()
        .map(|block| block.text)
        .collect();
    for directive in ["zig fmt: off", "zig fmt: on"] {
        assert!(
            as_rust.iter().any(|text| text == directive),
            "Rust has no directives, so {directive:?} stays: {as_rust:#?}"
        );
    }
}

#[test]
fn ranges_point_back_at_the_original_source() {
    for block in extract() {
        assert_eq!(
            &FIXTURE[block.start_byte..block.end_byte],
            block.raw,
            "raw must be exactly the bytes the range names"
        );
        // Zig has no block comment, so every block is a run of line comments.
        assert!(block.raw.starts_with("//"), "{block:?}");
        assert!(
            !block.raw.ends_with('\n'),
            "ranges stop at the last comment character"
        );
        // The lines name the same place as the bytes: an editor position is
        // built from one and a hover is matched against the other.
        assert_eq!(
            FIXTURE[..block.start_byte].matches('\n').count(),
            block.start_line as usize,
            "{block:?}"
        );
        assert_eq!(
            FIXTURE[..block.end_byte].matches('\n').count(),
            block.end_line as usize,
            "{block:?}"
        );
    }
}

/// Four slashes make a plain comment, in Zig as in Rust: a slash straight
/// after `///` turns the doc comment back into a line comment. Zig itself
/// says so about this very line: `zig ast-check` of 0.17.0 rejects it written
/// with three slashes ("expected statement, found 'a document comment'") and
/// accepts it with four. The parser reads it the same way - the block is
/// `Line`, and its text is what follows the `//`.
///
/// A known limitation, and not Zig's: `codegloss-core` takes the longest of
/// its line markers off first, so `CommentShape` strips `///` from this line
/// and leaves the fourth slash at the head of the unit the engine is handed.
/// A Rust `////` comes apart the same way, which the second half holds
/// (`docs/model-runtime-notes.md` §19 counts how often Zig writes one).
#[test]
fn four_slashes_are_a_plain_comment_and_keep_a_slash() {
    let four = block_at(24);
    let unit = "/ Four slashes make a plain comment, not a doc comment.";

    assert_eq!(four.style, CommentStyle::Line);
    assert_eq!(
        four.text,
        "// Four slashes make a plain comment, not a doc comment."
    );
    assert_eq!(shape(&four).units(), [unit]);

    let rust = extract_comment_blocks(&four.raw, SupportedLanguage::Rust).expect("it parses");
    assert_eq!(rust.len(), 1, "{rust:#?}");
    assert_eq!(rust[0].style, CommentStyle::Line);
    assert_eq!(rust[0].text, four.text);
    assert_eq!(shape(&rust[0]).units(), [unit]);
}

/// A line of a Zig doc comment can open with a builtin, and the builtin is
/// named there, not written as a tag: autodoc has no block tag. Extracted as
/// Zig, the block carries `CommentRules::FencedUntagged`, under which a line
/// that opens with `@` and a word is prose - so the two lines stay one
/// paragraph and reach the engine as one sentence.
///
/// Fixed rather than known: under `CommentRules::Fenced`, which reads JSDoc's
/// tags, `@intCast` was a tag nobody had heard of and the paragraph split at
/// it into `["Calls", "on the id."]`, `Calls` going to the engine on its own
/// (`docs/model-runtime-notes.md` §19.6). The two sets are set side by side on
/// this same comment in `codegloss-core`'s
/// `a_line_opening_with_an_at_word_is_prose_under_rules_without_tags`.
#[test]
fn a_builtin_at_the_start_of_a_line_is_prose() {
    let source = concat!(
        "/// Calls\n",
        "/// @intCast on the id.\n",
        "pub fn narrow(id: u64) u32 {\n",
        "    return @intCast(id);\n",
        "}\n",
    );

    let blocks = extract_comment_blocks(source, SupportedLanguage::Zig).expect("the source parses");
    assert_eq!(blocks.len(), 1, "{blocks:#?}");
    let block = &blocks[0];
    assert_eq!(block.raw, "/// Calls\n/// @intCast on the id.");
    assert_eq!(block.rules, CommentRules::FencedUntagged);

    assert_eq!(shape(block).units(), ["Calls @intCast on the id."]);
}
