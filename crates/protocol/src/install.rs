use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RequestId;

/// From `weacon update` to its own core's daemon that offers `update.leave`, over the daemon's
/// local socket: leave to install a build that restarts the daemon, then how the install went.
/// The daemon answers each with a [`crate::Receipt`] that echoes `request`, or with the error
/// `update.chat_busy` when it refuses leave, or `update.no_leave` for a report without leave.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstallCommand {
    pub request: RequestId,
    pub action: InstallAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallAction {
    /// Given only while no chat is busy. From then on no turn starts and no chat can be held
    /// until `failed`, until the asker hangs up, or until the leave runs out.
    Leave,
    /// The new build is in place and the daemon is about to be restarted; turns stay paused,
    /// even once the asker hangs up, until the leave runs out again. Refused once the leave has
    /// run out, and the asker then puts the old build back.
    Restarting,
    /// The install failed and the build before it is back; turns go on.
    Failed,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Envelope, Message};

    #[test]
    fn install_commands_carry_a_tagged_action() {
        let command = Envelope {
            id: 2,
            message: Message::InstallCommand(InstallCommand {
                request: RequestId::try_from("leave".to_owned()).unwrap(),
                action: InstallAction::Leave,
            }),
        };
        let wire = json!({
            "id": 2,
            "type": "install_command",
            "request": "leave",
            "action": { "kind": "leave" },
        });
        assert_eq!(serde_json::to_value(&command).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), command);
        let newer = json!({ "request": "x", "action": { "kind": "later" } });
        let newer: InstallCommand = serde_json::from_value(newer).unwrap();
        assert_eq!(newer.action, InstallAction::Newer);
    }
}
