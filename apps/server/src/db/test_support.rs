use weacon_model::Attribution;
use weacon_observability::Fault;

use crate::test_support::member_id;

/// When the db tests' changes are made, in seconds since the Unix epoch.
pub(super) const NOW: i64 = 1_762_077_600;

/// A change `member` makes at [`NOW`].
pub(super) fn by(member: &str) -> Attribution {
    Attribution::by(member_id(member), NOW)
}

pub(super) fn code(fault: &Fault) -> &str {
    fault.code().as_str()
}

/// The id `text` names, of whatever id type the test needs.
pub(super) fn id<T: TryFrom<String>>(text: &str) -> T {
    T::try_from(text.to_owned()).ok().unwrap()
}
