//! A repository in memory, for the code map's tests.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use weacon_observability::Fault;
use weacon_protocol::CommitId;

use super::{Source, content_key, read_failed};

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
    texts_read: Vec<String>,
}

pub(crate) fn blob(text: &str) -> String {
    format!("blob-{}", content_key(text))
}

/// The commit a test calls `name`, which must be hex.
pub(crate) fn commit_id(name: &str) -> CommitId {
    CommitId::try_from(format!("{name:0>40}")).unwrap()
}

impl Fake {
    pub(crate) fn commit(&self, name: &str, files: &[(&str, &str)]) {
        let mut state = self.state.lock().unwrap();
        let files = files
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()));
        let id = commit_id(name).as_str().to_owned();
        state.commits.insert(id.clone(), files.collect());
        state.head = id;
    }

    pub(crate) fn change(&self, changed: &[(&str, Option<&str>)]) {
        let changed = changed
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.map(str::to_owned)));
        self.state.lock().unwrap().changed = changed.collect();
    }

    pub(crate) fn texts_read(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().unwrap().texts_read)
    }

    pub(crate) fn blobs_read(&self) -> Vec<String> {
        std::mem::take(&mut self.state.lock().unwrap().blobs_read)
    }

    pub(crate) fn fail(&self, failing: bool) {
        self.state.lock().unwrap().failing = failing;
    }
}

impl Source for Fake {
    fn head(&self) -> Result<CommitId, Fault> {
        let state = self.state.lock().unwrap();
        if state.failing {
            return Err(read_failed());
        }
        Ok(CommitId::try_from(state.head.clone()).unwrap())
    }

    fn tree(&self, commit: &CommitId) -> Result<Vec<(String, String)>, Fault> {
        let state = self.state.lock().unwrap();
        let files = state
            .commits
            .get(commit.as_str())
            .cloned()
            .unwrap_or_default();
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

    fn changed(&self, _: &Path, _: &CommitId) -> Result<Vec<String>, Fault> {
        let state = self.state.lock().unwrap();
        Ok(state.changed.iter().map(|(file, _)| file.clone()).collect())
    }

    fn text(&self, _: &Path, file: &str) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        state.texts_read.push(file.to_owned());
        let (_, text) = state.changed.iter().find(|(changed, _)| changed == file)?;
        text.clone()
    }
}
