use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, MachineId, MachineName, ProposalChange, ProposalId,
};

use super::member_id;
use crate::db::Proposed;

/// `change`, proposed as `proposal` by m1's Claude on the machine `machine`.
pub(crate) fn proposed(proposal: &str, machine: &str, change: ProposalChange) -> Proposed {
    Proposed {
        id: ProposalId::try_from(proposal.to_owned()).unwrap(),
        change,
        owner: member_id("m1"),
        agent: AgentRef {
            kind: AgentKind::Claude,
            owner: AgentOwner::Member {
                member: member_id("m1"),
            },
            machine: Some(MachineName::try_from(format!("{machine} laptop")).unwrap()),
        },
        machine: MachineId::try_from(machine.to_owned()).unwrap(),
        fresh: format!("new-{proposal}"),
        at: 0,
    }
}
