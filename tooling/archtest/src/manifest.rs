//! Checks of each package's manifest and dependencies.

use std::collections::BTreeSet;
use std::iter;

use toml::{Table, Value};

use crate::rules::{APP_NEEDS, IO_PACKAGES, RESTRICTED, RULES, Rule};
use crate::{Kind, Package};

const INHERITED: [&str; 5] = ["version", "edition", "rust-version", "license", "publish"];
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

pub(crate) fn check_rules(names: &BTreeSet<&str>) -> Vec<String> {
    let ruled: BTreeSet<&str> = RULES.iter().map(|rule| rule.package).collect();
    let unruled = names
        .difference(&ruled)
        .map(|name| format!("{name}: has no row in rules.rs"));
    let stale = ruled
        .difference(names)
        .map(|name| format!("{name}: has a row in rules.rs but no package"));
    unruled.chain(stale).collect()
}

pub(crate) fn check_manifest(name: &str, manifest: &Table) -> Vec<String> {
    let mut out = Vec::new();
    if !is_inherited(manifest.get("lints")) {
        out.push(format!(
            "{name}: doesn't take the workspace lints ([lints] workspace = true)"
        ));
    }
    let package = manifest.get("package").and_then(Value::as_table);
    out.extend(
        INHERITED
            .iter()
            .filter(|field| !is_inherited(package.and_then(|package| package.get(**field))))
            .map(|field| {
                format!("{name}: sets package.{field} itself instead of taking the workspace's")
            }),
    );
    out.extend(
        dependency_specs(manifest)
            .filter(|(_, _, spec)| !is_inherited(Some(spec)))
            .map(|(table, dependency, _)| {
                format!(
                    "{name}: {table}.{dependency} must come from the workspace (workspace = true)"
                )
            }),
    );
    out
}

pub(crate) fn check_dependencies(
    package: &Package,
    rule: &Rule,
    names: &BTreeSet<&str>,
) -> Vec<String> {
    let name = &package.name;
    let mut out = Vec::new();
    for dependency in &package.dependencies {
        let used = dependency.name.as_str();
        if names.contains(used) && !rule.may_use.contains(&used) {
            out.push(format!("{name}: may not depend on {used}"));
        }
        if let Some((_, allowed)) = RESTRICTED.iter().find(|(restricted, _)| {
            used == *restricted || used.starts_with(&format!("{restricted}-"))
        }) && !allowed.contains(&name.as_str())
        {
            out.push(format!(
                "{name}: may not depend on {used}; only {} may",
                allowed.join(", ")
            ));
        }
        if !rule.io && dependency.kind() == Kind::Normal && IO_PACKAGES.contains(&used) {
            out.push(format!(
                "{name}: does no I/O, so it may not depend on {used}"
            ));
        }
    }
    out
}

/// What an app lacks of `APP_NEEDS`.
pub(crate) fn check_app_needs(package: &Package) -> Vec<String> {
    APP_NEEDS
        .iter()
        .filter(|(needed, kind)| {
            !package
                .dependencies
                .iter()
                .any(|dependency| dependency.name == *needed && dependency.kind() == *kind)
        })
        .map(|(needed, kind)| {
            format!(
                "{}: is an app, so it needs {needed} as a {} dependency",
                package.name,
                kind.name()
            )
        })
        .collect()
}

// Every dependency entry with its table, target-specific ones included.
fn dependency_specs(manifest: &Table) -> impl Iterator<Item = (&'static str, &String, &Value)> {
    let targets = manifest
        .get("target")
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(Table::values)
        .filter_map(Value::as_table);
    iter::once(manifest).chain(targets).flat_map(|section| {
        DEPENDENCY_TABLES.into_iter().flat_map(move |table| {
            section
                .get(table)
                .and_then(Value::as_table)
                .into_iter()
                .flatten()
                .map(move |(dependency, spec)| (table, dependency, spec))
        })
    })
}

fn is_inherited(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_table)
        .and_then(|table| table.get("workspace"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::Dependency;

    fn package(name: &str, dependencies: &[(&str, Option<Kind>)]) -> Package {
        Package {
            name: name.to_owned(),
            manifest_path: PathBuf::from(format!("/repo/crates/{name}/Cargo.toml")),
            dependencies: dependencies
                .iter()
                .map(|(name, kind)| Dependency {
                    name: (*name).to_owned(),
                    kind: *kind,
                })
                .collect(),
        }
    }

    fn rule(package: &'static str, may_use: &'static [&'static str], io: bool) -> Rule {
        Rule {
            package,
            may_use,
            io,
        }
    }

    #[test]
    fn every_package_has_a_rule_and_every_rule_a_package() {
        let names: BTreeSet<&str> = [
            "weacon-archtest",
            "weacon-observability",
            "weacon-protocol",
            "weacon-extra",
        ]
        .into();

        assert_eq!(
            check_rules(&names),
            [
                "weacon-extra: has no row in rules.rs",
                "weacon-version: has a row in rules.rs but no package"
            ]
        );
    }

    #[test]
    fn dependencies_follow_the_rules() {
        let names: BTreeSet<&str> = [
            "weacon-model",
            "weacon-protocol",
            "weacon-agents",
            "weacon-testing",
        ]
        .into();
        let model = package(
            "weacon-model",
            &[
                ("weacon-protocol", None),
                ("weacon-agents", Some(Kind::Dev)),
                ("sqlx", None),
                ("sentry-types", None),
                ("tokio", None),
                ("mio", Some(Kind::Dev)),
            ],
        );

        assert_eq!(
            check_dependencies(
                &model,
                &rule("weacon-model", &["weacon-protocol"], false),
                &names
            ),
            [
                "weacon-model: may not depend on weacon-agents",
                "weacon-model: may not depend on sqlx; only weacon-core, weacon-server may",
                "weacon-model: may not depend on sentry-types; only weacon-observability may",
                "weacon-model: does no I/O, so it may not depend on tokio",
            ]
        );
    }

    #[test]
    fn apps_log_and_report_their_version_from_the_start() {
        let bare = package("weacon-core", &[("weacon-version", None)]);
        let wired = package(
            "weacon-core",
            &[
                ("weacon-observability", None),
                ("weacon-version", None),
                ("weacon-version", Some(Kind::Build)),
            ],
        );

        assert_eq!(
            check_app_needs(&bare),
            [
                "weacon-core: is an app, so it needs weacon-observability as a normal dependency",
                "weacon-core: is an app, so it needs weacon-version as a build dependency",
            ]
        );
        assert!(check_app_needs(&wired).is_empty());
    }

    #[test]
    fn manifests_take_lints_fields_and_versions_from_the_workspace() {
        let manifest: Table = toml::from_str(
            r#"
            [package]
            name = "weacon-x"
            version = "0.1.0"
            edition.workspace = true
            rust-version.workspace = true
            license.workspace = true
            publish.workspace = true

            [dependencies]
            serde.workspace = true
            regex = "1"

            [target.'cfg(unix)'.dev-dependencies]
            libc = { version = "0.2" }
            "#,
        )
        .unwrap();

        assert_eq!(
            check_manifest("weacon-x", &manifest),
            [
                "weacon-x: doesn't take the workspace lints ([lints] workspace = true)",
                "weacon-x: sets package.version itself instead of taking the workspace's",
                "weacon-x: dependencies.regex must come from the workspace (workspace = true)",
                "weacon-x: dev-dependencies.libc must come from the workspace (workspace = true)",
            ]
        );
    }
}
