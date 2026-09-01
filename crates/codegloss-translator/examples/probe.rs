//! Where a gloss goes wrong: what the engine is actually asked, and what it
//! answers.
//!
//! `measure.rs` reports what the engine costs; this reports what it says. For
//! each probe it prints the units P6 cut the comment into, the masked text the
//! engine receives, the raw Japanese it returns and the finished gloss, and
//! then translates the same sentence with masking turned off so the two can be
//! compared side by side.
//!
//! ```sh
//! CODEGLOSS_MODEL_PACK=/path/to/pack cargo run -p codegloss-translator \
//!   --features candle --release --example probe
//! ```
//!
//! With comments piped in on standard input, blocks separated by a `%%%`
//! line, it probes those instead of the built-in list.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use codegloss_core::{CommentShape, GlossPlan, Segment};
use codegloss_translator::{CandleTranslator, Precision, Translator};

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

    let translator = match CandleTranslator::load_with(&pack, precision) {
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
    let probes: Vec<&str> = if piped.trim().is_empty() {
        PROBES.to_vec()
    } else {
        piped.split("\n%%%\n").map(str::trim_end).collect()
    };

    for raw in probes {
        println!("================================================================");
        println!("{raw}");
        println!("----------------------------------------------------------------");

        let plan = GlossPlan::new(raw);
        let shape = CommentShape::parse(raw);
        let bare_units = shape.units();
        let segments = plan.segments();
        let masked: Vec<&str> = segments.iter().map(Segment::text).collect();

        let translations = match translator.translate(&segments) {
            Ok(translations) => translations,
            Err(error) => {
                eprintln!("  translate failed: {error:?}");
                continue;
            }
        };

        for (unit, (input, output)) in masked.iter().zip(&translations).enumerate() {
            println!("  unit {unit}");
            println!("    masked  {input:?}");
            println!("    engine  {output:?}");

            // The same unit with the masking undone: if this reads well and
            // the masked one does not, the placeholders are what broke it.
            let bare = Segment::new(bare_units.get(unit).copied().unwrap_or_default());
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
