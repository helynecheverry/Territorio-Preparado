use anyhow::{Context, Result, ensure};
use candle_core::{DType, Device, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use candle_transformers::models::bert::{BertModel, Config};
use serde_json::{Value, json};
use std::path::Path;
use tokenizers::Tokenizer;

pub struct Bert {
    model: BertModel,
    pooler: Linear,
    classifier: Linear,
    tokenizer: Tokenizer,
    fingerprint: String,
}
impl Bert {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&std::fs::read(path.join("config.json"))?)?;
        let weights = std::fs::read(path.join("model.safetensors"))?;
        use sha2::Digest;
        let fingerprint = hex::encode(sha2::Sha256::digest(&weights));
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &Device::Cpu)?;
        let model = BertModel::load(vb.pp("bert"), &config)?;
        let pooler = candle_nn::linear(
            config.hidden_size,
            config.hidden_size,
            vb.pp("bert.pooler.dense"),
        )?;
        let classifier = candle_nn::linear(config.hidden_size, 36, vb.pp("classifier"))?;
        let mut tokenizer =
            Tokenizer::from_file(path.join("tokenizer.json")).map_err(anyhow::Error::msg)?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 128,
                ..Default::default()
            }))
            .map_err(anyhow::Error::msg)?;
        tokenizer.with_padding(None);
        Ok(Self {
            model,
            pooler,
            classifier,
            tokenizer,
            fingerprint,
        })
    }
    pub fn predict(&self, text: &str) -> Result<Value> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(anyhow::Error::msg)?;
        ensure!(!encoding.get_ids().is_empty(), "empty input");
        let ids = Tensor::new(encoding.get_ids(), &Device::Cpu)?.unsqueeze(0)?;
        let types = Tensor::new(encoding.get_type_ids(), &Device::Cpu)?.unsqueeze(0)?;
        let mask = Tensor::new(encoding.get_attention_mask(), &Device::Cpu)?.unsqueeze(0)?;
        let sequence = self
            .model
            .forward(&ids, &types, Some(&mask))
            .context("BERT forward")?;
        let cls = sequence.narrow(1, 0, 1)?.squeeze(1)?;
        let pooled = self.pooler.forward(&cls)?.tanh()?;
        let logits = self
            .classifier
            .forward(&pooled)?
            .squeeze(0)?
            .to_vec1::<f32>()?;
        let scores: Vec<Value> = logits
            .iter()
            .enumerate()
            .map(|(i, x)| json!({"label":format!("LABEL_{i}"),"score":1.0/(1.0+(-x).exp())}))
            .collect();
        let selected: Vec<_> = scores
            .iter()
            .filter(|s| s["score"].as_f64().unwrap_or(0.0) >= 0.5)
            .cloned()
            .collect();
        Ok(
            json!({"scores":scores,"selected":selected,"threshold":0.5,"model":"AventIQ-AI/Bert-Disaster-SOS-Message-Classifier","weights_sha256":self.fingerprint,"runtime":"candle-rust-cpu","priority":"human_review","semantic_labels_verified":false,"limitation":"Las etiquetas no tienen una correspondencia semántica verificada. Requiere revisión humana."}),
        )
    }
}
