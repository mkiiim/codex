//! Helpers for truncating rollouts based on "user turn" boundaries.
//!
//! In core, "user turns" are detected by scanning `ResponseItem::Message` items and
//! interpreting them via `event_mapping::parse_turn_item(...)`.

use crate::context_manager::is_user_turn_boundary;
use crate::event_mapping;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TurnCompleteEvent;

pub(crate) fn initial_history_has_prior_user_turns(conversation_history: &InitialHistory) -> bool {
    conversation_history.scan_rollout_items(rollout_item_is_user_turn_boundary)
}

fn rollout_item_is_user_turn_boundary(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::ResponseItem(item) => is_user_turn_boundary(item),
        _ => false,
    }
}

/// Return the indices of user message boundaries in a rollout.
///
/// A user message boundary is a `RolloutItem::ResponseItem(ResponseItem::Message { .. })`
/// whose parsed turn item is `TurnItem::UserMessage`.
///
/// Rollouts can contain `ThreadRolledBack` markers. Those markers indicate that the
/// last N user turns were removed from the effective thread history; we apply them here so
/// indexing uses the post-rollback history rather than the raw stream.
pub(crate) fn user_message_positions_in_rollout(items: &[RolloutItem]) -> Vec<usize> {
    let mut user_positions = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            RolloutItem::ResponseItem(item @ ResponseItem::Message { .. })
                if matches!(
                    event_mapping::parse_turn_item(item),
                    Some(TurnItem::UserMessage(_))
                ) =>
            {
                user_positions.push(idx);
            }
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
                let new_len = user_positions.len().saturating_sub(num_turns);
                user_positions.truncate(new_len);
            }
            _ => {}
        }
    }
    user_positions
}

/// Return the indices of fork-turn boundaries in a rollout.
///
/// A fork-turn boundary is either:
/// - a real user message boundary, or
/// - an assistant inter-agent envelope whose parsed `trigger_turn` is `true`.
///
/// Like `user_message_positions_in_rollout`, this applies `ThreadRolledBack` markers so indexing
/// reflects the effective post-rollback history. Rollback counts instruction turns, so a rollback
/// removes the stale suffix starting at the earliest rolled-back instruction-turn boundary instead
/// of simply truncating the mixed fork-boundary list.
pub(crate) fn fork_turn_positions_in_rollout(items: &[RolloutItem]) -> Vec<usize> {
    let mut rollback_turn_positions = Vec::new();
    let mut fork_turn_positions = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            RolloutItem::ResponseItem(item) => {
                if is_user_turn_boundary(item) {
                    rollback_turn_positions.push(idx);
                }
                if is_real_user_message_boundary(item) || is_trigger_turn_boundary(item) {
                    fork_turn_positions.push(idx);
                }
            }
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
                if num_turns == 0 {
                    continue;
                }
                let Some(rollback_start_idx) = rollback_turn_positions
                    .len()
                    .checked_sub(num_turns)
                    .map(|rollback_start| rollback_turn_positions[rollback_start])
                    .or_else(|| rollback_turn_positions.first().copied())
                else {
                    continue;
                };
                let new_rollback_len = rollback_turn_positions.len().saturating_sub(num_turns);
                rollback_turn_positions.truncate(new_rollback_len);
                fork_turn_positions.retain(|position| *position < rollback_start_idx);
            }
            _ => {}
        }
    }
    fork_turn_positions
}

/// Return a prefix of `items` obtained by cutting strictly before the nth user message.
///
/// The boundary index is 0-based from the start of `items` (so `n_from_start = 0` returns
/// a prefix that excludes the first user message and everything after it).
///
/// If `n_from_start` is `usize::MAX`, this returns the full rollout (no truncation).
/// If fewer than or equal to `n_from_start` user messages exist, this returns the full
/// rollout unchanged.
pub(crate) fn truncate_rollout_before_nth_user_message_from_start(
    items: &[RolloutItem],
    n_from_start: usize,
) -> Vec<RolloutItem> {
    if n_from_start == usize::MAX {
        return items.to_vec();
    }

    let user_positions = user_message_positions_in_rollout(items);

    // If fewer than or equal to n user messages exist, keep the full rollout.
    if user_positions.len() <= n_from_start {
        return items.to_vec();
    }

    // Cut strictly before the nth user message (do not keep the nth itself).
    let cut_idx = user_positions[n_from_start];
    items[..cut_idx].to_vec()
}

