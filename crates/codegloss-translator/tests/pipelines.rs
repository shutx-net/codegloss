//! Which masking policy produces the better gloss - measured rather than
//! argued.
//!
//! §7.1 of `docs/model-runtime-notes.md` records that masking costs meaning:
//! hiding `find_user` behind a placeholder also hides the noun that decides how
//! the rest of the sentence is translated. §7.4 then files the question of what
//! to do about it under "not fixed - there is nothing to decide it with".
//! Issue #32 asks for the thing to decide it with. This is that: the arms, the
//! corpus and the scoreboard, in the tree, so that the next person to ask
//! re-runs a command instead of rewriting a script.
//!
//! Four arms. Three are translated, the fourth is derived:
//!
//! | | what the engine is given | what comes back |
//! |---|---|---|
//! | A | every protected span hidden (**what the server ships**) | unmasked, per fragment |
//! | B | nothing hidden | as it stands - there is nothing to put back |
//! | C | code, URLs, tags and markers hidden; bare identifiers left in | unmasked, per fragment |
//! | D | - | B wherever every span came back verbatim, else A |
//!
//! Every arm is assembled from `codegloss-core`'s public API, in the pipeline's
//! own order: **mask, then split into sentences, then reveal the kinds this arm
//! reveals**. The order is not a detail. `split_sentences` is documented to run
//! on masked text because a full stop inside a URL or a `foo.bar()` call is a
//! sentence boundary to any rule simple enough to have; revealing first would
//! break the splitting and give back the improvement §7.2 measured for it.
//!
//! Nothing here is on the path the server takes: this measures the shipped
//! pipeline, it does not change it. Which is exactly why
//! [`arm_a_reproduces_the_shipped_pipeline`] exists - arm A is assembled out
//! here rather than being [`GlossPlan`], so the two have to be pinned together
//! or the scoreboard becomes a comparison between three things nobody runs.
//!
//! ```sh
//! CODEGLOSS_MODEL_PACK=~/codegloss-model \
//!   cargo test -p codegloss-translator --features candle --release \
//!   --test pipelines -- --ignored --nocapture
//! ```
//!
//! `CODEGLOSS_CORPUS=<file>` measures another corpus (`%%%` between blocks, as
//! `codegloss-parser`'s `extract` example writes it) and `CODEGLOSS_SHEET=<file>`
//! writes the blinded A/B sheet for the fragments no automatic metric can
//! separate. The corpus that ships here is the 62 blocks §7.2 and §9.3 were
//! measured on, frozen; where it came from, and how to build another, is §12.

mod support;

use std::fmt::Write as _;
use std::sync::OnceLock;

use codegloss_core::{
    CommentShape, GlossPlan, Masked, Segment, SpanKind, join_sentences, mask, placeholder,
    split_sentences,
};
use codegloss_translator::{PassthroughTranslator, Translator};

/// The frozen corpus: comments of this repository, `%%%` between blocks.
const CORPUS: &str = include_str!("fixtures/comment-corpus.txt");

/// Environment variable naming a corpus to measure instead of the frozen one.
const CORPUS_VARIABLE: &str = "CODEGLOSS_CORPUS";

/// Environment variable naming the file to write the blinded A/B sheet to.
const SHEET_VARIABLE: &str = "CODEGLOSS_SHEET";

/// Terms §7.1 records the model getting wrong, with the general-language word
/// it reaches for instead.
///
/// A probe, not a verdict: the match is a case-insensitive substring on both
/// sides. That is also why the `stray` count is printed beside the `hit` count.
/// `キュー` is inside `エグゼキュータ` and `値` is inside `価値`, so a
/// post-processing dictionary that fixed terminology by replacing Japanese
/// substrings would corrupt sentences that never contained the English word at
/// all. `stray` is how often this corpus offers it the chance.
const TERMS: [(&str, &str); 5] = [
    ("gloss", "光沢"),
    ("store", "店"),
    ("clock", "時計"),
    ("queue", "キュー"),
    ("value", "値"),
];

/// What one arm hides from the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    /// Every span the rules find. What `codegloss-lsp` ships.
    Everything,
    /// Nothing at all: the engine reads the comment as it was written.
    Nothing,
    /// Everything except bare identifiers - the arm that asks whether
    /// `find_user` is worth more to the engine as context than it costs as a
    /// thing that can come back wrong.
    AllButIdentifiers,
}

