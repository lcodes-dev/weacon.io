use std::io;
use std::mem;

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// The lines an agent prints, each at most `max` bytes; longer lines are skipped whole.
pub(crate) struct Lines<R> {
    reader: R,
    line: Vec<u8>,
    too_long: bool,
    max: usize,
}

impl<R: AsyncBufRead + Unpin> Lines<R> {
    pub(crate) const fn new(reader: R, max: usize) -> Self {
        Self {
            reader,
            line: Vec::new(),
            too_long: false,
            max,
        }
    }

    /// The next line that fits, without its newline, or `None` at the end. Safe to cancel.
    pub(crate) async fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            let chunk = self.reader.fill_buf().await?;
            if chunk.is_empty() {
                return Ok(None);
            }
            let end = chunk.iter().position(|byte| *byte == b'\n');
            let part = chunk.get(..end.unwrap_or(chunk.len())).unwrap_or_default();
            if self.line.len() + part.len() > self.max {
                self.too_long = true;
                self.line.clear();
            } else if !self.too_long {
                self.line.extend_from_slice(part);
            }
            let used = end.map_or(part.len(), |end| end + 1);
            self.reader.consume(used);
            if end.is_some() && !mem::take(&mut self.too_long) {
                return Ok(Some(mem::take(&mut self.line)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    async fn read_all(input: &[u8], max: usize) -> Vec<Vec<u8>> {
        // A tiny buffer, so lines arrive in pieces.
        let mut lines = Lines::new(BufReader::with_capacity(2, input), max);
        let mut out = Vec::new();
        while let Some(line) = lines.next().await.unwrap() {
            out.push(line);
        }
        out
    }

    #[tokio::test]
    async fn lines_up_to_the_limit_pass_and_longer_ones_are_skipped() {
        let input = b"abcd\nabcde\n\nxy\nunfinished";

        assert_eq!(
            read_all(input, 4).await,
            [b"abcd".to_vec(), Vec::new(), b"xy".to_vec()]
        );
        assert_eq!(read_all(input, 5).await.len(), 4);
    }
}