/// Return a suffix of `items` that keeps the last `n_from_end` fork turns.
///
/// If fewer than or equal to `n_from_end` fork turns exist, this keeps from the first fork-turn
/// boundary and still drops pre-turn startup context.
pub(crate) fn truncate_rollout_to_last_n_fork_turns(
    items: &[RolloutItem],
    n_from_end: usize,
) -> Vec<RolloutItem> {
    if n_from_end == 0 {
        return Vec::new();
    }

    let fork_turn_positions = fork_turn_positions_in_rollout(items);
    let Some(keep_idx) = fork_turn_positions
        .len()
        .checked_sub(n_from_end)
        .map(|position| fork_turn_positions[position])
        .or_else(|| fork_turn_positions.first().copied())
    else {
        return Vec::new();
    };
    items[keep_idx..].to_vec()
}

/// Truncate a rollout at a read anchor within the latest assistant message.
///
/// Equivalent to `truncate_rollout_at_assistant_read_anchor` with the last
/// assistant message index, but expressed without requiring the caller to
/// count messages.
pub fn truncate_rollout_at_latest_assistant_read_anchor(
    items: &[RolloutItem],
    source_line_index: usize,
    source_byte_offset: usize,
) -> CodexResult<Vec<RolloutItem>> {
    let assistant_message_index = assistant_message_positions_in_rollout(items)
        .len()
        .checked_sub(1)
        .ok_or_else(|| {
            CodexErr::InvalidRequest(
                "latest assistant read anchor requires an assistant message".to_string(),
            )
        })?;

    truncate_rollout_at_assistant_read_anchor(
        items,
        assistant_message_index,
        source_line_index,
        source_byte_offset,
    )
}

/// Truncate a rollout at a read anchor within the Nth assistant message.
///
/// The resulting history ends at the specified byte offset within the
/// specified assistant message, followed by a developer note indicating
/// that a branch was created at that point.
pub fn truncate_rollout_at_assistant_read_anchor(
    items: &[RolloutItem],
    assistant_message_index: usize,
    source_line_index: usize,
    source_byte_offset: usize,
) -> CodexResult<Vec<RolloutItem>> {
    let assistant_positions = assistant_message_positions_in_rollout(items);
    let Some(message_index) = assistant_positions.get(assistant_message_index).copied() else {
        return Err(CodexErr::InvalidRequest(format!(
            "assistant message index {assistant_message_index} is out of range"
        )));
    };

    let Some(RolloutItem::ResponseItem(item)) = items.get(message_index) else {
        return Err(CodexErr::InvalidRequest(format!(
            "rollout item at assistant message index {assistant_message_index} is not a response item"
        )));
    };

    let truncated_message = truncate_assistant_message(
        item,
        source_line_index,
        source_byte_offset,
        assistant_message_index,
    )?;

    // The ThreadHistoryBuilder drives the assistant turn display from EventMsg::AgentMessage
    // (handle_response_item ignores ResponseItem::Message with role != "user").  An
    // AgentMessage event for the same turn typically appears *before* the consolidated
    // ResponseItem in the rollout, so it is inside the prefix we keep.  Truncate the last
    // such event to the same byte cutoff so the displayed text matches the anchor point.
    let agent_text_cutoff = first_output_text_len(&truncated_message);
    let mut prefix = items[..message_index].to_vec();
    if let Some(cutoff) = agent_text_cutoff {
        truncate_last_agent_message_in_prefix(&mut prefix, cutoff);
    }

    // The TurnStarted event for the current turn is inside the prefix, but the matching
    // TurnComplete sits after message_index and is excluded.  Append a synthetic
    // TurnComplete so the ThreadHistoryBuilder closes the turn as Completed rather than
    // leaving it InProgress — which would otherwise cause the TUI to show "Working ..."
    // indefinitely in the branch session.
    if let Some(turn_id) = last_unclosed_turn_id(&prefix) {
        prefix.push(RolloutItem::EventMsg(EventMsg::TurnComplete(
            TurnCompleteEvent {
                turn_id,
                last_agent_message: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            },
        )));
    }

    prefix.push(RolloutItem::ResponseItem(truncated_message));
    prefix.push(RolloutItem::ResponseItem(branch_interruption_note()));
    Ok(prefix)
}

