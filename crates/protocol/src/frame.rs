use std::{fmt, mem};

use crate::{Code, Envelope, Message, code};

/// The largest frame accepted or sent, newline included.
pub const MAX_FRAME: usize = 1024 * 1024;

/// Why bytes couldn't become a message, or a message couldn't become bytes.
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

/// One message as a line of JSON.
///
/// # Errors
/// When the message is [`Message::Unknown`], which is only ever received, can't be written as
/// JSON, or would exceed [`MAX_FRAME`].
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    if envelope.message == Message::Unknown {
        return Err(FrameError::Invalid);
    }
    let mut line = serde_json::to_vec(envelope).map_err(|_| FrameError::Invalid)?;
    line.push(b'\n');
    if line.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    Ok(line)
}

/// Turns a stream of bytes into messages, one per line, whatever the chunks it arrives in.
#[derive(Debug, Default)]
pub struct Decoder {
    pending: Vec<u8>,
    discarding: bool,
}

impl Decoder {
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
    (!json.is_empty()).then(|| serde_json::from_slice(json).map_err(|_| FrameError::Invalid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{App, ErrorBody, Hello};

    fn envelope(id: u32) -> Envelope {
        Envelope {
            id,
            message: Message::Error(ErrorBody {
                code: code!("store.full"),
                retryable: false,
            }),
        }
    }

    #[test]
    fn a_message_is_one_line_of_json() {
        let line = encode(&envelope(1)).unwrap();

        assert_eq!(
            line,
            br#"{"id":1,"type":"error","code":"store.full","retryable":false}
"#
        );
    }

    #[test]
    fn messages_survive_any_split_into_chunks() {
        let mut bytes = encode(&envelope(1)).unwrap();
        bytes.extend(encode(&envelope(2)).unwrap());

        for size in 1..=bytes.len() {
            let mut decoder = Decoder::default();
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
        let mut decoder = Decoder::default();
        let mut bytes = b"not json\n\n\xff\xfe\n".to_vec();
        bytes.extend(encode(&envelope(3)).unwrap());

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
        let mut decoder = Decoder::default();
        let big = vec![b'x'; MAX_FRAME];

        assert_eq!(decoder.push(&big), []);
        assert_eq!(decoder.push(b"xx"), [Err(FrameError::TooLarge)]);
        assert!(decoder.pending.is_empty());
        assert_eq!(decoder.push(&big), []);
        let mut rest = b"xx\n".to_vec();
        rest.extend(encode(&envelope(4)).unwrap());
        assert_eq!(decoder.push(&rest), [Ok(envelope(4))]);
    }

    #[test]
    fn an_oversized_line_in_one_push_is_refused() {
        let mut decoder = Decoder::default();
        let mut line = vec![b' '; MAX_FRAME];
        line.push(b'\n');

        assert_eq!(MAX_FRAME, 1 << 20);
        assert_eq!(decoder.push(&line), [Err(FrameError::TooLarge)]);
        assert!(decoder.pending.is_empty());
        assert!(!decoder.discarding);
    }

    #[test]
    fn encoding_refuses_unknown_and_oversized_messages() {
        let hello = |padding: usize| Envelope {
            id: 6,
            message: Message::Hello(Hello {
                protocol: 1,
                app: App {
                    name: "n".repeat(padding),
                    version: "0.1.0".to_owned(),
                    commit: "1a2b3c4".to_owned(),
                },
            }),
        };
        let fits = MAX_FRAME - encode(&hello(1)).unwrap().len() + 1;
        let unknown = Envelope {
            id: 7,
            message: Message::Unknown,
        };

        assert_eq!(encode(&hello(fits)).unwrap().len(), MAX_FRAME);
        assert_eq!(encode(&hello(fits + 1)), Err(FrameError::TooLarge));
        assert_eq!(encode(&unknown), Err(FrameError::Invalid));
    }

    #[test]
    fn a_line_of_exactly_the_limit_is_accepted() {
        let mut decoder = Decoder::default();
        let mut line = encode(&envelope(5)).unwrap();
        let padding = MAX_FRAME - line.len();
        line.splice(0..0, std::iter::repeat_n(b' ', padding));

        assert_eq!(line.len(), MAX_FRAME);
        assert_eq!(decoder.push(&line), [Ok(envelope(5))]);
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
