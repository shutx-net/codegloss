//! End-to-end extraction over a fixture that holds one of each Go comment
//! shape.
//!
//! The fixture is stored as `.go.txt`: neither cargo nor `go build` should try
//! to compile it, and `tests/` is where cargo looks for test targets.
//!
//! Go is the reason `CommentRules` exists. Its doc comments carry no Markdown
//! fence - not one line of one in the whole of `GOROOT` - and mark an example
//! by indenting it instead, so the blocks this file checks have to arrive
//! stamped with the rules that say so (`docs/model-runtime-notes.md` §16).

use codegloss_core::{CommentRules, CommentShape, CommentStyle};
use codegloss_parser::{SupportedLanguage, extract_comment_blocks};

const FIXTURE: &str = include_str!("fixtures/sample.go.txt");

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
    // The `//go:build linux` on line 0 is not here: it addresses the toolchain
    // rather than a reader, and the parser drops it before a block is built.
    vec![
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 2,
            end_line: 3,
            text: "Package fixture holds one of every comment shape Go writes. It is stored as .go.txt so that neither cargo nor go build picks it up.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 8,
            end_line: 8,
            text: "Find looks the user up in the cache.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 10,
            end_line: 10,
            text: "The example below is indented, which is the only thing that says it is one:",
        },
        // The example, minus the `//\t}` line: a line with no alphanumerics in
        // it is dropped as decoration and the run breaks there. Both halves are
        // still indented, so both are still copied through rather than
        // translated - what is lost is the brace, not the gloss.
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 12,
            end_line: 14,
            text: "user, err := Find(id) if err != nil { return err",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 17,
            end_line: 17,
            text: "Anything after it is prose again.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 19,
            end_line: 21,
            text: "The // in the URL below is not a comment. It sits inside a string literal, and Tree-sitter leaves it there instead of guessing from the raw text.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 22,
            end_line: 22,
            text: "Trailing note.",
        },
        Summary {
            style: CommentStyle::Block,
            rules: CommentRules::Indented,
            start_line: 24,
            end_line: 27,
            text: "This block comment is indented by the file rather than by its writer, and its prose has to stay prose.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 31,
            end_line: 31,
            text: "Order applies the rules in this order:",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 33,
            end_line: 34,
            text: "- Anything else comes before RC4 - ECDHE comes before anything else",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 36,
            end_line: 36,
            text: "A list is written indented and is still a list.",
        },
        Summary {
            style: CommentStyle::Block,
            rules: CommentRules::Indented,
            start_line: 39,
            end_line: 46,
            text: "Sum adds two numbers. This comment carries no stars, and its example is indented with spaces: total := Sum(1, 2) See [strings.TrimSpace] for the shape of a doc link.",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 49,
            end_line: 49,
            text: "日本語のコメントも字下げされた例を持てる:",
        },
        Summary {
            style: CommentStyle::Line,
            rules: CommentRules::Indented,
            start_line: 51,
            end_line: 51,
            text: "fmt.Println(\"こんにちは\")",
        },
    ]
}

fn extract() -> Vec<codegloss_core::CommentBlock> {
    extract_comment_blocks(FIXTURE, SupportedLanguage::Go).expect("the fixture parses")
}

fn block_at(start_line: u32) -> codegloss_core::CommentBlock {
    extract()
        .into_iter()
        .find(|block| block.start_line == start_line)
        .unwrap_or_else(|| panic!("a block starts at line {start_line}"))
}

