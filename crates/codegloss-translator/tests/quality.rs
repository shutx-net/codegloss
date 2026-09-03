//! What the real model does to a comment: does the pre- and post-processing
//! still hold, and what does the Japanese look like. What it costs in time and
//! memory is `examples/measure.rs`, which loads exactly one model.
//!
//! These fixtures were fixed against an engine that returns its input, precisely
//! so that a failure here can be attributed. If a fixture that passes in
//! `codegloss-core` fails here, the model is at fault; if it fails in both,
//! this crate is not the place to look.
//!
//! All of it is `#[ignore]`d and needs `CODEGLOSS_MODEL_PACK`. Set
//! `CODEGLOSS_MODEL_PRECISION=f16` to hold the same bar up against F16. See
//! `support/mod.rs`.

mod support;

use codegloss_core::{CommentRules, CommentShape, GlossPlan, Segment, mask};
use codegloss_translator::Translator;

/// The three pinned blocks, included from `codegloss-core` itself rather
/// than copied: a fixture that drifts from the one it is meant to mirror
/// proves nothing.
const JAVADOC: &str = include_str!("../../codegloss-core/tests/fixtures/javadoc.txt");
const RUSTDOC: &str = include_str!("../../codegloss-core/tests/fixtures/rustdoc.txt");
const LINE_COMMENTS: &str = include_str!("../../codegloss-core/tests/fixtures/line_comments.txt");

/// A comment of the corpus.
struct Fixture {
    language: String,
    raw: String,
}

fn corpus() -> Vec<Fixture> {
    include_str!("fixtures/comments.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("every line of the corpus is JSON");
            Fixture {
                language: value["language"].as_str().unwrap_or("?").to_owned(),
                raw: value["raw"]
                    .as_str()
                    .expect("every entry has a raw comment")
                    .to_owned(),
            }
        })
        .collect()
}