impl Policy {
    fn hides(self, kind: SpanKind) -> bool {
        match self {
            Self::Everything => true,
            Self::Nothing => false,
            Self::AllButIdentifiers => kind != SpanKind::Identifier,
        }
    }

    /// Whether the answer goes back through [`Masked::unmask_fragment`].
    ///
    /// [`Policy::Nothing`] hid nothing, so there is nothing to put back and no
    /// placeholder whose absence could send a fragment back to English.
    /// Substituting anyway would be a second unmasking of text that was never
    /// masked.
    fn restores(self) -> bool {
        self != Self::Nothing
    }

    fn name(self) -> &'static str {
        match self {
            Self::Everything => "A hide everything",
            Self::Nothing => "B hide nothing",
            Self::AllButIdentifiers => "C keep identifiers",
        }
    }
}

/// The three translated arms, in the order they are reported.
const ARMS: [Policy; 3] = [
    Policy::Everything,
    Policy::Nothing,
    Policy::AllButIdentifiers,
];

/// Name of the derived arm.
const VERIFIED: &str = "D verify, else A";

/// One sentence of one unit of one block: the smallest thing an arm translates,
/// and the unit every count in the scoreboard is per.
struct Fragment {
    /// Arm A's input - the masked sentence. It is the same in every arm, so it
    /// is what identifies a fragment when the arms are lined up beside it.
    sentence: String,
    /// The same fragment with every placeholder put back: its English.
    english: String,
    /// Indices into the unit's masking table of the spans this fragment
    /// carries. A unit's table covers the whole unit; a fragment answers only
    /// for what it was given.
    spans: Vec<usize>,
}

/// One unit of a block: its masking table and its sentences.
struct Unit {
    masked: Masked,
    fragments: Vec<Fragment>,
}

/// One comment block, prepared once and shared by every arm.
struct Block {
    raw: String,
    shape: CommentShape,
    units: Vec<Unit>,
}

impl Block {
    /// The same preparation [`GlossPlan::new`] does, kept out here so that a
    /// policy can be applied between the masking and the engine.
    fn prepare(raw: &str) -> Self {
        let shape = CommentShape::parse(raw);
        let units = shape
            .units()
            .into_iter()
            .map(|text| {
                let masked = mask(text);
                let fragments = split_sentences(masked.masked())
                    .into_iter()
                    .map(|sentence| Fragment {
                        spans: carried(&masked, sentence),
                        english: masked.unmask_fragment(sentence, sentence),
                        sentence: sentence.to_owned(),
                    })
                    .collect();
                Unit { masked, fragments }
            })
            .collect();

        Self {
            raw: raw.to_owned(),
            shape,
            units,
        }
    }
}

/// The corpus, prepared.
struct Corpus {
    blocks: Vec<Block>,
}

impl Corpus {
    fn load() -> Self {
        let text = match std::env::var(CORPUS_VARIABLE) {
            Ok(path) if !path.is_empty() => std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{CORPUS_VARIABLE}={path}: {error}")),
            _ => CORPUS.to_owned(),
        };

        Self {
            blocks: text
                .split("\n%%%\n")
                .map(|block| block.trim_end_matches('\n'))
                .filter(|block| !block.trim().is_empty())
                .map(Block::prepare)
                .collect(),
        }
    }

    fn of(raw: &str) -> Self {
        Self {
            blocks: vec![Block::prepare(raw)],
        }
    }

    /// Every fragment of the corpus, flattened, beside the unit it belongs to.
    fn fragments(&self) -> impl Iterator<Item = (&Unit, &Fragment)> {
        self.blocks
            .iter()
            .flat_map(|block| block.units.iter())
            .flat_map(|unit| unit.fragments.iter().map(move |fragment| (unit, fragment)))
    }

    /// What `policy` hands the engine, one entry per fragment.
    fn inputs(&self, policy: Policy) -> Vec<String> {
        self.fragments()
            .map(|(unit, fragment)| reveal(&unit.masked, &fragment.sentence, policy))
            .collect()
    }

