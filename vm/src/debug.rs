//! Optional VM observation. Observers receive rendered values, never mutable program state.
use crate::error::RuntimeError;

pub struct Frame {
    pub function: crate::hir::FunctionId,
    pub name: String,
    pub instruction: usize,
    pub registers: std::collections::HashMap<usize, String>,
}

pub trait Observer: Send + Sync {
    fn registers(&self, function: crate::hir::FunctionId) -> Vec<usize>;
    fn should_stop(
        &self,
        function: crate::hir::FunctionId,
        instruction: usize,
        depth: usize,
    ) -> Result<bool, RuntimeError>;
    fn stopped(&self, frames: Vec<Frame>) -> Result<(), RuntimeError>;
}

pub(super) fn render(value: &super::Value) -> String {
    struct Preview(String);
    impl std::fmt::Write for Preview {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let remaining = 4096usize.saturating_sub(self.0.len());
            if text.len() <= remaining {
                self.0.push_str(text);
                return Ok(());
            }
            let mut end = remaining;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.0.push_str(&text[..end]);
            self.0.push('…');
            Err(std::fmt::Error)
        }
    }
    let mut preview = Preview(String::new());
    let _ = std::fmt::write(&mut preview, format_args!("{value}"));
    preview.0
}