/// Every span the pre-processing hid from the engine, as it is written in the
/// comment. Each of them has to be in the gloss, spelled the same way.
fn protected(raw: &str) -> Vec<String> {
    CommentShape::parse(raw, CommentRules::Fenced)
        .units()
        .into_iter()
        .flat_map(|unit| {
            mask(unit)
                .preserved()
                .iter()
                .map(|preserved| preserved.text().to_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The whole pipeline for one comment, exactly as `codegloss-lsp` runs it:
/// mask, translate, unmask, rebuild.
fn gloss(translator: &dyn Translator, raw: &str) -> String {
    let plan = GlossPlan::new(raw, CommentRules::Fenced);
    let translations = translator
        .translate(&plan.segments())
        .expect("the engine answers");
    plan.restore(&translations)
}

/// The engine translates at all.
///
/// Worth its own test because the failure it catches is invisible in every
/// other one: a unit whose placeholders came back wrong falls back to the
/// English source, and a comment that was never translated then looks exactly
/// like a comment whose identifiers were preserved perfectly.
#[test]
#[ignore = "needs a model pack"]
fn plain_prose_comes_back_in_japanese() {
    let translator = support::translator();
    let english = [
        "Returns the currently authenticated user.",
        "Creates a new cache with the given capacity.",
        "Blocks until the worker has finished the current batch.",
    ];
    let segments: Vec<Segment> = english.iter().map(|text| Segment::new(*text)).collect();

    let translated = translator.translate(&segments).expect("the engine answers");
    for (english, japanese) in english.iter().zip(&translated) {
        eprintln!("{english}\n  -> {japanese}");
        assert_ne!(english, japanese, "nothing was translated");
        assert!(
            japanese.chars().any(is_japanese),
            "{japanese:?} contains no Japanese"
        );
    }
}

/// Hiragana, katakana or a CJK ideograph.
fn is_japanese(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}' | '\u{ff66}'..='\u{ff9d}')
}

/// Issue #1's example, the one the whole preservation design exists for.
#[test]
#[ignore = "needs a model pack"]
fn the_identifier_of_issue_1_survives_the_real_model() {
    let translator = support::translator();
    let raw = "/// Returns `UserDetails` when authentication succeeds.";
    let gloss = gloss(&translator, raw);

    eprintln!("{raw}\n  -> {gloss}");
    assert!(
        gloss.contains("`UserDetails`"),
        "the identifier was lost: {gloss:?}"
    );
}

/// Issue #31's example: the clause after the comma survives the split, and does
/// not survive without it.
///
/// The failure this is about is invisible - the Japanese for the undivided
/// sentence is perfectly fluent and simply does not say what the second half
/// of the English says - so what is asserted is length against the same
/// engine's answer for the same sentence handed over whole. Not an exact
/// string: the model is not pinned, and a better one would have to keep
/// passing this.
///
/// `tests/decoding.rs::beam_search_keeps_a_clause_that_greedy_drops` looks like
/// this test and is not: it translates the sentence **unmasked**, and an
/// identifier hidden behind a placeholder makes the truncation more likely, not
/// less (`docs/model-runtime-notes.md` §7.6). What the pipeline sends is the
/// masked form, which is what goes through `GlossPlan` here.
#[test]
#[ignore = "needs a model pack"]
fn the_clause_after_a_comma_survives_the_split() {
    let translator = support::translator();
    let raw = "/// Dropping it closes the socket and wakes every task blocked on accept, \
               which is why the shutdown is not graceful.";

    let plan = GlossPlan::new(raw, CommentRules::Fenced);
    let segments = plan.segments();
    assert_eq!(
        segments.len(),
        2,
        "the comma was not a boundary: {segments:?}"
    );
    assert!(
        segments[0].text().ends_with('.'),
        "the engine was given an unfinished sentence: {:?}",
        segments[0].text()
    );

    // What the pipeline sent before the split: the same unit, masked, whole.
    let whole = mask(CommentShape::parse(raw, CommentRules::Fenced).units()[0])
        .masked()
        .to_owned();
    let undivided = translator
        .translate(&[Segment::new(whole)])
        .expect("the engine answers")
        .remove(0);
    let split = gloss(&translator, raw);

    eprintln!("undivided -> {undivided}\nsplit     -> {split}");
    assert!(
        split.chars().count() > undivided.chars().count(),
        "the split gloss is no longer than the undivided one, so the clause is \
         still missing:\nundivided: {undivided}\nsplit:     {split}"
    );
}

/// The pre- and post-processing fixtures, re-run against candle.
#[test]
#[ignore = "needs a model pack"]
fn the_p6_fixtures_keep_their_structure_and_their_protected_spans() {
    let translator = support::translator();
    let mut failures = Vec::new();

    for (name, raw) in [
        ("javadoc", JAVADOC),
        ("rustdoc", RUSTDOC),
        ("line_comments", LINE_COMMENTS),
    ] {
        let raw = raw.trim_end_matches('\n');
        let plan = GlossPlan::new(raw, CommentRules::Fenced);
        let gloss = gloss(&translator, raw);
        eprintln!("=== {name}\n{gloss}\n");

        // The source line count is the structure the pipeline promises to rebuild.
        let expected_lines = plan.source().lines().count();
        let actual_lines = gloss.lines().count();
        if expected_lines != actual_lines {
            failures.push(format!(
                "{name}: {expected_lines} lines went in and {actual_lines} came out"
            ));
        }

        for span in protected(raw) {
            if !gloss.contains(&span) {
                failures.push(format!("{name}: {span:?} is not in the gloss"));
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every comment of the corpus, checked for the one property the model cannot
/// be allowed to break: a protected span comes back spelled as it went in.
#[test]
#[ignore = "needs a model pack"]
fn the_corpus_keeps_every_protected_span() {
    let translator = support::translator();
    let mut failures = Vec::new();
    let mut glossed = 0usize;

    for fixture in corpus() {
        let gloss = gloss(&translator, &fixture.raw);
        glossed += 1;
        eprintln!("[{}] {}\n  -> {}\n", fixture.language, fixture.raw, gloss);

        for span in protected(&fixture.raw) {
            if !gloss.contains(&span) {
                failures.push(format!(
                    "[{}] {span:?} is not in {gloss:?}",
                    fixture.language
                ));
            }
        }
    }

    eprintln!("{glossed} comments, {} failures", failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
