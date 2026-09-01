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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, Shape, Tensor};
use candle_nn::var_builder::SimpleBackend;
use candle_nn::{Init, Linear, Module, VarBuilder};
use candle_transformers::models::marian;
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
pub const ENGINE_VERSION: &str = "candle-marian-2";

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
}

impl Manifest {
    fn read(pack: &Path) -> Result<Self> {
        let path = pack.join(MANIFEST_FILE);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "{} is missing from the model pack at {}; produce the pack with tools/convert-fugumt",
                MANIFEST_FILE,
                pack.display()
            )
        })?;
        serde_json::from_str(&text).with_context(|| format!("{} is not valid", path.display()))
    }
}

/// FuguMT, loaded and ready to translate.
///
/// The model is loaded once, when this is constructed, and held for the life
/// of the process: the weights are ~120 MB and re-reading them per batch would
/// dwarf the inference itself.
pub struct CandleTranslator {
    manifest: Manifest,
    /// [`ENGINE_VERSION`] and the pack's version together: a translation
    /// depends on both, so a cache key has to depend on both.
    model_version: String,
    /// `marian::MTModel` needs `&mut self` for every forward pass because it
    /// owns the KV cache, while `Translator::translate` takes `&self`. One
    /// mutex reconciles the two. Translation is serialised as a result, which
    /// costs nothing: `codegloss-lsp` runs exactly one worker.
    engine: Mutex<Engine>,
}

impl std::fmt::Debug for CandleTranslator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CandleTranslator")
            .field("model_id", &self.manifest.model_id)
            .field("model_version", &self.model_version)
            .finish_non_exhaustive()
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
    /// `marian::MTModel::decode` owns a pair of these already, but it also
    /// builds its causal mask in F32 and adds it to the attention weights -
    /// which is a dtype mismatch the moment the model is not F32, and there is
    /// no way to hand it a mask. Driving `MTModel::decoder` directly is what
    /// makes [`Precision`] a choice at all. It costs nothing: `VarBuilder`
    /// hands out tensors that share their storage, so this is the same
    /// embedding matrix the model is using, not a second copy.
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
    /// Loads the model pack in `pack`.
    ///
    /// Everything that can go wrong goes wrong here, which is the point: a
    /// caller can fall back to [`PassthroughTranslator`] on an error and know
    /// that the engine it did get will not fail later.
    ///
    /// [`PassthroughTranslator`]: crate::PassthroughTranslator
    pub fn load(pack: impl AsRef<Path>) -> Result<Self> {
        Self::load_with(pack, Precision::default())
    }

    /// Loads the model pack in `pack`, holding the weights in `precision`.
    ///
    /// See [`load`](CandleTranslator::load); this is the same thing with the
    /// numeric type spelled out. The precision is part of
    /// [`model_version`](Translator::model_version), so translations produced
    /// under one are never served for another.
    pub fn load_with(pack: impl AsRef<Path>, precision: Precision) -> Result<Self> {
        Self::load_with_beams(pack, precision, DEFAULT_BEAMS)
    }

    /// Loads the model pack in `pack`, holding the weights in `precision` and
    /// searching `beams` hypotheses wide.
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

        let device = Device::Cpu;
        let dtype = precision.dtype();
        let weights = weight_file(pack)?;
        let variables = load_weights(&weights, dtype, &device)?;
        let context = || format!("{} does not hold a Marian model", weights.display());

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

        let (source, target) = tokenizers(pack)?;

        // `bad_words_ids` in the upstream config forbids exactly one token, the
        // padding one. candle has no such mechanism, so it is applied here.
        let forbidden =
            forbidden_logits(&config, target.token_to_id(UNKNOWN_TOKEN), dtype, &device)?;

        tracing::info!(
            model = %manifest.model_id,
            version = %manifest.model_version,
            weights = %weights.display(),
            precision = %precision,
            beams,
            "loaded the translation model"
        );

        Ok(Self {
            model_version: format!(
                "{ENGINE_VERSION}-{precision}-b{beams}+{}",
                manifest.model_version
            ),
            manifest,
            engine: Mutex::new(Engine {
                config,
                model,
                source,
                target,
                device,
                dtype,
                lm_head,
                final_logits_bias,
                forbidden,
                beams,
            }),
        })
    }

    /// What the pack says about the weights, for anyone that has to reproduce
    /// the attribution the licence asks for.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl Translator for CandleTranslator {
    fn translate(&self, segments: &[Segment]) -> Result<Vec<String>> {
        let mut engine = self
            .engine
            .lock()
            .map_err(|_| anyhow!("the translation engine was poisoned by an earlier panic"))?;

        segments
            .iter()
            .map(|segment| engine.translate(segment.text()))
            .collect()
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }
}

