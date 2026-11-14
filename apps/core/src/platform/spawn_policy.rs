use std::ffi::OsString;
use std::fs;
use std::num::NonZeroUsize;
use std::path::Path;

use weacon_agents::SpawnPolicy;

use super::{Env, var};

// How far above the core agents and their tools are for the OOM killer, so it picks them first.
const TOOL_OOM_RAISE: i32 = 500;
const OOM_SCORE_MAX: i32 = 1000;

// Tools that would otherwise count the host's CPUs rather than the quota; the person's own
// settings win.
const CPU_COUNT_VARS: [&str; 5] = [
    "CARGO_BUILD_JOBS",
    "GOMAXPROCS",
    "OMP_NUM_THREADS",
    "PYTHON_CPU_COUNT",
    "RAYON_NUM_THREADS",
];

/// How agents start here; on Linux their tools go first when memory runs out and use only the
/// CPUs the core may.
pub(crate) fn spawn_policy(env: Env<'_>) -> SpawnPolicy {
    if !cfg!(target_os = "linux") {
        return SpawnPolicy::default();
    }
    // Counts the cgroup's CPU quota and affinity, not just the host's CPUs.
    let cpus = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    tool_policy(env, cpus, oom_score(Path::new("/proc/self/oom_score_adj")))
}

// None where the core can't change its score, as on WSL 1 or with procfs read-only; writing
// the same score back proves it can.
fn oom_score(file: &Path) -> Option<i32> {
    let score = fs::read_to_string(file).ok()?;
    fs::write(file, &score).ok()?;
    score.trim().parse().ok()
}

fn tool_policy(env: Env<'_>, cpus: usize, own_oom_score: Option<i32>) -> SpawnPolicy {
    let count = OsString::from(cpus.to_string());
    SpawnPolicy {
        env: CPU_COUNT_VARS
            .into_iter()
            .filter(|name| var(env, name).is_none())
            .map(|name| (name.into(), count.clone()))
            .collect(),
        oom_score_adj: own_oom_score
            .map(|own| own.saturating_add(TOOL_OOM_RAISE).min(OOM_SCORE_MAX)),
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::env;

    #[test]
    fn tools_get_the_cpu_count_and_a_higher_oom_score_than_the_core() {
        use std::collections::BTreeMap;

        let vars = env(&[("GOMAXPROCS", "8"), ("OMP_NUM_THREADS", "")]);
        let policy = tool_policy(&vars, 2, Some(100));
        let two = OsString::from("2");

        assert_eq!(
            policy.env,
            BTreeMap::from([
                ("CARGO_BUILD_JOBS".into(), two.clone()),
                ("OMP_NUM_THREADS".into(), two.clone()),
                ("PYTHON_CPU_COUNT".into(), two.clone()),
                ("RAYON_NUM_THREADS".into(), two),
            ])
        );
        assert_eq!(policy.oom_score_adj, Some(600));
        assert_eq!(tool_policy(&vars, 1, Some(700)).oom_score_adj, Some(1000));
        assert_eq!(tool_policy(&vars, 1, None).oom_score_adj, None);
    }

    #[test]
    fn the_cores_oom_score_is_read_when_there_is_one() {
        let dir = TempDir::new().unwrap();
        let file = dir.join("oom_score_adj");
        fs::write(&file, "-200\n").unwrap();

        assert_eq!(oom_score(&file), Some(-200));
        assert_eq!(fs::read_to_string(&file).unwrap(), "-200\n");
        assert_eq!(oom_score(&dir.join("missing")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_score_that_cant_be_written_is_left_alone() {
        // Readable, but refuses writes even from root.
        assert_eq!(oom_score(Path::new("/proc/self/oom_score")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_agents_start_under_a_policy() {
        let policy = spawn_policy(&env(&[]));

        // None where this test's procfs is read-only.
        let own = oom_score(Path::new("/proc/self/oom_score_adj"));

        assert_eq!(policy.env.len(), CPU_COUNT_VARS.len());
        assert_eq!(policy.oom_score_adj, own.map(|own| (own + 500).min(1000)));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn elsewhere_agents_start_as_they_are() {
        assert_eq!(spawn_policy(&env(&[])), SpawnPolicy::default());
    }
}
