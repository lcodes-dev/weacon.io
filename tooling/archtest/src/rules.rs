use crate::Kind;

/// Which workspace packages a package may depend on, and whether it may do I/O itself.
pub(crate) struct Rule {
    pub(crate) package: &'static str,
    pub(crate) may_use: &'static [&'static str],
    pub(crate) io: bool,
}

/// One row per package that exists today.
pub(crate) const RULES: &[Rule] = &[
    Rule {
        package: "weacon-archtest",
        may_use: &[],
        io: true,
    },
    Rule {
        package: "weacon-observability",
        may_use: &["weacon-protocol"],
        io: false,
    },
    Rule {
        package: "weacon-protocol",
        may_use: &[],
        io: false,
    },
    Rule {
        package: "weacon-version",
        may_use: &[],
        io: true,
    },
];

/// What every package under `apps/` depends on, and as which kind of dependency.
pub(crate) const APP_NEEDS: &[(&str, Kind)] = &[
    ("weacon-observability", Kind::Normal),
    ("weacon-version", Kind::Normal),
    ("weacon-version", Kind::Build),
];

/// External packages, with their `name-*` family, that only the listed packages may use.
pub(crate) const RESTRICTED: &[(&str, &[&str])] = &[
    ("diesel", &["weacon-core", "weacon-server"]),
    ("libsql", &["weacon-core", "weacon-server"]),
    ("postgres", &["weacon-server"]),
    ("rusqlite", &["weacon-core"]),
    ("sentry", &["weacon-observability"]),
    ("sqlx", &["weacon-core", "weacon-server"]),
    ("tokio-postgres", &["weacon-server"]),
];

/// Database drivers, by crate name in code: only a package's `db` module uses them.
pub(crate) const DB_DRIVERS: &[&str] = &[
    "diesel",
    "libsql",
    "postgres",
    "rusqlite",
    "sqlx",
    "tokio_postgres",
];

/// `cfg` names that select an operating system: only a package's `platform` module uses them.
pub(crate) const OS_CFGS: &[&str] = &[
    "target_family",
    "target_os",
    "target_vendor",
    "unix",
    "windows",
];

/// Standard library modules and packages that a package without I/O may not use.
pub(crate) const IO_MODULES: &[&str] = &["env", "fs", "net", "process"];
pub(crate) const IO_PACKAGES: &[&str] = &[
    "async-std",
    "hyper",
    "mio",
    "reqwest",
    "smol",
    "socket2",
    "tokio",
    "ureq",
    "walkdir",
];

/// The most lines a source file may hold before its test module.
pub(crate) const MAX_CODE_LINES: usize = 400;

/// Markers of unfinished work; split so this file doesn't match itself.
pub(crate) const MARKERS: &[&str] = &[concat!("FIX", "ME"), concat!("TO", "DO")];