impl Engine {
    /// Translates one segment, greedily.
    ///
    /// One segment at a time rather than a padded batch: batching Marian means
    /// padding the encoder input and masking the pad positions, and candle's
    /// `marian` has no attention mask on the encoder. Getting that wrong is
    /// silent - the translation is merely worse - so v0.1 does not do it.
    fn translate(&mut self, text: &str) -> Result<String> {
        // Nothing to say, and an empty encoder input makes the model produce a
        // sentence out of nowhere.
        if text.trim().is_empty() {
            return Ok(text.to_owned());
        }

        // The KV cache holds the previous sentence. Not resetting it is the
        // classic way to have translation number two contaminated by number
        // one.
        self.model.reset_kv_cache();

        let encoder_xs = self.encode(text)?;
        let tokens = if self.beams > 1 {
            self.search(&encoder_xs)?
        } else {
            self.greedy(&encoder_xs)?
        };

        // The start token is the decoder's, not the sentence's.
        self.target
            .decode(&tokens[1..], true)
            .map_err(|error| anyhow!("the translation could not be decoded: {error}"))
    }

    /// Takes the most likely token at every step, and never reconsiders.
    ///
    /// Cheap, and wrong in one specific way: the end-of-sentence token is
    /// often the single most likely next token part way through a sentence,
    /// and taking it there truncates the translation with no sign that
    /// anything is missing. [`Engine::search`] is what that costs, and what it
    /// buys.
    fn greedy(&mut self, encoder_xs: &Tensor) -> Result<Vec<u32>> {
        let mut tokens = vec![self.config.decoder_start_token_id];

        for index in 0..MAX_NEW_TOKENS {
            // After the first pass the KV cache holds everything before the
            // last token, so only that token is fed back in.
            let context_size = if index >= 1 { 1 } else { tokens.len() };
            let start_pos = tokens.len().saturating_sub(context_size);
            let input_ids = Tensor::new(&tokens[start_pos..], &self.device)?.unsqueeze(0)?;

            let logits = self.decode(&input_ids, encoder_xs, start_pos)?;
            let logits = logits.squeeze(1)?.squeeze(0)?;
            let logits = logits.broadcast_add(&self.forbidden)?;

            // `argmax`, not a sampler. Nothing about a translation wants
            // randomness, and a deterministic engine is what lets the cache and
            // the fixtures mean anything.
            let token = logits.argmax(0)?.to_scalar::<u32>()?;
            if self.is_end(token) {
                break;
            }
            tokens.push(token);
        }

        Ok(tokens)
    }

