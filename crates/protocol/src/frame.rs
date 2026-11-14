use std::{fmt, mem};

use crate::{
    ChatAction, ChatQueryKind, Code, DiffQueryKind, Envelope, MachineSettingsQueryKind, Message,
    WorkspaceChange, code,
};

/// The largest frame accepted or sent on any transport: one message's JSON, plus its newline on
/// a byte stream.
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    TooLarge,
    Invalid,
}

impl FrameError {
    #[must_use]
    pub const fn code(self) -> Code {
        match self {
            Self::TooLarge => code!("frame.too_large"),
            Self::Invalid => code!("frame.invalid"),
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "The frame is larger than the limit",
            Self::Invalid => "The frame isn't a valid message",
        })
    }
}

impl std::error::Error for FrameError {}

impl Envelope {
    /// The message as one frame of JSON, for transports that keep frames apart themselves.
    ///
    /// # Errors
    /// When the message is one only ever received, such as [`Message::Unknown`], or exceeds
    /// [`MAX_FRAME`].
    pub fn to_json(&self) -> Result<Vec<u8>, FrameError> {
        if only_received(&self.message) {
            return Err(FrameError::Invalid);
        }
        let json = serde_json::to_vec(self).map_err(|_| FrameError::Invalid)?;
        if json.len() > MAX_FRAME {
            return Err(FrameError::TooLarge);
        }
        Ok(json)
    }

    /// One frame of JSON, checked for size before it is parsed.
    ///
    /// # Errors
    /// When `json` exceeds [`MAX_FRAME`] or isn't a valid message.
    pub fn from_json(json: &[u8]) -> Result<Self, FrameError> {
        if json.len() > MAX_FRAME {
            return Err(FrameError::TooLarge);
        }
        serde_json::from_slice(json).map_err(|_| FrameError::Invalid)
    }
}

fn only_received(message: &Message) -> bool {
    match message {
        Message::Unknown => true,
        Message::Receipt(receipt) => receipt.outcome.is_newer(),
        Message::Run(update) => update.event.is_newer(),
        Message::Workspace(update) => matches!(update.change, WorkspaceChange::Newer),
        Message::ChatCommand(command) => matches!(command.action, ChatAction::Newer),
        Message::Chat(update) => update.entry.event.is_newer(),
        Message::ChatQuery(query) => matches!(query.query, ChatQueryKind::Newer),
        Message::ChatAnswer(answer) => answer.answer.is_newer(),
        Message::DiffQuery(query) => matches!(query.query, DiffQueryKind::Newer),
        Message::DiffAnswer(answer) => answer.answer.is_newer(),
        Message::MachineSettingsQuery(query) => {
            matches!(query.query, MachineSettingsQueryKind::Newer)
        }
        Message::MachineSettingsCommand(command) => command.action.is_newer(),
        Message::MachineSettingsAnswer(answer) => answer.answer.is_newer(),
        Message::Hello(_)
        | Message::Welcome(_)
        | Message::Error(_)
        | Message::Pick(_)
        | Message::Log(_) => false,
    }
}

/// One message as a line of JSON, for byte streams such as the core's socket.
///
/// # Errors
/// As [`Envelope::to_json`], counting the newline toward [`MAX_FRAME`].
pub fn encode_line(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    let mut line = envelope.to_json()?;
    line.push(b'\n');
    if line.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    Ok(line)
}

/// Turns a byte stream into messages, one per line, whatever the chunks it arrives in.
#[derive(Debug, Default)]
pub struct LineDecoder {
    pending: Vec<u8>,
    discarding: bool,
}

impl LineDecoder {
    /// The messages that `bytes` completes, in order. A line over [`MAX_FRAME`] is reported
    /// once and skipped without being kept in memory.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Envelope, FrameError>> {
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .filter_map(|chunk| self.take(chunk))
            .collect()
    }

    // A chunk ends at a newline or at the end of the bytes.
    fn take(&mut self, chunk: &[u8]) -> Option<Result<Envelope, FrameError>> {
        let complete = chunk.ends_with(b"\n");
        if self.discarding {
            self.discarding = !complete;
            return None;
        }
        if self.pending.len() + chunk.len() > MAX_FRAME {
            self.pending.clear();
            self.discarding = !complete;
            return Some(Err(FrameError::TooLarge));
        }
        self.pending.extend_from_slice(chunk);
        complete
            .then(|| parse(&mem::take(&mut self.pending)))
            .flatten()
    }
}

