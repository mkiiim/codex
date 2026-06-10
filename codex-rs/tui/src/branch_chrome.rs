//! Branch state notice types and label helpers for in-session branch UI chrome.

use codex_app_server_protocol::ThreadForkSnapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BranchStateNoticeKind {
    /// Entered a branch from a specific point in an older assistant reply.
    BranchFrom {
        depth: usize,
        selection_summary: Option<String>,
    },
    /// Returned to the parent thread from a branch.
    ReturnedToParent { depth: usize },
}

pub(crate) fn branch_context_label(
    depth: usize,
    _origin_snapshot: Option<&ThreadForkSnapshot>,
) -> String {
    if depth == 1 {
        "branch".to_string()
    } else {
        format!("branch (depth {depth})")
    }
}

pub(crate) fn branch_state_title(kind: &BranchStateNoticeKind) -> String {
    match kind {
        BranchStateNoticeKind::BranchFrom {
            depth,
            selection_summary,
        } => {
            let label = branch_context_label(*depth, None);
            if let Some(summary) = selection_summary.as_deref().filter(|s| !s.is_empty()) {
                let truncated = truncate_summary(summary, 48);
                format!("Branched ({label}) from: {truncated}")
            } else {
                format!("Started {label}")
            }
        }
        BranchStateNoticeKind::ReturnedToParent { depth } => {
            let label = branch_context_label(*depth, None);
            format!("Returned from {label}")
        }
    }
}

pub(crate) fn branch_state_body(kind: &BranchStateNoticeKind) -> Option<String> {
    match kind {
        BranchStateNoticeKind::BranchFrom { .. } => {
            Some("Press Esc or /return to go back to the parent thread.".to_string())
        }
        BranchStateNoticeKind::ReturnedToParent { .. } => None,
    }
}

fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}
