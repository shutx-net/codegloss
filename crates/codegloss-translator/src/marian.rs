//! Marian NMT, forked from candle so that the decoder can be driven properly.
//!
//! A copy of `candle_transformers::models::marian` (candle 0.11.0, MIT OR
//! Apache-2.0) with three changes. Each one is something the upstream module
//! makes impossible from outside rather than merely inconvenient, which is the
//! bar for carrying a copy at all:
//!
//! 1. [`Decoder::reorder_kv_cache`]. Upstream keeps `kv_cache` private and
//!    exposes only `reset_kv_cache`, so a beam search has no way to move a
//!    cached prefix when the beams are reordered - and one left in place is
//!    wrong in the worst way, a fluent translation of a sentence nobody wrote.
//!    Without this the prefix has to be re-read at every step, which is
//!    quadratic in the length of the translation.
//! 2. An attention mask on the encoder ([`Encoder::forward`]) and on
//!    cross-attention ([`Decoder::forward`]). Upstream passes `None` to both,
//!    so padding a batch makes the model attend to the pad positions and the
//!    translations get quietly worse. This is what `docs/model-runtime-notes.md`
//!    §6.5 measured and gave up on.
//! 3. `EncoderLayer` builds its attention with `is_decoder: false`. Upstream
//!    passes `true`, which gives an encoder layer a KV cache it can never use
//!    and sizes its heads from `decoder_attention_heads`.
//!
//! `MTModel::decode` is not carried over: it builds its causal mask in F32
//! whatever dtype the weights are in, which is why `candle.rs` drove
//! `MTModel::decoder` directly even before this fork. `lm_head` and
//! `final_logits_bias` went with it - [`crate::CandleTranslator`] reads its own
//! from the same `VarBuilder`, and they share their storage.
//!
//! Everything else is upstream's, including the parts this crate does not use.
//! Keeping it whole is what makes the next upstream release diffable against
//! this file.
//!
//! Upstream: "Marian: Fast Neural Machine Translation in C++",
//! Junczys-Dowmunt et al. 2018 - <https://aclanthology.org/P18-4020/>.

#[cfg(test)]
use candle_core::IndexOp;
use candle_core::{Result, Tensor};
use candle_nn::{Embedding, LayerNorm, Linear, VarBuilder, embedding, layer_norm, linear};

/// Every field is read out of the pack's `config.json`, so every field has to
/// be declared here whether or not this crate goes on to look at it.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub decoder_vocab_size: Option<usize>,
    pub max_position_embeddings: usize,
    pub encoder_layers: usize,
    pub encoder_ffn_dim: usize,
    pub encoder_attention_heads: usize,
    pub decoder_layers: usize,
    pub decoder_ffn_dim: usize,
    pub decoder_attention_heads: usize,
    pub use_cache: bool,
    pub is_encoder_decoder: bool,
    pub activation_function: candle_nn::Activation,
    pub d_model: usize,
    pub decoder_start_token_id: u32,
    pub scale_embedding: bool,
    pub pad_token_id: u32,
    pub eos_token_id: u32,
    pub forced_eos_token_id: u32,
    pub share_encoder_decoder_embeddings: bool,
}

#[derive(Debug, Clone)]
struct SinusoidalPositionalEmbedding {
    emb: Embedding,
}

impl SinusoidalPositionalEmbedding {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let dev = vb.device();
        let dtype = vb.dtype();
        let num_positions = cfg.max_position_embeddings;
        let dim = cfg.d_model;
        let inv_freq: Vec<_> = (0..dim)
            .step_by(2)
            .map(|i| 1f32 / 10000f32.powf(i as f32 / dim as f32))
            .collect();
        let inv_freq_len = inv_freq.len();
        let inv_freq = Tensor::from_vec(inv_freq, (1, inv_freq_len), dev)?.to_dtype(dtype)?;
        let t = Tensor::arange(0u32, num_positions as u32, dev)?
            .to_dtype(dtype)?
            .reshape((num_positions, 1))?;
        let freqs = t.matmul(&inv_freq)?;
        let sin = freqs.sin()?;
        let cos = freqs.cos()?;
        let weights = Tensor::cat(&[&sin, &cos], 1)?.contiguous()?;
        let emb = Embedding::new(weights, cfg.d_model);
        Ok(Self { emb })
    }

    fn forward(&self, input_ids: &Tensor, past_kv_len: usize) -> Result<Tensor> {
        let seq_len = input_ids.dim(1)?;
        Tensor::arange(
            past_kv_len as u32,
            (past_kv_len + seq_len) as u32,
            input_ids.device(),
        )?
        .apply(&self.emb)
    }
}

