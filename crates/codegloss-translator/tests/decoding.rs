//! What the search does, checked against the real model.
//!
//! These sit apart from `quality.rs` because they are about the decoder rather
//! than about the pipeline around it: the KV cache the fork in `src/marian.rs`
//! made reorderable, and the truncation beam search is there to stop.
//!
//! All `#[ignore]`d - see `tests/support/mod.rs`.

mod support;

use codegloss_core::Segment;
use codegloss_translator::Translator;

/// The failure the search exists for.
///
/// Greedy decoding takes the end-of-sentence token part way through this
/// sentence and drops the clause after the comma, and what it produces reads as
/// a complete Japanese sentence - so nothing downstream can tell. Beam search
/// keeps the clause because a length-normalised score stops rewarding the short
/// path.
#[test]
#[ignore = "needs a model pack: CODEGLOSS_MODEL_PACK=<dir> ... -- --ignored"]
fn beam_search_keeps_a_clause_that_greedy_drops() {
    let sentence = Segment::new(
        "Returns None once the queue is closed, which happens when the server is \
                      shutting down.",
    );

    let greedy = support::translator_with_beams(1);
    let greedy = &greedy.translate(std::slice::from_ref(&sentence)).unwrap()[0];
    let beams = support::translator_with_beams(4);
    let beams = &beams.translate(std::slice::from_ref(&sentence)).unwrap()[0];

    eprintln!("greedy: {greedy}\n beam 4: {beams}");
    assert!(
        beams.chars().count() > greedy.chars().count(),
        "the wider search should have kept more of the sentence:\n\
         greedy: {greedy}\nbeam 4: {beams}"
    );
}

/// A segment must translate the same whoever it travels with.
///
/// Two things could break it and neither would be visible in the result: a KV
/// cache carried from one segment into the next, and a padded batch whose pad
/// positions leak into the sentence beside them. The companion here is far
/// longer than the segment under test on purpose - that is what makes the
/// padding, and the padding is what the mask in `src/marian.rs` exists for.
#[test]
#[ignore = "needs a model pack: CODEGLOSS_MODEL_PACK=<dir> ... -- --ignored"]
fn a_segment_translates_the_same_whoever_came_before_it() {
    let translator = support::translator();
    let short = Segment::new("Returns the currently authenticated user.");
    let long = Segment::new(
        "The worker owns the only handle to the model, so a translation that arrives while \
         another is running waits for it rather than loading a second copy of the weights, \
         which is also why the queue is bounded rather than left to grow with the editor.",
    );

    let alone = translator.translate(std::slice::from_ref(&short)).unwrap();
    let after = translator
        .translate(&[long.clone(), short.clone()])
        .unwrap();
    let before = translator
        .translate(&[short.clone(), long.clone()])
        .unwrap();

    assert_eq!(
        after[1], alone[0],
        "padding it against a longer segment changed the translation"
    );
    assert_eq!(
        before[0], alone[0],
        "the order of the batch changed the translation"
    );

    // And the long one is not changed by the short one either, which is the
    // same property from the other side.
    let long_alone = translator.translate(std::slice::from_ref(&long)).unwrap();
    assert_eq!(before[1], long_alone[0]);
    assert_eq!(after[0], long_alone[0]);
}

/// The search is deterministic: it picks by score, never by chance, and the
/// cache and the fixtures are only worth anything if it stays that way.
#[test]
#[ignore = "needs a model pack: CODEGLOSS_MODEL_PACK=<dir> ... -- --ignored"]
fn the_same_input_gives_the_same_translation_every_time() {
    let translator = support::translator();
    let segment = Segment::new("Locks the map for the duration of the lookup.");

    let first = translator
        .translate(std::slice::from_ref(&segment))
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            translator
                .translate(std::slice::from_ref(&segment))
                .unwrap(),
            first
        );
    }
}
