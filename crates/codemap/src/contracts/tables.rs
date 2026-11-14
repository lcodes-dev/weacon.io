use std::collections::BTreeMap;

/// The tables some SQL leaves in place, by name, with the line each was last made on.
#[derive(Debug, Default)]
pub(crate) struct Tables(BTreeMap<String, usize>);

impl Tables {
    /// Applies each statement in one line of SQL that makes, drops or renames a table.
    pub(crate) fn apply(&mut self, sql: &str, line: usize) {
        let spaced: String = sql
            .chars()
            .map(|ch| {
                if matches!(ch, '(' | ')' | ',') {
                    ' '
                } else {
                    ch
                }
            })
            .collect();
        let spaced = spaced.replace(';', " ; ");
        let words: Vec<&str> = spaced.split_whitespace().collect();
        for statement in words.split(|word| *word == ";") {
            self.statement(statement, line);
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
            let qualifier = ["VIRTUAL", "TEMP", "TEMPORARY"].iter().any(|it| is(1, it));
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

/// `notes` for `"notes"`, `main.notes` or `` `notes` ``.
fn plain(word: &str) -> String {
    let name = word.rsplit('.').next().unwrap_or_default();
    name.chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(lines: &[&str]) -> Vec<(String, usize)> {
        let mut tables = Tables::default();
        for (line, sql) in (1..).zip(lines) {
            tables.apply(sql, line);
        }
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
    fn reads_only_statements() {
        assert_eq!(
            left(&["warn: create table now", "CREATE UNIQUE TABLE x", "CREATE"]),
            []
        );
    }
}
