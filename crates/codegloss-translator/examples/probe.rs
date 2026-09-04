//! Where a gloss goes wrong: what the engine is actually asked, and what it
//! answers.
//!
//! `measure.rs` reports what the engine costs; this reports what it says. For
//! each probe it prints the segments the comment was cut into, the masked text
//! the engine receives, the raw Japanese it returns and the finished gloss, and
//! then translates the same segment with masking undone so the two can be
//! compared side by side.
//!
//! ```sh
//! CODEGLOSS_MODEL_PACK=/path/to/pack cargo run -p codegloss-translator \
//!   --features candle --release --example probe
//! ```
//!
//! With comments piped in on standard input, blocks separated by a `%%%` line,
//! it probes those instead of the built-in list - under the rules the corpus
//! names in its `%%% rules:` header ([`codegloss_parser::corpus`]), or under
//! [`CommentRules::Fenced`] when it names none.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use codegloss_core::{CommentRules, GlossPlan, Segment};
use codegloss_parser::corpus;
use codegloss_translator::{CandleTranslator, DEFAULT_BEAMS, Precision, Translator};

/// Comments chosen to stress the cases a reader complains about: fragments,
/// units that are all identifier, and sentences carrying several protected
/// spans at once.
const PROBES: &[&str] = &[
    // A whole sentence with nothing to protect - the case that works.
    "/// Returns the currently authenticated user.",
    // One protected span in the middle.
    "/// Returns `UserDetails` when authentication succeeds.",
    // Identifier-dense: most of the sentence is masked away.
    "/// Copies `src` into `dst` using `copy_nonoverlapping` and returns `dst`.",
    "/// Wraps find_user, UserRepository::open and CacheHandle::warm in one call.",
    // A unit that is nothing but a protected span.
    "/// @return `UserDetails`",
    "/// @param id the id",
    // Fragments: no verb, no subject.
    "/** Closes the underlying stream. Idempotent. */",
    "/// Thread-safe.",
    "/// The number of entries.",
    // A tag line whose subject the lead took away.
    "/// @throws AuthenticationException if authentication failed",
    // A heading, which is a fragment by construction.
    "/// # Panics",
    // A long merged run, the opposite end of the range.
    concat!(
        "// The worker owns the only handle to the model, so a translation that\n",
        "// arrives while another is running waits for it rather than loading a\n",
        "// second copy of the weights.",
    ),
];

fn main() -> ExitCode {
    let Some(pack) = std::env::var_os("CODEGLOSS_MODEL_PACK") else {
        eprintln!("set CODEGLOSS_MODEL_PACK to a model pack directory");
        return ExitCode::FAILURE;
    };
    let pack = PathBuf::from(pack);

    let precision = match std::env::var("CODEGLOSS_MODEL_PRECISION") {
        Ok(name) => match Precision::parse(&name) {
            Some(precision) => precision,
            None => {
                eprintln!("{name:?} is not a precision");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => Precision::default(),
    };

    let beams = match std::env::var("CODEGLOSS_MODEL_BEAMS") {
        Ok(width) => match width.parse::<usize>() {
            Ok(beams) if beams >= 1 => beams,
            _ => {
                eprintln!("{width:?} is not a beam width");
                return ExitCode::FAILURE;
            }
        },
        Err(_) => DEFAULT_BEAMS,
    };

    let translator = match CandleTranslator::load_with_beams(&pack, precision, beams) {
        Ok(translator) => translator,
        Err(error) => {
            eprintln!("{} is not a usable model pack: {error:?}", pack.display());
            return ExitCode::FAILURE;
        }
    };

    // Only when something is piped in: reading from a terminal would look
    // like a hang rather than like a prompt.
    let mut piped = String::new();
    if !std::io::stdin().is_terminal()
        && let Err(error) = std::io::stdin().read_to_string(&mut piped)
    {
        eprintln!("standard input could not be read: {error}");
        return ExitCode::FAILURE;
    }
    // The built-in probes are Rust doc comments written here; a piped corpus
    // says at the top which rules it was extracted under, and reading a Go one
    // as Rust would show the engine indented examples as prose - the thing this
    // tool exists to make visible, hidden by the tool (Issue #62).
    let (rules, probes) = if piped.trim().is_empty() {
        (CommentRules::Fenced, PROBES.to_vec())
    } else {
        match corpus::rules(&piped) {
            Ok((rules, blocks)) => (rules, blocks.split("\n%%%\n").map(str::trim_end).collect()),
            Err(error) => {
                eprintln!("standard input: {error}");
                return ExitCode::FAILURE;
            }
        }
    };
    eprintln!("{} probes, read as {}", probes.len(), rules.tag());

    for raw in probes {
        println!("================================================================");
        println!("{raw}");
        println!("----------------------------------------------------------------");

        let plan = GlossPlan::new(raw, rules);
        let sources = plan.sources();
        let segments = plan.segments();
        let masked: Vec<&str> = segments.iter().map(Segment::text).collect();

        let translations = match translator.translate(&segments) {
            Ok(translations) => translations,
            Err(error) => {
                eprintln!("  translate failed: {error:?}");
                continue;
            }
        };

        for (index, (input, output)) in masked.iter().zip(&translations).enumerate() {
            println!("  segment {index}");
            println!("    masked  {input:?}");
            println!("    engine  {output:?}");

            // The same unit with the masking undone: if this reads well and
            // the masked one does not, the placeholders are what broke it.
            let bare = Segment::new(sources[index].as_str());
            match translator.translate(std::slice::from_ref(&bare)) {
                Ok(plain) => {
                    println!("    bare in {:?}", bare.text());
                    println!("    bare    {:?}", plain[0]);
                }
                Err(error) => println!("    bare    failed: {error:?}"),
            }
        }

        println!("  gloss");
        for line in plan.restore(&translations).lines() {
            println!("    {line}");
        }
    }

    ExitCode::SUCCESS
}
