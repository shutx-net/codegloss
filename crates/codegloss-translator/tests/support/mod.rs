//! Shared setup for the tests that need the real model.
//!
//! Every one of them is `#[ignore]`d: a model pack is ~120 MB of
//! CC-BY-SA-4.0 weights that this repository must not contain (AGENTS.md), so
//! CI cannot run them. Point `CODEGLOSS_MODEL_PACK` at a pack built with
//! `tools/convert-fugumt` and run them with `-- --ignored`.

// Two test binaries share this, and neither of them uses all of it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Instant;

use codegloss_translator::CandleTranslator;

/// Environment variable naming the model pack to test against.
pub const MODEL_PACK_VARIABLE: &str = "CODEGLOSS_MODEL_PACK";

/// Loads the pack named by the environment, and says how to get one when the
/// variable is unset rather than failing with a path error.
pub fn translator() -> CandleTranslator {
    let pack = pack();
    let started = Instant::now();
    let translator = CandleTranslator::load(&pack)
        .unwrap_or_else(|error| panic!("{} is not a usable model pack: {error:?}", pack.display()));
    eprintln!(
        "loaded {} in {:?}",
        translator.manifest().model_version,
        started.elapsed()
    );
    translator
}

pub fn pack() -> PathBuf {
    match std::env::var(MODEL_PACK_VARIABLE) {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => panic!(
            "these tests need a model pack. Build one with \
             `python3 tools/convert-fugumt/convert.py <dir>` and set \
             {MODEL_PACK_VARIABLE}=<dir>."
        ),
    }
}

/// Resident set size of this process, in mebibytes.
///
/// The number that matters for an editor plugin is what the server holds while
/// it sits there, so it is read after the model is loaded rather than
/// estimated from the file size.
#[cfg(target_os = "linux")]
pub fn resident_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<f64>()
        .ok()?;
    Some(line / 1024.0)
}

#[cfg(not(target_os = "linux"))]
pub fn resident_mib() -> Option<f64> {
    None
}