/// Collect the rollout indices of assistant `Message` items, applying any
/// `ThreadRolledBack` markers so the list reflects effective history.
fn assistant_message_positions_in_rollout(items: &[RolloutItem]) -> Vec<usize> {
    let mut rollback_turn_positions = Vec::new();
    let mut assistant_positions = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            RolloutItem::ResponseItem(item) => {
                if is_user_turn_boundary(item) {
                    rollback_turn_positions.push(idx);
                }
                if matches!(
                    event_mapping::parse_turn_item(item),
                    Some(TurnItem::AgentMessage(_))
                ) {
                    assistant_positions.push(idx);
                }
            }
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
                if num_turns == 0 {
                    continue;
                }
                let Some(rollback_start_idx) = rollback_turn_positions
                    .len()
                    .checked_sub(num_turns)
                    .map(|rollback_start| rollback_turn_positions[rollback_start])
                    .or_else(|| rollback_turn_positions.first().copied())
                else {
                    continue;
                };
                let new_rollback_len = rollback_turn_positions.len().saturating_sub(num_turns);
                rollback_turn_positions.truncate(new_rollback_len);
                assistant_positions.retain(|position| *position < rollback_start_idx);
            }
            _ => {}
        }
    }
    assistant_positions
}

/// Truncate an assistant `ResponseItem::Message` at the given line/byte anchor.
fn truncate_assistant_message(
    item: &ResponseItem,
    source_line_index: usize,
    source_byte_offset: usize,
    assistant_message_index: usize,
) -> CodexResult<ResponseItem> {
    let ResponseItem::Message {
        id,
        role,
        content,
        phase,
    } = item
    else {
        return Err(CodexErr::InvalidRequest(format!(
            "assistant message index {assistant_message_index} does not reference a message"
        )));
    };
    if role != "assistant" {
        return Err(CodexErr::InvalidRequest(format!(
            "assistant message index {assistant_message_index} does not reference an assistant message"
        )));
    }

    let mut output_text_count = 0usize;
    let mut truncated_content = Vec::with_capacity(content.len());
    let mut found_anchor = false;

    for content_item in content {
        match content_item {
            ContentItem::OutputText { text } => {
                if output_text_count < source_line_index {
                    truncated_content.push(content_item.clone());
                    output_text_count += 1;
                    continue;
                }

                if output_text_count == source_line_index {
                    if source_byte_offset >= text.len() {
                        return Err(CodexErr::InvalidRequest(format!(
                            "assistant message index {assistant_message_index} has no byte offset {source_byte_offset} on line {source_line_index}"
                        )));
                    }
                    let cutoff = inclusive_char_boundary(text, source_byte_offset)?;
                    truncated_content.push(ContentItem::OutputText {
                        text: text[..cutoff].to_string(),
                    });
                    found_anchor = true;
                    break;
                }

                break;
            }
            _ => truncated_content.push(content_item.clone()),
        }
    }

    if !found_anchor {
        return Err(CodexErr::InvalidRequest(format!(
            "assistant message index {assistant_message_index} has no source line {source_line_index}"
        )));
    }

    Ok(ResponseItem::Message {
        id: id.clone(),
        role: role.clone(),
        content: truncated_content,
        phase: phase.clone(),
    })
}

