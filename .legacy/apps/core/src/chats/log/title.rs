//! A chat's title, read from the start of its log.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use weacon_protocol::{ChatEntry, ChatEvent, ChatId};

/// How many entries at the start of a log are read for the prompt that titles it.
const LOOKS_AT: usize = 8;

/// `chat`'s title, from its log under its workspace's folder `workspace_root`: its first
/// prompt's first line, as the server titles it; empty when the log starts with none.
pub(crate) fn title(workspace_root: &Path, chat: &ChatId) -> io::Result<String> {
    let file = match File::open(super::path(workspace_root, chat)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        file => file?,
    };
    for line in BufReader::new(file).lines().take(LOOKS_AT) {
        let Ok(entry) = serde_json::from_str::<ChatEntry>(&line?) else {
            break;
        };
        if let ChatEvent::Prompt { text, .. } | ChatEvent::OutsidePrompt { text } = entry.event {
            return Ok(weacon_model::chat_title(&text));
        }
    }
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::chats::log::ChatLog;

    fn chat(id: &str) -> ChatId {
        id.parse().unwrap()
    }

    #[tokio::test]
    async fn a_chat_is_titled_by_the_first_prompt_near_the_start_of_its_log() {
        let dir = TempDir::new().unwrap();
        let mut log = ChatLog::open(&dir, &chat("c-1")).await.unwrap();
        let status = |state| ChatEvent::RunStatus {
            status: weacon_protocol::RunStatus {
                run: "run-1".parse().unwrap(),
                state,
                agent: None,
            },
        };
        for _ in 0..LOOKS_AT - 1 {
            log.append(status(weacon_protocol::RunState::Idle))
                .await
                .unwrap();
        }
        let text = "  Fix the header\nthen the footer".to_owned();
        log.append(ChatEvent::OutsidePrompt { text }).await.unwrap();

        assert_eq!(title(&dir, &chat("c-1")).unwrap(), "Fix the header");
        assert_eq!(title(&dir, &chat("c-2")).unwrap(), "");
    }

    #[tokio::test]
    async fn a_log_with_no_prompt_near_its_start_or_a_broken_line_has_no_title() {
        let dir = TempDir::new().unwrap();
        let mut late = ChatLog::open(&dir, &chat("c-1")).await.unwrap();
        for _ in 0..=LOOKS_AT {
            let text = "Go".to_owned();
            late.append(ChatEvent::OutsidePrompt { text })
                .await
                .unwrap();
        }
        let path = super::super::path(&dir, &chat("c-1"));
        let lines = fs::read_to_string(&path).unwrap();
        let shifted = lines.replacen("outside_prompt", "outside", LOOKS_AT);
        fs::write(&path, shifted).unwrap();
        let prompt = lines.lines().next().unwrap();
        fs::write(
            super::super::path(&dir, &chat("c-2")),
            format!("not json\n{prompt}\n"),
        )
        .unwrap();
        fs::create_dir_all(super::super::path(&dir, &chat("c-3"))).unwrap();

        assert_eq!(title(&dir, &chat("c-1")).unwrap(), "");
        assert_eq!(title(&dir, &chat("c-2")).unwrap(), "");
        title(&dir, &chat("c-3")).unwrap_err();
    }
}
