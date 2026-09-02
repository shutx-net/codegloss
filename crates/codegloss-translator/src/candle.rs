//! FuguMT (Marian) inference on candle.
//!
//! The engine AGENTS.md picks for v0.1: a pure-Rust runtime and a
//! CC-BY-SA-4.0 model, both running inside the native `codegloss-lsp` binary
//! rather than inside the WASM extension.
//!
//! It reads a *model pack*: a directory produced by `tools/convert-fugumt`
//! holding the weights, the two tokenizers, the model config and a manifest.
//! The pack is never part of this repository - the weights are CC-BY-SA-4.0
//! and the code is MIT, and AGENTS.md keeps them apart.
//!
//! IMPORTANT: [`Translator::translate`] is a blocking call and this
//! implementation makes that real - one comment takes on the order of a tenth
//! of a second. `codegloss-lsp` runs it on its background worker; nothing on
//! the path of an LSP request may call it.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::marian;
use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, Shape, Tensor};
use candle_nn::var_builder::SimpleBackend;
use candle_nn::{Init, Linear, Module, VarBuilder};
use codegloss_core::Segment;
use serde::Deserialize;
use tokenizers::Tokenizer;

use crate::Translator;

/// Describes the pack: which model it holds and under what licence.
pub const MANIFEST_FILE: &str = "manifest.json";
/// Hyper-parameters, in the shape `marian::Config` deserializes from. Copied
/// out of the upstream repository unchanged.
pub const CONFIG_FILE: &str = "config.json";
/// Weights, preferred form.
pub const SAFETENSORS_FILE: &str = "model.safetensors";
/// Weights, as the upstream repository publishes them. candle reads a pickle
/// archive directly, so converting is optional - see the converter's README.
pub const PYTORCH_FILE: &str = "pytorch_model.bin";
/// Tokenizer for the source language (English).
pub const SOURCE_TOKENIZER_FILE: &str = "tokenizer-source.json";
/// Tokenizer for the target language (Japanese). Marian shares one
/// SentencePiece model between the two, but they are separate files upstream
/// and the pack keeps them separate.
pub const TARGET_TOKENIZER_FILE: &str = "tokenizer-target.json";

/// Cache-key identity of *this* engine, as opposed to of the weights it runs.
///
/// It covers everything about the runtime that can change a translation
/// without the model pack changing: the search, the forbidden tokens. Bump it
/// whenever one of those moves, or the cache keeps serving what the old code
/// produced. The precision and the beam width are not in here because they are
/// appended to it - see [`CandleTranslator::model_version`].
///
/// - `candle-marian-1` - greedy only.
/// - `candle-marian-2` - beam search, length-normalised.
/// - `candle-marian-3` - a finished hypothesis wins over a running one.
pub const ENGINE_VERSION: &str = "candle-marian-3";

/// The vocabulary entry a tokenizer uses for text it cannot represent.
const UNKNOWN_TOKEN: &str = "<unk>";

/// Numeric type the weights are held in, and therefore the one every matrix
/// multiplication runs in: candle's Marian has no mixed precision, the tensors
/// carry their own dtype through the whole graph.
///
/// FuguMT is published as `float16` (`torch_dtype` in its `config.json`, 116
/// MiB on disk). Reading it as [`Float32`](Precision::Float32) doubles that;
/// reading it as [`Float16`](Precision::Float16) keeps it. What that costs in
/// time is not obvious either way and is measured rather than guessed - see
/// `docs/model-runtime-notes.md` and `examples/measure.rs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Precision {
    /// Single precision. The default: it is the only one of the three that
    /// every candle CPU kernel has a real implementation for.
    #[default]
    Float32,
    /// Half precision, as the weights are published.
    Float16,
    /// Brain float. Same size as [`Float16`](Precision::Float16), same
    /// exponent range as [`Float32`](Precision::Float32).
    BFloat16,
}

impl Precision {
    /// Short name, as the command line spells it and as it appears in
    /// [`CandleTranslator::model_version`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Float32 => "f32",
            Self::Float16 => "f16",
            Self::BFloat16 => "bf16",
        }
    }

    /// Parses [`as_str`](Precision::as_str), case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "f32" | "float32" => Some(Self::Float32),
            "f16" | "float16" => Some(Self::Float16),
            "bf16" | "bfloat16" => Some(Self::BFloat16),
            _ => None,
        }
    }

    const fn dtype(self) -> DType {
        match self {
            Self::Float32 => DType::F32,
            Self::Float16 => DType::F16,
            Self::BFloat16 => DType::BF16,
        }
    }
}

impl std::fmt::Display for Precision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Hard stop on the decoder loop.
///
/// Greedy decoding has no beam to fall back on and can repeat itself forever
/// on an input the model does not understand. A comment that needs more than
/// this many tokens is not a comment.
const MAX_NEW_TOKENS: usize = 512;

/// How many hypotheses [`beam search`](Engine::search) keeps.
///
/// FuguMT's own `generation_config.json` asks for 12; this is the width the
/// model was evaluated at, not one that has to be paid at read time. The cost
/// is close to linear in the width and the benefit is not, so the shipped
/// default is measured rather than inherited - see
/// `docs/model-runtime-notes.md`.
///
/// `1` is greedy decoding, and is a different code path rather than a beam
/// search of width one: there is no reason to pay for the bookkeeping.
pub const DEFAULT_BEAMS: usize = 4;

/// Rows of the decoder batch to aim for.
///
/// One segment occupies [`beams`](Engine::beams) rows, so this is a budget in
/// segments only after dividing. Widening the batch stops paying once the
/// matrix multiplications are large enough to keep the cores busy, and starts
/// costing once a group spans segments of unlike length and pads to the
/// longest - the measured curve is in `docs/model-runtime-notes.md`.
const MAX_BATCH_ROWS: usize = 32;

/// Exponent the score of a finished hypothesis is divided by.
///
/// A hypothesis is scored by summing log probabilities, and every term is
/// negative, so a longer translation scores worse for being longer. Left
/// alone, the search would reliably prefer the hypothesis that stops early -
/// which is the failure beam search is here to fix. `1.0` divides the score by
/// the length and takes the preference back out; it is what the upstream
/// generation config leaves at its default.
const LENGTH_PENALTY: f64 = 1.0;

