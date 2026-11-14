/// The outcome of merging a proposal into the doc it was written against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merge {
    Clean(String),
    /// The whole doc in order; joining the hunks, with one text chosen for each conflict,
    /// gives the merged doc.
    Conflicted(Vec<Hunk>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hunk {
    Same(String),
    /// Lines the doc and the proposal both changed, each in its own way.
    Conflict {
        base: String,
        current: String,
        proposed: String,
    },
}

/// Merges `proposed`, written against `base`, into `current`, line by line with each line's
/// ending kept. Where only one side changed lines, that change is taken; where both changed the
/// same or neighbouring lines differently, the hunk is a conflict. Its time grows with the
/// product of the lines each side rewrote, so callers bound the texts' size.
#[must_use]
pub fn merge(base: &str, current: &str, proposed: &str) -> Merge {
    let [base, current, proposed]: [Vec<&str>; 3] =
        [base, current, proposed].map(|text| text.split_inclusive('\n').collect());
    let (to_current, to_proposed) = (matches(&base, &current), matches(&base, &proposed));
    let stable = to_current.iter().zip(&to_proposed).enumerate();
    let stable = stable.filter_map(|(line, (ours, theirs))| Some([line, (*ours)?, (*theirs)?]));
    let mut hunks = Vec::new();
    let mut from = [0; 3];
    for upto in stable.chain([[base.len(), current.len(), proposed.len()]]) {
        let ([base_from, current_from, proposed_from], [base_upto, current_upto, proposed_upto]) =
            (from, upto);
        let old = span(&base, base_from, base_upto);
        let ours = span(&current, current_from, current_upto);
        let theirs = span(&proposed, proposed_from, proposed_upto);
        if ours == old {
            same(&mut hunks, theirs);
        } else if theirs == old || theirs == ours {
            same(&mut hunks, ours);
        } else {
            hunks.push(Hunk::Conflict {
                base: old,
                current: ours,
                proposed: theirs,
            });
        }
        same(&mut hunks, span(&base, base_upto, base_upto + 1));
        from = upto.map(|line| line + 1);
    }
    match hunks.as_slice() {
        [] => Merge::Clean(String::new()),
        [Hunk::Same(text)] => Merge::Clean(text.clone()),
        _ => Merge::Conflicted(hunks),
    }
}

fn span(lines: &[&str], from: usize, upto: usize) -> String {
    lines.get(from..upto).unwrap_or_default().concat()
}

fn same(hunks: &mut Vec<Hunk>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(Hunk::Same(last)) = hunks.last_mut() {
        last.push_str(&text);
    } else {
        hunks.push(Hunk::Same(text));
    }
}

/// For each line of `old`, the line of `new` it matches in a longest common subsequence.
fn matches(old: &[&str], new: &[&str]) -> Vec<Option<usize>> {
    let mut found = vec![None; old.len()];
    align(old, new, 0, &mut found);
    found
}

/// Fills `found`, one slot for each line of `old`, with the lines of `new`, offset by `at`, that
/// they match. It halves `old` instead of filling a table, so it needs memory only in proportion
/// to the lines.
fn align(old: &[&str], new: &[&str], at: usize, found: &mut [Option<usize>]) {
    let start = old
        .iter()
        .zip(new)
        .take_while(|(one, other)| one == other)
        .count();
    let (old_rest, new_rest) = (old.iter().skip(start), new.iter().skip(start));
    let end = old_rest
        .rev()
        .zip(new_rest.rev())
        .take_while(|(one, other)| one == other)
        .count();
    let (Some(old), Some(new)) = (
        old.get(start..old.len() - end),
        new.get(start..new.len() - end),
    ) else {
        return;
    };
    for (i, slot) in found.iter_mut().take(start).enumerate() {
        *slot = Some(at + i);
    }
    let tail = start + old.len();
    for (i, slot) in found.iter_mut().skip(tail).enumerate() {
        *slot = Some(at + start + new.len() + i);
    }
    let (Some(found), at) = (found.get_mut(start..tail), at + start) else {
        return;
    };
    if let [line] = old {
        if let Some(slot) = found.first_mut() {
            *slot = new.iter().position(|other| other == line).map(|to| at + to);
        }
    } else if !old.is_empty() && !new.is_empty() {
        let half = old.len() / 2;
        let (top, bottom) = old.split_at(half);
        let split = split(top, bottom, new);
        let (left, right) = new.split_at(split);
        let (found_top, found_bottom) = found.split_at_mut(half);
        align(top, left, at, found_top);
        align(bottom, right, at + split, found_bottom);
    }
}

/// Where to cut `new` so that its two parts, aligned with `top` and `bottom`, give a longest
/// common subsequence.
fn split(top: &[&str], bottom: &[&str], new: &[&str]) -> usize {
    let down = lengths(top.iter(), &new.iter());
    let up = lengths(bottom.iter().rev(), &new.iter().rev());
    (0..=new.len())
        .zip(down.iter().zip(up.iter().rev()))
        .max_by_key(|(_, (down, up))| *down + *up)
        .map_or(0, |(split, _)| split)
}

/// The lengths of the longest common subsequences of all of `old` and each start of `new`.
fn lengths<'a>(
    old: impl Iterator<Item = &'a &'a str>,
    new: &(impl Iterator<Item = &'a &'a str> + Clone),
) -> Vec<usize> {
    let mut row = vec![0; new.clone().count() + 1];
    for line in old {
        let (mut diagonal, mut left) = (0, 0);
        for (cell, other) in row.iter_mut().skip(1).zip(new.clone()) {
            let above = *cell;
            *cell = if line == other {
                diagonal + 1
            } else {
                above.max(left)
            };
            (diagonal, left) = (above, *cell);
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> Merge {
        Merge::Clean(text.to_owned())
    }

    fn same(text: &str) -> Hunk {
        Hunk::Same(text.to_owned())
    }

    fn conflict(base: &str, current: &str, proposed: &str) -> Hunk {
        Hunk::Conflict {
            base: base.to_owned(),
            current: current.to_owned(),
            proposed: proposed.to_owned(),
        }
    }

    #[test]
    fn empty_texts_merge() {
        assert_eq!(merge("", "", ""), clean(""));
        assert_eq!(merge("", "", "a"), clean("a"));
        assert_eq!(merge("", "a\n", ""), clean("a\n"));
        assert_eq!(merge("a\n", "a\n", ""), clean(""));
        assert_eq!(merge("a\n", "", "a\n"), clean(""));
        assert_eq!(merge("", "a\n", "a\n"), clean("a\n"));
        assert_eq!(
            merge("", "a\n", "b\n"),
            Merge::Conflicted(vec![conflict("", "a\n", "b\n")])
        );
    }

    #[test]
    fn a_side_that_changed_nothing_takes_the_other() {
        let base = "# Plan\n\nOne\nTwo\n";
        let other = "# Plan\n\nOne, then\nTwo\nThree\n";
        assert_eq!(merge(base, base, other), clean(other));
        assert_eq!(merge(base, other, base), clean(other));
        assert_eq!(merge(base, other, other), clean(other));
    }

    #[test]
    fn changes_apart_both_land() {
        let base = "a\nb\nc\nd\ne\n";
        assert_eq!(
            merge(base, "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n"),
            clean("A\nb\nc\nd\nE\n")
        );
        assert_eq!(
            merge(base, "a\nb\nc\nd\ne\nf\n", "a\nc\nd\ne\n"),
            clean("a\nc\nd\ne\nf\n")
        );
        assert_eq!(
            merge(base, "x\na\nb\nc\nd\ne\n", "a\nb\nC\nd\ne\n"),
            clean("x\na\nb\nC\nd\ne\n")
        );
        assert_eq!(
            merge(base, "a\nb\nd\ne\n", "a\nb\nd\ne\n"),
            clean("a\nb\nd\ne\n")
        );
    }

    #[test]
    fn a_doc_that_moved_on_still_takes_the_proposal() {
        let base = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let current = "intro\na\nb\nB\nc\nd\ne\nf\ng\nh\nend\n";
        let proposed = "a\nb\nc\nd\nE\nf\nh\n";
        let merged = "intro\na\nb\nB\nc\nd\nE\nf\nh\nend\n";
        assert_eq!(merge(base, current, proposed), clean(merged));
    }

    #[test]
    fn identical_changes_merge_once() {
        let base = "a\nb\nc\n";
        assert_eq!(merge(base, "a\nB\nc\n", "a\nB\nc\n"), clean("a\nB\nc\n"));
        assert_eq!(merge(base, "a\nc\n", "a\nc\n"), clean("a\nc\n"));
        assert_eq!(
            merge(base, "a\nb\nx\nc\n", "a\nb\nx\nc\n"),
            clean("a\nb\nx\nc\n")
        );
    }

    #[test]
    fn different_changes_to_one_line_conflict_amid_the_rest() {
        let merged = merge("a\nb\nc\nd\n", "A\nb\nc one\nd\n", "a\nb\nc two\nd\n");
        let hunks = vec![
            same("A\nb\n"),
            conflict("c\n", "c one\n", "c two\n"),
            same("d\n"),
        ];
        assert_eq!(merged, Merge::Conflicted(hunks));
    }

    #[test]
    fn adjacent_changes_conflict() {
        let merged = merge("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n");
        let hunks = vec![
            same("a\n"),
            conflict("b\nc\n", "B\nc\n", "b\nC\n"),
            same("d\n"),
        ];
        assert_eq!(merged, Merge::Conflicted(hunks));
    }

    #[test]
    fn a_change_beside_a_deletion_conflicts() {
        let merged = merge("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nd\n");
        let hunks = vec![
            same("a\n"),
            conflict("b\nc\n", "B\nc\n", "b\n"),
            same("d\n"),
        ];
        assert_eq!(merged, Merge::Conflicted(hunks));
    }

    #[test]
    fn a_change_and_a_deletion_with_a_line_between_merge() {
        let merged = merge("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nc\n");
        assert_eq!(merged, clean("a\nB\nc\n"));
        let merged = merge("a\nb\nc\nd\n", "a\nc\nd\n", "a\nb\nc\nD\n");
        assert_eq!(merged, clean("a\nc\nD\n"));
    }

    #[test]
    fn insertions_at_one_place_conflict() {
        let merged = merge("a\nb\n", "a\nx\nb\n", "a\ny\nb\n");
        let hunks = vec![same("a\n"), conflict("", "x\n", "y\n"), same("b\n")];
        assert_eq!(merged, Merge::Conflicted(hunks));
    }

    #[test]
    fn a_missing_final_newline_is_kept_or_changed_like_any_line() {
        assert_eq!(merge("a\nm\nb", "A\nm\nb", "a\nm\nb\n"), clean("A\nm\nb\n"));
        assert_eq!(
            merge("a\nm\nb", "a\nm\nb", "a\nm\nb\nc"),
            clean("a\nm\nb\nc")
        );
        assert_eq!(merge("a\nm\nb\n", "A\nm\nb\n", "a\nm\nb"), clean("A\nm\nb"));
        let merged = merge("a\nb", "a\nb\n", "a\nB");
        assert_eq!(
            merged,
            Merge::Conflicted(vec![same("a\n"), conflict("b", "b\n", "B")])
        );
    }

    #[test]
    fn crlf_lines_keep_their_endings() {
        let base = "a\r\nm\r\nb\r\n";
        assert_eq!(
            merge(base, "A\r\nm\r\nb\r\n", "a\r\nm\r\nB\r\n"),
            clean("A\r\nm\r\nB\r\n")
        );
        let merged = merge(base, "a\r\nm\r\nb\n", "a\r\nm\r\nB\r\n");
        let hunks = vec![same("a\r\nm\r\n"), conflict("b\r\n", "b\n", "B\r\n")];
        assert_eq!(merged, Merge::Conflicted(hunks));
        assert_eq!(merge("a\rb\n", "a\rb\n", "a\rc\n"), clean("a\rc\n"));
    }

    // Lines from a small alphabet, so that many of them repeat.
    fn texts(seed: u64, count: usize) -> Vec<Vec<&'static str>> {
        let mut state = seed;
        let mut next = move |below: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from((state >> 33) % below).unwrap()
        };
        (0..count)
            .map(|_| {
                let len = next(12);
                (0..len)
                    .map(|_| ["a\n", "b\n", "é 🙂\n", " \n", "\n"][next(5)])
                    .collect()
            })
            .collect()
    }

    fn docs(seed: u64, count: usize) -> Vec<String> {
        texts(seed, count)
            .iter()
            .map(|lines| lines.concat())
            .collect()
    }

    fn longest(old: &[&str], new: &[&str]) -> usize {
        let mut table = vec![vec![0; new.len() + 1]; old.len() + 1];
        for (i, line) in old.iter().enumerate() {
            for (j, other) in new.iter().enumerate() {
                let diagonal = table[i][j] + usize::from(line == other);
                table[i + 1][j + 1] = diagonal.max(table[i][j + 1]).max(table[i + 1][j]);
            }
        }
        table[old.len()][new.len()]
    }

    #[test]
    fn matches_form_a_longest_common_subsequence() {
        for pair in texts(7, 2000).chunks(2) {
            let [old, new] = pair else { continue };
            let found = matches(old, new);
            let pairs: Vec<(usize, usize)> = found
                .iter()
                .enumerate()
                .filter_map(|(i, to)| Some((i, (*to)?)))
                .collect();
            assert_eq!(found.len(), old.len());
            assert_eq!(pairs.len(), longest(old, new), "{old:?} {new:?}");
            assert!(
                pairs.iter().all(|&(i, j)| old[i] == new[j]),
                "{old:?} {new:?} {pairs:?}"
            );
            assert!(
                pairs.windows(2).all(|two| two[0].1 < two[1].1),
                "{old:?} {new:?} {pairs:?}"
            );
        }
    }

    #[test]
    fn a_side_that_changed_nothing_takes_the_other_whatever_the_lines() {
        for pair in docs(3, 1000).chunks(2) {
            let [base, other] = pair else {
                continue;
            };
            assert_eq!(merge(base, base, other), clean(other));
            assert_eq!(merge(base, other, base), clean(other));
            assert_eq!(merge(base, other, other), clean(other));
        }
    }

    #[test]
    fn conflicts_come_between_agreed_text() {
        let mut conflicted = 0;
        for three in docs(11, 3000).chunks(3) {
            let [base, current, proposed] = three else {
                continue;
            };
            let Merge::Conflicted(hunks) = merge(base, current, proposed) else {
                continue;
            };
            conflicted += 1;
            let agreed = |hunk: &Hunk| matches!(hunk, Hunk::Same(text) if !text.is_empty());
            assert!(
                hunks
                    .iter()
                    .all(|hunk| agreed(hunk) || matches!(hunk, Hunk::Conflict { .. }))
            );
            assert!(!hunks.iter().all(agreed), "{hunks:?}");
            assert!(
                hunks
                    .windows(2)
                    .all(|two| !(agreed(&two[0]) && agreed(&two[1]))),
                "{hunks:?}"
            );
        }
        assert!(conflicted > 100);
    }
}
