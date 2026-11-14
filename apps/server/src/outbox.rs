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

// A letter holds a live code, so one that fails must leave nothing behind, even when it failed
// after landing, as when its folder can't be synced.
fn post(dir: &Path, name: &str, letter: &Letter<'_>) -> io::Result<()> {
    let json = serde_json::to_vec(letter).map_err(io::Error::other)?;
    let path = dir.join(format!("{name}.json"));
    weacon_appkit::write_private(&path, &json).inspect_err(|_| drop(fs::remove_file(&path)))
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::email;

    #[test]
    fn each_code_waits_whole_in_the_outbox() {
        let dir = TempDir::new().unwrap();
        Outbox::new(dir.to_path_buf())
            .send(&email("ada@example.com"), "123456")
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
        assert_ne!(
            weacon_appkit::is_private(&fs::metadata(file).unwrap()),
            Some(false)
        );
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
        assert_eq!(
            fs::read_dir(&*dir).unwrap().count(),
            1,
            "only the folder is left"
        );
    }

    // A folder that can be written but not read takes the letter but can't be synced, except
    // for root, who reads anything.
    #[cfg(unix)]
    #[test]
    fn a_letter_that_lands_but_cant_be_synced_is_removed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let outbox = dir.join("outbox");
        fs::create_dir(&outbox).unwrap();
        fs::set_permissions(&outbox, fs::Permissions::from_mode(0o300)).unwrap();
        let letter = Letter {
            to: "ada@example.com",
            code: "123456",
        };
        let posted = post(&outbox, "letter", &letter);
        let landed = outbox.join("letter.json").exists();
        fs::set_permissions(&outbox, fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(landed, posted.is_ok());
        assert_eq!(fs::read_dir(&outbox).unwrap().count(), usize::from(landed));
    }

    #[test]
    fn a_missing_outbox_is_a_failure() {
        let dir = TempDir::new().unwrap();
        let error = Outbox::new(dir.join("missing"))
            .send(&email("ada@example.com"), "123456")
            .unwrap_err();

        assert_eq!(error.code().as_str(), "mail.failed");
    }
}