/// What a model pack says about itself.
///
/// The licence and the attribution are carried in the pack rather than in this
/// repository on purpose: they travel with the weights they apply to.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Upstream identity, e.g. `staka/fugumt-en-ja`.
    pub model_id: String,
    /// Goes into every cache key. Changing the weights has to change this.
    pub model_version: String,
    /// SPDX identifier of the weights' licence.
    pub license: String,
    /// Attribution text the licence requires to be kept.
    pub attribution: String,
    /// Every file of the pack, by name, with what it should hash to.
    ///
    /// This is what makes a pack downloadable: the manifest is small enough to
    /// fetch first, and everything else is checked against it. A pack built
    /// before `tools/convert-fugumt` wrote this cannot be verified and is
    /// rejected rather than trusted.
    pub files: BTreeMap<String, PackFile>,
}

/// One file of a pack, as [`Manifest`] describes it.
#[derive(Debug, Clone, Deserialize)]
pub struct PackFile {
    /// Lower-case hex, as `tools/convert-fugumt` writes it.
    pub sha256: String,
    pub bytes: u64,
}

impl Manifest {
    /// Reads the manifest of the pack in `pack`.
    pub fn read_from(pack: &Path) -> Result<Self> {
        Self::read(pack)
    }

    fn read(pack: &Path) -> Result<Self> {
        let path = pack.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "{} is missing from the model pack at {}; produce the pack with tools/convert-fugumt",
                MANIFEST_FILE,
                pack.display()
            )
        })?;
        Self::parse(&text).with_context(|| format!("{} is not valid", path.display()))
    }

    /// Reads a manifest that is not on disk yet - the one a download fetches
    /// before it knows what else to fetch.
    pub fn parse(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// Checks that `directory` holds every file this manifest names, at the
    /// length and digest it names them at.
    ///
    /// A pack is 120 MB fetched over a network into a cache directory that
    /// outlives the process. Anything can go wrong there - a truncated
    /// download, a full disk, a half-written file from a killed process - and
    /// none of it is visible in the translations: bad weights produce fluent
    /// nonsense rather than an error. So the pack is checked before it is
    /// used, not after it disappoints.
    pub fn verify(&self, directory: &Path) -> Result<()> {
        if self.files.is_empty() {
            bail!(
                "the manifest lists no files, so nothing about this pack can be checked; \
                 rebuild it with tools/convert-fugumt"
            );
        }
        for (name, file) in &self.files {
            let path = directory.join(name);
            let found = fs::metadata(&path)
                .with_context(|| format!("{} is missing from the pack", path.display()))?
                .len();
            if found != file.bytes {
                bail!(
                    "{} is {found} bytes, and the manifest says {}",
                    path.display(),
                    file.bytes
                );
            }
            let digest = sha256(&path)?;
            if !digest.eq_ignore_ascii_case(&file.sha256) {
                bail!(
                    "{} hashes to {digest}, and the manifest says {}",
                    path.display(),
                    file.sha256
                );
            }
        }
        Ok(())
    }
}

