use weacon_protocol::{Author, MemberId};

/// How many changes made something what it is, its creation being the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(u64);

impl Revision {
    pub const FIRST: Self = Self(1);

    #[must_use]
    pub const fn new(version: u64) -> Self {
        Self(version)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Who is behind one revision, and when. An agent's change takes effect only once a person
/// accepts it, so every change names that person; for a person's own it is them.
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
