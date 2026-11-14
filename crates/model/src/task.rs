use weacon_protocol::{TaskId, TaskStatus};

use crate::{OrderKey, Revision, Title};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub title: Title,
    pub status: TaskStatus,
    pub order: OrderKey,
    pub version: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskEdit {
    Title(Title),
    Status(TaskStatus),
    Order(OrderKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The task changed since the version the edit was made against.
    Stale,
    /// The status is one this build doesn't know.
    UnknownStatus,
    /// The task is at the last version there is.
    LastVersion,
}

impl Task {
    /// A new task, to do, at its first version.
    #[must_use]
    pub const fn new(id: TaskId, title: Title, order: OrderKey) -> Self {
        Self {
            id,
            title,
            status: TaskStatus::Todo,
            order,
            version: Revision::FIRST,
        }
    }

    /// The task after `edit`, one version on. `read_at` is the version the edit was made
    /// against: any other than the current one is refused.
    ///
    /// # Errors
    /// When the task has changed since `read_at`, the edit sets a status this build doesn't
    /// know, or no version is left.
    pub fn edit(mut self, read_at: Revision, edit: TaskEdit) -> Result<Self, EditError> {
        if read_at != self.version {
            return Err(EditError::Stale);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn title(text: &str) -> Title {
        Title::try_from(text.to_owned()).unwrap()
    }

    fn order(text: &str) -> OrderKey {
        OrderKey::try_from(text.to_owned()).unwrap()
    }

    fn task() -> Task {
        let id = TaskId::try_from("t-1".to_owned()).unwrap();
        Task::new(id, title("Write the docs"), order("V"))
    }

    #[test]
    fn a_new_task_is_to_do_at_its_first_version() {
        let task = task();

        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.version, Revision::FIRST);
        assert_eq!(task.version.get(), 1);
        assert_eq!(Revision::new(7).get(), 7);
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
            .edit(Revision::FIRST, TaskEdit::Title(title("Ship")))
            .unwrap();
        let done = renamed
            .clone()
            .edit(Revision::new(2), TaskEdit::Status(TaskStatus::Done))
            .unwrap();
        let moved = done
            .clone()
            .edit(Revision::new(3), TaskEdit::Order(order("W")))
            .unwrap();

        assert_eq!(
            renamed,
            Task {
                title: title("Ship"),
                version: Revision::new(2),
                ..task
            }
        );
        assert_eq!(
            done,
            Task {
                status: TaskStatus::Done,
                version: Revision::new(3),
                ..renamed
            }
        );
        assert_eq!(
            moved,
            Task {
                order: order("W"),
                version: Revision::new(4),
                ..done
            }
        );
    }

    #[test]
    fn an_edit_against_another_version_is_refused() {
        let task = task()
            .edit(Revision::FIRST, TaskEdit::Status(TaskStatus::Done))
            .unwrap();
        for read_at in [Revision::FIRST, Revision::new(3)] {
            let edit = TaskEdit::Title(title("Ship"));
            assert_eq!(task.clone().edit(read_at, edit), Err(EditError::Stale));
        }
    }

    #[test]
    fn a_status_this_build_doesnt_know_is_refused() {
        let edit = TaskEdit::Status(TaskStatus::Newer);

        assert_eq!(
            task().edit(Revision::FIRST, edit),
            Err(EditError::UnknownStatus)
        );
    }

    #[test]
    fn a_task_at_the_last_version_cant_be_edited() {
        let last = Revision::new(u64::MAX);
        let task = Task {
            version: last,
            ..task()
        };

        assert_eq!(
            task.edit(last, TaskEdit::Status(TaskStatus::Done)),
            Err(EditError::LastVersion)
        );
    }
}
