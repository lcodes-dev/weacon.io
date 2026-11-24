//! Finds the parts of a project's docs that a piece of text is about, by meaning and by the
//! names written in it. A page is cut into parts at its headings, each part gets an
//! embedding, and a question is ranked against them. Where the embeddings come from is
//! `embed.rs` alone.

mod embed;

pub use embed::{MODEL, embed};

/// A page's text under one heading, led by the page's title and that heading.
pub fn parts(title: &str, markdown: &str) -> Vec<String> {
    let mut parts = vec![format!("{title}\n")];
    let mut fenced = false;
    for line in markdown.lines() {
        fenced ^= line.trim_start().starts_with("```");
        let part = parts.last_mut().expect("there is a first part");
        // A page's text before its first heading is a part of its own.
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

/// The places of `parts` in the order they answer `question`, the best first. A part is its
/// text and its embedding; `asked` is the question's. A part that holds a name from the
/// question as it is written, such as a function's or a file's, comes before every part that
/// is only alike in meaning.
pub fn rank(question: &str, asked: &[f32], parts: &[(&str, &[f32])]) -> Vec<usize> {
    let names: Vec<&str> = question.split_whitespace().filter_map(name).collect();
    let score = |(text, embedding): &(&str, &[f32])| {
        let named = names.iter().any(|name| text.contains(name));
        near(asked, embedding) + if named { 2.0 } else { 0.0 }
    };
    let scores: Vec<f32> = parts.iter().map(score).collect();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]));
    order
}

/// A word that is a name in code and not a word of the language: it has a mark such as `_`,
/// `.`, `/` or `:` inside it, or a capital after its first letter.
fn name(word: &str) -> Option<&str> {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
    let marked = word.contains(['_', '.', '/', ':']);
    let cased =
        word.chars().skip(1).any(char::is_uppercase) && word.chars().any(char::is_lowercase);
    (word.len() > 2 && (marked || cased)).then_some(word)
}

/// How alike two embeddings are, from -1 to 1.
fn near(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    let long = |of: &[f32]| of.iter().map(|x| x * x).sum::<f32>().sqrt();
    let longs = long(a) * long(b);
    if longs == 0.0 { 0.0 } else { dot / longs }
}
