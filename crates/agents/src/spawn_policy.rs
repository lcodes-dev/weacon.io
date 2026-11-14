use std::collections::BTreeMap;
use std::ffi::OsString;

use tokio::process::{Child, Command};

/// What every agent process starts with here; the core builds it for its machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnPolicy {
    /// Variables added to the agent's environment, which its tools inherit.
    pub env: BTreeMap<OsString, OsString>,
    /// Written to the agent's `/proc/<pid>/oom_score_adj` on Linux; its tools inherit it.
    pub oom_score_adj: Option<i32>,
}

impl SpawnPolicy {
    /// Adds the variables the driver doesn't set or remove itself, so a sign-in can't change.
    pub(crate) fn add_env(&self, command: &mut Command) {
        let own: Vec<OsString> = command
            .as_std()
            .get_envs()
            .map(|(key, _)| key.to_owned())
            .collect();
        for (key, value) in &self.env {
            if !own.contains(key) {
                command.env(key, value);
            }
        }
    }

    /// Adjusts the agent just started, long before it can start a tool. The core sets a score
    /// only where it could write its own, so this fails only for an agent already gone.
    pub(crate) fn set_oom_score(&self, child: &Child) {
        if let (Some(score), Some(pid)) = (self.oom_score_adj, child.id()) {
            drop(std::fs::write(
                format!("/proc/{pid}/oom_score_adj"),
                score.to_string(),
            ));
        }
    }
}
