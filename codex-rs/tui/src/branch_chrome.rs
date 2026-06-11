//! Branch state notice types and label helpers for in-session branch UI chrome.

use codex_app_server_protocol::ThreadForkSnapshot;
use codex_protocol::ThreadId;
use std::sync::OnceLock;

const BRANCH_DEBUG_ENV_VAR: &str = "CODEX_TUI_BRANCH_DEBUG";
pub(crate) const PREVIOUS_THREAD_TOKEN_USAGE_PREFIX: &str = "Previous thread token usage:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BranchStateNoticeKind {
    /// Entered a branch from a specific point in an older assistant reply.
    BranchFrom {
        depth: usize,
        selection_summary: Option<String>,
    },
    /// Resumed a branch thread from `/resume` or the branch navigator.
    Resumed {
        depth: usize,
        selection_summary: Option<String>,
        previous_thread_usage_line: Option<String>,
    },
    /// Returned to the parent thread from a branch.
    ReturnedToParent {
        depth: usize,
        selection_summary: Option<String>,
        previous_thread_usage_line: Option<String>,
    },
}

pub(crate) fn branch_chrome_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os(BRANCH_DEBUG_ENV_VAR).is_some_and(|value| {
            value.to_string_lossy().trim().eq_ignore_ascii_case("1")
                || value.to_string_lossy().trim().eq_ignore_ascii_case("true")
                || value.to_string_lossy().trim().eq_ignore_ascii_case("yes")
        })
    })
}

/// Returns the last 4 characters of a thread ID for compact display in notices.
pub(crate) fn branch_thread_id_suffix(thread_id: ThreadId) -> String {
    let s = thread_id.to_string();
    let start = s.len().saturating_sub(4);
    s[start..].to_string()
}

pub(crate) fn branch_context_label(
    branch_depth: usize,
    thread_id: Option<ThreadId>,
    selection_summary: Option<&str>,
    _origin_snapshot: Option<&ThreadForkSnapshot>,
) -> String {
    let mut label = String::new();
    label.push_str("Child branch ");
    label.push_str(&format!("d{branch_depth}"));
    if let Some(thread_id) = thread_id {
        label.push_str(" · ");
        label.push_str(&branch_thread_id_suffix(thread_id));
    }
    if let Some(selection_summary) = selection_summary
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        label.push_str(" · ");
        label.push('"');
        label.push_str("...");
        label.push_str(selection_summary);
        label.push('"');
    }
    label.push_str(" · Use /branch-list to switch");
    label
}

pub(crate) fn branch_state_title(kind: &BranchStateNoticeKind, thread_id_suffix: &str) -> String {
    let suffix_part = if thread_id_suffix.is_empty() {
        String::new()
    } else {
        format!(" · {thread_id_suffix}")
    };
    match kind {
        BranchStateNoticeKind::BranchFrom {
            depth,
            selection_summary,
        } => {
            if let Some(summary) = selection_summary.as_deref().filter(|s| !s.is_empty()) {
                let truncated = truncate_summary(summary, 48);
                format!("Branched (branch d{depth}{suffix_part}) from: {truncated}")
            } else {
                format!("Started branch d{depth}{suffix_part}")
            }
        }
        BranchStateNoticeKind::Resumed { depth, .. } => {
            format!("Resumed child branch d{depth}{suffix_part}")
        }
        BranchStateNoticeKind::ReturnedToParent { depth, .. } => {
            format!("Returned to parent from branch d{depth}{suffix_part}")
        }
    }
}

/// Returns zero or more body lines for the notice. Callers push each into the
/// history lines vector so they render as separate wrapped rows.
pub(crate) fn branch_state_body(kind: &BranchStateNoticeKind) -> Vec<String> {
    match kind {
        BranchStateNoticeKind::BranchFrom { .. } => {
            vec!["Press Esc or /return to go back to the parent thread.".to_string()]
        }
        BranchStateNoticeKind::Resumed {
            selection_summary,
            previous_thread_usage_line,
            ..
        } => {
            let mut lines = Vec::new();
            if let Some(s) = selection_summary.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("Branch focus: \"{s}\""));
            }
            if let Some(usage_line) = previous_thread_usage_line.as_deref() {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                let usage = usage_line
                    .strip_prefix("Token usage: ")
                    .unwrap_or(usage_line);
                lines.push(format!("{PREVIOUS_THREAD_TOKEN_USAGE_PREFIX} {usage}"));
            }
            lines.push("Type your reply to continue in this branch.".to_string());
            lines
        }
        BranchStateNoticeKind::ReturnedToParent {
            selection_summary,
            previous_thread_usage_line,
            ..
        } => {
            let mut lines = Vec::new();
            if let Some(s) = selection_summary.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("Returned branch focus: \"{s}\""));
            }
            if let Some(usage_line) = previous_thread_usage_line.as_deref() {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                let usage = usage_line
                    .strip_prefix("Token usage: ")
                    .unwrap_or(usage_line);
                lines.push(format!("{PREVIOUS_THREAD_TOKEN_USAGE_PREFIX} {usage}"));
            }
            lines.push("Type your reply to continue in the parent thread.".to_string());
            lines
        }
    }
}

fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn resumed_branch_notice_includes_previous_thread_usage() {
        let notice = BranchStateNoticeKind::Resumed {
            depth: 1,
            selection_summary: Some("Phase 2 migration".to_string()),
            previous_thread_usage_line: Some("Token usage: total=10 input=8 output=2".to_string()),
        };

        assert_eq!(
            branch_state_body(&notice),
            vec![
                "Branch focus: \"Phase 2 migration\"".to_string(),
                String::new(),
                "Previous thread token usage: total=10 input=8 output=2".to_string(),
                "Type your reply to continue in this branch.".to_string(),
            ]
        );
    }

    #[test]
    fn returned_parent_notice_includes_previous_thread_usage() {
        let notice = BranchStateNoticeKind::ReturnedToParent {
            depth: 1,
            selection_summary: Some("Phase 2 migration".to_string()),
            previous_thread_usage_line: Some("Token usage: total=10 input=8 output=2".to_string()),
        };

        assert_eq!(
            branch_state_body(&notice),
            vec![
                "Returned branch focus: \"Phase 2 migration\"".to_string(),
                String::new(),
                "Previous thread token usage: total=10 input=8 output=2".to_string(),
                "Type your reply to continue in the parent thread.".to_string(),
            ]
        );
    }
}
