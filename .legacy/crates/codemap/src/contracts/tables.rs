use std::collections::BTreeMap;
use std::iter::Peekable;
use std::str::Chars;

/// The tables some SQL leaves in place, by name, with the line each was last made on.
#[derive(Debug, Default)]
pub(crate) struct Tables(BTreeMap<String, usize>);

impl Tables {
    /// Applies each statement in `sql`, which starts on `line`, that makes, drops or renames
    /// a table; a statement may span lines.
    pub(crate) fn apply(&mut self, sql: &str, line: usize) {
        let mut line = line;
        for statement in statements(sql) {
            let statement = statement.as_str();
            let leading = statement.len() - statement.trim_start().len();
            let before = statement.get(..leading).unwrap_or_default();
            let starts = line + before.matches('\n').count();
            let spaced = statement.replace(['(', ')', ','], " ");
            let words: Vec<&str> = spaced.split_whitespace().collect();
            self.statement(&words, starts);
            line += statement.matches('\n').count();
        }
    }

    fn statement(&mut self, words: &[&str], line: usize) {
        let is = |at: usize, expected: &str| {
            words
                .get(at)
                .is_some_and(|word| word.eq_ignore_ascii_case(expected))
        };
        let name = |at: usize| words.get(at).map(|word| plain(word));
        if is(0, "CREATE") {
            let table = if is(1, "TABLE") { 1 } else { 2 };
            let qualifier = ["VIRTUAL", "TEMP", "TEMPORARY"]
                .iter()
                .any(|word| is(1, word));
            if !is(table, "TABLE") || (table == 2 && !qualifier) {
                return;
            }
            let at = if is(table + 1, "IF") {
                table + 4
            } else {
                table + 1
            };
            self.0.extend(name(at).map(|name| (name, line)));
        } else if is(0, "DROP") && is(1, "TABLE") {
            let at = if is(2, "IF") { 4 } else { 2 };
            if let Some(name) = name(at) {
                self.0.remove(&name);
            }
        } else if is(0, "ALTER")
            && is(1, "TABLE")
            && is(3, "RENAME")
            && is(4, "TO")
            && let (Some(from), Some(to)) = (name(2), name(5))
            && self.0.remove(&from).is_some()
        {
            self.0.insert(to, line);
        }
    }

    pub(crate) fn into_lines(self) -> impl Iterator<Item = (String, usize)> {
        self.0.into_iter()
    }
}

/// The statements of `sql`, split at semicolons outside quotes, with comments and quoted
/// strings blanked out but their line breaks kept.
fn statements(sql: &str) -> Vec<String> {
    let mut found = vec![String::new()];
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        let Some(current) = found.last_mut() else {
            break;
        };
        match ch {
            '\'' => blank(&mut chars, current, |_, inside| inside == '\''),
            '"' | '`' => quoted(&mut chars, current, ch),
            '-' if chars.peek() == Some(&'-') => {
                blank(&mut chars, current, |_, inside| inside == '\n');
            }
            '/' if chars.peek() == Some(&'*') => blank(&mut chars, current, |previous, inside| {
                previous == '*' && inside == '/'
            }),
            ';' => found.push(String::new()),
            _ => current.push(ch),
        }
    }
    found
}

/// A quoted name as one word, its text kept exactly between markers that tokenizing leaves alone.
fn quoted(chars: &mut Peekable<Chars<'_>>, out: &mut String, quote: char) {
    out.push(QUOTED);
    while let Some(inside) = chars.next() {
        // A doubled quote stands for one inside the name.
        if inside == quote && chars.next_if_eq(&quote).is_none() {
            break;
        }
        let hidden = SPLITS.iter().position(|split| *split == inside);
        out.push(
            hidden
                .and_then(|at| HIDDEN.get(at))
                .copied()
                .unwrap_or(inside),
        );
    }
    out.push(QUOTED);
}

/// Where a quoted name starts and ends.
const QUOTED: char = '\u{1}';
/// What tokenizing splits words at, and what stands in for each inside a quoted name.
const SPLITS: [char; 5] = [' ', ',', '(', ')', '.'];
const HIDDEN: [char; 5] = ['\u{2}', '\u{3}', '\u{4}', '\u{5}', '\u{6}'];

