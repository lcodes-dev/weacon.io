use model2vec_rs::model::StaticModel;
use std::sync::OnceLock;

pub const MODEL: &str = "minishlab/potion-base-8M";

pub fn embed(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    static LOADED: OnceLock<Result<StaticModel, String>> = OnceLock::new();
    let loaded = LOADED.get_or_init(|| {
        let model = StaticModel::from_pretrained(MODEL, None, None, None);
        model.map_err(|error| format!("The model {MODEL} couldn't be loaded: {error}"))
    });
    Ok(loaded.as_ref().map_err(String::clone)?.encode(texts))
}

pub fn parts(title: &str, markdown: &str) -> Vec<String> {
    let mut parts = vec![format!("{title}\n")];
    let mut fenced = false;
    for line in markdown.lines() {
        fenced ^= line.trim_start().starts_with("```");
        let part = parts.last_mut().expect("there is a first part");
        if line.starts_with('#') && !fenced && part.trim() != title {
            parts.push(format!("{title}\n"));
        }
        let part = parts.last_mut().expect("there is a first part");
        part.push_str(line);
        part.push('\n');
    }
    parts.retain(|part| part.trim() != title);
    parts
}

pub fn score(question: &str, asked: &[f32], text: &str, embedding: &[f32]) -> f32 {
    let named = question
        .split_whitespace()
        .filter_map(name)
        .any(|name| text.contains(name));
    near(asked, embedding) + if named { 2.0 } else { 0.0 }
}

fn name(word: &str) -> Option<&str> {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
    let marked = word.contains(['_', '.', '/', ':']);
    let cased =
        word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
    (word.len() > 2 && (marked || cased)).then_some(word)
}

fn near(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    let long = |of: &[f32]| of.iter().map(|x| x * x).sum::<f32>().sqrt();
    let longs = long(a) * long(b);
    if longs == 0.0 { 0.0 } else { dot / longs }
}
