use anyhow::{Result, ensure};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian::{Config, MTModel};
use sentencepiece_rs::SentencePieceProcessor;
use std::{collections::HashMap, path::Path};

pub struct Translator {
    model: MTModel,
    config: Config,
    source: SentencePieceProcessor,
    vocab: HashMap<String, u32>,
    reverse: HashMap<u32, String>,
}
impl Translator {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&std::fs::read(path.join("config.json"))?)?;
        let source = SentencePieceProcessor::open(path.join("source.spm"))?;
        let vocab: HashMap<String, u32> =
            serde_json::from_slice(&std::fs::read(path.join("vocab.json"))?)?;
        let reverse = vocab.iter().map(|(k, v)| (*v, k.clone())).collect();
        let vb = if path.join("model.safetensors").exists() {
            VarBuilder::from_buffered_safetensors(
                std::fs::read(path.join("model.safetensors"))?,
                DType::F32,
                &Device::Cpu,
            )?
        } else {
            VarBuilder::from_pth(path.join("pytorch_model.bin"), DType::F32, &Device::Cpu)?
        };
        let model = MTModel::new(&config, vb)?;
        Ok(Self {
            model,
            config,
            source,
            vocab,
            reverse,
        })
    }
    pub fn translate(&mut self, text: &str) -> Result<String> {
        self.model.reset_kv_cache();
        let pieces = self.source.encode(text)?;
        ensure!(
            pieces.len() < 512,
            "El mensaje supera 511 tokens de traducción; divídelo en reportes más cortos"
        );
        let unknown = *self.vocab.get("<unk>").unwrap_or(&1);
        let mut tokens: Vec<_> = pieces
            .iter()
            .map(|p| *self.vocab.get(p).unwrap_or(&unknown))
            .collect();
        tokens.push(self.config.eos_token_id);
        let encoder = self.model.encoder().forward(
            &Tensor::new(tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?,
            0,
        )?;
        let mut next = self.config.decoder_start_token_id;
        let mut output = String::new();
        for pos in 0..256 {
            let input = Tensor::new(&[next], &Device::Cpu)?.unsqueeze(0)?;
            let scores = self
                .model
                .decode(&input, &encoder, pos)?
                .squeeze(0)?
                .squeeze(0)?
                .to_vec1::<f32>()?;
            next = scores
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != self.config.pad_token_id as usize)
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as u32)
                .ok_or_else(|| anyhow::anyhow!("No translation logits"))?;
            if next == self.config.eos_token_id {
                return Ok(output.replace('▁', " ").trim().to_owned());
            }
            output.push_str(
                self.reverse
                    .get(&next)
                    .ok_or_else(|| anyhow::anyhow!("Unknown output token"))?,
            );
        }
        anyhow::bail!("La traducción superó el límite de generación")
    }
}
