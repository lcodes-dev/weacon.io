//! A repository in memory, for the code map's tests.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use weacon_observability::Fault;
use weacon_protocol::code;

use super::{Source, content_key};

/// A repository in memory: commits by id, each its files' text by path.
#[derive(Clone, Default)]
pub(crate) struct Fake {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    failing: bool,
    head: String,
    commits: BTreeMap<String, BTreeMap<String, String>>,
    changed: Vec<(String, Option<String>)>,
    blobs_read: Vec<String>,
}

pub(crate) fn blob(text: &str) -> String {
    format!("blob-{}", content_key(text))
}

impl Fake {
    pub(crate) fn commit(&self, id: &str, files: &[(&str, &str)]) {
        let mut state = self.state.lock().unwrap();
        let files = files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()));
        state.commits.insert(id.to_owned(), files.collect());
        id.clone_into(&mut state.head);
    }

    pub(crate) fn change(&self, changed: &[(&str, Option<&str>)]) {
        let changed = changed
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.map(str::to_owned)));
        self.state.lock().unwrap().changed = changed.collect();
    }

    pub(crate) fn blobs_read(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().unwrap().blobs_read)
    }
}

impl Fake {
    pub(crate) fn fail(&self, failing: bool) {
        self.state.lock().unwrap().failing = failing;
    }
}

impl Source for Fake {
    fn head(&self) -> Result<String, Fault> {
        let state = self.state.lock().unwrap();
        if state.failing {
            return Err(Fault::new(
                code!("map.read_failed"),
                "The repository couldn't be read",
            ));
        }
        Ok(state.head.clone())
    }

    fn tree(&self, commit: &str) -> Result<Vec<(String, String)>, Fault> {
        let state = self.state.lock().unwrap();
        let files = state.commits.get(commit).cloned().unwrap_or_default();
        Ok(files
            .into_iter()
            .map(|(path, text)| (path, blob(&text)))
            .collect())
    }

    fn blobs(&self, blobs: &[String]) -> Result<BTreeMap<String, String>, Fault> {
        let mut state = self.state.lock().unwrap();
        state.blobs_read.extend(blobs.iter().cloned());
        let texts: BTreeMap<String, String> = state
            .commits
            .values()
            .flat_map(|files| files.values().map(|text| (blob(text), text.clone())))
            .collect();
        Ok(blobs
            .iter()
            .filter_map(|id| Some((id.clone(), texts.get(id)?.clone())))
            .collect())
    }

    fn changed(&self, _: &Path, _: &str) -> Result<Vec<(String, Option<String>)>, Fault> {
        Ok(self.state.lock().unwrap().changed.clone())
    }
}