    /// The finished gloss of each fragment, given what the engine answered.
    fn restore(&self, policy: Policy, inputs: &[String], answers: &[String]) -> Vec<String> {
        self.fragments()
            .zip(inputs)
            .zip(answers)
            .map(|(((unit, _), input), answer)| {
                if policy.restores() {
                    unit.masked.unmask_fragment(input, answer)
                } else {
                    answer.clone()
                }
            })
            .collect()
    }

    /// The finished gloss of each *block*: fragments joined into units, units
    /// poured back into the comment's own structure.
    ///
    /// The two steps [`GlossPlan::restore`] ends with, which is what makes the
    /// drift test below a comparison of like with like.
    fn glosses(&self, fragments: &[String]) -> Vec<String> {
        let mut rest = fragments;
        self.blocks
            .iter()
            .map(|block| {
                let units: Vec<String> = block
                    .units
                    .iter()
                    .map(|unit| {
                        let (mine, tail) = rest.split_at(unit.fragments.len());
                        rest = tail;
                        join_sentences(mine)
                    })
                    .collect();
                block.shape.rebuild(&units)
            })
            .collect()
    }
}

/// Indices of the spans whose placeholders appear in `sentence`.
fn carried(masked: &Masked, sentence: &str) -> Vec<usize> {
    (0..masked.preserved().len())
        .filter(|index| sentence.contains(&placeholder(*index)))
        .collect()
}

/// `sentence` with the placeholders `policy` does not hide replaced by the
/// spans they stand for.
///
/// One left-to-right pass, never a `replace` per index: a span that is put back
/// is text like any other, and a second pass would substitute into what the
/// first one just wrote.
fn reveal(masked: &Masked, sentence: &str, policy: Policy) -> String {
    let markers: Vec<(String, &str)> = masked
        .preserved()
        .iter()
        .enumerate()
        .filter(|(_, span)| !policy.hides(span.kind()))
        .map(|(index, span)| (placeholder(index), span.text()))
        .collect();

    let mut revealed = String::with_capacity(sentence.len());
    let mut rest = sentence;
    loop {
        let next = markers
            .iter()
            .filter_map(|(marker, text)| rest.find(marker.as_str()).map(|at| (at, marker, *text)))
            .min_by_key(|(at, marker, _)| (*at, std::cmp::Reverse(marker.len())));

        match next {
            Some((at, marker, text)) => {
                revealed.push_str(&rest[..at]);
                revealed.push_str(text);
                rest = &rest[at + marker.len()..];
            }
            None => {
                revealed.push_str(rest);
                return revealed;
            }
        }
    }
}

/// Whether every placeholder this fragment still carries under `policy` came
/// back in `answer`.
///
/// The rule [`Masked::unmask_fragment`] applies when it chooses between the
/// translation and the English, spelled out so that the scoreboard counts the
/// decision instead of inferring it from the text - a gloss that happens to
/// equal its English would otherwise be counted as a fallback.
fn placeholders_survived(unit: &Unit, fragment: &Fragment, policy: Policy, answer: &str) -> bool {
    fragment
        .spans
        .iter()
        .filter(|index| policy.hides(unit.masked.preserved()[**index].kind()))
        .all(|index| answer.contains(&placeholder(*index)))
}

/// Hiragana, katakana or a CJK ideograph.
fn is_japanese(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{30ff}' | '\u{4e00}'..='\u{9fff}' | '\u{ff66}'..='\u{ff9d}')
}

/// `text` with all whitespace taken out.
///
/// Two arms that differ only in where the engine put a space around a restored
/// identifier have not made a different translation, and counting them as one
/// would put a page of noise in front of whoever reads the A/B sheet.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect()
}

/// A count per rule, in the order [`SpanKind::ALL`] lists them.
#[derive(Default, Clone, Copy)]
struct Counts([usize; SpanKind::ALL.len()]);

impl Counts {
    fn add(&mut self, kind: SpanKind) {
        let slot = SpanKind::ALL
            .iter()
            .position(|candidate| *candidate == kind)
            .expect("SpanKind::ALL lists every kind");
        self.0[slot] += 1;
    }

    fn total(self) -> usize {
        self.0.iter().sum()
    }
}

/// How one arm did.
#[derive(Default)]
struct Score {
    japanese: usize,
    /// Fragments whose answer was thrown away because a placeholder did not
    /// come back - the ones a reader sees in English.
    fell_back: usize,
    /// Spans the arm was asked to carry, by rule.
    carried: Counts,
    /// Of those, the ones not spelled the same way in the finished gloss.
    lost: Counts,
    /// Fragments whose gloss differs from arm A's.
    differ: usize,
    /// Of those, the ones that differ by more than whitespace.
    differ_really: usize,
}

