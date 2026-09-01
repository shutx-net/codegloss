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

/// The cache must not carry anything from one segment into the next.
///
/// `Engine::translate` resets it per segment and the search reorders it per
/// step, and a mistake in either shows up here and nowhere else: a translation
/// contaminated by its predecessor is still fluent Japanese.
#[test]
#[ignore = "needs a model pack: CODEGLOSS_MODEL_PACK=<dir> ... -- --ignored"]
fn a_segment_translates_the_same_whoever_came_before_it() {
    let translator = support::translator();
    let first = Segment::new("The worker owns the only handle to the model.");
    let second = Segment::new("Returns the currently authenticated user.");

    let together = translator
        .translate(&[first.clone(), second.clone()])
        .unwrap();
    let alone = translator.translate(std::slice::from_ref(&second)).unwrap();
    let reversed = translator
        .translate(&[second.clone(), first.clone()])
        .unwrap();

    assert_eq!(
        together[1], alone[0],
        "the batch changed the second segment"
    );
    assert_eq!(
        reversed[0], alone[0],
        "the order of the batch changed a segment"
    );
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
