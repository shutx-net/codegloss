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

use codegloss_translator::{CandleTranslator, Precision, Translator};

/// Environment variable naming the model pack to test against.
pub const MODEL_PACK_VARIABLE: &str = "CODEGLOSS_MODEL_PACK";
/// Environment variable choosing the precision, so that the same quality bar
/// can be held up against F16 as against the default F32. Same name the server
/// reads.
pub const PRECISION_VARIABLE: &str = "CODEGLOSS_MODEL_PRECISION";

/// Loads the pack named by the environment, and says how to get one when the
/// variable is unset rather than failing with a path error.
pub fn translator() -> CandleTranslator {
    let pack = pack();
    let started = Instant::now();
    let translator = CandleTranslator::load_with(&pack, precision())
        .unwrap_or_else(|error| panic!("{} is not a usable model pack: {error:?}", pack.display()));
    eprintln!(
        "loaded {} in {:?}",
        translator.model_version(),
        started.elapsed()
    );
    translator
}

fn precision() -> Precision {
    match std::env::var(PRECISION_VARIABLE) {
        Ok(text) if !text.is_empty() => Precision::parse(&text)
            .unwrap_or_else(|| panic!("{PRECISION_VARIABLE}={text:?} is not f32, f16 or bf16")),
        _ => Precision::default(),
    }
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
