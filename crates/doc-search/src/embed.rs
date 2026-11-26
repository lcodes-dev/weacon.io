use std::sync::OnceLock;

use model2vec_rs::model::StaticModel;

pub const MODEL: &str = "minishlab/potion-base-8M";

pub fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    static LOADED: OnceLock<Result<StaticModel, String>> = OnceLock::new();
    let loaded = LOADED.get_or_init(|| {
        let model = StaticModel::from_pretrained(MODEL, None, None, None);
        model.map_err(|error| format!("The model {MODEL} couldn't be loaded: {error}"))
    });
    Ok(loaded.as_ref().map_err(String::clone)?.encode(texts))
}
