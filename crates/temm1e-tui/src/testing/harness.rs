//! TestBackend-based TUI test harness.

#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use inner::*;

#[cfg(test)]
#[allow(dead_code)]
mod inner {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::AppState;

    /// Test harness wrapping a ratatui TestBackend.
    pub struct TuiTestHarness {
        pub terminal: Terminal<TestBackend>,
        pub state: AppState,
    }

    impl TuiTestHarness {
        pub fn new(width: u16, height: u16) -> Self {
            let backend = TestBackend::new(width, height);
            let terminal = Terminal::new(backend).unwrap();
            let state = AppState::new();
            Self { terminal, state }
        }

        /// Assert the terminal buffer contains the given text.
        pub fn assert_contains(&self, text: &str) {
            let buf = self.terminal.backend().buffer().clone();
            let mut content = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    content.push_str(buf[(x, y)].symbol());
                }
            }
            assert!(
                content.contains(text),
                "Terminal buffer does not contain '{}'\nBuffer:\n{}",
                text,
                content,
            );
        }
    }
}
