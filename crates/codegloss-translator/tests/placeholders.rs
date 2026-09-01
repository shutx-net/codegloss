//! Which placeholder format survives FuguMT.
//!
//! The format had to be picked before there was a model to ask, and the bracket form
//! `\u{27e6}0\u{27e7}` and left a note saying the choice was provisional. This
//! is the measurement that note asks for: candidate formats, the same
//! sentences, and a count of how many placeholders come back out of the model
//! intact. It is what settled on `X0Q`; the table is in
//! `docs/model-runtime-notes.md`.
//!
//! A placeholder fails in one of four ways, and they are counted separately
//! because they call for different answers:
//!
//! - **unknown**: the tokenizer maps part of it to `<unk>`, so the model never
//!   sees it at all
//! - **lost**: it is not in the translation
//! - **duplicated**: it is in the translation more than once, so restoring it
//!   would duplicate an identifier
//!
//! `#[ignore]`d and needs `CODEGLOSS_MODEL_PACK`. See `support/mod.rs`.

mod support;

use codegloss_core::{Segment, placeholder};
use codegloss_translator::Translator;
use tokenizers::Tokenizer;

/// A candidate format. `{}` stands for the index.
///
/// The first two are the ones the open question named: the bracket form P6
/// chose provisionally and the underscore form offered as its alternative. The
/// rest are here to tell *why* a format fails - whether it is being outside the
/// vocabulary or being made of punctuation the model rewrites at will - and to
/// keep the winner honest by measuring it against near neighbours rather than
/// against straw men.
const CANDIDATES: [(&str, &str); 8] = [
    ("brackets", "\u{27e6}{}\u{27e7}"),
    ("underscore", "__CG{}__"),
    ("square", "[{}]"),
    ("bare-tag", "CG{}"),
    ("q", "Q{}"),
    ("q-x", "Q{}X"),
    ("x-q", "X{}Q"),
    ("q-under", "Q{}_"),
];

/// Sentences with slots, in the shape the pre-processing produces them: the
/// prose is what the model translates and the slots are what it must carry
/// through untouched. `{0}` / `{1}` mark the slots.
const SENTENCES: [&str; 32] = [
    "Returns {0} when authentication succeeds.",
    "Calls {0} before {1}.",
    "{0} return the cached user when {1} hits.",
    "See {0} for the protocol.",
    "{0} the id to look up",
    "{0} authenticated user",
    "Panics when {0} is called before {1}.",
    "Creates a new {0} with the given capacity.",
    "The default value of {0} is {1}.",
    "{0} drop the cache before the next run.",
    "Reads the whole file into memory. Prefer {0} for large files.",
    "Removes {0} and returns {1}, if it was present.",
    "{0} must not be null.",
    "Blocks until {0} has finished the current batch.",
    "The result of {0}.",
    "Sends the request and waits up to {0} for the answer.",
    "An iterator over the keys of {0}, in insertion order.",
    "This is a blocking call and must not run inside {0}.",
    "Returns an error when {0} does not exist.",
    "Splits the input on whitespace and drops empty fields; see {0}.",
    "{0} is called once per request, so keep it cheap.",
    "Registers a listener that {0} notifies on every change.",
    "Compares {0} with {1} and returns a negative number when it is smaller.",
    "Guarded by the monitor of {0}.",
    "Only {0} may call this.",
    "Retries {0} three times before giving up.",
    "Skips files that {0} cannot read instead of failing the run.",
    "Builds {0} lazily: most callers never need it.",
    "{0} releases the resources held by {1}.",
    "It is safe to call {0} from multiple threads.",
    "Formats {0} as a human readable string.",
    "Deliberately not exported: {0} is an implementation detail.",
];

/// How one candidate did.
#[derive(Default)]
struct Tally {
    slots: usize,
    unknown: usize,
    lost: usize,
    duplicated: usize,
    examples: Vec<String>,
}

impl Tally {
    fn broken(&self) -> usize {
        self.unknown + self.lost + self.duplicated
    }

    fn rate(&self) -> f64 {
        if self.slots == 0 {
            return 0.0;
        }
        (self.slots - self.broken()) as f64 / self.slots as f64 * 100.0
    }
}

fn render(template: &str, index: usize) -> String {
    template.replace("{}", &index.to_string())
}

/// The sentence with its slots filled in with `template`'s placeholders, and
/// how many slots it had.
fn fill(sentence: &str, template: &str) -> (String, usize) {
    let mut filled = sentence.to_owned();
    let mut slots = 0;
    for index in 0..4 {
        let slot = format!("{{{index}}}");
        if filled.contains(&slot) {
            filled = filled.replace(&slot, &render(template, index));
            slots += 1;
        }
    }
    (filled, slots)
}