/// The shape of a block as its own rules read it, which is what the LSP worker
/// builds a gloss from.
fn shape(block: &codegloss_core::CommentBlock) -> CommentShape {
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
fn a_url_in_a_string_literal_is_not_a_go_comment() {
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

/// Go has no doc-comment marker: `///` is two slashes and a third, and what
/// makes a comment a doc comment is what it sits above. The grammar says so
/// with a single `(comment)` node, and the style is read off the tree rather
/// than off the text.
#[test]
fn every_go_comment_is_a_plain_marker() {
    let source = concat!(
        "package p\n\n",
        "/// Three slashes.\n",
        "func A() {}\n\n",
        "//! Bang.\n",
        "func B() {}\n\n",
        "/* A block. */\n",
        "func C() {}\n",
    );

    let styles: Vec<CommentStyle> = extract_comment_blocks(source, SupportedLanguage::Go)
        .expect("the source parses")
        .iter()
        .map(|block| block.style)
        .collect();

    assert_eq!(
        styles,
        [CommentStyle::Line, CommentStyle::Line, CommentStyle::Block]
    );
}

/// Issue #30 through a real file: the example is indented and nothing else says
/// it is one, so it has to reach `CommentShape` as code and stay there.
#[test]
fn an_indented_example_reaches_the_shape_as_code() {
    let example = block_at(12);
    let shape = shape(&example);

    assert!(
        shape.units().is_empty(),
        "an indented example has nothing to translate: {example:?}"
    );
    // Not just "nothing to translate" but "nothing changed": the tabs are the
    // shape of the code and they are still there.
    assert_eq!(
        shape.source(),
        "\tuser, err := Find(id)\n\tif err != nil {\n\t\treturn err"
    );
}

/// The line that holds only `}` has no alphanumerics in it, so the parser drops
/// it as decoration and the run breaks in two. Both halves are indented, so
/// both are still copied through: what is lost is the brace, not the gloss.
/// Measured over `GOROOT` that is 1,018 lines in 267 files
/// (`docs/model-runtime-notes.md` §16).
#[test]
fn an_example_split_by_a_word_less_line_is_still_an_example() {
    let source = concat!(
        "package p\n\n",
        "// Example:\n",
        "//\n",
        "//\tfunc main() {\n",
        "//\t\tfmt.Println(\"hi\")\n",
        "//\t}\n",
        "//\ty()\n",
        "func A() {}\n",
    );

    let blocks = extract_comment_blocks(source, SupportedLanguage::Go).expect("the source parses");
    assert_eq!(blocks.len(), 3, "{blocks:#?}");

    let units: Vec<String> = blocks
        .iter()
        .flat_map(|block| {
            CommentShape::parse(&block.raw, block.rules)
                .units()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(units, ["Example:"]);
}

/// A list is indented too, and is not an example. Whatever the rules of the
/// language, a bulleted paragraph is prose and gets a gloss.
#[test]
fn a_list_in_a_go_comment_is_still_translated() {
    assert_eq!(
        shape(&block_at(33)).units(),
        [
            "Anything else comes before RC4",
            "ECDHE comes before anything else"
        ]
    );
}

/// A `/* ... */` written inside a function is indented by the file, not by its
/// writer. Taking the block's common indentation off first is what keeps this
/// prose; without it the comment reads as one example and its gloss is gone.
#[test]
fn a_block_comment_indented_by_the_file_keeps_its_prose() {
    assert_eq!(
        shape(&block_at(24)).units(),
        [
            "This block comment is indented by the file rather than by its writer, and its prose has to stay prose."
        ]
    );
}

/// The same block comment written at column 0 has no common indentation to
/// take off, so the space-indented example inside it is an example.
#[test]
fn an_example_indented_with_spaces_is_an_example_too() {
    let shape = shape(&block_at(39));

    assert_eq!(
        shape.units(),
        [
            "Sum adds two numbers. This comment carries no stars, and its example is indented with spaces:",
            "See [strings.TrimSpace] for the shape of a doc link."
        ]
    );
    assert!(shape.source().contains("total := Sum(1, 2)"));
}

/// A build constraint is an instruction to the toolchain, not a sentence, and
/// glossing one puts a meaningless lens on the first line of a great many Go
/// files. Measured over `GOROOT`, 6,676 blocks - 3.3% of them all - are nothing
/// but such lines (`docs/model-runtime-notes.md` §16).
///
/// The judgement belongs to the language, and this is what that buys: the same
/// line in a Rust file is still a comment, by construction rather than by luck.
#[test]
fn a_toolchain_directive_is_not_prose() {
    let source = concat!(
        "//go:build linux\n",
        "//go:generate go run mkasm.go\n\n",
        "package p\n\n",
        "// note: this is prose\n",
        "func A() {}\n\n",
        "//TODO: fix this\n",
        "func B() {}\n",
    );

    let texts: Vec<String> = extract_comment_blocks(source, SupportedLanguage::Go)
        .expect("the source parses")
        .into_iter()
        .map(|block| block.text)
        .collect();

    assert_eq!(
        texts,
        [
            "note: this is prose".to_owned(),
            "TODO: fix this".to_owned()
        ]
    );

    // Rust has no directives, so the same file read as Rust keeps every line.
    let as_rust: Vec<String> = extract_comment_blocks(source, SupportedLanguage::Rust)
        .expect("the source parses")
        .into_iter()
        .map(|block| block.text)
        .collect();

    assert_eq!(
        as_rust,
        [
            "go:build linux go:generate go run mkasm.go".to_owned(),
            "note: this is prose".to_owned(),
            "TODO: fix this".to_owned(),
        ]
    );
}

/// Go never writes a Markdown fence, but the machinery is not language-scoped
/// and still works if one appears. It is read first, so an indented line inside
/// a fence is fence content rather than an example of its own.
#[test]
fn a_fence_still_wins_in_go() {
    let source = concat!(
        "package p\n\n",
        "// Example:\n",
        "//\n",
        "// ```\n",
        "//\tfmt.Println()\n",
        "// ```\n",
        "func A() {}\n",
    );

    let blocks = extract_comment_blocks(source, SupportedLanguage::Go).expect("the source parses");
    let fenced = blocks
        .iter()
        .find(|block| block.raw.contains("```"))
        .expect("the fenced block is extracted");

    // The fence carries the block across the blank `//` line, exactly as it
    // does in Rust (Issue #53).
    assert_eq!(fenced.raw, "// ```\n//\tfmt.Println()\n// ```");
    let shape = CommentShape::parse(&fenced.raw, fenced.rules);
    assert!(shape.units().is_empty(), "{shape:?}");
    assert_eq!(shape.source(), "```\n\tfmt.Println()\n```");
}