    /// Keeps [`beams`](Engine::beams) hypotheses alive and returns the best of
    /// the finished ones.
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
    /// IMPORTANT: there is no incremental KV cache here. candle's `marian`
    /// keeps the cache private and offers only `reset_kv_cache`, so there is no
    /// way to permute it when the beams are reordered - and a cache carried
    /// across a reordering silently attributes one beam's prefix to another.
    /// The prefix is therefore re-read at every step, which is quadratic in the
    /// length of the translation. What that costs is measured in
    /// `docs/model-runtime-notes.md`; taking it back means forking `marian`,
    /// which is the same fork batching needs.
    fn search(&mut self, encoder_xs: &Tensor) -> Result<Vec<u32>> {
        let (_, source_len, model_dim) = encoder_xs.dims3()?;
        // Every beam translates the same sentence, so they all attend to the
        // same encoder states.
        let encoder_xs = encoder_xs
            .broadcast_as((self.beams, source_len, model_dim))?
            .contiguous()?;

        let mut live = vec![Hypothesis {
            tokens: vec![self.config.decoder_start_token_id],
            score: 0.0,
        }];
        let mut finished: Vec<Hypothesis> = Vec::with_capacity(self.beams);

        for _ in 0..MAX_NEW_TOKENS {
            let width = live.len();
            let length = live[0].tokens.len();

            self.model.reset_kv_cache();
            let prefixes: Vec<u32> = live
                .iter()
                .flat_map(|hypothesis| hypothesis.tokens.iter().copied())
                .collect();
            let input_ids = Tensor::from_vec(prefixes, (width, length), &self.device)?;

            let logits = self.decode(&input_ids, &encoder_xs.narrow(0, 0, width)?, 0)?;
            let logits = logits.squeeze(1)?.broadcast_add(&self.forbidden)?;
            // In F32 whatever the weights are held in: a sum of log
            // probabilities over a whole sentence is exactly the accumulation
            // F16 is bad at.
            let logprobs = candle_nn::ops::log_softmax(&logits.to_dtype(DType::F32)?, 1)?;
            let logprobs: Vec<f32> = logprobs.flatten_all()?.to_vec1()?;
            let vocabulary = logprobs.len() / width;

            let mut next = Vec::with_capacity(self.beams);
            for (score, beam, token) in best(&logprobs, vocabulary, &live, 2 * self.beams) {
                let mut tokens = live[beam].tokens.clone();
                if self.is_end(token) {
                    finished.push(Hypothesis { tokens, score });
                } else if next.len() < self.beams {
                    tokens.push(token);
                    next.push(Hypothesis { tokens, score });
                }
            }

            if finished.len() >= self.beams || next.is_empty() {
                live = next;
                break;
            }
            live = next;
        }

        // Nothing finished inside the budget: the best hypothesis still running
        // is a truncated translation, but it is the translation.
        let best = finished
            .iter()
            .chain(live.iter())
            .max_by(|left, right| left.normalised().total_cmp(&right.normalised()))
            .ok_or_else(|| anyhow!("the search ended with no hypothesis at all"))?;
        Ok(best.tokens.clone())
    }

    fn is_end(&self, token: u32) -> bool {
        token == self.config.eos_token_id || token == self.config.forced_eos_token_id
    }

    /// One decoder step, plus the projection onto the vocabulary.
    ///
    /// This is `marian::MTModel::decode` with the causal mask built in the
    /// model's own dtype instead of always in F32 - see [`Engine::lm_head`].
    fn decode(&mut self, input_ids: &Tensor, encoder_xs: &Tensor, past: usize) -> Result<Tensor> {
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

        let hidden = self
            .model
            .decoder()
            .forward(input_ids, Some(encoder_xs), past, &mask)?;
        // Only the last position can produce the next token. Projecting the
        // rest onto a 32k vocabulary is the most expensive way there is to
        // throw work away, and the search feeds whole prefixes.
        let last = hidden.narrow(1, hidden.dim(1)? - 1, 1)?;
        Ok(self
            .lm_head
            .forward(&last)?
            .broadcast_add(&self.final_logits_bias)?)
    }

    fn encode(&mut self, text: &str) -> Result<Tensor> {
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

        let tokens = Tensor::new(tokens.as_slice(), &self.device)?.unsqueeze(0)?;
        Ok(self.model.encoder().forward(&tokens, 0)?)
    }
}

/// The `wanted` best continuations of `live`, best first.
///
/// `logprobs` is one row of `vocabulary` log probabilities per hypothesis, and
/// a continuation is scored by adding one of them to the hypothesis it extends.
/// The result is `(score, hypothesis, token)`.
fn best(
    logprobs: &[f32],
    vocabulary: usize,
    live: &[Hypothesis],
    wanted: usize,
) -> Vec<(f64, usize, u32)> {
    let mut ranked: Vec<(f64, usize, u32)> = Vec::with_capacity(logprobs.len());
    for (beam, hypothesis) in live.iter().enumerate() {
        for (token, logprob) in logprobs[beam * vocabulary..(beam + 1) * vocabulary]
            .iter()
            .enumerate()
        {
            ranked.push((hypothesis.score + f64::from(*logprob), beam, token as u32));
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