#[test]
#[ignore = "needs a model pack"]
fn the_placeholder_format_is_the_one_that_survives() {
    let translator = support::translator();
    let mut results: Vec<(&str, &str, Tally)> = Vec::new();

    for (name, template) in CANDIDATES {
        let mut tally = Tally::default();

        for sentence in SENTENCES {
            let (filled, slots) = fill(sentence, template);
            tally.slots += slots;

            let translated = translator
                .translate(&[Segment::new(&filled)])
                .expect("the engine answers")
                .remove(0);

            for index in 0..slots {
                let wanted = render(template, index);
                match translated.matches(&wanted).count() {
                    1 => {}
                    0 => {
                        // `<unk>` in the output means the tokenizer could not
                        // represent the placeholder, which is a different
                        // problem from the model dropping it.
                        if translated.contains("<unk>") || translated.contains('⁇') {
                            tally.unknown += 1;
                        } else {
                            tally.lost += 1;
                        }
                        if tally.examples.len() < 3 {
                            tally
                                .examples
                                .push(format!("{filled:?}\n      -> {translated:?}"));
                        }
                    }
                    count => {
                        tally.duplicated += 1;
                        if tally.examples.len() < 3 {
                            tally
                                .examples
                                .push(format!("{wanted} appears {count} times: {translated:?}"));
                        }
                    }
                }
            }
        }

        eprintln!(
            "{name:<14} {:>6.1}%  ({} of {} slots broken: {} unknown, {} lost, {} duplicated)",
            tally.rate(),
            tally.broken(),
            tally.slots,
            tally.unknown,
            tally.lost,
            tally.duplicated
        );
        for example in &tally.examples {
            eprintln!("      {example}");
        }
        results.push((name, template, tally));
    }

    let best = results
        .iter()
        .max_by(|left, right| left.2.rate().total_cmp(&right.2.rate()))
        .expect("there is at least one candidate");
    eprintln!("\nbest: {} ({}) at {:.1}%", best.0, best.1, best.2.rate());

    // The format `codegloss-core` writes has to be one of the measured ones,
    // and it has to be the one that did best. Without this the measurement
    // would be a report nobody acts on.
    let chosen = results
        .iter()
        .find(|(_, template, _)| render(template, 7) == placeholder(7))
        .unwrap_or_else(|| {
            panic!(
                "codegloss-core writes {:?}, which is not among the candidates measured here",
                placeholder(7)
            )
        });

    assert_eq!(
        chosen.2.rate(),
        best.2.rate(),
        "{} ({}) scores {:.1}% while {} ({}) scores {:.1}%",
        chosen.0,
        chosen.1,
        chosen.2.rate(),
        best.0,
        best.1,
        best.2.rate()
    );

    // Not 100%: one sentence of the set is one the model shortens, dropping
    // the clause the placeholder sits in along with it. No format survives
    // that, and a run where several do would mean the sentences drifted rather
    // than that the format improved.
    assert!(
        chosen.2.rate() >= 95.0,
        "{} ({}) only scores {:.1}%",
        chosen.0,
        chosen.1,
        chosen.2.rate()
    );
}

/// The tokenizer half of the same question, without the model.
///
/// A format that already breaks here cannot be fixed by anything downstream,
/// and the tokens it costs are paid on every comment that uses it.
#[test]
#[ignore = "needs a model pack"]
fn the_tokenizer_cost_of_each_candidate_is_measured() {
    let path = support::pack().join(codegloss_translator::SOURCE_TOKENIZER_FILE);
    let tokenizer = Tokenizer::from_file(&path)
        .unwrap_or_else(|error| panic!("{} is not a tokenizer: {error}", path.display()));
    let unknown = tokenizer.token_to_id("<unk>");

    for (name, template) in CANDIDATES {
        let mut tokens = 0usize;
        let mut unknowns = 0usize;
        let mut intact = 0usize;
        let mut slots = 0usize;
        let mut shown = false;

        for sentence in SENTENCES {
            let (filled, count) = fill(sentence, template);
            slots += count;

            let encoded = tokenizer
                .encode(filled.as_str(), true)
                .expect("the sentence tokenizes");
            tokens += encoded.get_ids().len();
            unknowns += encoded
                .get_ids()
                .iter()
                .filter(|id| Some(**id) == unknown)
                .count();

            let decoded = tokenizer
                .decode(encoded.get_ids(), true)
                .expect("the sentence decodes");
            for index in 0..count {
                if decoded.contains(&render(template, index)) {
                    intact += 1;
                }
            }

            if !shown {
                shown = true;
                eprintln!("{name:<14} {:?}", encoded.get_tokens());
            }
        }

        eprintln!(
            "{name:<14} {intact}/{slots} placeholders survive the tokenizer, \
             {tokens} tokens for {} sentences, {unknowns} unknown tokens",
            SENTENCES.len()
        );
    }
}
