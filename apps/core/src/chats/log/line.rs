//! A chat log's entry as the line it is stored as.

use weacon_observability::Fault;
use weacon_protocol::{ChatEntry, code};

use super::ENTRY_MAX;

/// The entry as one line of the log, if it can be written and fits one frame.
pub(super) fn entry_line(entry: &ChatEntry) -> Result<Vec<u8>, Fault> {
    let mut line = serde_json::to_vec(entry).map_err(|_| {
        Fault::new(
            code!("chat.event_unwritable"),
            "The event holds a part only ever received",
        )
    })?;
    line.push(b'\n');
    if line.len() > ENTRY_MAX {
        return Err(Fault::new(
            code!("chat.event_too_large"),
            "The event doesn't fit one frame",
        ));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatEvent, Outcome, Receipt};

    use super::*;

    #[test]
    fn an_entry_is_one_line_unless_it_holds_a_newer_event() {
        let entry = |event| ChatEntry { seq: 1, event };
        let accepted = ChatEvent::Receipt {
            receipt: Receipt {
                request: "q-1".parse().unwrap(),
                outcome: Outcome::Accepted,
            },
        };

        let line = entry_line(&entry(accepted)).unwrap();
        let newer = entry_line(&entry(ChatEvent::Newer)).unwrap_err();

        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert!(line.ends_with(b"\n"));
        assert_eq!(newer.code().as_str(), "chat.event_unwritable");
    }
}
