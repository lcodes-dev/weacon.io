use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::Serialize;
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::email::Email;

/// One code for a mail relay to send.
#[derive(Serialize)]
struct Letter<'a> {
    to: &'a str,
    code: &'a str,
}

/// A folder where each sign-in code waits as a JSON file, readable only by this user, for a
/// mail relay to send and delete.
#[derive(Clone, Debug)]
pub(crate) struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    pub(crate) const fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Leaves `code` for `to`; it blocks, so async code calls it through `spawn_blocking`.
    pub(crate) fn send(&self, to: &Email, code: &str) -> Result<(), Fault> {
        let letter = Letter {
            to: to.as_str(),
            code,
        };
        post(&self.dir, &crate::app::new_id(), &letter)
            .map_err(|error| mail_failed().because(error))
    }
}

pub(crate) fn mail_failed() -> Fault {
    Fault::new(code!("mail.failed"), "The code couldn't be sent")
}

// Written beside its final name and renamed, so a relay never reads half a letter; one that
// fails is removed, since it holds a live code.
fn post(dir: &Path, name: &str, letter: &Letter<'_>) -> io::Result<()> {
    let json = serde_json::to_vec(letter).map_err(io::Error::other)?;
    let writing = dir.join(format!("{name}.writing"));
    let posted = weacon_appkit::private_append(&writing)
        .and_then(|mut file| {
            file.write_all(&json)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&writing, dir.join(format!("{name}.json"))));
    if posted.is_err() {
        drop(fs::remove_file(&writing));
    }
    posted
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;

    #[test]
    fn each_code_waits_whole_in_the_outbox() {
        let dir = TempDir::new().unwrap();
        Outbox::new(dir.to_path_buf())
            .send(&Email::parse("ada@example.com").unwrap(), "123456")
            .unwrap();
        let files: Vec<PathBuf> = fs::read_dir(&*dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let [file] = files.as_slice() else {
            panic!("{files:?}");
        };
        let letter: serde_json::Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();

        assert_eq!(file.extension().unwrap(), "json");
        assert_eq!(
            letter,
            serde_json::json!({ "to": "ada@example.com", "code": "123456" })
        );
    }

    #[test]
    fn a_letter_that_cant_be_posted_is_removed() {
        let dir = TempDir::new().unwrap();
        // The final name is taken by a folder, so the rename fails.
        fs::create_dir(dir.join("letter.json")).unwrap();
        let letter = Letter {
            to: "ada@example.com",
            code: "123456",
        };

        post(&dir, "letter", &letter).unwrap_err();
        assert!(!dir.join("letter.writing").exists());
    }

    #[test]
    fn a_missing_outbox_is_a_failure() {
        let dir = TempDir::new().unwrap();
        let error = Outbox::new(dir.join("missing"))
            .send(&Email::parse("ada@example.com").unwrap(), "123456")
            .unwrap_err();

        assert_eq!(error.code().as_str(), "mail.failed");
    }
}
