//! End-to-end extraction over a fixture that holds one of each comment shape.
//!
//! The fixture is stored as `.rs.txt`: a `.rs` file under `tests/` would be
//! picked up as a test target by cargo.

use codegloss_core::CommentStyle;
use codegloss_parser::{SupportedLanguage, extract_comment_blocks};

const FIXTURE: &str = include_str!("fixtures/sample.rs.txt");

/// The parts of a block this test cares about, in a shape that fails readably.
#[derive(Debug, PartialEq, Eq)]
struct Summary {
    style: CommentStyle,
    start_line: u32,
    end_line: u32,
    text: &'static str,
}

fn expected() -> Vec<Summary> {
    vec![
        Summary {
            style: CommentStyle::DocLine,
            start_line: 0,
            end_line: 1,
            text: "CodeGloss parser fixture. Stored as .rs.txt so that cargo does not try to build it.",
        },
        Summary {
            style: CommentStyle::DocLine,
            start_line: 3,
            end_line: 4,
            text: "Looks the user up in the cache. Falls back to the database on a miss.",
        },
        Summary {
            style: CommentStyle::Line,
            start_line: 6,
            end_line: 8,
            text: "The `//` in the URL below is not a comment. It sits inside a string literal, and Tree-sitter leaves it there instead of guessing from the raw text.",
        },
        Summary {
            style: CommentStyle::Line,
            start_line: 9,
            end_line: 9,
            text: "Trailing note.",
        },
        Summary {
            style: CommentStyle::Line,
            start_line: 13,
            end_line: 13,
            text: "First paragraph of the second run.",
        },
        Summary {
            style: CommentStyle::Line,
            start_line: 15,
            end_line: 15,
            text: "Second paragraph, split off by the empty comment above.",
        },
        Summary {
            style: CommentStyle::Block,
            start_line: 17,
            end_line: 20,
            text: "A block comment that continues on this line and ends here.",
        },
        Summary {
            style: CommentStyle::DocBlock,
            start_line: 25,
            end_line: 25,
            text: "Doc block attached to a struct.",
        },
    ]
}

fn extract() -> Vec<codegloss_core::CommentBlock> {
    extract_comment_blocks(FIXTURE, SupportedLanguage::Rust).expect("the fixture parses")
}

#[test]
fn the_fixture_yields_exactly_the_expected_blocks() {
    let actual: Vec<_> = extract()
        .iter()
        .map(|block| {
            format!(
                "{:?} {}-{} {}",
                block.style, block.start_line, block.end_line, block.text
            )
        })
        .collect();
    let expected: Vec<_> = expected()
        .iter()
        .map(|summary| {
            format!(
                "{:?} {}-{} {}",
                summary.style, summary.start_line, summary.end_line, summary.text
            )
        })
        .collect();

    assert_eq!(actual, expected);
}

/// The reason CodeGloss parses with Tree-sitter instead of matching `//` with a
/// regular expression. A URL inside a string literal is not a comment.
#[test]
fn a_url_in_a_string_literal_is_not_extracted() {
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

#[test]
fn three_consecutive_line_comments_become_one_block() {
    let run = extract()
        .into_iter()
        .find(|block| block.start_line == 6)
        .expect("the three-line run is extracted");

    assert_eq!(run.end_line, 8);
    assert_eq!(run.style, CommentStyle::Line);
    // Three source lines, joined with spaces rather than newlines so that the
    // translator sees one sentence.
    assert!(!run.text.contains('\n'));
    assert!(run.text.starts_with("The `//` in the URL below"));
    assert!(run.text.ends_with("from the raw text."));
}

#[test]
fn a_horizontal_rule_comment_is_dropped() {
    assert!(FIXTURE.contains("//////////"));
    assert!(
        extract().iter().all(|block| !block.text.contains("///")),
        "a rule made of slashes is decoration, not prose"
    );
    // Line 11 holds nothing but the rule, so no block may start there.
    assert!(extract().iter().all(|block| block.start_line != 11));
}

#[test]
fn ranges_point_back_at_the_original_source() {
    for block in extract() {
        assert_eq!(
            &FIXTURE[block.start_byte..block.end_byte],
            block.raw,
            "raw must be exactly the bytes the range names"
        );
        assert!(block.raw.starts_with("//") || block.raw.starts_with("/*"));
        assert!(
            !block.raw.ends_with('\n'),
            "ranges stop at the last comment character"
        );
        assert!(block.start_line <= block.end_line);
    }
}
