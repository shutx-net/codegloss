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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
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
/// without the model pack changing: greedy decoding, the forbidden tokens, the
/// dtype. Bump it whenever one of those moves, or the cache keeps serving what
/// the old code produced.
pub const ENGINE_VERSION: &str = "candle-marian-1";

/// The vocabulary entry a tokenizer uses for text it cannot represent.
const UNKNOWN_TOKEN: &str = "<unk>";

/// Hard stop on the decoder loop.
///
/// Greedy decoding has no beam to fall back on and can repeat itself forever
/// on an input the model does not understand. A comment that needs more than
/// this many tokens is not a comment.
const MAX_NEW_TOKENS: usize = 512;

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
    source: Tokenizer,
    target: Tokenizer,
    device: Device,
    /// Added to the logits before picking a token: `0` everywhere and `-inf`
    /// on the tokens the model must never emit.
    forbidden: Tensor,
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
        let weights = weight_file(pack)?;
        let variables = load_weights(&weights, &device)?;
        let model = marian::MTModel::new(&config, variables)
            .with_context(|| format!("{} does not hold a Marian model", weights.display()))?;

        let source = tokenizer(&pack.join(SOURCE_TOKENIZER_FILE))?;
        let target = tokenizer(&pack.join(TARGET_TOKENIZER_FILE))?;

        // `bad_words_ids` in the upstream config forbids exactly one token, the
        // padding one. candle has no such mechanism, so it is applied here.
        let forbidden = forbidden_logits(&config, target.token_to_id(UNKNOWN_TOKEN), &device)?;

        tracing::info!(
            model = %manifest.model_id,
            version = %manifest.model_version,
            weights = %weights.display(),
            "loaded the translation model"
        );

        Ok(Self {
            model_version: format!("{ENGINE_VERSION}+{}", manifest.model_version),
            manifest,
            engine: Mutex::new(Engine {
                config,
                model,
                source,
                target,
                device,
                forbidden,
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
        let mut token_ids = vec![self.config.decoder_start_token_id];

        for index in 0..MAX_NEW_TOKENS {
            // After the first pass the KV cache holds everything before the
            // last token, so only that token is fed back in.
            let context_size = if index >= 1 { 1 } else { token_ids.len() };
            let start_pos = token_ids.len().saturating_sub(context_size);
            let input_ids = Tensor::new(&token_ids[start_pos..], &self.device)?.unsqueeze(0)?;

            let logits = self.model.decode(&input_ids, &encoder_xs, start_pos)?;
            let logits = logits.squeeze(0)?;
            let logits = logits.get(logits.dim(0)? - 1)?;
            let logits = logits.broadcast_add(&self.forbidden)?;

            // Greedy: `argmax`, not a sampler. Nothing about a translation
            // wants randomness, and a deterministic engine is what lets the
            // cache and the fixtures mean anything.
            let token = logits.argmax(0)?.to_scalar::<u32>()?;
            if token == self.config.eos_token_id || token == self.config.forced_eos_token_id {
                break;
            }
            token_ids.push(token);
        }

        // The start token is the decoder's, not the sentence's.
        self.target
            .decode(&token_ids[1..], true)
            .map_err(|error| anyhow!("the translation could not be decoded: {error}"))
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

/// Reads either weight format into a `VarBuilder`.
///
/// F32 in both cases. FuguMT is published as F16 and candle's Marian runs the
/// attention in the tensors' own dtype; F16 on a CPU is emulated and slower,
/// not faster.
fn load_weights<'a>(weights: &Path, device: &Device) -> Result<VarBuilder<'a>> {
    let context = || format!("{} could not be read as weights", weights.display());

    if weights
        .extension()
        .is_some_and(|extension| extension == "safetensors")
    {
        // `from_mmaped_safetensors` is the faster route and is what candle's
        // own example uses, but it is `unsafe` (the file may change under the
        // mapping) and this crate forbids unsafe code. Reading the file costs
        // one pass over ~120 MB, once per process.
        let data = fs::read(weights).with_context(context)?;
        return VarBuilder::from_buffered_safetensors(data, DType::F32, device)
            .with_context(context);
    }

    // Pickle, read directly: no Python, no torch, no conversion step. See the
    // converter's README for what this buys.
    VarBuilder::from_pth(weights, DType::F32, device).with_context(context)
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
    device: &Device,
) -> Result<Tensor> {
    let vocabulary = config.decoder_vocab_size.unwrap_or(config.vocab_size);
    let mut mask = vec![0f32; vocabulary];
    for token in [Some(config.pad_token_id), unknown].into_iter().flatten() {
        if (token as usize) < vocabulary {
            mask[token as usize] = f32::NEG_INFINITY;
        }
    }
    Ok(Tensor::from_vec(mask, vocabulary, device)?)
}
