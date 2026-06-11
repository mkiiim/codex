//! Styled history cell for branch state notices (Branched, Resumed, Returned).

use super::*;
use crate::branch_chrome::PREVIOUS_THREAD_TOKEN_USAGE_PREFIX;
use crate::style::branch_notice_style;

/// A visually distinct cell rendered with a teal-tinted background for
/// branch lifecycle transitions: entering a branch, resuming one, or returning
/// to the parent.
#[derive(Debug)]
pub(crate) struct BranchStateNoticeCell {
    title: String,
    body_lines: Vec<String>,
}

impl BranchStateNoticeCell {
    pub(crate) fn new(title: String, body_lines: Vec<String>) -> Self {
        Self { title, body_lines }
    }
}

impl HistoryCell for BranchStateNoticeCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let style = branch_notice_style();
        let mut lines: Vec<Line<'static>> = vec![Line::from("").style(style)];
        lines.push(Line::from(self.title.clone()).style(style.bold()));
        for body in &self.body_lines {
            let body_style = if body.starts_with(PREVIOUS_THREAD_TOKEN_USAGE_PREFIX) {
                style.bold()
            } else {
                style
            };
            lines.push(Line::from(body.clone()).style(body_style));
        }
        lines.push(Line::from("").style(style));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(self.title.clone())];
        lines.extend(self.body_lines.iter().map(|l| Line::from(l.clone())));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_lines_bold_previous_thread_token_usage() {
        let cell = BranchStateNoticeCell::new(
            "Resumed child branch d1 · 1234".to_string(),
            vec![
                "Branch focus: \"Phase 2\"".to_string(),
                String::new(),
                "Previous thread token usage: total=10 input=8 output=2".to_string(),
                "Type your reply to continue in this branch.".to_string(),
            ],
        );

        let lines = cell.display_lines(/*width*/ 80);

        assert!(lines[4].style.add_modifier.contains(Modifier::BOLD));
        assert!(!lines[2].style.add_modifier.contains(Modifier::BOLD));
        assert!(!lines[5].style.add_modifier.contains(Modifier::BOLD));
    }
}
