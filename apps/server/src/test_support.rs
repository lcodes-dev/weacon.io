use weacon_version::Version;

pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

pub(crate) const VERSION: Version = Version {
    name: "weacon-server",
    version: "0.1.0",
    commit: "1a2b3c4",
};

pub(crate) fn workspace_id(id: &str) -> weacon_protocol::WorkspaceId {
    weacon_protocol::WorkspaceId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn member_id(id: &str) -> weacon_protocol::MemberId {
    weacon_protocol::MemberId::try_from(id.to_owned()).unwrap()
}