// An empty line is no message at all.
fn parse(line: &[u8]) -> Option<Result<Envelope, FrameError>> {
    let json = line.trim_ascii();
    (!json.is_empty()).then(|| Envelope::from_json(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{request, run, workspace};
    use crate::{App, ErrorBody, Hello, Outcome, Receipt, RunEvent, RunUpdate, WorkspaceUpdate};

    fn envelope(id: u32) -> Envelope {
        Envelope {
            id,
            message: Message::Error(ErrorBody {
                code: code!("store.full"),
                retryable: false,
            }),
        }
    }

    // A hello whose app name is `padding` bytes long, to size frames exactly.
    fn hello(padding: usize) -> Envelope {
        Envelope {
            id: 6,
            message: Message::Hello(Hello {
                protocol: 1,
                app: App {
                    name: "n".repeat(padding),
                    version: "0.1.0".to_owned(),
                    commit: "1a2b3c4".to_owned(),
                },
                capabilities: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_message_is_one_line_of_json() {
        let line = encode_line(&envelope(1)).unwrap();

        assert_eq!(
            line,
            br#"{"id":1,"type":"error","code":"store.full","retryable":false}
"#
        );
    }

    #[test]
    fn messages_survive_any_split_into_chunks() {
        let mut bytes = encode_line(&envelope(1)).unwrap();
        bytes.extend(encode_line(&envelope(2)).unwrap());

        for size in 1..=bytes.len() {
            let mut decoder = LineDecoder::default();
            let messages: Vec<_> = bytes
                .chunks(size)
                .flat_map(|chunk| decoder.push(chunk))
                .collect();

            assert_eq!(
                messages,
                [Ok(envelope(1)), Ok(envelope(2))],
                "chunks of {size}"
            );
        }
    }

    #[test]
    fn bad_lines_are_reported_and_the_stream_goes_on() {
        let mut decoder = LineDecoder::default();
        let mut bytes = b"not json\n\n\xff\xfe\n".to_vec();
        bytes.extend(encode_line(&envelope(3)).unwrap());

        assert_eq!(
            decoder.push(&bytes),
            [
                Err(FrameError::Invalid),
                Err(FrameError::Invalid),
                Ok(envelope(3))
            ]
        );
    }

    #[test]
    fn an_oversized_line_is_refused_before_it_is_kept() {
        let mut decoder = LineDecoder::default();
        let big = vec![b'x'; MAX_FRAME];

        assert_eq!(decoder.push(&big), []);
        assert_eq!(decoder.push(b"xx"), [Err(FrameError::TooLarge)]);
        assert!(decoder.pending.is_empty());
        assert_eq!(decoder.push(&big), []);
        let mut rest = b"xx\n".to_vec();
        rest.extend(encode_line(&envelope(4)).unwrap());
        assert_eq!(decoder.push(&rest), [Ok(envelope(4))]);
    }

    #[test]
    fn an_oversized_line_in_one_push_is_refused() {
        let mut decoder = LineDecoder::default();
        let mut line = vec![b' '; MAX_FRAME];
        line.push(b'\n');

        assert_eq!(MAX_FRAME, 1 << 20);
        assert_eq!(decoder.push(&line), [Err(FrameError::TooLarge)]);
        assert!(decoder.pending.is_empty());
        assert!(!decoder.discarding);
    }

    #[test]
    fn messages_only_received_are_never_sent() {
        let messages = [
            Message::Unknown,
            Message::Receipt(Receipt {
                request: request(),
                outcome: Outcome::Newer,
            }),
            Message::Run(RunUpdate {
                run: run(),
                event: RunEvent::Newer,
            }),
            Message::Run(RunUpdate {
                run: run(),
                event: RunEvent::Finished {
                    outcome: Outcome::Newer,
                },
            }),
            Message::Workspace(WorkspaceUpdate {
                workspace: workspace(),
                seq: 1,
                change: WorkspaceChange::Newer,
            }),
        ];

        for message in messages {
            let envelope = Envelope { id: 7, message };
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{envelope:?}");
            assert_eq!(
                encode_line(&envelope),
                Err(FrameError::Invalid),
                "{envelope:?}"
            );
        }
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_accepted() {
        let mut decoder = LineDecoder::default();
        let mut line = encode_line(&envelope(5)).unwrap();
        let padding = MAX_FRAME - line.len();
        line.splice(0..0, std::iter::repeat_n(b' ', padding));

        assert_eq!(line.len(), MAX_FRAME);
        assert_eq!(decoder.push(&line), [Ok(envelope(5))]);
    }

    #[test]
    fn a_frame_is_one_message_of_json_without_a_newline() {
        let json = envelope(1).to_json().unwrap();
        let mut line = json.clone();
        line.push(b'\n');

        assert_eq!(encode_line(&envelope(1)).unwrap(), line);
        assert_eq!(Envelope::from_json(&json), Ok(envelope(1)));
        assert_eq!(Envelope::from_json(b"[]"), Err(FrameError::Invalid));
        assert_eq!(
            Envelope::from_json(&vec![b' '; MAX_FRAME + 1]),
            Err(FrameError::TooLarge)
        );
    }

    #[test]
    fn a_frame_may_use_the_whole_limit_but_a_line_needs_room_for_its_newline() {
        let full = MAX_FRAME - hello(1).to_json().unwrap().len() + 1;
        let mut padded = vec![b' '; MAX_FRAME - envelope(9).to_json().unwrap().len()];
        padded.extend(envelope(9).to_json().unwrap());

        assert_eq!(hello(full).to_json().unwrap().len(), MAX_FRAME);
        assert_eq!(encode_line(&hello(full - 1)).unwrap().len(), MAX_FRAME);
        assert_eq!(encode_line(&hello(full)), Err(FrameError::TooLarge));
        assert_eq!(hello(full + 1).to_json(), Err(FrameError::TooLarge));
        assert_eq!(Envelope::from_json(&padded), Ok(envelope(9)));
    }

    #[test]
    fn errors_have_codes_and_fixed_messages() {
        assert_eq!(FrameError::TooLarge.code().as_str(), "frame.too_large");
        assert_eq!(FrameError::Invalid.code().as_str(), "frame.invalid");
        assert_eq!(
            FrameError::TooLarge.to_string(),
            "The frame is larger than the limit"
        );
        assert_eq!(
            FrameError::Invalid.to_string(),
            "The frame isn't a valid message"
        );
    }
}
