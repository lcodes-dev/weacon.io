//! The static tables of what is known safe: the readers and the options each may take.

/// A reader's allowed options: the short-option letters and the exact long options it may take,
/// the most operands it may be given before one is a file it writes, and operand text that would
/// read or run something. An option outside these asks.
#[derive(Clone, Copy)]
pub(super) struct Options {
    pub(super) short: &'static str,
    pub(super) long: &'static [&'static str],
    pub(super) operands: usize,
    pub(super) text: &'static [&'static str],
}

// Any option is safe.
const ANY: Option<Options> = None;

// A reader whose only dangerous options are spelled out; its operands and text are unrestricted.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the table holds Option<Options>, so this builds the Some"
)]
const fn options(short: &'static str, long: &'static [&'static str]) -> Option<Options> {
    Some(Options {
        short,
        long,
        operands: usize::MAX,
        text: &[],
    })
}

// `-R`/`--dereference-recursive` follows symlinks out; plain `-r`/`--recursive` stays put.
const GREP: Option<Options> = options(
    "nricvlLoswxeEFHhq",
    &[
        "--line-number",
        "--recursive",
        "--ignore-case",
        "--invert-match",
        "--count",
        "--files-with-matches",
        "--files-without-match",
        "--only-matching",
        "--word-regexp",
        "--extended-regexp",
        "--fixed-strings",
        "--color",
        "--regexp",
        "--before-context",
        "--after-context",
        "--context",
        "--directories",
        "--include",
        "--exclude",
    ],
);

/// Programs that only read. `ANY` marks one with no option that writes, runs, or reads a file
/// it isn't handed as an operand, so any option is safe; the rest list the exact options they
/// may take, and any other option asks. Operands are read-checked against the credential and
/// Weacon folders before this.
pub(super) const READERS: [(&str, Option<Options>); 33] = [
    ("ls", ANY),
    ("cat", ANY),
    ("head", ANY),
    ("tail", ANY),
    ("wc", ANY),
    ("pwd", ANY),
    ("echo", ANY),
    ("printf", ANY),
    ("true", ANY),
    ("false", ANY),
    ("test", ANY),
    ("[", ANY),
    ("stat", ANY),
    ("which", ANY),
    ("basename", ANY),
    ("dirname", ANY),
    ("realpath", ANY),
    ("readlink", ANY),
    ("uname", ANY),
    ("whoami", ANY),
    ("printenv", ANY),
    ("cut", ANY),
    ("tr", ANY),
    ("du", ANY),
    // `-s`/`--set` would set the clock.
    (
        "date",
        options(
            "udrRI",
            &["--utc", "--date", "--rfc-3339", "--iso-8601", "--reference"],
        ),
    ),
    ("grep", GREP),
    ("egrep", GREP),
    ("fgrep", GREP),
    // `-L`/`--follow` would leave the worktree through a symlink; `--pre*` runs a program.
    (
        "rg",
        options(
            "nilcwxeEsoHApC",
            &[
                "--line-number",
                "--ignore-case",
                "--count",
                "--word-regexp",
                "--regexp",
                "--fixed-strings",
                "--color",
                "--glob",
                "--type",
                "--context",
                "--before-context",
                "--after-context",
                "--hidden",
                "--no-ignore",
                "--files",
                "--smart-case",
                "--multiline",
            ],
        ),
    ),
    (
        "diff",
        options(
            "uUyqrbBiwNpcd",
            &[
                "--unified",
                "--recursive",
                "--brief",
                "--ignore-case",
                "--ignore-all-space",
                "--ignore-space-change",
                "--new-file",
                "--color",
                "--context",
                "--side-by-side",
            ],
        ),
    ),
    // `-o`/`-T`/`--compress-program`/`--files0-from` would write a file or run a program.
    (
        "sort",
        options(
            "nrubfkSsc",
            &[
                "--numeric-sort",
                "--reverse",
                "--unique",
                "--ignore-leading-blanks",
                "--dictionary-order",
                "--key",
                "--field-separator",
                "--stable",
                "--check",
                "--human-numeric-sort",
                "--version-sort",
                "--month-sort",
            ],
        ),
    ),
    // A second operand is the file it writes.
    (
        "uniq",
        Some(Options {
            short: "cduiwfs",
            long: &[
                "--count",
                "--repeated",
                "--unique",
                "--ignore-case",
                "--check-chars",
                "--skip-fields",
                "--skip-chars",
            ],
            operands: 1,
            text: &[],
        }),
    ),
    // `-L`/`-f` read a file, and `import`/`include` in the filter program reads a module.
    (
        "jq",
        Some(Options {
            short: "nrceSMas",
            long: &[
                "--null-input",
                "--raw-output",
                "--compact-output",
                "--slurp",
                "--arg",
                "--argjson",
                "--args",
                "--raw-input",
                "--tab",
                "--sort-keys",
                "--join-output",
                "--exit-status",
            ],
            operands: usize::MAX,
            text: &["import", "include"],
        }),
    ),
];