/// Blanks out what follows up to and including the character that `ends` it, given the one
/// before, keeping line breaks.
fn blank(chars: &mut Peekable<Chars<'_>>, out: &mut String, ends: impl Fn(char, char) -> bool) {
    let mut previous = '\0';
    for inside in chars.by_ref() {
        out.push(if inside == '\n' { '\n' } else { ' ' });
        if ends(previous, inside) {
            break;
        }
        previous = inside;
    }
}

/// `notes` for `"notes"`, `main.notes` or `` `notes` ``.
fn plain(word: &str) -> String {
    let name = word.rsplit('.').next().unwrap_or_default();
    if let Some(quoted) = name
        .strip_prefix(QUOTED)
        .and_then(|rest| rest.strip_suffix(QUOTED))
    {
        return quoted
            .chars()
            .map(|ch| {
                let hidden = HIDDEN.iter().position(|hidden| *hidden == ch);
                hidden.and_then(|at| SPLITS.get(at)).copied().unwrap_or(ch)
            })
            .collect();
    }
    name.chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(lines: &[&str]) -> Vec<(String, usize)> {
        let mut tables = Tables::default();
        tables.apply(&lines.join("\n"), 1);
        tables.into_lines().collect()
    }

    fn named(found: &[(&str, usize)]) -> Vec<(String, usize)> {
        found
            .iter()
            .map(|(name, line)| ((*name).to_owned(), *line))
            .collect()
    }

    #[test]
    fn follows_a_migration_through_drops_and_renames() {
        let found = left(&[
            "CREATE TABLE sign_in(id INTEGER);create table if not exists notes (id);",
            "CREATE VIRTUAL TABLE found USING fts5(text); CREATE INDEX x ON notes (id);",
            "CREATE TABLE old_tasks (id); CREATE TEMP TABLE main.\"scratch\" (id);",
            "DROP TABLE sign_in; DROP TABLE IF EXISTS never;",
            "ALTER TABLE old_tasks RENAME TO tasks; ALTER TABLE gone RENAME TO ghost;",
            "CREATE TABLE notes (id, body);",
        ]);
        assert_eq!(
            found,
            named(&[("found", 2), ("notes", 6), ("scratch", 3), ("tasks", 5)])
        );
    }

    #[test]
    fn reads_a_statement_across_lines() {
        assert_eq!(
            left(&[
                "",
                "CREATE TABLE",
                "notes (id INTEGER);",
                "  CREATE TABLE b (x);"
            ]),
            named(&[("b", 4), ("notes", 2)])
        );
    }

    #[test]
    fn leaves_out_comments_and_quoted_text() {
        let found = left(&[
            "-- the first step",
            "CREATE TABLE notes(body TEXT); /* not;",
            "DROP TABLE notes; */ INSERT INTO notes VALUES ('', 'x; DROP TABLE notes;');",
            "CREATE TABLE \"main\".\"quoted\" (id); CREATE TABLE b (x); -- DROP TABLE b",
            "CREATE TABLE c (x DEFAULT -1, y DEFAULT 4/2); CREATE TABLE d (z);",
            "CREATE TABLE \"notes--archive\" (id); CREATE TABLE `tasks/*x` (id); CREATE TABLE e (id);",
        ]);
        let tables = [
            ("b", 4),
            ("c", 5),
            ("d", 5),
            ("e", 6),
            ("notes", 2),
            ("notes--archive", 6),
            ("quoted", 4),
            ("tasks/*x", 6),
        ];
        assert_eq!(found, named(&tables));
    }

    #[test]
    fn keeps_a_quoted_name_exactly() {
        let found = left(&[
            "CREATE TABLE \"a,b\"(id); CREATE TABLE a(id); DROP TABLE \"a,b\";",
            "CREATE TABLE \"my table\" (id); CREATE TABLE main.\"a.b\" (id);",
            "CREATE TABLE \"(odd)\" (id); CREATE TABLE plain_name(id);",
            "CREATE TABLE \"a\"\"b\"(id); CREATE TABLE `c``d`(id); CREATE TABLE \"\"\"e\"\"\"(id);",
        ]);
        assert_eq!(
            found,
            named(&[
                ("\"e\"", 4),
                ("(odd)", 3),
                ("a", 1),
                ("a\"b", 4),
                ("a.b", 2),
                ("c`d", 4),
                ("my table", 2),
                ("plain_name", 3)
            ])
        );
    }

    #[test]
    fn reads_only_statements() {
        assert_eq!(
            left(&[
                "warn: create table now;",
                "CREATE UNIQUE TABLE x;",
                "CREATE"
            ]),
            []
        );
    }
}
