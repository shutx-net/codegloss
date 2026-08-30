//! Round-trip invariance of the pre- and post-processing, on the patterns
//! Issue #1 names.
//!
//! The property under test is `restore(translate(mask(x))) == x` for an engine
//! that returns its input. It is checkable exactly once: while the engine is a
//! passthrough. After P7 the same fixtures go through candle, where a failure
//! could be the model or could be this code - and telling those apart is only
//! possible if this half is known to be right first.
//!
//! The fixtures are the comment blocks as they appear in a file
//! ([`CommentBlock::raw`](codegloss_core::CommentBlock::raw)), because that is
//! what the pipeline hands to [`GlossPlan`].

use codegloss_core::{CommentShape, GlossPlan};

const JAVADOC: &str = include_str!("fixtures/javadoc.txt");
const RUSTDOC: &str = include_str!("fixtures/rustdoc.txt");
const LINE_COMMENTS: &str = include_str!("fixtures/line_comments.txt");

/// A fixture as the parser would hand it over: no trailing newline.
fn fixture(raw: &str) -> &str {
    raw.trim_end_matches('\n')
}

fn fixtures() -> [&'static str; 3] {
    [fixture(JAVADOC), fixture(RUSTDOC), fixture(LINE_COMMENTS)]
}

/// The engine of P4: every segment comes back as it went in.
fn passthrough(plan: &GlossPlan) -> Vec<String> {
    plan.segments()
        .iter()
        .map(|segment| segment.text().to_owned())
        .collect()
}

/// The gloss of a block, translated by an engine that changes nothing.
fn round_trip(raw: &str) -> String {
    let plan = GlossPlan::new(raw);
    plan.restore(&passthrough(&plan))
}

#[test]
fn a_passthrough_gloss_is_the_source_prose_unchanged() {
    for raw in fixtures() {
        assert_eq!(round_trip(raw), GlossPlan::new(raw).source(), "in {raw:?}");
    }
}

/// Spelled out rather than compared against `source()`: this is the structure
/// the phase promises, and a bug in `source()` would make the test above agree
/// with itself.
#[test]
fn the_javadoc_of_issue_1_comes_back_line_for_line() {
    assert_eq!(
        round_trip(fixture(JAVADOC)),
        concat!(
            "Returns the currently authenticated user.\n",
            "\n",
            "@param id the id to look up\n",
            "@return authenticated user\n",
            "@throws AuthenticationException if authentication failed",
        )
    );
}

#[test]
fn a_rustdoc_block_keeps_its_heading_its_list_and_its_fence() {
    assert_eq!(
        round_trip(fixture(RUSTDOC)),
        concat!(
            "Returns `UserDetails` when authentication succeeds.\n",
            "\n",
            "The protocol is described at https://example.com/docs/auth.\n",
            "\n",
            "# Panics\n",
            "\n",
            "- Panics when `find_user` is called before UserRepository::open().\n",
            "\n",
            "```\n",
            "let details = find_user(id).unwrap();\n",
            "```",
        )
    );
}

/// A run of `//` lines is one sentence, as P3 decided, and it comes back as
/// one line.
#[test]
fn a_run_of_line_comments_comes_back_as_one_line() {
    assert_eq!(
        round_trip(fixture(LINE_COMMENTS)),
        concat!(
            "TODO: return the cached user when find_user hits, ",
            "and fall back to UserRepository::load() otherwise.",
        )
    );
}

/// Every pattern Issue #1 lists, checked one at a time so that a failure names
/// the rule that broke.
#[test]
fn every_pattern_of_issue_1_survives_untouched() {
    for (pattern, raw) in [
        (
            "`UserDetails`",
            "/// Returns `UserDetails` when authentication succeeds.",
        ),
        (
            "https://example.com/a_b",
            "/// See https://example.com/a_b for the protocol.",
        ),
        ("@return", "/// @return authenticated user"),
        (
            "@throws",
            "/// @throws AuthenticationException if authentication failed",
        ),
        ("TODO:", "// TODO: drop the cache."),
        ("FIXME:", "// FIXME: drop the cache."),
        ("find_user", "// Calls find_user before anything else."),
        ("UserDetails", "// Returns UserDetails on success."),
        ("fetch()", "// Calls fetch() twice."),
        ("codegloss::core", "// Lives in codegloss::core."),
    ] {
        let gloss = round_trip(raw);
        assert!(gloss.contains(pattern), "{pattern:?} is not in {gloss:?}");
    }
}

/// The other half of the promise: what is protected never reaches the engine,
/// and what is prose does.
#[test]
fn the_engine_sees_placeholders_instead_of_code() {
    let plan = GlossPlan::new(fixture(RUSTDOC));
    let segments: Vec<String> = plan
        .segments()
        .iter()
        .map(|segment| segment.text().to_owned())
        .collect();
    let sent = segments.join("\n");

    for hidden in [
        "`UserDetails`",
        "https://example.com/docs/auth",
        "`find_user`",
        "UserRepository::open()",
        "let details = find_user(id).unwrap();",
    ] {
        assert!(
            !sent.contains(hidden),
            "{hidden:?} reached the engine: {sent:?}"
        );
    }

    // Prose is still prose: an over-eager rule that masked whole sentences
    // would pass the test above and translate nothing.
    assert!(sent.contains("when authentication succeeds."), "{sent:?}");
    assert!(sent.contains("The protocol is described at"), "{sent:?}");
    assert!(sent.contains("Panics"), "{sent:?}");
}

/// Japanese reorders what it translates; a placeholder has to follow its span
/// rather than its position.
#[test]
fn a_gloss_whose_placeholders_moved_is_still_restored() {
    let plan = GlossPlan::new("/// Calls `load()` before find_user.");
    let translated = plan.restore(&["X1Q の前に X0Q を呼ぶ。".to_owned()]);

    assert_eq!(translated, "find_user の前に `load()` を呼ぶ。");
}

/// A translation that dropped a placeholder has dropped an identifier with it.
/// The English is returned instead, for that unit alone.
#[test]
fn a_gloss_that_lost_a_placeholder_falls_back_to_the_english() {
    let plan = GlossPlan::new("/// Returns `UserDetails` when authentication succeeds.");

    assert_eq!(
        plan.restore(&["認証に成功したときに返します。".to_owned()]),
        "Returns `UserDetails` when authentication succeeds."
    );
}

/// The shape is read off the raw comment, so a block that was never parsed as
/// one - a single line, a fragment - still works.
#[test]
fn a_comment_with_nothing_to_translate_yields_no_segments() {
    let plan = GlossPlan::new("//");
    assert!(plan.is_empty());
    assert_eq!(plan.restore(&[]), "");
    assert_eq!(CommentShape::parse("//").units(), Vec::<&str>::new());
}