/// Scores one arm's finished fragment glosses against arm A's.
///
/// `policy` and `answers` are the ones the glosses were produced with, so that
/// the fallback count means what it says; for the derived arm they are arm A's,
/// which is the arm its fallbacks come from.
fn score(
    corpus: &Corpus,
    policy: Policy,
    answers: &[String],
    glosses: &[String],
    baseline: &[String],
) -> Score {
    let mut score = Score::default();

    for ((((unit, fragment), answer), gloss), base) in
        corpus.fragments().zip(answers).zip(glosses).zip(baseline)
    {
        score.japanese += gloss
            .chars()
            .filter(|character| is_japanese(*character))
            .count();

        if policy.restores() && !placeholders_survived(unit, fragment, policy, answer) {
            score.fell_back += 1;
        }

        for index in &fragment.spans {
            let span = &unit.masked.preserved()[*index];
            score.carried.add(span.kind());
            if !gloss.contains(span.text()) {
                score.lost.add(span.kind());
            }
        }

        if gloss != base {
            score.differ += 1;
            if squeezed(gloss) != squeezed(base) {
                score.differ_really += 1;
            }
        }
    }

    score
}

/// One translated arm.
struct Run {
    policy: Policy,
    answers: Vec<String>,
    glosses: Vec<String>,
}

/// Everything the model-backed tests share, computed once.
///
/// One process, one model, every arm - `docs/model-runtime-notes.md` §6.1. Two
/// `#[ignore]`d tests in one binary run in parallel, and a translator per test
/// would mean two sets of weights resident and every segment translated twice.
struct Measurement {
    corpus: Corpus,
    runs: Vec<Run>,
    /// Arm D: arm B's gloss wherever every span came back verbatim, arm A's
    /// everywhere else.
    verified: Vec<String>,
    /// Fragments arm D took from arm B.
    taken: usize,
}

fn measurement() -> &'static Measurement {
    static MEASUREMENT: OnceLock<Measurement> = OnceLock::new();
    MEASUREMENT.get_or_init(|| {
        let translator = support::translator();
        let corpus = Corpus::load();

        let runs: Vec<Run> = ARMS
            .into_iter()
            .map(|policy| {
                let inputs = corpus.inputs(policy);
                let segments: Vec<Segment> = inputs.iter().map(Segment::new).collect();
                let started = std::time::Instant::now();
                let answers = translator
                    .translate(&segments)
                    .expect("the engine answers the whole corpus");
                eprintln!(
                    "{}: {} fragments in {:?}",
                    policy.name(),
                    segments.len(),
                    started.elapsed()
                );
                let glosses = corpus.restore(policy, &inputs, &answers);
                Run {
                    policy,
                    answers,
                    glosses,
                }
            })
            .collect();

        let mut taken = 0;
        let verified = corpus
            .fragments()
            .enumerate()
            .map(|(index, (unit, fragment))| {
                if intact(unit, fragment, &runs[1].glosses[index]) {
                    taken += 1;
                    runs[1].glosses[index].clone()
                } else {
                    runs[0].glosses[index].clone()
                }
            })
            .collect();

        Measurement {
            corpus,
            runs,
            verified,
            taken,
        }
    })
}

/// Whether every span the fragment carries is spelled the same way in `gloss`.
///
/// Arm D's whole rule. Note what it does not check: *where* the span ended up.
/// A translation that moved an identifier into the wrong clause passes this,
/// and no metric here would catch it (§12, open questions).
fn intact(unit: &Unit, fragment: &Fragment, gloss: &str) -> bool {
    fragment
        .spans
        .iter()
        .all(|index| gloss.contains(unit.masked.preserved()[*index].text()))
}

