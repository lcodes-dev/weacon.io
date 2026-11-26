use std::time::Duration;

use core_link::{Agent, Found, Ready, ToCore};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::db::machines::Machine;
use crate::error::{ApiError, Result};
use crate::{AppState, db};

const PATIENCE: Duration = Duration::from_secs(15 * 60);
const AWAY: Duration = Duration::from_secs(10 * 60);

pub async fn ask(
    state: &AppState,
    user_id: &str,
    agent: Agent,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    let machines = db::machines::list(&state.pool, user_id).await?;
    let signed_in = Found {
        agent,
        ready: Ready::SignedIn,
    };
    let ready = |machine: &&Machine| {
        let found: Vec<Found> = serde_json::from_str(&machine.agents).unwrap_or_default();
        state.cores.connected(&machine.id) && found.contains(&signed_in)
    };
    let Some(machine) = machines.iter().find(ready) else {
        let program = agent.program();
        return Err(ApiError::invalid(match machines.len() {
            0 => "Pair a machine of yours first, under Machines".to_owned(),
            _ => format!(
                "None of your machines is connected with {program} signed in. Look under Machines"
            ),
        }));
    };
    let slow = format!("{} took too long", agent.program());
    let message = |id| ToCore::Ask {
        id,
        agent,
        model: model.to_owned(),
        system: system.to_owned(),
        prompt: prompt.to_owned(),
    };
    answered(state, (&machine.id, &machine.name), &slow, message).await
}

pub async fn answered(
    state: &AppState,
    (machine, name): (&str, &str),
    slow: &str,
    message: impl FnOnce(String) -> ToCore,
) -> Result<String> {
    let id = uuid::Uuid::now_v7().to_string();
    let message = message(id.clone());
    let (asker, mut answer) = oneshot::channel();
    let asked = (machine.to_owned(), message.clone(), asker);
    state.cores.lock().asks.insert(id.clone(), asked);
    state.cores.send(machine, message);

    let started = Instant::now();
    let gave_up = loop {
        tokio::select! {
            answer = &mut answer => {
                let answer = answer.unwrap_or_else(|_| Err("Something went wrong".into()));
                return answer.map_err(ApiError::invalid);
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
        let left = state.cores.lock().left.get(machine).copied();
        if left.is_some_and(|left| left.elapsed() > AWAY) {
            break format!("{name} went away and didn't come back");
        }
        if started.elapsed() > PATIENCE {
            break slow.to_owned();
        }
    };
    state.cores.lock().asks.remove(&id);
    Err(ApiError::invalid(gave_up))
}
