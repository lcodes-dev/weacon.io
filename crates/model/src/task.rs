use weacon_protocol::{Author, MemberId, TaskId, TaskStatus};

use crate::OrderKey;

const TITLE_MAX: usize = 200;

/// A task's title: one line of 1 to 200 characters, with no space at either end and nothing
/// that changes the direction of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskTitle(String);

impl TaskTitle {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TaskTitle {
    type Error = String;

    fn try_from(title: String) -> Result<Self, Self::Error> {
        let fits = (1..=TITLE_MAX).contains(&title.chars().count())
            && !title.contains(|char: char| char.is_control() || breaks_or_turns(char));
        if fits && title.trim() == title {
            Ok(Self(title))
        } else {
            Err(title)
        }
    }
}

/// Whether `char` is a line or paragraph separator, or overrides or isolates text direction.
const fn breaks_or_turns(char: char) -> bool {
    matches!(char, '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// How many changes made a task what it is, its creation being the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskVersion(u64);

impl TaskVersion {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn new(version: u64) -> Self {
        Self(version)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: TaskTitle,
    pub status: TaskStatus,
    pub order: OrderKey,
    pub version: TaskVersion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskEdit {
    Title(TaskTitle),
    Status(TaskStatus),
    Order(OrderKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The task changed since the version the edit was made against.
    Stale { current: TaskVersion },
    /// The status is one this build doesn't know.
    UnknownStatus,
    /// The task is at the last version there is.
    LastVersion,
}

impl Task {
    /// A new task, to do, at its first version.
    #[must_use]
    pub const fn new(id: TaskId, title: TaskTitle, order: OrderKey) -> Self {
        Self {
            id,
            title,
            status: TaskStatus::Todo,
            order,
            version: TaskVersion::FIRST,
        }
    }

    /// The task after `edit`, one version on. `read_at` is the version the edit was made
    /// against: any other than the current one is refused.
    ///
    /// # Errors
    /// When the task has changed since `read_at`, the edit sets a status this build doesn't
    /// know, or no version is left.
    pub fn edit(mut self, read_at: TaskVersion, edit: TaskEdit) -> Result<Self, EditError> {
        if read_at != self.version {
            return Err(EditError::Stale {
                current: self.version,
            });
        }
        match edit {
            TaskEdit::Title(title) => self.title = title,
            TaskEdit::Status(TaskStatus::Newer) => return Err(EditError::UnknownStatus),
            TaskEdit::Status(status) => self.status = status,
            TaskEdit::Order(order) => self.order = order,
        }
        self.version = self.version.next().ok_or(EditError::LastVersion)?;
        Ok(self)
    }
}

/// Who is behind one version of a task, and when. An agent's change takes effect only once
/// a person accepts it, so every change names that person; for a person's own it is them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    author: Author,
    accepted_by: MemberId,
    at: i64,
}

impl Change {
    /// A change a person made themselves, at `at` seconds since the Unix epoch.
    #[must_use]
    pub fn by(member: MemberId, at: i64) -> Self {
        let author = Author::Person {
            member: member.clone(),
        };
        Self {
            author,
            accepted_by: member,
            at,
        }
    }

    /// A change that the agent of `owner` proposed and `accepted_by` accepted, at `at`
    /// seconds since the Unix epoch.
    #[must_use]
    pub const fn accepted(owner: MemberId, accepted_by: MemberId, at: i64) -> Self {
        Self {
            author: Author::Agent { owner },
            accepted_by,
            at,
        }
    }

    #[must_use]
    pub const fn author(&self) -> &Author {
        &self.author
    }

    #[must_use]
    pub const fn accepted_by(&self) -> &MemberId {
        &self.accepted_by
    }

    #[must_use]
    pub const fn at(&self) -> i64 {
        self.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> TaskTitle {
        TaskTitle::try_from(text.to_owned()).unwrap()
    }

    fn order(text: &str) -> OrderKey {
        OrderKey::try_from(text.to_owned()).unwrap()
    }

    fn task() -> Task {
        let id = TaskId::try_from("t-1".to_owned()).unwrap();
        Task::new(id, title("Write the docs"), order("V"))
    }

    #[test]
    fn titles_are_one_trimmed_line_of_limited_length() {
        let longest = "é".repeat(TITLE_MAX);
        for good in ["a", "Write the docs", longest.as_str()] {
            assert_eq!(title(good).as_str(), good);
        }
        let too_long = "a".repeat(TITLE_MAX + 1);
        let turned = [
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{202A}b",
            "a\u{202E}b",
            "a\u{2066}b",
            "a\u{2069}b",
        ];
        for good in ["a\u{2027}b", "a\u{202F}b", "a\u{2065}b", "a\u{206A}b"] {
            assert_eq!(title(good).as_str(), good);
        }
        for bad in ["", " ", " a", "a ", "a\nb", "a\tb", too_long.as_str()]
            .into_iter()
            .chain(turned)
        {
            assert_eq!(TaskTitle::try_from(bad.to_owned()), Err(bad.to_owned()));
        }
    }

    #[test]
    fn a_new_task_is_to_do_at_its_first_version() {
        let task = task();

        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.version, TaskVersion::FIRST);
        assert_eq!(task.version.get(), 1);
        assert_eq!(TaskVersion::new(7).get(), 7);
        assert_eq!(
            (task.title, task.order),
            (title("Write the docs"), order("V"))
        );
    }

    #[test]
    fn each_edit_changes_one_thing_and_raises_the_version() {
        let task = task();
        let renamed = task
            .clone()
            .edit(TaskVersion::FIRST, TaskEdit::Title(title("Ship")))
            .unwrap();
        let done = renamed
            .clone()
            .edit(TaskVersion::new(2), TaskEdit::Status(TaskStatus::Done))
            .unwrap();
        let moved = done
            .clone()
            .edit(TaskVersion::new(3), TaskEdit::Order(order("W")))
            .unwrap();

        assert_eq!(
            renamed,
            Task {
                title: title("Ship"),
                version: TaskVersion::new(2),
                ..task
            }
        );
        assert_eq!(
            done,
            Task {
                status: TaskStatus::Done,
                version: TaskVersion::new(3),
                ..renamed
            }
        );
        assert_eq!(
            moved,
            Task {
                order: order("W"),
                version: TaskVersion::new(4),
                ..done
            }
        );
    }

    #[test]
    fn an_edit_against_another_version_is_refused() {
        let task = task()
            .edit(TaskVersion::FIRST, TaskEdit::Status(TaskStatus::Done))
            .unwrap();
        let stale = EditError::Stale {
            current: TaskVersion::new(2),
        };

        for read_at in [TaskVersion::FIRST, TaskVersion::new(3)] {
            let edit = TaskEdit::Title(title("Ship"));
            assert_eq!(task.clone().edit(read_at, edit), Err(stale));
        }
    }

    #[test]
    fn a_status_this_build_doesnt_know_is_refused() {
        let edit = TaskEdit::Status(TaskStatus::Newer);

        assert_eq!(
            task().edit(TaskVersion::FIRST, edit),
            Err(EditError::UnknownStatus)
        );
    }

    #[test]
    fn a_task_at_the_last_version_cant_be_edited() {
        let last = TaskVersion::new(u64::MAX);
        let task = Task {
            version: last,
            ..task()
        };

        assert_eq!(
            task.edit(last, TaskEdit::Status(TaskStatus::Done)),
            Err(EditError::LastVersion)
        );
    }

    #[test]
    fn a_change_names_who_made_it_and_who_accepted_it() {
        let member = |id: &str| MemberId::try_from(id.to_owned()).unwrap();
        let own = Change::by(member("m-1"), 1_700_000_000);
        let agents = Change::accepted(member("m-1"), member("m-2"), 1_700_000_001);

        assert_eq!(
            own.author(),
            &Author::Person {
                member: member("m-1")
            }
        );
        assert_eq!(
            (own.accepted_by(), own.at()),
            (&member("m-1"), 1_700_000_000)
        );
        assert_eq!(
            agents.author(),
            &Author::Agent {
                owner: member("m-1")
            }
        );
        assert_eq!(
            (agents.accepted_by(), agents.at()),
            (&member("m-2"), 1_700_000_001)
        );
    }
}
