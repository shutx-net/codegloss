//! What the engine costs: memory, and time per comment.
//!
//! This is a binary rather than a `#[test]` on purpose. The test harness runs
//! `#[ignore]`d tests concurrently in one process, so every test that builds a
//! `CandleTranslator` holds its own copy of the weights at the same time; a
//! resident-set reading taken in the middle of that measures the harness, not
//! the server. `codegloss-lsp` loads exactly one model, and so does this.
//!
//! ```sh
//! CODEGLOSS_MODEL_PACK=/path/to/pack cargo run -p codegloss-translator \
//!   --features candle --release --example measure -- --dtype f32
//! ```
//!
//! Options:
//!
//! - `--dtype f32|f16|bf16` - how the weights are held (default `f32`).
//! - `--beams <n>` - hypotheses the search keeps; `1` is greedy.
//! - `--pack <dir>` - the model pack, overriding `CODEGLOSS_MODEL_PACK`.
//! - `--segments <n>` - stop after this many segments of the corpus.
//! - `--load-only` - load the model and report memory, skipping inference.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use codegloss_core::{CommentRules, GlossPlan, Segment};
use codegloss_translator::{CandleTranslator, DEFAULT_BEAMS, Precision, Translator};

/// The corpus `tests/quality.rs` checks against, so that the timings and the
/// quality results describe the same text.
const CORPUS: &str = include_str!("../tests/fixtures/comments.jsonl");

fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    println!("pack:    {}", options.pack.display());
    println!("dtype:   {}", options.precision);
    println!("beams:   {}", options.beams);
    report("before load");

    reset_peak();
    let opened = Instant::now();
    let translator =
        match CandleTranslator::load_with_beams(&options.pack, options.precision, options.beams) {
            Ok(translator) => translator,
            Err(error) => {
                eprintln!(
                    "{} is not a usable model pack: {error:?}",
                    options.pack.display()
                );
                return ExitCode::FAILURE;
            }
        };
    println!("open:    {:.0} ms", opened.elapsed().as_secs_f64() * 1000.0);
    println!("version: {}", translator.model_version());
    // What a server with a fully warm gloss cache costs: it can answer every
    // lookup from the version above, and never gets as far as the line below.
    report("after opening the pack");

    // Forced, because the server would not read the weights until its first
    // batch and this has to measure them. Peak reset first, so that "after
    // load" is the load's own peak and stays comparable with the older
    // numbers in `docs/model-runtime-notes.md`.
    reset_peak();
    let started = Instant::now();
    if let Err(error) = translator.prepare() {
        eprintln!("{} could not be loaded: {error:?}", options.pack.display());
        return ExitCode::FAILURE;
    }
    println!(
        "load:    {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    report("after load");

    if options.load_only {
        return ExitCode::SUCCESS;
    }

    let mut segments = corpus();
    if let Some(limit) = options.segments {
        segments.truncate(limit);
    }

    // The first inference pays for lazily faulted pages and for whatever the
    // gemm backend sets up once; it is not what a running server sees.
    if let Err(error) = translator.translate(&segments[..1.min(segments.len())]) {
        eprintln!("the engine failed: {error:?}");
        return ExitCode::FAILURE;
    }

    reset_peak();
    let mut singles = Vec::with_capacity(segments.len());
    for segment in &segments {
        let started = Instant::now();
        if let Err(error) = translator.translate(std::slice::from_ref(segment)) {
            eprintln!("the engine failed: {error:?}");
            return ExitCode::FAILURE;
        }
        singles.push(started.elapsed().as_secs_f64() * 1000.0);
    }

    let started = Instant::now();
    if let Err(error) = translator.translate(&segments) {
        eprintln!("the engine failed: {error:?}");
        return ExitCode::FAILURE;
    }
    let batch = started.elapsed().as_secs_f64() * 1000.0;

    singles.sort_by(f64::total_cmp);
    let total: f64 = singles.iter().sum();
    let count = singles.len() as f64;
    let percentile = |p: f64| singles[((count - 1.0) * p).round() as usize];

    println!("segments: {}", singles.len());
    println!(
        "per segment: mean {:.0} ms, p50 {:.0} ms, p90 {:.0} ms, max {:.0} ms, min {:.0} ms",
        total / count,
        percentile(0.5),
        percentile(0.9),
        singles[singles.len() - 1],
        singles[0]
    );
    println!(
        "whole batch: {batch:.0} ms ({:.0} ms per segment)",
        batch / count
    );
    report("after inference");
    ExitCode::SUCCESS
}

/// Every unit of every comment of the corpus, as the LSP worker would hand
/// them to the engine: masked, one paragraph or tag line each.
fn corpus() -> Vec<Segment> {
    CORPUS
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("every line of the corpus is JSON");
            let raw = value["raw"]
                .as_str()
                .expect("every entry has a raw comment");
            GlossPlan::new(raw, CommentRules::Fenced).segments()
        })
        .collect()
}

struct Options {
    pack: PathBuf,
    precision: Precision,
    beams: usize,
    segments: Option<usize>,
    load_only: bool,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut pack = std::env::var("CODEGLOSS_MODEL_PACK")
            .ok()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let mut precision = Precision::default();
        let mut beams = DEFAULT_BEAMS;
        let mut segments = None;
        let mut load_only = false;

        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--dtype" => {
                    let text = value()?;
                    precision = Precision::parse(&text)
                        .ok_or_else(|| format!("{text:?} is not f32, f16 or bf16"))?;
                }
                "--beams" => {
                    let text = value()?;
                    beams = text
                        .parse()
                        .ok()
                        .filter(|width| *width >= 1)
                        .ok_or_else(|| format!("{text:?} is not a beam width"))?;
                }
                "--pack" => pack = Some(PathBuf::from(value()?)),
                "--segments" => {
                    let text = value()?;
                    segments = Some(
                        text.parse()
                            .map_err(|_| format!("{text:?} is not a number"))?,
                    );
                }
                "--load-only" => load_only = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }

        let pack = pack.ok_or(
            "no model pack: pass --pack <dir> or set CODEGLOSS_MODEL_PACK. Build one with \
             `python3 tools/convert-fugumt/convert.py <dir>`.",
        )?;
        Ok(Self {
            pack,
            precision,
            beams,
            segments,
            load_only,
        })
    }
}

/// Resident set size now, and the highest it has been since the last
/// [`reset_peak`], in mebibytes.
///
/// The peak is the number that decides whether the server fits on a laptop
/// alongside an editor: a load that briefly doubles the model is a load that
/// can fail, even though the steady figure afterwards looks fine.
fn report(stage: &str) {
    match (status_mib("VmRSS:"), status_mib("VmHWM:")) {
        (Some(resident), Some(peak)) => {
            println!("{stage}: {resident:.1} MiB resident, {peak:.1} MiB peak");
        }
        _ => println!("{stage}: memory is not readable on this platform"),
    }
}

#[cfg(target_os = "linux")]
fn status_mib(field: &str) -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes: f64 = status
        .lines()
        .find(|line| line.starts_with(field))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kibibytes / 1024.0)
}

#[cfg(not(target_os = "linux"))]
fn status_mib(_field: &str) -> Option<f64> {
    None
}

/// Sets `VmHWM` back to the current `VmRSS`, so the next stage's peak is its
/// own rather than the whole process's so far.
#[cfg(target_os = "linux")]
fn reset_peak() {
    // Best effort: a kernel that does not support this leaves the peak alone,
    // which over-reports rather than under-reports.
    let _ = std::fs::write("/proc/self/clear_refs", "5\n");
}

#[cfg(not(target_os = "linux"))]
fn reset_peak() {}