/// Arm A is the shipped pipeline, byte for byte.
///
/// The arms are assembled out here rather than inside [`GlossPlan`] so that
/// measuring a policy costs no change to the code that ships one. The price is
/// drift: the day `GlossPlan` cuts a paragraph differently, arm A quietly stops
/// being what the server does and the scoreboard starts comparing three things
/// nobody runs. This is the whole guard against that, and it needs no model, so
/// it runs wherever the rest of the suite does.
#[test]
fn arm_a_reproduces_the_shipped_pipeline() {
    let corpus = Corpus::load();
    assert!(!corpus.blocks.is_empty(), "the corpus is empty");

    let inputs = corpus.inputs(Policy::Everything);
    let segments: Vec<Segment> = inputs.iter().map(Segment::new).collect();
    let answers = PassthroughTranslator
        .translate(&segments)
        .expect("the passthrough engine answers");
    let glosses = corpus.glosses(&corpus.restore(Policy::Everything, &inputs, &answers));

    let mut offset = 0;
    for (block, gloss) in corpus.blocks.iter().zip(&glosses) {
        let plan = GlossPlan::new(&block.raw);
        let shipped: Vec<String> = plan
            .segments()
            .iter()
            .map(|segment| segment.text().to_owned())
            .collect();

        assert!(
            offset + shipped.len() <= inputs.len(),
            "GlossPlan cut {:?} into more segments than arm A did",
            block.raw
        );
        assert_eq!(
            inputs[offset..offset + shipped.len()],
            shipped[..],
            "arm A cuts {:?} into different segments than GlossPlan does",
            block.raw
        );
        assert_eq!(
            *gloss,
            plan.restore(&shipped),
            "arm A rebuilds {:?} differently than GlossPlan does",
            block.raw
        );
        offset += shipped.len();
    }

    assert_eq!(
        offset,
        inputs.len(),
        "arm A produced segments GlossPlan did not"
    );
    eprintln!(
        "{} blocks, {} fragments, arm A identical to GlossPlan",
        corpus.blocks.len(),
        inputs.len()
    );
}

/// Each policy reveals exactly what it names, and nothing else.
///
/// The control the whole comparison rests on. If an arm's input differed
/// somewhere its policy does not reach, the differences the scoreboard reports
/// could not be attributed to the policy.
#[test]
fn an_arm_reveals_exactly_what_its_policy_names() {
    let corpus = Corpus::load();
    let masked = corpus.inputs(Policy::Everything);
    let bare = corpus.inputs(Policy::Nothing);
    let kept = corpus.inputs(Policy::AllButIdentifiers);

    let mut with_a_span = 0;
    let mut with_an_identifier = 0;

    for ((((unit, fragment), masked), bare), kept) in
        corpus.fragments().zip(&masked).zip(&bare).zip(&kept)
    {
        assert_eq!(*masked, fragment.sentence, "arm A is the masked sentence");

        let kinds: Vec<SpanKind> = fragment
            .spans
            .iter()
            .map(|index| unit.masked.preserved()[*index].kind())
            .collect();

        if kinds.is_empty() {
            assert_eq!(
                *bare, fragment.sentence,
                "a fragment with nothing to hide was changed"
            );
        } else {
            with_a_span += 1;
            assert_eq!(*bare, fragment.english, "the bare arm is the English");
        }

        if kinds.contains(&SpanKind::Identifier) {
            with_an_identifier += 1;
            assert_ne!(*kept, fragment.sentence, "an identifier was not revealed");
        } else {
            assert_eq!(
                *kept, fragment.sentence,
                "arm C revealed something that is not an identifier"
            );
        }
    }

    eprintln!(
        "{} fragments, {with_a_span} carry a protected span, \
         {with_an_identifier} carry a bare identifier",
        masked.len()
    );
    assert!(
        with_an_identifier > 0,
        "this corpus cannot answer the identifier question"
    );
}

/// Arm D's rule, without a model: a revealed span that did not come back takes
/// its own fragment back to the masked arm, and only its own.
///
/// The rule is the whole of arm D's claim - "the unmasked translation wherever
/// it can be checked" - so it is worth a test that does not depend on what a
/// model happens to answer. The engine here translates by deleting, which is
/// the failure the check exists for.
#[test]
fn a_span_that_did_not_come_back_falls_its_own_fragment_back() {
    let corpus = Corpus::of("/// Calls find_user first. Returns UserDetails on success.");

    assert_eq!(
        corpus.inputs(Policy::Nothing),
        [
            "Calls find_user first.".to_owned(),
            "Returns UserDetails on success.".to_owned(),
        ],
        "the identifiers have to reach the engine for this to test anything"
    );

    // Kept in the first answer, eaten in the second.
    let bare = [
        "最初に find_user を呼び出します。".to_owned(),
        "成功時に返します。".to_owned(),
    ];
    let masked = ["A の訳 1".to_owned(), "A の訳 2".to_owned()];

    let verified: Vec<String> = corpus
        .fragments()
        .enumerate()
        .map(|(index, (unit, fragment))| {
            if intact(unit, fragment, &bare[index]) {
                bare[index].clone()
            } else {
                masked[index].clone()
            }
        })
        .collect();

    assert_eq!(verified, [bare[0].clone(), masked[1].clone()]);
}

