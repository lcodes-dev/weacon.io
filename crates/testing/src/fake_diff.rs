//! A made-up change of about 5,000 diff lines over [`FILES`] files, which the fake server sends
//! one file per answer to a client's diff query.

use weacon_protocol::{
    DiffAnswer, DiffAnswerKind, DiffContent, DiffFile, DiffLine, DiffQueryKind, Envelope,
    FileChange, Hunk, LineOp, Message,
};

/// How many files the change touches.
pub(crate) const FILES: u32 = 40;
// The lines of an added or removed file.
const PER_FILE: usize = 125;
// The hunks of an edited file.
const HUNKS: usize = 6;

const WORDS: [&str; 12] = [
    "meadow", "harbor", "lantern", "orchard", "pebble", "quill", "river", "saddle", "thistle",
    "willow", "ember", "fable",
];

// One hunk's lines: a few edits between context, as a refactor makes them.
const SHAPE: [(LineOp, u32); 7] = [
    (LineOp::Context, 3),
    (LineOp::Removed, 4),
    (LineOp::Added, 6),
    (LineOp::Context, 4),
    (LineOp::Removed, 2),
    (LineOp::Added, 2),
    (LineOp::Context, 3),
];
// Unchanged lines between hunks.
const GAP: u32 = 40;

/// The answers to the diff query in `envelope`, each under its id; none for any other message.
pub(crate) fn answers(envelope: &Envelope) -> Vec<Envelope> {
    let Message::DiffQuery(query) = &envelope.message else {
        return Vec::new();
    };
    if query.query != DiffQueryKind::Changes {
        return Vec::new();
    }
    change()
        .into_iter()
        .map(|file| Envelope {
            id: envelope.id,
            message: Message::DiffAnswer(DiffAnswer {
                request: query.request.clone(),
                workspace: query.workspace.clone(),
                chat: query.chat.clone(),
                answer: DiffAnswerKind::File { files: FILES, file },
            }),
        })
        .collect()
}

/// The change's files, in path order.
pub(crate) fn change() -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = (0..FILES)
        .map(|at| {
            let at = usize::try_from(at).unwrap_or(0);
            let kind = if at == 19 { "png" } else { "rs" };
            let path = format!("src/{}/part_{at:02}.{kind}", word(at));
            let (change, content) = match at {
                19 => (FileChange::Modified, DiffContent::Binary),
                39 => (FileChange::Modified, DiffContent::TooLarge),
                _ => match at % 10 {
                    3 => (FileChange::Added, one_sided(at, LineOp::Added)),
                    6 => (FileChange::Removed, one_sided(at, LineOp::Removed)),
                    8 => (
                        FileChange::Renamed {
                            from: format!("src/{}/old_{at:02}.rs", word(at)),
                        },
                        edited(at),
                    ),
                    _ => (FileChange::Modified, edited(at)),
                },
            };
            DiffFile {
                path,
                change,
                content,
            }
        })
        .collect();
    files.sort_by(|one, other| one.path.cmp(&other.path));
    files
}

fn word(at: usize) -> &'static str {
    WORDS.get(at % WORDS.len()).copied().unwrap_or("misc")
}

// A whole file added or removed: one hunk, the last line without its newline.
fn one_sided(file: usize, op: LineOp) -> DiffContent {
    let mut lines: Vec<DiffLine> = (1..=PER_FILE).map(|at| line(op, file, at)).collect();
    if let Some(last) = lines.last_mut() {
        last.no_newline = true;
    }
    let (old_start, new_start) = if op == LineOp::Added { (0, 1) } else { (1, 0) };
    DiffContent::Text {
        hunks: vec![Hunk {
            old_start,
            new_start,
            lines,
        }],
    }
}

// `HUNKS` hunks shaped like `SHAPE`, `GAP` lines apart.
fn edited(file: usize) -> DiffContent {
    let (mut old, mut new) = (1, 1);
    let hunks = (0..HUNKS)
        .map(|_| {
            old += GAP;
            new += GAP;
            hunk(file, &mut old, &mut new)
        })
        .collect();
    DiffContent::Text { hunks }
}

// One hunk at `old` and `new`, which it moves past its lines.
fn hunk(file: usize, old: &mut u32, new: &mut u32) -> Hunk {
    let (old_start, new_start) = (*old, *new);
    let mut lines = Vec::new();
    for (op, count) in SHAPE {
        for _ in 0..count {
            let at = if op == LineOp::Removed { *old } else { *new };
            lines.push(line(op, file, usize::try_from(at).unwrap_or(0)));
            *old += u32::from(op != LineOp::Added);
            *new += u32::from(op != LineOp::Removed);
        }
    }
    Hunk {
        old_start,
        new_start,
        lines,
    }
}