/// Find the next char boundary at or after `source_byte_offset` within `text`.
fn inclusive_char_boundary(text: &str, source_byte_offset: usize) -> CodexResult<usize> {
    let mut boundaries = text.char_indices().map(|(idx, _)| idx).peekable();
    while let Some(start) = boundaries.next() {
        let end = boundaries.peek().copied().unwrap_or(text.len());
        if source_byte_offset >= start && source_byte_offset < end {
            return Ok(end);
        }
    }

    Err(CodexErr::InvalidRequest(format!(
        "byte offset {source_byte_offset} is not within assistant text content"
    )))
}

/// Return the turn_id of the last `TurnStarted` event that has no matching `TurnComplete` or
/// `TurnAborted` in `items`, or `None` if every started turn is already closed.
fn last_unclosed_turn_id(items: &[RolloutItem]) -> Option<String> {
    let mut last_started: Option<String> = None;
    for item in items {
        if let RolloutItem::EventMsg(ev) = item {
            match ev {
                EventMsg::TurnStarted(payload) => {
                    last_started = Some(payload.turn_id.clone());
                }
                EventMsg::TurnComplete(payload) => {
                    if last_started.as_deref() == Some(payload.turn_id.as_str()) {
                        last_started = None;
                    }
                }
                EventMsg::TurnAborted(payload) => {
                    if payload.turn_id.as_deref() == last_started.as_deref()
                        || payload.turn_id.is_none()
                    {
                        last_started = None;
                    }
                }
                _ => {}
            }
        }
    }
    last_started
}

/// Return the byte length of the first `OutputText` content item in a response item, if any.
fn first_output_text_len(item: &ResponseItem) -> Option<usize> {
    let ResponseItem::Message { content, .. } = item else {
        return None;
    };
    content.iter().find_map(|ci| {
        if let ContentItem::OutputText { text } = ci {
            Some(text.len())
        } else {
            None
        }
    })
}

/// Truncate `message` of the last `EventMsg::AgentMessage` in `prefix` to `cutoff` bytes.
///
/// If `cutoff` falls within a multi-byte UTF-8 character the cutoff is walked back to
/// the previous char boundary to avoid a panic.
fn truncate_last_agent_message_in_prefix(prefix: &mut Vec<RolloutItem>, cutoff: usize) {
    let Some(idx) = prefix
        .iter()
        .rposition(|item| matches!(item, RolloutItem::EventMsg(EventMsg::AgentMessage(_))))
    else {
        return;
    };
    let RolloutItem::EventMsg(EventMsg::AgentMessage(ref agent_msg)) = prefix[idx] else {
        return;
    };
    if cutoff >= agent_msg.message.len() {
        return;
    }
    let safe_cutoff = {
        let mut off = cutoff.min(agent_msg.message.len());
        while !agent_msg.message.is_char_boundary(off) {
            off -= 1;
        }
        off
    };
    let truncated = agent_msg.message[..safe_cutoff].to_string();
    prefix[idx] = RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
        message: truncated,
        phase: agent_msg.phase.clone(),
        memory_citation: agent_msg.memory_citation.clone(),
    }));
}

/// Developer-role note appended to a branched rollout to explain the cutoff.
fn branch_interruption_note() -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "Branch context note: the user branched from the prior assistant reply at the \
                   preserved read point. Content that originally followed that point was not read \
                   and has been excluded from this branch context."
                .to_string(),
        }],
        phase: None,
    }
}

fn is_real_user_message_boundary(item: &ResponseItem) -> bool {
    matches!(
        event_mapping::parse_turn_item(item),
        Some(TurnItem::UserMessage(_))
    )
}

fn is_trigger_turn_boundary(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };

    role == "assistant"
        && InterAgentCommunication::from_message_content(content)
            .is_some_and(|communication| communication.trigger_turn)
}

#[cfg(test)]
#[path = "thread_rollout_truncation_tests.rs"]
mod tests;