/// The scoreboard.
///
/// Prints, and asserts once. What is asserted is the axis that settles itself:
/// **the shipped arm must not lose more protected spans than any other**. A
/// span that did not come back is a mistranslated identifier in somebody's
/// editor, counted exactly, with no judgement in the counting. Everything else
/// here - how much Japanese, how many fragments differ, which terms went wrong -
/// is printed and not asserted, because none of it can say which gloss reads
/// better. That question goes to a human, on the sheet this writes.
#[test]
#[ignore = "needs a model pack"]
fn the_masking_policies_are_measured_against_each_other() {
    let measured = measurement();
    let corpus = &measured.corpus;
    let baseline = &measured.runs[0].glosses;

    let mut scores: Vec<(&str, Score)> = measured
        .runs
        .iter()
        .map(|run| {
            (
                run.policy.name(),
                score(corpus, run.policy, &run.answers, &run.glosses, baseline),
            )
        })
        .collect();
    // Arm D has no inputs of its own; it is scored with arm A's, which is the
    // arm its fallbacks come from.
    scores.push((
        VERIFIED,
        score(
            corpus,
            Policy::Everything,
            &measured.runs[0].answers,
            &measured.verified,
            baseline,
        ),
    ));

    let fragments = baseline.len();
    eprintln!("\n{} blocks, {fragments} fragments", corpus.blocks.len());

    eprintln!("\narm                  japanese   english   differ (not space)   spans lost");
    for (name, score) in &scores {
        eprintln!(
            "{name:<20}{:>10}{:>10}{:>12} ({:>3}){:>13} / {}",
            score.japanese,
            score.fell_back,
            score.differ,
            score.differ_really,
            score.lost.total(),
            score.carried.total(),
        );
    }

    eprintln!("\nspans lost, by rule (lost / carried)");
    eprint!("{:<20}", "arm");
    for kind in SpanKind::ALL {
        eprint!("{:>14}", format!("{kind:?}"));
    }
    eprintln!();
    for (name, score) in &scores {
        eprint!("{name:<20}");
        for slot in 0..SpanKind::ALL.len() {
            eprint!(
                "{:>14}",
                format!("{} / {}", score.lost.0[slot], score.carried.0[slot])
            );
        }
        eprintln!();
    }

    eprintln!(
        "\nD took the unmasked gloss for {} of {fragments} fragments",
        measured.taken
    );

    term_probe(measured);
    let pairs = sheet(measured);
    eprintln!(
        "\n{pairs} fragments differ by more than whitespace between A and B while both \
         arms carried everything they were given - those are what a human has to read"
    );

    // The one axis that settles itself. Arm A hides everything, so a span lost
    // there would mean the model rewrote text it never saw: a bug in the
    // masking rather than a property of a policy.
    let shipped = scores[0].1.lost.total();
    for (name, score) in &scores {
        assert!(
            shipped <= score.lost.total(),
            "the shipped arm loses {shipped} spans and {name} loses {}",
            score.lost.total()
        );
    }
}

/// A fragment carrying nothing to protect is the same in every arm.
///
/// Not a property of the arms - a property of the experiment. Such a fragment
/// is handed identical bytes by all three policies, so a difference in the
/// answer would mean the engine is not deterministic, and then no difference
/// this file reports could be attributed to anything.
#[test]
#[ignore = "needs a model pack"]
fn fragments_with_nothing_to_protect_are_identical_in_every_arm() {
    let measured = measurement();
    let mut compared = 0;
    let mut differences = Vec::new();

    for (index, (_, fragment)) in measured.corpus.fragments().enumerate() {
        if !fragment.spans.is_empty() {
            continue;
        }
        compared += 1;
        for run in &measured.runs[1..] {
            if run.glosses[index] != measured.runs[0].glosses[index] {
                differences.push(format!(
                    "{:?}\n  A  {:?}\n  {}  {:?}",
                    fragment.english,
                    measured.runs[0].glosses[index],
                    run.policy.name(),
                    run.glosses[index]
                ));
            }
        }
    }

    eprintln!("{compared} fragments carry nothing to protect");
    assert!(
        differences.is_empty(),
        "the engine answered the same input differently:\n{}",
        differences.join("\n")
    );
}

