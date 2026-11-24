//! Where embeddings come from: a small model that runs here, and sends a text nowhere.
//! Another way to embed, such as a vendor's API, is this file written again.

use std::sync::OnceLock;

use model2vec_rs::model::StaticModel;

/// Which model makes the embeddings, kept with each one so that those of another are made again.
pub const MODEL: &str = "minishlab/potion-base-8M";

/// An embedding for each text. The first call fetches the model, about 30 MB, from where its
/// makers publish it into this machine's Hugging Face folder, which `HF_HOME` names, unless
/// it is there already; it can take a while, and blocks.
pub fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    static LOADED: OnceLock<Result<StaticModel, String>> = OnceLock::new();
    let loaded = LOADED.get_or_init(|| {
        let model = StaticModel::from_pretrained(MODEL, None, None, None);
        model.map_err(|error| format!("The model {MODEL} couldn't be loaded: {error}"))
    });
    Ok(loaded.as_ref().map_err(String::clone)?.encode(texts))
}
