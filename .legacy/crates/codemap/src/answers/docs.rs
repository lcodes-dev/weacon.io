use super::Text;
use crate::flows::pinned;
use crate::{Anchor, Index};

impl Index {
    /// The doc lines that pin `path`, as an item or a step of a flow, each with its file and
    /// line; the lines that don't fit within `budget` are counted.
    #[must_use]
    pub fn docs_for(&self, path: &str, budget: usize) -> String {
        let found: Vec<String> = self
            .docs
            .iter()
            .flat_map(|(file, text)| pinning(file, text, path))
            .collect();
        let mut text = Text::new(budget);
        if found.is_empty() {
            text.line(&format!("No doc pins {path}."));
        } else {
            text.line(&format!("Docs that pin {path}: {}.", found.len()));
        }
        for line in &found {
            text.line(line);
        }
        text.finish()
    }
}

/// Each line of `text` that pins `path`, once however many pins it holds.
fn pinning(file: &str, text: &str, path: &str) -> Vec<String> {
    let mut lines: Vec<usize> = pinned(text)
        .into_iter()
        .filter(|(anchor, _)| names(anchor, path))
        .map(|(_, line)| line)
        .collect();
    lines.dedup();
    let written: Vec<&str> = text.lines().collect();
    lines
        .into_iter()
        .filter_map(|line| {
            let said = written.get(line.checked_sub(1)?)?;
            Some(format!("- {file}:{line}  {}", said.trim()))
        })
        .collect()
}

fn names(anchor: &Anchor, path: &str) -> bool {
    match anchor {
        Anchor::Symbol(item) => item == path,
        Anchor::Flow(steps) => steps.iter().any(|step| step == path),
        Anchor::Area(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::index;
    use crate::test_support::files;

    fn repo(docs: &str) -> crate::Index {
        index(&files(&[
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("a/src/lib.rs", "pub fn run() { go(); }\npub fn go() {}\n"),
            ("docs/a.md", docs),
            ("README.md", "Start at `sym:a::go#00ff`.\n"),
        ]))
        .unwrap()
    }

    #[test]
    fn lists_each_line_pinning_an_item_or_a_step_once() {
        let index = repo(
            "# Serving\nThe call: flow:a::run->a::go and sym:a::go.\nNot here: sym:a::run.\n\
             Nor workflow:a::go->a::run.\n",
        );

        assert_eq!(
            index.docs_for("a::go", 10_000),
            "Docs that pin a::go: 2.\n- README.md:1  Start at `sym:a::go#00ff`.\n\
             - docs/a.md:2  The call: flow:a::run->a::go and sym:a::go.\n"
        );
        assert_eq!(index.docs_for("a::gone", 10_000), "No doc pins a::gone.\n");
    }

    #[test]
    fn lines_past_the_budget_are_counted_and_the_answer_ends_whole() {
        let docs = (0..100).fold(String::new(), |text, n| {
            text + &format!("Step {n}: flow:a::run->a::go\n")
        });
        let index = repo(&docs);

        let answer = index.docs_for("a::run", 1000);

        assert!(answer.len() <= 1000, "{answer}");
        assert!(
            answer.starts_with("Docs that pin a::run: 100.\n"),
            "{answer}"
        );
        assert!(answer.ends_with(" more lines\n"), "{answer}");
    }
}