/// Lower-case hex SHA-256 of a file, read in chunks: a pack holds 120 MB and
/// reading it into memory to hash it would double what loading it costs.
fn sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};

    let mut file =
        fs::File::open(path).with_context(|| format!("{} is unreadable", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .with_context(|| format!("{} could not be read to the end", path.display()))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// FuguMT, ready to translate - though not read until it has to be.
///
/// The weights are read once, the first time something is actually
/// translated, and held for the life of the process: they are ~120 MB and
/// re-reading them per batch would dwarf the inference itself. Reading them
/// only then is what lets a server whose gloss cache is already warm answer
/// every request without ever paying for them - the cache key needs
/// [`model_version`](Translator::model_version), and that comes from the
/// pack's manifest.
pub struct CandleTranslator {
    manifest: Manifest,
    /// [`ENGINE_VERSION`] and the pack's version together: a translation
    /// depends on both, so a cache key has to depend on both.
    ///
    /// IMPORTANT: this is a stored string and must stay one. It is read on the
    /// path of every LSP request, to look a gloss up; answering it by loading
    /// the model would put the ~280 MiB back on the first hover and buy
    /// nothing.
    model_version: String,
    /// What the deferred load will need, resolved when the pack was opened.
    recipe: Recipe,
    /// `marian::MTModel` needs `&mut self` for every forward pass because it
    /// owns the KV cache, while `Translator::translate` takes `&self`. One
    /// mutex reconciles the two, and the deferred load happens under it as
    /// well. Translation is serialised as a result, which costs nothing:
    /// `codegloss-lsp` runs exactly one worker.
    engine: Mutex<Load>,
}

impl std::fmt::Debug for CandleTranslator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`, because a load holds this for the better part of a
        // second and whatever is logging must not wait for it.
        let weights = match self.engine.try_lock() {
            Ok(slot) => slot.state(),
            Err(std::sync::TryLockError::Poisoned(_)) => "poisoned",
            Err(std::sync::TryLockError::WouldBlock) => "in use",
        };
        formatter
            .debug_struct("CandleTranslator")
            .field("model_id", &self.manifest.model_id)
            .field("model_version", &self.model_version)
            .field("weights", &weights)
            .finish_non_exhaustive()
    }
}

/// Everything the real load needs, kept from when the pack was opened.
///
/// It holds no weights: this is the recipe, not the meal. Resolving the paths
/// up front is also what makes a remembered failure meaningful - a retry could
/// not pick a different file to be unhappy about.
struct Recipe {
    /// The pack directory, for the error messages.
    pack: PathBuf,
    config: marian::Config,
    weights: PathBuf,
    precision: Precision,
    beams: usize,
}

/// The model slot: empty until something is translated, and settled after.
enum Load {
    /// The pack has been opened and nothing has been read from it yet.
    Deferred,
    /// The weights are in memory. Boxed because an `Engine` is 352 bytes and
    /// the other two states are not, and this is held for the life of the
    /// process either way.
    Ready(Box<Engine>),
    /// The load was tried and failed; the string is why.
    ///
    /// A failure is remembered rather than retried. A corrupt pack is the case
    /// that matters - without this, every `didChange` would re-read 120 MB to
    /// fail the same way. It does mean a load that failed for a passing reason
    /// (no memory at that moment, say) stays failed for the life of the
    /// process: that is a deliberate trade, not an oversight, and the way out
    /// of it is to restart the server rather than to make the engine retry on
    /// a schedule nobody can see.
    Failed(String),
}

impl Load {
    /// What [`CandleTranslator`]'s `Debug` calls the slot.
    fn state(&self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Ready(_) => "loaded",
            Self::Failed(_) => "failed",
        }
    }
}

struct Engine {
    config: marian::Config,
    model: marian::MTModel,
    source: Arc<Tokenizer>,
    target: Arc<Tokenizer>,
    device: Device,
    dtype: DType,
    /// Output projection, and the bias Marian adds to it.
    ///
    /// Upstream's `MTModel::decode` owned a pair of these, but it also built
    /// its causal mask in F32 and added it to the attention weights - a dtype
    /// mismatch the moment the model is not F32, with no way to hand it a mask
    /// instead. Driving `MTModel::decoder` directly is what makes
    /// [`Precision`] a choice at all, and why the fork in `marian.rs` drops
    /// `decode` altogether. It costs nothing: `VarBuilder` hands out tensors
    /// that share their storage, so this is the same embedding matrix the
    /// model is using, not a second copy.
    lm_head: Linear,
    final_logits_bias: Tensor,
    /// Added to the logits before picking a token: `0` everywhere and `-inf`
    /// on the tokens the model must never emit.
    forbidden: Tensor,
    /// Hypotheses kept by the search; `1` means greedy.
    beams: usize,
}

/// One partial or finished translation, and the sum of the log probabilities
/// of the tokens that make it up.
#[derive(Debug, Clone)]
struct Hypothesis {
    /// Which segment of the batch this is a translation of.
    segment: usize,
    /// The decoder start token, then what the search has chosen.
    tokens: Vec<u32>,
    score: f64,
}

impl Hypothesis {
    /// The score the hypotheses are compared on: per generated token, so that
    /// a short translation cannot win by being short.
    fn normalised(&self) -> f64 {
        let generated = self.tokens.len().saturating_sub(1).max(1) as f64;
        self.score / generated.powf(LENGTH_PENALTY)
    }
}

impl CandleTranslator {
    /// Opens the model pack in `pack`.
    ///
    /// This reads the manifest and the model config and checks that the
    /// weights and both tokenizers are where they should be. It does not read
    /// them: the 280.8 MiB and the ~0.4 s they cost (measured, f32) are paid
    /// on the first call to [`translate`](Translator::translate) instead, so a
    /// server whose gloss cache is already warm never pays them at all - it
    /// costs 4.2 MiB and stops there. That is the whole point of the deferral;
    /// the numbers are in `docs/model-runtime-notes.md` §11.
    ///
    /// What it gives up is the old guarantee that an engine which opened will
    /// not fail later. A pack that passes the checks here and still cannot be
    /// read reports the failure from the first batch, where the worker drops
    /// it (`codegloss-lsp`: hover still answers with the English source, and
    /// the code lens keeps saying it is translating). Everything a missing or
    /// malformed pack can be caught by cheaply is still caught here, which is
    /// what keeps a caller's fallback to [`PassthroughTranslator`] where it
    /// was.
    ///
    /// [`PassthroughTranslator`]: crate::PassthroughTranslator
    pub fn load(pack: impl AsRef<Path>) -> Result<Self> {
        Self::load_with(pack, Precision::default())
    }

    /// Opens the model pack in `pack`, to hold the weights in `precision`.
    ///
    /// See [`load`](CandleTranslator::load); this is the same thing with the
    /// numeric type spelled out. The precision is part of
    /// [`model_version`](Translator::model_version), so translations produced
    /// under one are never served for another.
    pub fn load_with(pack: impl AsRef<Path>, precision: Precision) -> Result<Self> {
        Self::load_with_beams(pack, precision, DEFAULT_BEAMS)
    }

    /// Opens the model pack in `pack`, to hold the weights in `precision` and
    /// search `beams` hypotheses wide.
    ///
    /// `beams` of `1` is greedy decoding. Like the precision, the width is part
    /// of [`model_version`](Translator::model_version): two widths do not agree
    /// on a translation, so they must not share a cache entry.
    pub fn load_with_beams(
        pack: impl AsRef<Path>,
        precision: Precision,
        beams: usize,
    ) -> Result<Self> {
        let beams = beams.max(1);
        let pack = pack.as_ref();
        let manifest = Manifest::read(pack)?;

        let config: marian::Config = {
            let path = pack.join(CONFIG_FILE);
            let text = fs::read_to_string(&path)
                .with_context(|| format!("{} is unreadable", path.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("{} is not a Marian config", path.display()))?
        };

        // Both of these only stat: what is left of the old load that is cheap
        // enough to keep doing at startup, and enough to catch an incomplete
        // pack while the caller can still fall back to English.
        let weights = weight_file(pack)?;
        tokenizer_files(pack)?;

        tracing::info!(
            model = %manifest.model_id,
            version = %manifest.model_version,
            weights = %weights.display(),
            precision = %precision,
            beams,
            "opened the model pack; its weights are read on the first translation"
        );

        Ok(Self {
            model_version: format!(
                "{ENGINE_VERSION}-{precision}-b{beams}+{}",
                manifest.model_version
            ),
            manifest,
            recipe: Recipe {
                pack: pack.to_path_buf(),
                config,
                weights,
                precision,
                beams,
            },
            engine: Mutex::new(Load::Deferred),
        })
    }

    /// What the pack says about the weights, for anyone that has to reproduce
    /// the attribution the licence asks for.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Reads the weights now, instead of at the first translation.
    ///
    /// For measurement and for tests: `examples/measure.rs` would otherwise
    /// report the memory of a model it never read, and the model-backed tests
    /// want a bad pack to fail while they are setting up rather than in the
    /// middle of an assertion. **The server does not call this** - a warm
    /// cache never paying for the model is exactly what the deferral is for.
    pub fn prepare(&self) -> Result<()> {
        self.with_engine(|_| Ok(()))
    }

    /// Runs `work` against the engine, loading it if this is the first thing
    /// to ask for it.
    ///
    /// IMPORTANT: this blocks for as long as the load takes. It is reachable
    /// only from [`translate`](Translator::translate) and
    /// [`prepare`](CandleTranslator::prepare), and `codegloss-lsp` calls the
    /// first from `spawn_blocking` and the second not at all.
    fn with_engine<T>(&self, work: impl FnOnce(&mut Engine) -> Result<T>) -> Result<T> {
        let mut slot = self
            .engine
            .lock()
            .map_err(|_| anyhow!("the translation engine was poisoned by an earlier panic"))?;

        if matches!(*slot, Load::Deferred) {
            *slot = match self.recipe.build() {
                Ok(engine) => Load::Ready(Box::new(engine)),
                Err(error) => {
                    let reason = format!("{error:#}");
                    // The one line that explains a server which answers every
                    // hover in English and never finishes a code lens. The
                    // failure is reported from every batch after this, but
                    // only ever logged here.
                    tracing::error!(
                        pack = %self.recipe.pack.display(),
                        "the model pack passed its checks but could not be read, so nothing \
                         will be translated: {reason}"
                    );
                    Load::Failed(reason)
                }
            };
        }

        match &mut *slot {
            Load::Ready(engine) => work(engine),
            Load::Failed(reason) => Err(anyhow!(
                "the model pack at {} could not be read: {reason}",
                self.recipe.pack.display()
            )),
            // Not reachable: the slot was just filled. An error rather than a
            // panic all the same, because an engine must never be the reason
            // the server stops.
            Load::Deferred => Err(anyhow!(
                "the model pack at {} was not loaded",
                self.recipe.pack.display()
            )),
        }
    }
}

impl Recipe {
    /// Reads the weights and builds the tokenizers: everything the open
    /// deferred.
    ///
    /// The order here is load-bearing and is the one that was measured. The
    /// `VarBuilder` is lazy and [`Weights`] memoises what it hands out, so
    /// asking for `model.shared.weight` before `MTModel::new` consumes the
    /// builder costs nothing, where asking after it would cost another copy of
    /// the embedding matrix. See `docs/model-runtime-notes.md` §6.2.
    fn build(&self) -> Result<Engine> {
        let device = Device::Cpu;
        let dtype = self.precision.dtype();
        let variables = load_weights(&self.weights, dtype, &device)?;
        let context = || format!("{} does not hold a Marian model", self.weights.display());

        let config = self.config.clone();
        let vocabulary = config.decoder_vocab_size.unwrap_or(config.vocab_size);
        let lm_head = Linear::new(
            variables
                .get((config.vocab_size, config.d_model), "model.shared.weight")
                .with_context(context)?,
            None,
        );
        let final_logits_bias = variables
            .get((1, vocabulary), "final_logits_bias")
            .with_context(context)?;
        let model = marian::MTModel::new(&config, variables).with_context(context)?;

        let (source, target) = tokenizers(&self.pack)?;

        // `bad_words_ids` in the upstream config forbids exactly one token, the
        // padding one. candle has no such mechanism, so it is applied here.
        let forbidden =
            forbidden_logits(&config, target.token_to_id(UNKNOWN_TOKEN), dtype, &device)?;

        tracing::info!(
            weights = %self.weights.display(),
            precision = %self.precision,
            beams = self.beams,
            "loaded the translation model"
        );

        Ok(Engine {
            config,
            model,
            source,
            target,
            device,
            dtype,
            lm_head,
            final_logits_bias,
            forbidden,
            beams: self.beams,
        })
    }
}

impl Translator for CandleTranslator {
    fn translate(&self, segments: &[Segment]) -> Result<Vec<String>> {
        // Before the lock and before the load: a batch with nothing in it is
        // not a reason to read 120 MB. `codegloss-lsp`'s worker returns early
        // on one of its own accord, and this keeps that true for every other
        // caller too.
        if segments.is_empty() {
            return Ok(Vec::new());
        }

        self.with_engine(|engine| engine.translate(segments))
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }
}

impl Engine {
    /// Translates a whole batch.
    ///
    /// The segments are regrouped before they reach the model: sorted by
    /// length, cut into groups that fill the decoder batch, and each group
    /// padded to its longest member. Nothing about the result depends on the
    /// grouping - a padded position is masked out of every attention, so a
    /// segment is translated the same whoever it travels with - but everything
    /// about the speed does.
    fn translate(&mut self, segments: &[Segment]) -> Result<Vec<String>> {
        let mut translations = vec![String::new(); segments.len()];
        let mut jobs = Vec::with_capacity(segments.len());

        for (index, segment) in segments.iter().enumerate() {
            let text = segment.text();
            // Nothing to say, and an empty encoder input makes the model
            // produce a sentence out of nowhere.
            if text.trim().is_empty() {
                translations[index] = text.to_owned();
                continue;
            }
            jobs.push(Job {
                index,
                source: self.tokenize(text)?,
            });
        }

        // Longest first. A group pads to its longest member, so grouping
        // like with like is what keeps the padding from being most of the
        // batch.
        jobs.sort_by_key(|job| std::cmp::Reverse(job.source.len()));

        // Every segment of a group occupies `beams` rows of the decoder batch.
        let per_group = (MAX_BATCH_ROWS / self.beams).max(1);
        for group in jobs.chunks(per_group) {
            for (job, translation) in group.iter().zip(self.translate_group(group)?) {
                translations[job.index] = translation;
            }
        }

        Ok(translations)
    }

    /// Source tokens for one segment, as the encoder wants them.
    fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
        let encoded = self
            .source
            .encode(text, true)
            .map_err(|error| anyhow!("{text:?} could not be tokenized: {error}"))?;
        let mut tokens = encoded.get_ids().to_vec();

        // The sinusoidal position embeddings are built for exactly this many
        // positions; one token more indexes past the end of the table.
        tokens.truncate(self.config.max_position_embeddings - 1);

        // The tokenizer may or may not append the end marker itself, depending
        // on how the pack was converted. Both are fine, two of them are not.
        if tokens.last() != Some(&self.config.eos_token_id) {
            tokens.push(self.config.eos_token_id);
        }
        Ok(tokens)
    }

    /// Encodes a group together and searches all of it at once.
    fn translate_group(&mut self, group: &[Job]) -> Result<Vec<String>> {
        // The KV cache holds the previous group. Not resetting it is the
        // classic way to have translation number two contaminated by number
        // one.
        self.model.reset_kv_cache();

        let source_len = group.iter().map(|job| job.source.len()).max().unwrap_or(0);
        let mut padded = Vec::with_capacity(group.len() * source_len);
        let mut pad_mask = Vec::with_capacity(group.len() * source_len);
        for job in group {
            let padding = source_len - job.source.len();
            padded.extend_from_slice(&job.source);
            padded.extend(std::iter::repeat_n(self.config.pad_token_id, padding));
            pad_mask.extend(std::iter::repeat_n(0f32, job.source.len()));
            pad_mask.extend(std::iter::repeat_n(f32::NEG_INFINITY, padding));
        }

        let tokens = Tensor::from_vec(padded, (group.len(), source_len), &self.device)?;
        // A group whose members are all the same length has nothing to hide,
        // and no mask at all is cheaper than one made of zeroes.
        let pad_mask = pad_mask
            .iter()
            .any(|value| value.is_infinite())
            .then(|| Tensor::from_vec(pad_mask, (group.len(), source_len), &self.device))
            .transpose()?
            .map(|mask| mask.to_dtype(self.dtype))
            .transpose()?;

        let rows: Vec<u32> = (0..group.len() as u32).collect();
        let rows = Tensor::from_vec(rows, (group.len(),), &self.device)?;
        let encoder_mask = self.attention_mask(
            pad_mask.as_ref(),
            &rows,
            self.config.encoder_attention_heads,
        )?;
        let encoder_xs = self
            .model
            .encoder()
            .forward(&tokens, 0, encoder_mask.as_ref())?;

        self.search(group.len(), &encoder_xs, pad_mask.as_ref())?
            .iter()
            // The start token is the decoder's, not the sentence's.
            .map(|tokens| {
                self.target
                    .decode(&tokens[1..], true)
                    .map_err(|error| anyhow!("the translation could not be decoded: {error}"))
            })
            .collect()
    }

    /// Keeps [`beams`](Engine::beams) hypotheses alive per segment and returns
    /// the best finished one of each.
    ///
    /// Greedy decoding - `beams` of one - is this with a width of one rather
    /// than a path of its own: taking the single most likely token is what a
    /// search one wide does, and the end-of-sentence token that truncates a
    /// translation part way through is the top one exactly as often.
    ///
    /// Two deliberate departures from the upstream generation config:
    ///
    /// - the width is [`DEFAULT_BEAMS`] rather than its 12, because the cost is
    ///   linear in the width and the benefit is not;
    /// - the search stops as soon as `beams` hypotheses have finished
    ///   (`early_stopping=True`), rather than proving that nothing still
    ///   running can beat them. Proving it needs a bound on a score whose
    ///   normaliser grows with the length, which is only a bound in the
    ///   direction that does not help.
    ///
    /// The KV cache is carried across steps, and moved with the beams: the
    /// hypothesis in row `i` changes parent at nearly every step, and a cache
    /// left in place would answer with the prefix that used to be there. That
    /// is what [`marian::Decoder::reorder_kv_cache`] is for, and what this
    /// crate carries its own copy of `marian` to be able to call. It also
    /// carries the batch: a segment whose beams have all finished leaves, and
    /// its rows go with it.
    fn search(
        &mut self,
        segments: usize,
        encoder_xs: &Tensor,
        pad_mask: Option<&Tensor>,
    ) -> Result<Vec<Vec<u32>>> {
        let mut live: Vec<Hypothesis> = (0..segments)
            .map(|segment| Hypothesis {
                segment,
                tokens: vec![self.config.decoder_start_token_id],
                score: 0.0,
            })
            .collect();
        let mut finished: Vec<Vec<Hypothesis>> = vec![Vec::new(); segments];

        // Which segment each row of the batch is translating, and the encoder
        // states and mask that follow from it. Rebuilt only when it changes -
        // which is when a segment leaves the batch, and once at the start as
        // the one row per segment opens out into `beams` - because expanding
        // the encoder states is a copy of the whole source, and doing it per
        // step costs more than the search saves.
        let mut mapping: Vec<u32> = Vec::new();
        let mut encoder_rows = encoder_xs.clone();
        let mut cross_mask = None;

        for _ in 0..MAX_NEW_TOKENS {
            let rows = live.len();
            // The cache holds every token but the last, so the last is all that
            // goes back in. Every live hypothesis is the same length - a
            // segment advances one token per step or leaves - so one `past`
            // covers the batch.
            let past = live[0].tokens.len() - 1;
            let last: Vec<u32> = live
                .iter()
                .map(|hypothesis| hypothesis.tokens[past])
                .collect();
            let input_ids = Tensor::from_vec(last, (rows, 1), &self.device)?;

            let of_row: Vec<u32> = live
                .iter()
                .map(|hypothesis| hypothesis.segment as u32)
                .collect();
            if of_row != mapping {
                let rows = Tensor::from_vec(of_row.clone(), (of_row.len(),), &self.device)?;
                encoder_rows = encoder_xs.index_select(&rows, 0)?.contiguous()?;
                cross_mask =
                    self.attention_mask(pad_mask, &rows, self.config.decoder_attention_heads)?;
                mapping = of_row;
            }

            let logits = self.decode(&input_ids, &encoder_rows, past, cross_mask.as_ref())?;
            let logits = logits.squeeze(1)?.broadcast_add(&self.forbidden)?;
            // In F32 whatever the weights are held in: a sum of log
            // probabilities over a whole sentence is exactly the accumulation
            // F16 is bad at.
            let logprobs = candle_nn::ops::log_softmax(&logits.to_dtype(DType::F32)?, 1)?;
            let logprobs: Vec<f32> = logprobs.flatten_all()?.to_vec1()?;
            let vocabulary = logprobs.len() / rows;

            let mut next: Vec<Hypothesis> = Vec::with_capacity(rows);
            let mut parents: Vec<u32> = Vec::with_capacity(rows);
            for (segment, endings) in finished.iter_mut().enumerate() {
                let mine: Vec<usize> = (0..rows)
                    .filter(|row| live[*row].segment == segment)
                    .collect();
                if mine.is_empty() {
                    continue;
                }

                let mut kept: Vec<(f64, usize, u32)> = Vec::with_capacity(self.beams);
                for (score, row, token) in best(&logprobs, vocabulary, &live, &mine, 2 * self.beams)
                {
                    if self.is_end(token) {
                        endings.push(Hypothesis {
                            segment,
                            tokens: live[row].tokens.clone(),
                            score,
                        });
                    } else if kept.len() < self.beams {
                        kept.push((score, row, token));
                    }
                }

                // Enough endings: this segment is answered, and its rows are
                // not carried into the next step.
                if endings.len() >= self.beams {
                    continue;
                }
                for (score, row, token) in kept {
                    let mut tokens = live[row].tokens.clone();
                    tokens.push(token);
                    next.push(Hypothesis {
                        segment,
                        tokens,
                        score,
                    });
                    parents.push(row as u32);
                }
            }

            live = next;
            if live.is_empty() {
                break;
            }

            // Row `i` of the batch now continues what row `parents[i]` held,
            // and the cache has to say the same. On the first pass this also
            // widens it from the one row per segment the start token needed.
            let parents = Tensor::from_vec(parents, (live.len(),), &self.device)?;
            self.model.reorder_kv_cache(&parents)?;
        }

        // A finished hypothesis beats a running one whatever they score.
        // Ending is not free - the end token has to be the likely one where it
        // falls - so a running hypothesis of the same length can always score
        // better by not having paid for it, and returning one means returning
        // a sentence the model never ended. That is the failure this whole
        // search exists to remove. A segment with nothing finished ran out of
        // budget instead, and there its best running hypothesis is a truncated
        // translation, but it is the translation.
        finished
            .iter()
            .enumerate()
            .map(|(segment, endings)| {
                endings
                    .iter()
                    .chain(live.iter().filter(|it| it.segment == segment))
                    .max_by(|left, right| left.normalised().total_cmp(&right.normalised()))
                    .map(|best| best.tokens.clone())
                    .ok_or_else(|| anyhow!("the search ended with no hypothesis at all"))
            })
            .collect()
    }

    /// The mask that hides padded source positions, shaped for the attention
    /// weights of `heads` heads over the batch rows named by `rows`.
    ///
    /// `attn_weights` is `(batch * heads, queries, keys)` with the heads of one
    /// batch row adjacent, so a mask over keys repeats head by head. It says
    /// nothing about the query, which is what lets one mask serve both the
    /// encoder - where the queries are the source - and cross-attention, where
    /// they are the translation so far.
    fn attention_mask(
        &self,
        pad_mask: Option<&Tensor>,
        rows: &Tensor,
        heads: usize,
    ) -> Result<Option<Tensor>> {
        let Some(pad_mask) = pad_mask else {
            return Ok(None);
        };
        let per_row = pad_mask.index_select(rows, 0)?;
        let (rows, source_len) = per_row.dims2()?;
        Ok(Some(
            per_row
                .unsqueeze(1)?
                .broadcast_as((rows, heads, source_len))?
                .contiguous()?
                .reshape((rows * heads, 1, source_len))?,
        ))
    }

    fn is_end(&self, token: u32) -> bool {
        token == self.config.eos_token_id || token == self.config.forced_eos_token_id
    }

    /// One decoder step, plus the projection onto the vocabulary.
    ///
    /// This is what upstream's `MTModel::decode` did, with the causal mask
    /// built in the model's own dtype instead of always in F32 - see
    /// [`Engine::lm_head`] - and with a mask for the padded source positions
    /// that upstream had no way to pass.
    fn decode(
        &mut self,
        input_ids: &Tensor,
        encoder_xs: &Tensor,
        past: usize,
        cross_mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let length = input_ids.dim(1)?;
        let mask: Vec<f32> = (0..length)
            .flat_map(|row| {
                (0..length).map(move |column| {
                    if column > row {
                        f32::NEG_INFINITY
                    } else {
                        0f32
                    }
                })
            })
            .collect();
        let mask = Tensor::from_vec(mask, (length, length), &self.device)?.to_dtype(self.dtype)?;

        let hidden =
            self.model
                .decoder()
                .forward(input_ids, Some(encoder_xs), past, &mask, cross_mask)?;
        // Only the last position can produce the next token. Projecting the
        // rest onto a 32k vocabulary is the most expensive way there is to
        // throw work away.
        let last = hidden.narrow(1, hidden.dim(1)? - 1, 1)?;
        Ok(self
            .lm_head
            .forward(&last)?
            .broadcast_add(&self.final_logits_bias)?)
    }
}

/// One segment on its way through a batch.
#[derive(Debug, Clone)]
struct Job {
    /// Where it goes in the caller's order, which the grouping does not keep.
    index: usize,
    source: Vec<u32>,
}

/// The `wanted` best continuations of the hypotheses in `rows`, best first.
///
/// `logprobs` is one row of `vocabulary` log probabilities per row of the
/// batch, and a continuation is scored by adding one of them to the hypothesis
/// it extends. Only the rows of one segment are ranked together: the segments
/// of a batch are separate searches that happen to share a forward pass. The
/// result is `(score, row, token)`.
fn best(
    logprobs: &[f32],
    vocabulary: usize,
    live: &[Hypothesis],
    rows: &[usize],
    wanted: usize,
) -> Vec<(f64, usize, u32)> {
    let mut ranked: Vec<(f64, usize, u32)> = Vec::with_capacity(rows.len() * vocabulary);
    for &row in rows {
        for (token, logprob) in logprobs[row * vocabulary..(row + 1) * vocabulary]
            .iter()
            .enumerate()
        {
            ranked.push((live[row].score + f64::from(*logprob), row, token as u32));
        }
    }

    let wanted = wanted.min(ranked.len());
    if wanted == 0 {
        return Vec::new();
    }
    ranked.select_nth_unstable_by(wanted - 1, |left, right| right.0.total_cmp(&left.0));
    ranked.truncate(wanted);
    ranked.sort_unstable_by(|left, right| right.0.total_cmp(&left.0));
    ranked
}

/// The weights of the pack: safetensors when the pack was converted, the
/// upstream pickle archive otherwise.
fn weight_file(pack: &Path) -> Result<PathBuf> {
    for name in [SAFETENSORS_FILE, PYTORCH_FILE] {
        let candidate = pack.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "neither {SAFETENSORS_FILE} nor {PYTORCH_FILE} is in the model pack at {}",
        pack.display()
    )
}

/// Checks that both tokenizers are there, without building either.
///
/// Building one is 30 MiB and is deferred with the weights, but a pack that is
/// missing one is a pack that will never translate anything, and the moment to
/// say so is while the caller can still fall back to English.
fn tokenizer_files(pack: &Path) -> Result<()> {
    for name in [SOURCE_TOKENIZER_FILE, TARGET_TOKENIZER_FILE] {
        let path = pack.join(name);
        if !path.is_file() {
            bail!("{} is missing from the model pack", path.display());
        }
    }
    Ok(())
}

/// Opens either weight format, lazily: one tensor is read, converted to
/// `dtype` and handed over each time the model asks for a name.
///
/// Laziness is what keeps the load flat. Reading the archive in one go would
/// hold the F16 file and the F32 model at the same time - and worse, FuguMT's
/// pickle stores the 32001x512 embedding under four names that all point at
/// one storage record, so an eager reader materialises it four times. Measured
/// peaks are in `docs/model-runtime-notes.md`.
fn load_weights<'a>(weights: &Path, dtype: DType, device: &Device) -> Result<VarBuilder<'a>> {
    let context = || format!("{} could not be read as weights", weights.display());

    let backend: Box<dyn SimpleBackend> = if weights
        .extension()
        .is_some_and(|extension| extension == "safetensors")
    {
        // `from_mmaped_safetensors` would avoid holding the file, but it is
        // `unsafe` (the file may change under the mapping) and this crate
        // forbids unsafe code. See `docs/model-runtime-notes.md` for what the
        // buffer costs and what mmap would save.
        let data = fs::read(weights).with_context(context)?;
        Box::new(candle_core::safetensors::BufferedSafetensors::new(data).with_context(context)?)
    } else {
        // Pickle, read directly: no Python, no torch, no conversion step. See
        // the converter's README for what this buys.
        Box::new(candle_core::pickle::PthTensors::new(weights, None).with_context(context)?)
    };

    Ok(VarBuilder::from_backend(
        Box::new(Weights::new(backend)),
        dtype,
        device.clone(),
    ))
}

/// A weight reader that hands out the same tensor every time a name is asked
/// for.
///
/// candle's lazy readers re-read and re-convert on every call, which matters
/// here because [`Engine::lm_head`] asks for the embedding matrix that
/// `marian::MTModel` is already holding. Without this, that second request
/// would cost another 65 MiB in F32. `Tensor` is a handle over shared storage,
/// so remembering one costs nothing beyond the map entry.
struct Weights {
    inner: Box<dyn SimpleBackend>,
    seen: Mutex<HashMap<String, Tensor>>,
}

impl Weights {
    fn new(inner: Box<dyn SimpleBackend>) -> Self {
        Self {
            inner,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// A poisoned lock is recovered from rather than propagated: what is under
    /// it is a hash lookup, and losing the model over it would be worse.
    fn remembered(&self, name: &str) -> Option<Tensor> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
    }

    fn remember(&self, name: &str, tensor: &Tensor) {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_owned(), tensor.clone());
    }
}

impl SimpleBackend for Weights {
    fn get(
        &self,
        shape: Shape,
        name: &str,
        hints: Init,
        dtype: DType,
        device: &Device,
    ) -> candle_core::Result<Tensor> {
        // A remembered tensor of the wrong shape falls through rather than
        // being reported here: the reader below raises the error it already
        // has the words for.
        if let Some(tensor) = self.remembered(name)
            && tensor.shape() == &shape
            && tensor.dtype() == dtype
        {
            return Ok(tensor);
        }

        let tensor = self.inner.get(shape, name, hints, dtype, device)?;
        self.remember(name, &tensor);
        Ok(tensor)
    }

    fn get_unchecked(
        &self,
        name: &str,
        dtype: DType,
        device: &Device,
    ) -> candle_core::Result<Tensor> {
        if let Some(tensor) = self.remembered(name)
            && tensor.dtype() == dtype
        {
            return Ok(tensor);
        }

        let tensor = self.inner.get_unchecked(name, dtype, device)?;
        self.remember(name, &tensor);
        Ok(tensor)
    }

    fn contains_tensor(&self, name: &str) -> bool {
        self.inner.contains_tensor(name)
    }
}

/// The pack's two tokenizers, as one instance when the two files are the same
/// file.
///
/// Marian trains a single SentencePiece model and publishes it once per
/// direction, and FuguMT's pair is identical byte for byte (md5
/// `32df5391...`). A `Tokenizer` built from either is ~30 MiB of vocabulary
/// and scores, so building both is 30 MiB spent on a duplicate. Comparing the
/// files is 4.8 MB of reading and cannot be wrong: two packs that really do
/// differ still get two tokenizers.
fn tokenizers(pack: &Path) -> Result<(Arc<Tokenizer>, Arc<Tokenizer>)> {
    let source_path = pack.join(SOURCE_TOKENIZER_FILE);
    let target_path = pack.join(TARGET_TOKENIZER_FILE);
    let source = Arc::new(tokenizer(&source_path)?);

    let identical = match (fs::read(&source_path), fs::read(&target_path)) {
        (Ok(left), Ok(right)) => left == right,
        // Unreadable here means unreadable below too, which is where the error
        // belongs.
        _ => false,
    };
    if identical {
        return Ok((Arc::clone(&source), source));
    }

    Ok((source, Arc::new(tokenizer(&target_path)?)))
}

fn tokenizer(path: &Path) -> Result<Tokenizer> {
    Tokenizer::from_file(path)
        .map_err(|error| anyhow!("{} is not a tokenizer: {error}", path.display()))
}

/// `0` for every token the model may emit and `-inf` for the two it may not.
///
/// Padding is forbidden because the upstream `generation_config.json` forbids
/// it (`bad_words_ids: [[32000]]`) and candle has no mechanism of its own.
///
/// The unknown token is forbidden on top of that, which upstream does not do.
/// A model that emits it puts the literal text `<unk>` in the middle of a
/// gloss - measured on `/** Closes the underlying stream. Idempotent. */` -
/// and a reader cannot tell that from a translation. Second best is a real
/// word.
fn forbidden_logits(
    config: &marian::Config,
    unknown: Option<u32>,
    dtype: DType,
    device: &Device,
) -> Result<Tensor> {
    let vocabulary = config.decoder_vocab_size.unwrap_or(config.vocab_size);
    let mut mask = vec![0f32; vocabulary];
    for token in [Some(config.pad_token_id), unknown].into_iter().flatten() {
        if (token as usize) < vocabulary {
            mask[token as usize] = f32::NEG_INFINITY;
        }
    }
    Ok(Tensor::from_vec(mask, vocabulary, device)?.to_dtype(dtype)?)
}

#[cfg(test)]
mod pack_tests {
    use std::fs;

    use super::*;

    const MANIFEST: &str = r#"{
        "model_id": "staka/fugumt-en-ja",
        "model_version": "fugumt-en-ja-test",
        "license": "CC-BY-SA-4.0",
        "attribution": "test",
        "files": {
            "NOTICE": {
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "bytes": 0
            }
        }
    }"#;

    /// FuguMT's own hyper-parameters, cut down to the fields
    /// `marian::Config` reads. Nothing is built from them here - they are only
    /// what makes the pack openable.
    const CONFIG: &str = r#"{
        "vocab_size": 32001,
        "decoder_vocab_size": 32001,
        "max_position_embeddings": 512,
        "encoder_layers": 6,
        "encoder_ffn_dim": 2048,
        "encoder_attention_heads": 8,
        "decoder_layers": 6,
        "decoder_ffn_dim": 2048,
        "decoder_attention_heads": 8,
        "use_cache": true,
        "is_encoder_decoder": true,
        "activation_function": "swish",
        "d_model": 512,
        "decoder_start_token_id": 32000,
        "scale_embedding": true,
        "pad_token_id": 32000,
        "eos_token_id": 0,
        "forced_eos_token_id": 0,
        "share_encoder_decoder_embeddings": true
    }"#;

    /// A directory of this test's own. The harness runs these in one process
    /// and in parallel, so a name shared between them is a name they race on.
    fn directory(test: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("codegloss-pack-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        path
    }

    /// A pack that can be opened and can never be loaded: a real manifest and
    /// a real config, and empty files standing in for the 120 MB of weights
    /// and the two tokenizers.
    ///
    /// That is what makes it a test of the deferral. Opening this pack has to
    /// succeed, which it can only do without reading the files that are junk;
    /// loading it has to fail, which is the other half of the state machine.
    fn deferred_pack(test: &str) -> std::path::PathBuf {
        let path = directory(test);
        fs::write(path.join(MANIFEST_FILE), MANIFEST).expect("the manifest is written");
        fs::write(path.join(CONFIG_FILE), CONFIG).expect("the config is written");
        for name in [PYTORCH_FILE, SOURCE_TOKENIZER_FILE, TARGET_TOKENIZER_FILE] {
            fs::write(path.join(name), b"").expect("the stand-in is written");
        }
        path
    }

    /// Opening a pack must not read its weights, and the version a cache key
    /// is built from must come out of the manifest.
    ///
    /// The pack below holds nothing that could be read as weights or as a
    /// tokenizer, so an open that succeeds is proof that neither was touched.
    #[test]
    fn opening_a_pack_reads_only_its_manifest() {
        let pack = deferred_pack("deferred-open");
        let translator = CandleTranslator::load_with_beams(&pack, Precision::Float32, 4)
            .expect("nothing that is read at open time is missing");

        assert_eq!(
            translator.model_version(),
            format!(
                "{ENGINE_VERSION}-{}-b4+fugumt-en-ja-test",
                Precision::Float32.as_str()
            )
        );
        let _ = fs::remove_dir_all(&pack);
    }

    /// A batch with nothing in it must not be what pays for the model.
    #[test]
    fn an_empty_batch_never_loads_the_weights() {
        let pack = deferred_pack("deferred-empty");
        let translator = CandleTranslator::load_with_beams(&pack, Precision::Float32, 4)
            .expect("the pack opens");

        let translations = translator
            .translate(&[])
            .expect("an empty batch is answered without loading anything");
        assert!(translations.is_empty());
        let _ = fs::remove_dir_all(&pack);
    }

    /// A load that fails must not move the cache key.
    ///
    /// The version is what every gloss already on disk is filed under. An
    /// engine that renamed itself on a bad day would orphan all of them, and
    /// the point of this change is that a warm cache keeps working.
    #[test]
    fn a_deferred_load_that_fails_keeps_the_model_version() {
        let pack = deferred_pack("deferred-failure");
        let translator = CandleTranslator::load_with_beams(&pack, Precision::Float32, 4)
            .expect("the pack opens");
        let before = translator.model_version().to_owned();

        translator
            .translate(&[Segment::new("Return the user.")])
            .expect_err("an empty file is not a Marian model");

        assert_eq!(translator.model_version(), before);
        let _ = fs::remove_dir_all(&pack);
    }

    /// A pack that cannot be read must be read once, not once per batch.
    ///
    /// The weight path was resolved when the pack was opened, so a retry would
    /// go looking for a file that is no longer there and say something else.
    /// Two identical messages are therefore proof that the second call read
    /// nothing at all.
    #[test]
    fn a_failed_load_is_not_retried() {
        let pack = deferred_pack("deferred-sticky");
        let translator = CandleTranslator::load_with_beams(&pack, Precision::Float32, 4)
            .expect("the pack opens");

        let first = translator
            .translate(&[Segment::new("Return the user.")])
            .expect_err("an empty file is not a Marian model");
        fs::remove_file(pack.join(PYTORCH_FILE)).expect("the weights are removed");
        let second = translator
            .translate(&[Segment::new("Return the user.")])
            .expect_err("a failed engine keeps failing");

        assert_eq!(format!("{first:#}"), format!("{second:#}"));
        let _ = fs::remove_dir_all(&pack);
    }

    /// An incomplete pack has to be refused while the caller can still fall
    /// back to English, not at the first gloss.
    #[test]
    fn a_pack_missing_a_tokenizer_is_refused_when_it_is_opened() {
        let pack = deferred_pack("deferred-tokenizer");
        fs::remove_file(pack.join(TARGET_TOKENIZER_FILE)).expect("the tokenizer is removed");

        let error = CandleTranslator::load_with_beams(&pack, Precision::Float32, 4)
            .expect_err("half a pack is not a pack");
        assert!(
            format!("{error:#}").contains(TARGET_TOKENIZER_FILE),
            "{error:#}"
        );
        let _ = fs::remove_dir_all(&pack);
    }

    #[test]
    fn a_pack_that_matches_its_manifest_verifies() {
        let manifest = Manifest::parse(MANIFEST).expect("the manifest parses");
        let directory = directory("matches");
        fs::write(directory.join("NOTICE"), b"").expect("the file is written");

        manifest
            .verify(&directory)
            .expect("an empty file hashes to the empty digest");
        let _ = fs::remove_dir_all(&directory);
    }

    /// The failure a download has to be caught by: right name, wrong bytes.
    #[test]
    fn a_file_of_the_wrong_length_is_rejected() {
        let manifest = Manifest::parse(MANIFEST).expect("the manifest parses");
        let directory = directory("length");
        fs::write(directory.join("NOTICE"), b"truncated").expect("the file is written");

        let error = manifest
            .verify(&directory)
            .expect_err("the length is wrong");
        assert!(format!("{error}").contains("bytes"), "{error}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_missing_file_is_rejected() {
        let manifest = Manifest::parse(MANIFEST).expect("the manifest parses");
        let directory = directory("missing");

        let error = manifest
            .verify(&directory)
            .expect_err("the file is not there");
        assert!(format!("{error}").contains("missing"), "{error}");
        let _ = fs::remove_dir_all(&directory);
    }

    /// A pack built before the manifest carried digests cannot be checked, and
    /// an unverifiable pack is refused rather than trusted.
    #[test]
    fn a_manifest_without_files_is_refused() {
        let manifest = Manifest::parse(
            r#"{"model_id":"x","model_version":"y","license":"z","attribution":"w","files":{}}"#,
        )
        .expect("the manifest parses");
        let error = manifest
            .verify(std::path::Path::new("."))
            .expect_err("nothing can be checked");
        assert!(format!("{error}").contains("no files"), "{error}");
    }
}