/// Prints, per arm, how often the model reached for a general-language word
/// where the corpus meant a software one.
fn term_probe(measured: &Measurement) {
    eprintln!(
        "\nterm probe: hit / at risk, +stray \
         (stray = the wrong word where the English never had the term)"
    );
    eprint!("{:<20}", "arm");
    for (english, japanese) in TERMS {
        eprint!("{:>18}", format!("{english}/{japanese}"));
    }
    eprintln!();

    let englishes: Vec<String> = measured
        .corpus
        .fragments()
        .map(|(_, fragment)| fragment.english.to_lowercase())
        .collect();

    let arms = measured
        .runs
        .iter()
        .map(|run| (run.policy.name(), &run.glosses))
        .chain(std::iter::once((VERIFIED, &measured.verified)));

    for (name, glosses) in arms {
        eprint!("{name:<20}");
        for (english, japanese) in TERMS {
            let (mut at_risk, mut hit, mut stray) = (0, 0, 0);
            for (source, gloss) in englishes.iter().zip(glosses) {
                match (source.contains(english), gloss.contains(japanese)) {
                    (true, true) => {
                        at_risk += 1;
                        hit += 1;
                    }
                    (true, false) => at_risk += 1,
                    (false, true) => stray += 1,
                    (false, false) => {}
                }
            }
            eprint!("{:>18}", format!("{hit}/{at_risk} +{stray}"));
        }
        eprintln!();
    }
}

/// Writes the fragments no automatic metric can separate, with the arm that
/// produced each rendering hidden.
///
/// Returns how many pairs there are. A pair qualifies when arm A and arm B
/// differ by more than whitespace **and** neither of them lost anything: where
/// one of them did, the automatic count has already decided, and asking a human
/// would only invite them to trade a correct identifier for a nicer sentence.
fn sheet(measured: &Measurement) -> usize {
    let corpus = &measured.corpus;
    let masked = &measured.runs[0];
    let bare = &measured.runs[1].glosses;

    let mut rendered = String::new();
    let mut key = String::new();
    let mut pairs = 0;

    for (index, (unit, fragment)) in corpus.fragments().enumerate() {
        if squeezed(&masked.glosses[index]) == squeezed(&bare[index]) {
            continue;
        }
        if !placeholders_survived(unit, fragment, masked.policy, &masked.answers[index]) {
            continue;
        }
        if !intact(unit, fragment, &bare[index]) {
            continue;
        }

        pairs += 1;
        let first_is_a = coin(&fragment.english);
        let (first, second) = if first_is_a {
            (&masked.glosses[index], &bare[index])
        } else {
            (&bare[index], &masked.glosses[index])
        };
        let _ = write!(
            rendered,
            "\n{pairs}. {}\n   1) {first}\n   2) {second}\n   better: [ 1 / 2 / 同じ ]\n",
            fragment.english
        );
        let _ = writeln!(key, "{pairs}. 1 = {}", if first_is_a { "A" } else { "B" });
    }

    let sheet = format!(
        "# masking A/B sheet\n\n{pairs} pairs.\n{rendered}\n\n\
         # key - do not read until the column above is filled in\n\n{key}"
    );

    match std::env::var(SHEET_VARIABLE) {
        Ok(path) if !path.is_empty() => match std::fs::write(&path, &sheet) {
            Ok(()) => eprintln!("\nwrote the A/B sheet to {path}"),
            Err(error) => eprintln!("\n{path} could not be written: {error}"),
        },
        _ => eprintln!("\n{sheet}"),
    }
    pairs
}

/// Which of the two renderings is shown first: stable for a given fragment, so
/// that a re-run produces the same sheet, and unrelated to which arm produced
/// it, so that the reader cannot learn the pattern.
fn coin(text: &str) -> bool {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash & 1 == 1
}
