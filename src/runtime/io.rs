//! Simula system procedure I/O (OutText / OutImage / SysIn·SysOut images).

/// 1-based image buffer used by SysOut / SysIn and image files (BASICIO MVP).
#[derive(Debug, Clone, Default)]
pub struct ImageBuffer {
    /// Character contents of the current image (unbounded MVP growth).
    content: String,
    /// Standard 1-based character position (`pos`). Writing starts at `pos`.
    pos: usize,
    /// Set after a failed `InImage` (EOF).
    endfile: bool,
}

impl ImageBuffer {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            pos: 1,
            endfile: false,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn setpos(&mut self, pos: usize) {
        self.pos = pos.max(1);
    }

    pub fn length(&self) -> usize {
        self.content.chars().count()
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn endfile(&self) -> bool {
        self.endfile
    }

    pub fn set_endfile(&mut self, endfile: bool) {
        self.endfile = endfile;
    }

    fn char_byte_index(s: &str, char_index: usize) -> usize {
        s.char_indices()
            .nth(char_index)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    /// Write `text` starting at `pos`, growing the image as needed; advance `pos`.
    pub fn out_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let start_char = self.pos.saturating_sub(1);
        let existing_chars = self.content.chars().count();
        if start_char > existing_chars {
            self.content
                .push_str(&" ".repeat(start_char - existing_chars));
        }
        let start = Self::char_byte_index(&self.content, start_char);
        let text_chars = text.chars().count();
        let end_char = start_char + text_chars;
        let end = Self::char_byte_index(&self.content, end_char);
        if end <= self.content.len() && start <= end {
            self.content.replace_range(start..end, text);
        } else {
            self.content.truncate(start);
            self.content.push_str(text);
        }
        self.pos = start_char + text_chars + 1;
    }

    /// Write one character at `pos` and advance.
    pub fn out_char(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        self.out_text(ch.encode_utf8(&mut buf));
    }

    /// Reset image after a successful transmit.
    pub fn reset(&mut self) {
        self.content.clear();
        self.pos = 1;
    }

    /// Characters from 1 through `pos - 1` (Standard BreakOutImage payload).
    pub fn break_payload(&self) -> String {
        let end_char = self.pos.saturating_sub(1);
        let end = Self::char_byte_index(&self.content, end_char);
        self.content[..end].to_string()
    }

    /// Replace image contents with a full input line and reset `pos` to 1.
    pub fn load_line(&mut self, line: &str) {
        self.content.clear();
        self.content.push_str(line);
        self.pos = 1;
        self.endfile = false;
    }

    /// Next character at `pos`, or `None` when past the image.
    pub fn in_char(&mut self) -> Option<char> {
        if self.endfile {
            return None;
        }
        let idx = self.pos.saturating_sub(1);
        let ch = self.content.chars().nth(idx)?;
        self.pos += 1;
        Some(ch)
    }
}

/// SysOut image buffer. Flushed records are handed to [`super::host::IoHost`].
#[derive(Debug, Default)]
pub struct Output {
    image: ImageBuffer,
}

impl Output {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn out_text(&mut self, text: &str) {
        self.image.out_text(text);
    }

    pub fn out_char(&mut self, ch: char) {
        self.image.out_char(ch);
    }

    /// Flush the current image as one stdout record (content + newline).
    pub fn out_image(&mut self) -> String {
        let mut line = self.image.content.clone();
        line.push('\n');
        self.image.reset();
        line
    }

    /// Emit characters 1..pos-1 plus newline, then reset (BreakOutImage).
    pub fn break_out_image(&mut self) -> String {
        let mut line = self.image.break_payload();
        line.push('\n');
        self.image.reset();
        line
    }

    /// Whether [`Self::out_image`] would emit a leftover record at process end.
    pub fn has_pending(&self) -> bool {
        !self.image.content.is_empty() || self.image.pos > 1
    }

    pub fn finish(mut self) -> String {
        if self.has_pending() {
            self.out_image()
        } else {
            String::new()
        }
    }

    pub fn image(&self) -> &ImageBuffer {
        &self.image
    }

    pub fn image_mut(&mut self) -> &mut ImageBuffer {
        &mut self.image
    }
}

/// SysIn image state for the interpreter.
#[derive(Debug, Default)]
pub struct Input {
    image: ImageBuffer,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn image(&self) -> &ImageBuffer {
        &self.image
    }

    pub fn image_mut(&mut self) -> &mut ImageBuffer {
        &mut self.image
    }

    /// Apply one SYSIN record from the [`super::host::IoHost`].
    pub fn apply_record(&mut self, record: super::host::StdinRecord) {
        match record {
            super::host::StdinRecord::Eof => {
                self.image.set_endfile(true);
                self.image.reset();
            }
            super::host::StdinRecord::Line(line) => {
                self.image.load_line(&line);
            }
        }
    }

    pub fn in_char(&mut self) -> Result<char, String> {
        if self.image.endfile() {
            return Err("InChar: end of file".into());
        }
        self.image
            .in_char()
            .ok_or_else(|| "InChar: no more characters in image (call InImage first)".into())
    }

    pub fn endfile(&self) -> bool {
        self.image.endfile()
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageBuffer, Output};

    #[test]
    fn out_text_and_out_image_write_a_line() {
        let mut output = Output::new();
        output.out_text("hello world");
        assert_eq!(output.out_image(), "hello world\n");
    }

    #[test]
    fn out_char_and_break_out_image() {
        let mut output = Output::new();
        output.out_char('A');
        output.out_char('B');
        assert_eq!(output.break_out_image(), "AB\n");
    }

    #[test]
    fn image_pos_advances_with_out_text() {
        let mut image = ImageBuffer::new();
        image.out_text("hi");
        assert_eq!(image.pos(), 3);
        assert_eq!(image.content(), "hi");
    }
}
