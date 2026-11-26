use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::{AppState, chats, db, live};

#[derive(Clone, Serialize, Deserialize)]
pub struct StartTurn {
    pub machine_id: String,
    pub chat_id: String,
}

impl StartTurn {
    pub async fn handle(self, state: &AppState) -> Result<()> {
        live::chat(state, &self.chat_id).await;
        let running = db::chats::running(&state.pool, Some(&self.machine_id)).await?;
        if let Some(chat) = running.into_iter().find(|chat| chat.id == self.chat_id) {
            chats::send_turn(state, &self.machine_id, chat).await;
        }
        Ok(())
    }
}