#[derive(Debug, Clone)]
struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    out_proj: Linear,
    scaling: f64,
    num_heads: usize,
    head_dim: usize,
    kv_cache: Option<(Tensor, Tensor)>,
    is_decoder: bool,
}

impl Attention {
    fn new(cfg: &Config, is_decoder: bool, vb: VarBuilder) -> Result<Self> {
        let num_heads = if is_decoder {
            cfg.decoder_attention_heads
        } else {
            cfg.encoder_attention_heads
        };
        let embed_dim = cfg.d_model;
        let head_dim = embed_dim / num_heads;
        let scaling = (head_dim as f64).powf(-0.5);
        let q_proj = linear(embed_dim, embed_dim, vb.pp("q_proj"))?;
        let k_proj = linear(embed_dim, embed_dim, vb.pp("k_proj"))?;
        let v_proj = linear(embed_dim, embed_dim, vb.pp("v_proj"))?;
        let out_proj = linear(embed_dim, embed_dim, vb.pp("out_proj"))?;
        Ok(Self {
            q_proj,
            k_proj,
            v_proj,
            out_proj,
            scaling,
            num_heads,
            head_dim,
            kv_cache: None,
            is_decoder,
        })
    }

    fn _shape(&self, tensor: &Tensor, bsz: usize) -> Result<Tensor> {
        tensor
            .reshape((bsz, (), self.num_heads, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()
    }

    fn forward(
        &mut self,
        xs: &Tensor,
        kv_states: Option<&Tensor>,
        attn_mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let (b_sz, tgt_len, _) = xs.dims3()?;
        let query_states = (xs.apply(&self.q_proj)? * self.scaling)?;
        let (key_states, value_states) = match kv_states {
            None => {
                let key_states = self._shape(&xs.apply(&self.k_proj)?, b_sz)?;
                let value_states = self._shape(&xs.apply(&self.v_proj)?, b_sz)?;
                if self.is_decoder {
                    let kv_states = match &self.kv_cache {
                        None => (key_states, value_states),
                        Some((p_key_states, p_value_states)) => {
                            let key_states = Tensor::cat(&[p_key_states, &key_states], 2)?;
                            let value_states = Tensor::cat(&[p_value_states, &value_states], 2)?;
                            (key_states, value_states)
                        }
                    };
                    self.kv_cache = Some(kv_states.clone());
                    kv_states
                } else {
                    (key_states, value_states)
                }
            }
            Some(kv_states) => {
                let key_states = self._shape(&kv_states.apply(&self.k_proj)?, b_sz)?;
                let value_states = self._shape(&kv_states.apply(&self.v_proj)?, b_sz)?;
                (key_states, value_states)
            }
        };
        let proj_shape = (b_sz * self.num_heads, (), self.head_dim);
        let query_states = self._shape(&query_states, b_sz)?.reshape(proj_shape)?;
        let key_states = key_states.reshape(proj_shape)?;
        let value_states = value_states.reshape(proj_shape)?;
        let attn_weights = query_states.matmul(&key_states.transpose(1, 2)?)?;
        let attn_weights = match attn_mask {
            None => attn_weights,
            Some(attn_mask) => attn_weights.broadcast_add(attn_mask)?,
        };
        let attn_probs = candle_nn::ops::softmax_last_dim(&attn_weights)?;
        let attn_output = attn_probs.matmul(&value_states)?;
        attn_output
            .reshape((b_sz, self.num_heads, tgt_len, self.head_dim))?
            .transpose(1, 2)?
            .reshape((b_sz, tgt_len, self.head_dim * self.num_heads))?
            .apply(&self.out_proj)
    }

    fn reset_kv_cache(&mut self) {
        self.kv_cache = None
    }

    /// Rearranges the cached keys and values along the batch dimension.
    ///
    /// `beams` holds, for each row of the new batch, the row of the old batch
    /// it continues.
    fn reorder_kv_cache(&mut self, beams: &Tensor) -> Result<()> {
        if let Some((keys, values)) = &self.kv_cache {
            self.kv_cache = Some((
                keys.index_select(beams, 0)?.contiguous()?,
                values.index_select(beams, 0)?.contiguous()?,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct EncoderLayer {
    self_attn: Attention,
    self_attn_layer_norm: LayerNorm,
    activation_fn: candle_nn::Activation,
    fc1: Linear,
    fc2: Linear,
    final_layer_norm: LayerNorm,
}

impl EncoderLayer {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        // Upstream passes `true` here. An encoder layer then keeps a KV
        // cache it can never use - the encoder sees the whole source at
        // once - and sizes its heads from `decoder_attention_heads`. Both
        // are invisible on a model whose two head counts agree, which
        // FuguMT's do, and neither is what the code means.
        let self_attn = Attention::new(cfg, false, vb.pp("self_attn"))?;
        let self_attn_layer_norm = layer_norm(cfg.d_model, 1e-5, vb.pp("self_attn_layer_norm"))?;
        let fc1 = linear(cfg.d_model, cfg.encoder_ffn_dim, vb.pp("fc1"))?;
        let fc2 = linear(cfg.encoder_ffn_dim, cfg.d_model, vb.pp("fc2"))?;
        let final_layer_norm = layer_norm(cfg.d_model, 1e-5, vb.pp("final_layer_norm"))?;
        Ok(Self {
            self_attn,
            self_attn_layer_norm,
            activation_fn: cfg.activation_function,
            fc1,
            fc2,
            final_layer_norm,
        })
    }

    fn forward(&mut self, xs: &Tensor, attn_mask: Option<&Tensor>) -> Result<Tensor> {
        let residual = xs;
        let xs = (self.self_attn.forward(xs, None, attn_mask)? + residual)?
            .apply(&self.self_attn_layer_norm)?;
        let residual = &xs;
        let xs = xs
            .apply(&self.fc1)?
            .apply(&self.activation_fn)?
            .apply(&self.fc2)?;
        (xs + residual)?.apply(&self.final_layer_norm)
    }

    fn reset_kv_cache(&mut self) {
        self.self_attn.reset_kv_cache()
    }
}

#[derive(Debug, Clone)]
struct DecoderLayer {
    self_attn: Attention,
    self_attn_layer_norm: LayerNorm,
    activation_fn: candle_nn::Activation,
    encoder_attn: Attention,
    encoder_attn_layer_norm: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    final_layer_norm: LayerNorm,
}

impl DecoderLayer {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let self_attn = Attention::new(cfg, true, vb.pp("self_attn"))?;
        let self_attn_layer_norm = layer_norm(cfg.d_model, 1e-5, vb.pp("self_attn_layer_norm"))?;
        let encoder_attn = Attention::new(cfg, true, vb.pp("encoder_attn"))?;
        let encoder_attn_layer_norm =
            layer_norm(cfg.d_model, 1e-5, vb.pp("encoder_attn_layer_norm"))?;
        let fc1 = linear(cfg.d_model, cfg.decoder_ffn_dim, vb.pp("fc1"))?;
        let fc2 = linear(cfg.decoder_ffn_dim, cfg.d_model, vb.pp("fc2"))?;
        let final_layer_norm = layer_norm(cfg.d_model, 1e-5, vb.pp("final_layer_norm"))?;
        Ok(Self {
            self_attn,
            self_attn_layer_norm,
            activation_fn: cfg.activation_function,
            encoder_attn,
            encoder_attn_layer_norm,
            fc1,
            fc2,
            final_layer_norm,
        })
    }

    fn forward(
        &mut self,
        xs: &Tensor,
        encoder_xs: Option<&Tensor>,
        attn_mask: &Tensor,
        encoder_attn_mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let residual = xs;
        let xs = (self.self_attn.forward(xs, None, Some(attn_mask))? + residual)?
            .apply(&self.self_attn_layer_norm)?;
        let xs = match encoder_xs {
            None => xs,
            Some(encoder_xs) => {
                let residual = &xs;
                let xs = self
                    .encoder_attn
                    .forward(&xs, Some(encoder_xs), encoder_attn_mask)?;
                (residual + xs)?.apply(&self.encoder_attn_layer_norm)?
            }
        };
        let residual = &xs;
        let xs = xs
            .apply(&self.fc1)?
            .apply(&self.activation_fn)?
            .apply(&self.fc2)?;
        let xs = (xs + residual)?.apply(&self.final_layer_norm)?;
        Ok(xs)
    }

    fn reset_kv_cache(&mut self) {
        self.self_attn.reset_kv_cache();
        self.encoder_attn.reset_kv_cache()
    }

    fn reorder_kv_cache(&mut self, beams: &Tensor) -> Result<()> {
        self.self_attn.reorder_kv_cache(beams)?;
        // Cross-attention takes its keys and values from `encoder_xs` on every
        // call rather than from the cache, so there is nothing there to move.
        // Reordering it anyway keeps the two in step if that ever changes.
        self.encoder_attn.reorder_kv_cache(beams)
    }
}

#[derive(Debug, Clone)]
pub struct Encoder {
    embed_tokens: Embedding,
    embed_positions: SinusoidalPositionalEmbedding,
    layers: Vec<EncoderLayer>,
    embed_scale: Option<f64>,
}

impl Encoder {
    fn new(cfg: &Config, embed_tokens: &Embedding, vb: VarBuilder) -> Result<Self> {
        let embed_positions = SinusoidalPositionalEmbedding::new(cfg, vb.pp("embed_positions"))?;
        let mut layers = Vec::with_capacity(cfg.encoder_layers);
        let vb_l = vb.pp("layers");
        for idx in 0..cfg.encoder_layers {
            let layer = EncoderLayer::new(cfg, vb_l.pp(idx))?;
            layers.push(layer)
        }
        let embed_scale = if cfg.scale_embedding {
            Some((cfg.d_model as f64).sqrt())
        } else {
            None
        };
        Ok(Self {
            embed_tokens: embed_tokens.clone(),
            embed_positions,
            layers,
            embed_scale,
        })
    }

    pub fn forward(
        &mut self,
        xs: &Tensor,
        past_kv_len: usize,
        attn_mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let xs = xs.apply(&self.embed_tokens)?;
        let xs = match self.embed_scale {
            None => xs,
            Some(scale) => (xs * scale)?,
        };
        let embed_pos = self
            .embed_positions
            .forward(&xs, past_kv_len)?
            .unsqueeze(0)?;
        let mut xs = xs.broadcast_add(&embed_pos)?;
        for layer in self.layers.iter_mut() {
            xs = layer.forward(&xs, attn_mask)?
        }
        Ok(xs)
    }

    pub fn reset_kv_cache(&mut self) {
        for layer in self.layers.iter_mut() {
            layer.reset_kv_cache()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Decoder {
    embed_tokens: Embedding,
    embed_positions: SinusoidalPositionalEmbedding,
    layers: Vec<DecoderLayer>,
    embed_scale: Option<f64>,
}

impl Decoder {
    fn new(cfg: &Config, embed_tokens: &Embedding, vb: VarBuilder) -> Result<Self> {
        let embed_positions = SinusoidalPositionalEmbedding::new(cfg, vb.pp("embed_positions"))?;
        let mut layers = Vec::with_capacity(cfg.decoder_layers);
        let vb_l = vb.pp("layers");
        for idx in 0..cfg.decoder_layers {
            let layer = DecoderLayer::new(cfg, vb_l.pp(idx))?;
            layers.push(layer)
        }
        let embed_scale = if cfg.scale_embedding {
            Some((cfg.d_model as f64).sqrt())
        } else {
            None
        };
        Ok(Self {
            embed_tokens: embed_tokens.clone(),
            embed_positions,
            layers,
            embed_scale,
        })
    }

    pub fn forward(
        &mut self,
        xs: &Tensor,
        encoder_xs: Option<&Tensor>,
        past_kv_len: usize,
        attn_mask: &Tensor,
        encoder_attn_mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let xs = xs.apply(&self.embed_tokens)?;
        let xs = match self.embed_scale {
            None => xs,
            Some(scale) => (xs * scale)?,
        };
        let embed_pos = self
            .embed_positions
            .forward(&xs, past_kv_len)?
            .unsqueeze(0)?;
        let mut xs = xs.broadcast_add(&embed_pos)?;
        for layer in self.layers.iter_mut() {
            xs = layer.forward(&xs, encoder_xs, attn_mask, encoder_attn_mask)?;
        }
        Ok(xs)
    }

    pub fn reset_kv_cache(&mut self) {
        for layer in self.layers.iter_mut() {
            layer.reset_kv_cache()
        }
    }

    /// Rearranges the KV cache so that row `i` of the batch continues the
    /// prefix that row `beams[i]` held.
    ///
    /// This is what a beam search needs and what upstream cannot offer: the
    /// beam that row `i` extends changes at every step, and a cache left in
    /// place would go on answering with the prefix that used to be there -
    /// silently, as a fluent translation of a sentence nobody wrote.
    ///
    /// `beams` is a `U32` tensor of one index per row of the batch.
    pub fn reorder_kv_cache(&mut self, beams: &Tensor) -> Result<()> {
        for layer in self.layers.iter_mut() {
            layer.reorder_kv_cache(beams)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Model {
    /// The embedding the encoder and the decoder were both built from. Nothing
    /// reads it once they hold their own handles to it; it is kept because
    /// upstream keeps it, and a field that is there in one copy and not the
    /// other is a diff nobody wants to explain.
    #[allow(dead_code)]
    shared: Embedding,
    encoder: Encoder,
    decoder: Decoder,
}

impl Model {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let shared = embedding(cfg.vocab_size, cfg.d_model, vb.pp("shared"))?;
        let encoder = Encoder::new(cfg, &shared, vb.pp("encoder"))?;
        let decoder = Decoder::new(cfg, &shared, vb.pp("decoder"))?;
        Ok(Self {
            shared,
            encoder,
            decoder,
        })
    }

    fn reset_kv_cache(&mut self) {
        self.encoder.reset_kv_cache();
        self.decoder.reset_kv_cache();
    }
}

#[derive(Debug, Clone)]
pub struct MTModel {
    model: Model,
}

impl MTModel {
    pub fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            model: Model::new(cfg, vb.pp("model"))?,
        })
    }

    pub fn encoder(&mut self) -> &mut Encoder {
        &mut self.model.encoder
    }

    pub fn decoder(&mut self) -> &mut Decoder {
        &mut self.model.decoder
    }

    pub fn reset_kv_cache(&mut self) {
        self.model.reset_kv_cache()
    }

    /// See [`Decoder::reorder_kv_cache`].
    pub fn reorder_kv_cache(&mut self, beams: &Tensor) -> Result<()> {
        self.model.decoder.reorder_kv_cache(beams)
    }
}

#[cfg(test)]
mod tests {
    use candle_core::{DType, Device};
    use candle_nn::VarMap;

    use super::*;

    /// A model small enough to build in a test, with weights that are random
    /// rather than meaningful: what is under test is which positions the
    /// attention reaches, and that does not depend on what the weights say.
    fn tiny() -> (Config, MTModel) {
        let config = Config {
            vocab_size: 16,
            decoder_vocab_size: None,
            max_position_embeddings: 32,
            encoder_layers: 1,
            encoder_ffn_dim: 16,
            encoder_attention_heads: 2,
            decoder_layers: 1,
            decoder_ffn_dim: 16,
            decoder_attention_heads: 2,
            use_cache: true,
            is_encoder_decoder: true,
            activation_function: candle_nn::Activation::Relu,
            d_model: 8,
            decoder_start_token_id: 15,
            scale_embedding: true,
            pad_token_id: 15,
            eos_token_id: 0,
            forced_eos_token_id: 0,
            share_encoder_decoder_embeddings: true,
        };
        let variables = VarMap::new();
        let builder = VarBuilder::from_varmap(&variables, DType::F32, &Device::Cpu);
        let model = MTModel::new(&config, builder).expect("the tiny model builds");
        (config, model)
    }

    fn row(tensor: &Tensor, batch: usize, position: usize) -> Vec<f32> {
        tensor
            .i((batch, position))
            .and_then(|row| row.to_vec1())
            .expect("the encoder produced a (batch, position, model) tensor")
    }

    /// A masked position must not reach the ones that carry text.
    ///
    /// This is the change upstream cannot make from outside, and the whole
    /// reason padding a batch is safe here and is not safe there: without the
    /// mask, the two encodings below differ, and the difference is a
    /// translation that is merely a little worse.
    #[test]
    fn a_padded_position_does_not_change_the_ones_before_it() {
        let (config, mut model) = tiny();
        let device = Device::Cpu;

        let alone = Tensor::new(&[[1u32, 2, 3]], &device).unwrap();
        let padded = Tensor::new(&[[1u32, 2, 3, config.pad_token_id]], &device).unwrap();

        // `-inf` on the pad column, for every query position.
        let mask = Tensor::new(&[[0f32, 0., 0., f32::NEG_INFINITY]], &device)
            .unwrap()
            .broadcast_as((4, 4))
            .unwrap()
            .contiguous()
            .unwrap();

        let alone = model.encoder().forward(&alone, 0, None).unwrap();
        let padded = model.encoder().forward(&padded, 0, Some(&mask)).unwrap();

        for position in 0..3 {
            let (left, right) = (row(&alone, 0, position), row(&padded, 0, position));
            for (left, right) in left.iter().zip(&right) {
                assert!(
                    (left - right).abs() < 1e-4,
                    "position {position} moved when a pad was appended: {left} vs {right}"
                );
            }
        }
    }

    /// Without the mask it does change them - which is what upstream does on
    /// every padded batch, silently.
    #[test]
    fn without_the_mask_the_pad_leaks_into_the_sentence() {
        let (config, mut model) = tiny();
        let device = Device::Cpu;

        let alone = Tensor::new(&[[1u32, 2, 3]], &device).unwrap();
        let padded = Tensor::new(&[[1u32, 2, 3, config.pad_token_id]], &device).unwrap();

        let alone = model.encoder().forward(&alone, 0, None).unwrap();
        let padded = model.encoder().forward(&padded, 0, None).unwrap();

        let moved = (0..3).any(|position| {
            row(&alone, 0, position)
                .iter()
                .zip(&row(&padded, 0, position))
                .any(|(left, right)| (left - right).abs() > 1e-4)
        });
        assert!(moved, "the pad was expected to leak without a mask");
    }

    /// Reordering has to move whole rows, and has to be able to widen the
    /// batch: a search that starts from one hypothesis and opens out to four
    /// asks for exactly that.
    #[test]
    fn the_kv_cache_follows_the_beams() {
        let (config, mut model) = tiny();
        let device = Device::Cpu;

        let source = Tensor::new(&[[1u32, 2, 3]], &device).unwrap();
        let encoded = model.encoder().forward(&source, 0, None).unwrap();
        let mask = Tensor::zeros((1, 1), DType::F32, &device).unwrap();

        // One row in, and the cache holds one row.
        let start = Tensor::new(&[[config.decoder_start_token_id]], &device).unwrap();
        model
            .decoder()
            .forward(&start, Some(&encoded), 0, &mask, None)
            .unwrap();

        // Widen it to four copies of that row, as the first step of a search
        // of width four does.
        let beams = Tensor::new(&[0u32, 0, 0, 0], &device).unwrap();
        model.reorder_kv_cache(&beams).unwrap();

        let encoded = encoded.broadcast_as((4, 3, config.d_model)).unwrap();
        let next = Tensor::new(&[[1u32], [2], [3], [4]], &device).unwrap();
        let hidden = model
            .decoder()
            .forward(&next, Some(&encoded), 1, &mask, None)
            .unwrap();
        assert_eq!(hidden.dims3().unwrap(), (4, 1, config.d_model));

        // And narrow it again, as a step that loses beams to the end token
        // does.
        let beams = Tensor::new(&[2u32, 0], &device).unwrap();
        model.reorder_kv_cache(&beams).unwrap();
        let encoded = encoded.narrow(0, 0, 2).unwrap().contiguous().unwrap();
        let next = Tensor::new(&[[5u32], [6]], &device).unwrap();
        let hidden = model
            .decoder()
            .forward(&next, Some(&encoded), 2, &mask, None)
            .unwrap();
        assert_eq!(hidden.dims3().unwrap(), (2, 1, config.d_model));
    }
}