// A line of made-up Rust; every 50th is long, to need scrolling sideways.
fn line(op: LineOp, file: usize, at: usize) -> DiffLine {
    let call = if op == LineOp::Removed {
        "gather"
    } else {
        "collect"
    };
    let mut text = match at % 7 {
        0 => format!("fn {}_{at}(input: &str) -> usize {{", word(file + at)),
        6 => "}".to_owned(),
        _ => format!(
            "    let {}_{at} = {call}({}, {at});",
            word(file + at + 1),
            word(file + at + 2)
        ),
    };
    if at.is_multiple_of(50) {
        text = format!("{text} // {}", [word(file + at + 3); 24].join(" then "));
    }
    DiffLine {
        op,
        text,
        no_newline: false,
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatId, DiffQuery, RequestId, WorkspaceId};

    use super::*;

    fn hunks(file: &DiffFile) -> &[Hunk] {
        match &file.content {
            DiffContent::Text { hunks } => hunks,
            DiffContent::Binary | DiffContent::TooLarge | DiffContent::Newer => &[],
        }
    }

    #[test]
    fn the_change_is_about_five_thousand_lines_over_many_kinds_of_file() {
        let files = change();
        let total: usize = files
            .iter()
            .flat_map(hunks)
            .map(|hunk| hunk.lines.len())
            .sum();
        let kind =
            |want: fn(&FileChange) -> bool| files.iter().filter(|file| want(&file.change)).count();

        assert_eq!(files.len(), 40);
        assert_eq!(total, 30 * 6 * 24 + 8 * 125);
        assert_eq!(kind(|change| matches!(change, FileChange::Added)), 4);
        assert_eq!(kind(|change| matches!(change, FileChange::Removed)), 4);
        assert_eq!(
            kind(|change| matches!(change, FileChange::Renamed { .. })),
            4
        );
        for content in [DiffContent::Binary, DiffContent::TooLarge] {
            assert_eq!(
                files.iter().filter(|file| file.content == content).count(),
                1
            );
        }
        assert!(files.windows(2).all(|pair| pair[0].path < pair[1].path));
        let at = |path: &str| files.iter().find(|file| file.path == path).unwrap();
        assert_eq!(at("src/saddle/part_19.png").content, DiffContent::Binary);
        assert_eq!(
            at("src/thistle/part_08.rs").change,
            FileChange::Renamed {
                from: "src/thistle/old_08.rs".to_owned()
            }
        );
        assert!(
            files
                .iter()
                .flat_map(hunks)
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.text.len() > 200)
        );
    }

    #[test]
    fn hunks_count_lines_on_each_side() {
        let files = change();
        let edited = files
            .iter()
            .find(|file| file.change == FileChange::Modified && !hunks(file).is_empty())
            .unwrap();
        let [first, second, ..] = hunks(edited) else {
            panic!("fewer than two hunks");
        };
        let side = |without| {
            let count = first.lines.iter().filter(|line| line.op != without).count();
            u32::try_from(count).unwrap()
        };
        let added = files
            .iter()
            .find(|file| file.change == FileChange::Added)
            .unwrap();
        let [whole] = hunks(added) else {
            panic!("not one hunk")
        };

        assert_eq!((first.old_start, first.new_start), (41, 41));
        assert_eq!(second.old_start, first.old_start + side(LineOp::Added) + 40);
        assert_eq!(
            second.new_start,
            first.new_start + side(LineOp::Removed) + 40
        );
        assert_eq!((whole.old_start, whole.new_start), (0, 1));
        assert!(whole.lines.last().unwrap().no_newline);
        assert!(!whole.lines.first().unwrap().no_newline);
        assert!(whole.lines.iter().all(|line| line.op == LineOp::Added));
    }

    #[test]
    fn lines_are_made_up_rust_that_differs_between_the_sides() {
        let text = |op, at| line(op, 2, at).text;

        assert_eq!(
            text(LineOp::Context, 14),
            "fn pebble_14(input: &str) -> usize {"
        );
        assert_eq!(text(LineOp::Context, 13), "}");
        assert_eq!(
            text(LineOp::Removed, 15),
            "    let river_15 = gather(saddle, 15);"
        );
        assert_eq!(
            text(LineOp::Added, 15),
            "    let river_15 = collect(saddle, 15);"
        );
        assert!(
            text(LineOp::Added, 50)
                .starts_with("    let quill_50 = collect(river, 50); // saddle then saddle")
        );
    }

    #[test]
    fn a_hunk_numbers_each_line_by_its_own_side() {
        let (mut old, mut new) = (41, 43);
        let hunk = hunk(0, &mut old, &mut new);

        assert_eq!((hunk.old_start, hunk.new_start), (41, 43));
        assert_eq!(hunk.lines.get(2), Some(&line(LineOp::Context, 0, 45)));
        assert_eq!(hunk.lines.get(3), Some(&line(LineOp::Removed, 0, 44)));
        assert_eq!(hunk.lines.get(7), Some(&line(LineOp::Added, 0, 46)));
        assert_eq!((old, new), (41 + 16, 43 + 18));
    }

    #[test]
    fn a_diff_query_is_answered_one_file_each_under_its_id_and_request() {
        let query = Envelope {
            id: 7,
            message: Message::DiffQuery(DiffQuery {
                request: RequestId::try_from("q-1".to_owned()).unwrap(),
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                query: DiffQueryKind::Changes,
            }),
        };
        let answers = answers(&query);

        assert_eq!(answers.len(), 40);
        for (answer, file) in answers.iter().zip(change()) {
            answer.to_json().unwrap();
            assert_eq!(answer.id, 7);
            let Message::DiffAnswer(body) = &answer.message else {
                panic!("not an answer");
            };
            assert_eq!(body.request.as_str(), "q-1");
            assert_eq!(
                (body.workspace.as_str(), body.chat.as_str()),
                ("w-1", "c-1")
            );
            assert_eq!(body.answer, DiffAnswerKind::File { files: 40, file });
        }
        let other = Envelope {
            id: 1,
            message: Message::Unknown,
        };
        assert_eq!(super::answers(&other), Vec::new());
        let Message::DiffQuery(mut newer) = query.message else {
            panic!("not a query");
        };
        newer.query = DiffQueryKind::Newer;
        let newer = Envelope {
            id: 7,
            message: Message::DiffQuery(newer),
        };
        assert_eq!(super::answers(&newer), Vec::new());
    }
}
