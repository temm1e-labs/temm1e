//! Multi-line input state machine.

/// State of the text input area.
#[derive(Debug, Clone)]
pub struct InputState {
    /// Lines of text in the input buffer.
    pub lines: Vec<String>,
    /// Cursor position: (line_index, column_index).
    pub cursor: (usize, usize),
    /// Command history.
    pub history: Vec<Vec<String>>,
    /// Current position in history (None = current input).
    pub history_pos: Option<usize>,
    /// Saved current input when browsing history.
    saved_input: Option<Vec<String>>,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            history: Vec::new(),
            history_pos: None,
            saved_input: None,
        }
    }
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the full input text (lines joined by newlines).
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether the input is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Whether input is multi-line.
    pub fn is_multiline(&self) -> bool {
        self.lines.len() > 1
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        let (line, col) = self.cursor;
        self.lines[line].insert(col, c);
        self.cursor.1 += c.len_utf8();
        self.history_pos = None;
    }

    /// Insert a newline at cursor, splitting the current line.
    pub fn insert_newline(&mut self) {
        let (line, col) = self.cursor;
        let rest = self.lines[line][col..].to_string();
        self.lines[line].truncate(col);
        self.lines.insert(line + 1, rest);
        self.cursor = (line + 1, 0);
        self.history_pos = None;
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        let (line, col) = self.cursor;
        if col > 0 {
            // Find the previous character boundary
            let prev = self.lines[line][..col]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.lines[line].replace_range(prev..col, "");
            self.cursor.1 = prev;
        } else if line > 0 {
            // Merge with previous line
            let current = self.lines.remove(line);
            self.cursor = (line - 1, self.lines[line - 1].len());
            self.lines[line - 1].push_str(&current);
        }
    }

    /// Delete the character at the cursor (delete key).
    pub fn delete(&mut self) {
        let (line, col) = self.cursor;
        if col < self.lines[line].len() {
            let next = self.lines[line][col..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| col + i)
                .unwrap_or(self.lines[line].len());
            self.lines[line].replace_range(col..next, "");
        } else if line + 1 < self.lines.len() {
            // Merge next line into current
            let next = self.lines.remove(line + 1);
            self.lines[line].push_str(&next);
        }
    }

    /// Move cursor left.
    pub fn move_left(&mut self) {
        let (line, col) = self.cursor;
        if col > 0 {
            let prev = self.lines[line][..col]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cursor.1 = prev;
        } else if line > 0 {
            self.cursor = (line - 1, self.lines[line - 1].len());
        }
    }

    /// Move cursor right.
    pub fn move_right(&mut self) {
        let (line, col) = self.cursor;
        if col < self.lines[line].len() {
            let next = self.lines[line][col..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| col + i)
                .unwrap_or(self.lines[line].len());
            self.cursor.1 = next;
        } else if line + 1 < self.lines.len() {
            self.cursor = (line + 1, 0);
        }
    }

    /// Move cursor up — history if single-line, line movement if multi-line.
    pub fn move_up(&mut self) -> bool {
        if self.is_multiline() && self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
            true
        } else {
            self.history_prev()
        }
    }

    /// Move cursor down — history if single-line, line movement if multi-line.
    pub fn move_down(&mut self) -> bool {
        if self.is_multiline() && self.cursor.0 + 1 < self.lines.len() {
            self.cursor.0 += 1;
            self.cursor.1 = self.cursor.1.min(self.lines[self.cursor.0].len());
            true
        } else {
            self.history_next()
        }
    }

    /// Move cursor to start of line.
    pub fn home(&mut self) {
        self.cursor.1 = 0;
    }

    /// Move cursor to end of line.
    pub fn end(&mut self) {
        self.cursor.1 = self.lines[self.cursor.0].len();
    }

    /// Kill text from cursor to end of line.
    pub fn kill_to_end(&mut self) {
        let (line, col) = self.cursor;
        self.lines[line].truncate(col);
    }

    /// Kill text from start of line to cursor.
    pub fn kill_to_start(&mut self) {
        let (line, col) = self.cursor;
        self.lines[line] = self.lines[line][col..].to_string();
        self.cursor.1 = 0;
    }

    /// Submit the current input and return it. Clears the input state.
    pub fn submit(&mut self) -> String {
        let text = self.text();
        if !text.trim().is_empty() {
            self.history.push(self.lines.clone());
        }
        self.lines = vec![String::new()];
        self.cursor = (0, 0);
        self.history_pos = None;
        self.saved_input = None;
        text
    }

    /// Clear the input without submitting.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = (0, 0);
        self.history_pos = None;
        self.saved_input = None;
    }

    /// Navigate to previous history entry.
    fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        match self.history_pos {
            None => {
                self.saved_input = Some(self.lines.clone());
                self.history_pos = Some(self.history.len() - 1);
            }
            Some(0) => return false,
            Some(pos) => {
                self.history_pos = Some(pos - 1);
            }
        }
        if let Some(pos) = self.history_pos {
            self.lines = self.history[pos].clone();
            self.cursor = (0, self.lines[0].len());
        }
        true
    }

    /// Navigate to next history entry.
    fn history_next(&mut self) -> bool {
        match self.history_pos {
            None => return false,
            Some(pos) if pos + 1 >= self.history.len() => {
                self.history_pos = None;
                if let Some(saved) = self.saved_input.take() {
                    self.lines = saved;
                } else {
                    self.lines = vec![String::new()];
                }
                self.cursor = (0, self.lines[0].len());
            }
            Some(pos) => {
                self.history_pos = Some(pos + 1);
                self.lines = self.history[pos + 1].clone();
                self.cursor = (0, self.lines[0].len());
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        let state = InputState::new();
        assert!(state.is_empty());
        assert!(!state.is_multiline());
        assert_eq!(state.text(), "");
    }

    #[test]
    fn insert_and_submit() {
        let mut state = InputState::new();
        state.insert_char('h');
        state.insert_char('i');
        assert_eq!(state.text(), "hi");
        let text = state.submit();
        assert_eq!(text, "hi");
        assert!(state.is_empty());
    }

    #[test]
    fn multiline_input() {
        let mut state = InputState::new();
        state.insert_char('a');
        state.insert_newline();
        state.insert_char('b');
        assert!(state.is_multiline());
        assert_eq!(state.text(), "a\nb");
    }

    #[test]
    fn backspace_merges_lines() {
        let mut state = InputState::new();
        state.insert_char('a');
        state.insert_newline();
        state.insert_char('b');
        state.home();
        state.backspace();
        assert_eq!(state.text(), "ab");
        assert_eq!(state.cursor, (0, 1));
    }

    #[test]
    fn history_navigation() {
        let mut state = InputState::new();
        state.insert_char('1');
        state.submit();
        state.insert_char('2');
        state.submit();
        assert!(state.move_up());
        assert_eq!(state.text(), "2");
        assert!(state.move_up());
        assert_eq!(state.text(), "1");
        assert!(state.move_down());
        assert_eq!(state.text(), "2");
    }

    #[test]
    fn kill_operations() {
        let mut state = InputState::new();
        for c in "hello world".chars() {
            state.insert_char(c);
        }
        // Move to middle
        state.cursor.1 = 5;
        state.kill_to_end();
        assert_eq!(state.text(), "hello");
        state.kill_to_start();
        assert_eq!(state.text(), "");
    }
}
