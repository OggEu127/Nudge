//! Bounded untrusted child-output presentation, never an unbounded text widget.

use std::collections::VecDeque;

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 512 * 1024;
const MAX_LINE_BYTES: usize = 8192;

#[derive(Default)]
pub(crate) struct LogBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

impl LogBuffer {
    pub(crate) fn append(&mut self, text: &str) {
        for line in text.split('\n').filter(|line| !line.is_empty()) {
            // egui renders plain text, not terminal commands or rich markup
            // supplied by an arbitrary mod.
            let clean: String = line
                .chars()
                .filter(|character| !character.is_control() || *character == '\t')
                .scan(0_usize, |size, character| {
                    *size += character.len_utf8();
                    (*size <= MAX_LINE_BYTES).then_some(character)
                })
                .collect();
            self.bytes += clean.len();
            self.lines.push_back(clean);
            while self.lines.len() > MAX_LINES || self.bytes > MAX_BYTES {
                if let Some(old) = self.lines.pop_front() {
                    self.bytes -= old.len();
                }
            }
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub(crate) fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::{LogBuffer, MAX_BYTES, MAX_LINES};

    #[test]
    fn bounded_log_discards_oldest_and_strips_controls() {
        let mut logs = LogBuffer::default();
        logs.append("a\0b\u{1b}c\td");
        assert_eq!(logs.iter().next(), Some("abc\td"));
        for number in 0..=MAX_LINES {
            logs.append(&format!("line {number}"));
        }
        assert_eq!(logs.iter().count(), MAX_LINES);
        assert!(logs.bytes <= MAX_BYTES);
        assert_eq!(logs.iter().last(), Some("line 2000"));
        logs.clear();
        assert_eq!(logs.iter().count(), 0);
    }

    #[test]
    fn multibyte_oversized_lines_are_truncated_at_character_boundaries() {
        let mut logs = LogBuffer::default();
        logs.append(&"中文".repeat(10_000));
        assert!(logs.iter().next().is_some_and(|line| line.len() <= 8192));
    }
}
