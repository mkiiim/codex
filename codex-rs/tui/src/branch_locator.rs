//! Snippet-to-branch-anchor resolution for reply-level branching.
//!
//! Given a text snippet copied from an assistant response and the current conversation history,
//! this module locates the unique anchor point within the raw markdown source from which a branch
//! should diverge. The anchor is expressed as a `ThreadForkSnapshot` suitable for passing
//! directly to the app-server's `thread/fork` request.
//!
//! Disambiguation strategy:
//! - The snippet is matched against all assistant responses in the conversation.
//! - An ambiguous match (same text in multiple responses) is rejected with `AmbiguousMatch`.
//! - A match with no anchorable source (no raw markdown) is `NoAnchor`.
//! - A unique anchorable match is converted to a line/byte offset within the response's raw
//!   markdown and wrapped in `LatestAssistantReadAnchor` or `AssistantReadAnchor` depending on
//!   whether the matched response is the most-recent one.

use std::sync::Arc;

use codex_app_server_protocol::ThreadForkSnapshot;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;

use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BranchReplyTarget {
    pub(crate) snapshot: ThreadForkSnapshot,
    pub(crate) origin_snapshot: ThreadForkSnapshot,
    pub(crate) selection_summary: Option<String>,
    pub(crate) anchor_head_summary: Option<String>,
    pub(crate) anchor_tail_summary: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BranchSnippetError {
    EmptySnippet,
    NoAssistantResponse,
    NoMatch,
    AmbiguousMatch,
    NoAnchor,
}

pub(crate) fn locate_branch_snippet(
    cells: &[Arc<dyn HistoryCell>],
    turns: Option<&[Turn]>,
    latest_raw_assistant_markdown: Option<&str>,
    snippet: &str,
) -> Result<BranchReplyTarget, BranchSnippetError> {
    let snippets = normalized_snippet_candidates(snippet);
    if snippets
        .iter()
        .all(|snippet| snippet.text.trim().is_empty())
    {
        return Err(BranchSnippetError::EmptySnippet);
    }

    let responses = assistant_responses(cells, turns, latest_raw_assistant_markdown);
    if responses.is_empty() {
        let Some(raw_assistant_markdown) =
            latest_raw_assistant_markdown.filter(|markdown| !markdown.trim().is_empty())
        else {
            return Err(BranchSnippetError::NoAssistantResponse);
        };
        return locate_in_latest_markdown_only(raw_assistant_markdown, &snippets);
    }

    let mut matched_target = None;
    let mut saw_unanchorable_match = false;
    for (response_index, response) in responses.iter().enumerate() {
        let target = match locate_branch_snippet_in_response(
            response,
            response_index + 1 == responses.len(),
            latest_raw_assistant_markdown,
            &snippets,
        ) {
            Ok(Some(target)) => target,
            Ok(None) => continue,
            Err(BranchSnippetError::NoAnchor) => {
                saw_unanchorable_match = true;
                continue;
            }
            Err(err) => return Err(err),
        };
        if matched_target.is_some() {
            return Err(BranchSnippetError::AmbiguousMatch);
        }
        matched_target = Some(target);
    }

    matched_target.ok_or({
        if saw_unanchorable_match {
            BranchSnippetError::NoAnchor
        } else {
            BranchSnippetError::NoMatch
        }
    })
}

struct AssistantResponse {
    assistant_message_index: usize,
    display_lines: Vec<String>,
    source_text: Option<String>,
    source_segments: Option<Vec<String>>,
    allow_single_segment_fallback: bool,
}

impl AssistantResponse {
    fn source_search_lines(
        &self,
        latest_raw_assistant_markdown: Option<&str>,
        is_latest_response: bool,
    ) -> Vec<String> {
        self.raw_markdown_text(latest_raw_assistant_markdown, is_latest_response)
            .map(source_search_lines_from_source_text)
            .filter(|lines| !lines.is_empty())
            .unwrap_or_else(|| self.display_lines.clone())
    }

    fn raw_markdown_text<'a>(
        &'a self,
        latest_raw_assistant_markdown: Option<&'a str>,
        is_latest_response: bool,
    ) -> Option<&'a str> {
        if is_latest_response
            && let Some(raw_markdown) =
                latest_raw_assistant_markdown.filter(|markdown| !markdown.trim().is_empty())
        {
            return Some(raw_markdown);
        }

        self.source_text
            .as_deref()
            .filter(|source_text| !source_text.trim().is_empty())
    }
}

fn assistant_responses(
    cells: &[Arc<dyn HistoryCell>],
    turns: Option<&[Turn]>,
    latest_raw_assistant_markdown: Option<&str>,
) -> Vec<AssistantResponse> {
    let mut responses = if let Some(turns) = turns {
        assistant_responses_from_turns(turns)
    } else {
        assistant_responses_from_cells(cells)
    };

    // Synthesize an entry for in-progress streaming content only when there are no consolidated
    // AgentMarkdownCells yet. Once cells exist they hold the canonical content and the latest
    // markdown is redundant (even if the combined text differs from any individual cell).
    if responses.is_empty()
        && latest_raw_assistant_markdown
            .map(|m| !m.trim().is_empty())
            .unwrap_or(false)
    {
        responses.push(AssistantResponse {
            assistant_message_index: 0,
            display_lines: render_markdown_to_plain_lines(
                latest_raw_assistant_markdown.unwrap_or_default(),
            ),
            source_text: latest_raw_assistant_markdown.map(ToString::to_string),
            source_segments: None,
            allow_single_segment_fallback: true,
        });
    }

    responses
}

fn assistant_responses_from_turns(turns: &[Turn]) -> Vec<AssistantResponse> {
    turns
        .iter()
        .flat_map(|turn| turn.items.iter())
        .filter_map(|item| match item {
            ThreadItem::AgentMessage {
                text,
                source_text,
                source_segments,
                ..
            } => Some((
                text.as_str(),
                source_text.as_deref(),
                source_segments.as_deref(),
            )),
            _ => None,
        })
        .enumerate()
        .map(
            |(assistant_message_index, (display_text, source_text, source_segments))| {
                AssistantResponse {
                    assistant_message_index,
                    display_lines: render_markdown_to_plain_lines(
                        source_text.unwrap_or(display_text),
                    ),
                    source_text: source_text
                        .map(ToString::to_string)
                        .or_else(|| Some(display_text.to_string())),
                    source_segments: source_segments.map(<[String]>::to_vec),
                    allow_single_segment_fallback: false,
                }
            },
        )
        .collect()
}

fn assistant_responses_from_cells(cells: &[Arc<dyn HistoryCell>]) -> Vec<AssistantResponse> {
    let mut responses = Vec::new();
    let mut current_message_cells: Vec<&AgentMessageCell> = Vec::new();

    for cell in cells {
        if let Some(message_cell) = cell.as_any().downcast_ref::<AgentMessageCell>() {
            if !message_cell.is_stream_continuation() && !current_message_cells.is_empty() {
                responses.push(assistant_response_from_message_cells(
                    responses.len(),
                    &current_message_cells,
                ));
                current_message_cells.clear();
            }
            current_message_cells.push(message_cell);
            continue;
        }

        if !current_message_cells.is_empty() {
            responses.push(assistant_response_from_message_cells(
                responses.len(),
                &current_message_cells,
            ));
            current_message_cells.clear();
        }

        let Some(markdown_cell) = cell.as_any().downcast_ref::<AgentMarkdownCell>() else {
            continue;
        };
        let raw_markdown = markdown_cell.raw_markdown_text();
        if raw_markdown.trim().is_empty() {
            continue;
        }
        responses.push(AssistantResponse {
            assistant_message_index: responses.len(),
            display_lines: markdown_cell.source_lines_plain_text(),
            source_text: Some(raw_markdown.to_string()),
            source_segments: None,
            allow_single_segment_fallback: true,
        });
    }

    if !current_message_cells.is_empty() {
        responses.push(assistant_response_from_message_cells(
            responses.len(),
            &current_message_cells,
        ));
    }

    responses
}

fn assistant_response_from_message_cells(
    assistant_message_index: usize,
    cells: &[&AgentMessageCell],
) -> AssistantResponse {
    let source_text = cells
        .iter()
        .rev()
        .find_map(|cell| cell.raw_markdown_text())
        .map(ToString::to_string);
    let display_lines = cells
        .iter()
        .flat_map(|cell| cell.source_lines_plain_text())
        .collect();
    let source_segments = cells
        .iter()
        .rev()
        .find_map(|cell| cell.source_segments())
        .map(<[String]>::to_vec);

    AssistantResponse {
        assistant_message_index,
        display_lines,
        source_text,
        source_segments,
        allow_single_segment_fallback: true,
    }
}

fn locate_in_latest_markdown_only(
    raw_assistant_markdown: &str,
    snippets: &[NormalizedText],
) -> Result<BranchReplyTarget, BranchSnippetError> {
    let source_search_lines = source_search_lines_from_source_text(raw_assistant_markdown);
    let normalized_source = NormalizedText::new(&source_search_lines);
    let match_range = find_branch_snippet_match(&normalized_source.text, snippets)?;
    let (match_start_line_index, _) = normalized_source
        .mapping
        .get(match_range.start)
        .copied()
        .ok_or(BranchSnippetError::NoAnchor)?;
    let source_line_start_index =
        structure_start_line(&source_search_lines, match_start_line_index);
    let source_line_index = structure_end_line(&source_search_lines, source_line_start_index);
    let snapshot = snapshot_from_display_line_index(
        raw_assistant_markdown,
        None,
        /*assistant_message_index*/ 0,
        source_line_index,
        /*is_latest_response*/ true,
        /*allow_single_segment_fallback*/ true,
    )
    .ok_or(BranchSnippetError::NoAnchor)?;

    Ok(BranchReplyTarget {
        origin_snapshot: snapshot.clone(),
        snapshot,
        selection_summary: branch_anchor_selection_summary(snippets),
        anchor_head_summary: branch_anchor_head_summary(
            &source_search_lines,
            source_line_start_index,
        ),
        anchor_tail_summary: branch_anchor_tail_summary_for_line(
            &source_search_lines,
            source_line_index,
        ),
    })
}

fn source_search_lines_from_source_text(text: &str) -> Vec<String> {
    split_lines(text)
        .into_iter()
        .map(|line| normalize_search_source_line(&line))
        .collect()
}

fn normalize_search_source_line(line: &str) -> String {
    line.replace("**", "").replace("__", "").replace('`', "")
}

fn locate_branch_snippet_in_response(
    response: &AssistantResponse,
    is_latest_response: bool,
    latest_raw_assistant_markdown: Option<&str>,
    snippets: &[NormalizedText],
) -> Result<Option<BranchReplyTarget>, BranchSnippetError> {
    let source_search_lines =
        response.source_search_lines(latest_raw_assistant_markdown, is_latest_response);
    let normalized_source = NormalizedText::new(&source_search_lines);
    let match_range = match find_branch_snippet_match(&normalized_source.text, snippets) {
        Ok(range) => range,
        Err(BranchSnippetError::NoMatch) => return Ok(None),
        Err(err) => return Err(err),
    };

    let (match_start_line_index, _) = normalized_source
        .mapping
        .get(match_range.start)
        .copied()
        .ok_or(BranchSnippetError::NoAnchor)?;
    let source_line_start_index =
        structure_start_line(&source_search_lines, match_start_line_index);
    let source_line_index = structure_end_line(&source_search_lines, source_line_start_index);

    let snapshot = response
        .raw_markdown_text(latest_raw_assistant_markdown, is_latest_response)
        .and_then(|raw_markdown| {
            snapshot_from_display_line_index(
                raw_markdown,
                response.source_segments.as_deref(),
                response.assistant_message_index,
                source_line_index,
                is_latest_response,
                response.allow_single_segment_fallback,
            )
        })
        .ok_or(BranchSnippetError::NoAnchor)?;
    let origin_snapshot = origin_snapshot_for_response(response.assistant_message_index, &snapshot);

    Ok(Some(BranchReplyTarget {
        snapshot,
        origin_snapshot,
        selection_summary: branch_anchor_selection_summary(snippets),
        anchor_head_summary: branch_anchor_head_summary(
            &source_search_lines,
            source_line_start_index,
        ),
        anchor_tail_summary: branch_anchor_tail_summary_for_line(
            &source_search_lines,
            source_line_index,
        ),
    }))
}

fn snapshot_from_display_line_index(
    source_text: &str,
    source_segments: Option<&[String]>,
    assistant_message_index: usize,
    display_line_index: usize,
    is_latest_response: bool,
    allow_single_segment_fallback: bool,
) -> Option<ThreadForkSnapshot> {
    let source_lines = split_lines(source_text);
    let source_line_index = display_line_index.min(source_lines.len().saturating_sub(1));
    let raw_anchor = inclusive_line_end_anchor(source_text, &source_lines, source_line_index)?;
    let (source_line_index, source_byte_offset) =
        match source_segments.and_then(|segments| segment_local_anchor(segments, raw_anchor)) {
            Some(anchor) => anchor,
            None if is_latest_response || allow_single_segment_fallback => (0, raw_anchor),
            None => return None,
        };

    if is_latest_response {
        Some(ThreadForkSnapshot::LatestAssistantReadAnchor {
            source_line_index: u32::try_from(source_line_index).ok()?,
            source_byte_offset: u32::try_from(source_byte_offset).ok()?,
        })
    } else {
        Some(ThreadForkSnapshot::AssistantReadAnchor {
            assistant_message_index: u32::try_from(assistant_message_index).ok()?,
            source_line_index: u32::try_from(source_line_index).ok()?,
            source_byte_offset: u32::try_from(source_byte_offset).ok()?,
        })
    }
}

fn origin_snapshot_for_response(
    assistant_message_index: usize,
    snapshot: &ThreadForkSnapshot,
) -> ThreadForkSnapshot {
    match snapshot {
        ThreadForkSnapshot::AssistantReadAnchor {
            assistant_message_index,
            source_line_index,
            source_byte_offset,
        } => ThreadForkSnapshot::AssistantReadAnchor {
            assistant_message_index: *assistant_message_index,
            source_line_index: *source_line_index,
            source_byte_offset: *source_byte_offset,
        },
        ThreadForkSnapshot::LatestAssistantReadAnchor {
            source_line_index,
            source_byte_offset,
        } => ThreadForkSnapshot::AssistantReadAnchor {
            assistant_message_index: u32::try_from(assistant_message_index).unwrap_or(u32::MAX),
            source_line_index: *source_line_index,
            source_byte_offset: *source_byte_offset,
        },
        ThreadForkSnapshot::Interrupted => ThreadForkSnapshot::Interrupted,
    }
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(ToString::to_string).collect()
}

fn inclusive_line_end_anchor(
    source_text: &str,
    source_lines: &[String],
    line_index: usize,
) -> Option<usize> {
    let line_start = source_lines
        .iter()
        .take(line_index)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let line = source_lines.get(line_index)?;
    if line.is_empty() {
        return None;
    }
    let exclusive_anchor = line_start + line.len();
    let inclusive_anchor = exclusive_anchor.saturating_sub(1);
    (inclusive_anchor < source_text.len())
        .then_some(previous_char_boundary(source_text, inclusive_anchor))
}

fn normalized_snippet_candidates(snippet: &str) -> Vec<NormalizedText> {
    let mut variants = Vec::new();
    push_normalized_candidate(&mut variants, vec![snippet.to_string()]);
    push_normalized_candidate(
        &mut variants,
        snippet
            .lines()
            .map(strip_main_view_assistant_chrome)
            .collect::<Vec<_>>(),
    );
    push_normalized_candidate(
        &mut variants,
        snippet
            .lines()
            .map(|line| strip_main_view_assistant_chrome(line).trim().to_string())
            .collect::<Vec<_>>(),
    );

    if let Some(unquoted) = strip_surrounding_quotes(snippet) {
        push_normalized_candidate(&mut variants, vec![unquoted.to_string()]);
        push_normalized_candidate(
            &mut variants,
            unquoted
                .lines()
                .map(strip_main_view_assistant_chrome)
                .collect::<Vec<_>>(),
        );
        push_normalized_candidate(
            &mut variants,
            unquoted
                .lines()
                .map(|line| strip_main_view_assistant_chrome(line).trim().to_string())
                .collect::<Vec<_>>(),
        );
    }

    variants
}

fn strip_main_view_assistant_chrome(line: &str) -> String {
    let trimmed = line.trim_start_matches([' ', '\t']);
    trimmed
        .strip_prefix('•')
        .map_or_else(|| line.to_string(), |rest| rest.trim_start().to_string())
}

fn push_normalized_candidate(candidates: &mut Vec<NormalizedText>, lines: Vec<String>) {
    let candidate = NormalizedText::new(&lines);
    if candidates
        .iter()
        .all(|existing| existing.text != candidate.text)
    {
        candidates.push(candidate);
    }
}

fn strip_surrounding_quotes(snippet: &str) -> Option<&str> {
    let trimmed = snippet.trim();
    let quote_pairs = [
        ('\"', '\"'),
        ('\'', '\''),
        ('\u{201C}', '\u{201D}'),
        ('\u{2018}', '\u{2019}'),
    ];
    quote_pairs.iter().find_map(|(open, close)| {
        trimmed
            .strip_prefix(*open)
            .and_then(|rest| rest.strip_suffix(*close))
            .map(str::trim)
            .filter(|inner| !inner.is_empty())
    })
}

fn find_branch_snippet_match(
    source: &str,
    snippets: &[NormalizedText],
) -> Result<std::ops::Range<usize>, BranchSnippetError> {
    let mut last_error = BranchSnippetError::NoMatch;
    for snippet in snippets {
        match find_unique_match(source, &snippet.text) {
            Ok(range) => return Ok(range),
            Err(BranchSnippetError::NoMatch) => {}
            Err(err) => last_error = err,
        }
    }
    Err(last_error)
}

fn find_unique_match(
    source: &str,
    snippet: &str,
) -> Result<std::ops::Range<usize>, BranchSnippetError> {
    let Some(start) = source.find(snippet) else {
        return Err(BranchSnippetError::NoMatch);
    };
    let end = start + snippet.len();
    if source[end..].contains(snippet) {
        return Err(BranchSnippetError::AmbiguousMatch);
    }
    Ok(start..end)
}

fn structure_end_line(source_lines: &[String], line_index: usize) -> usize {
    if let Some(list_item_start) = containing_list_item_start(source_lines, line_index) {
        return source_lines
            .iter()
            .enumerate()
            .skip(list_item_start)
            .take_while(|(index, line)| {
                *index == list_item_start || (!line.trim().is_empty() && !is_list_item_start(line))
            })
            .map(|(index, _)| index)
            .last()
            .unwrap_or(line_index);
    }

    source_lines
        .iter()
        .enumerate()
        .skip(line_index)
        .take_while(|(_, line)| !line.trim().is_empty())
        .map(|(index, _)| index)
        .last()
        .unwrap_or(line_index)
}

fn structure_start_line(source_lines: &[String], line_index: usize) -> usize {
    if let Some(list_item_start) = containing_list_item_start(source_lines, line_index) {
        return list_item_start;
    }

    (0..=line_index)
        .rev()
        .take_while(|index| {
            source_lines
                .get(*index)
                .is_some_and(|line| !line.trim().is_empty())
        })
        .last()
        .unwrap_or(line_index)
}

fn branch_anchor_head_summary(source_lines: &[String], source_line_index: usize) -> Option<String> {
    const SUMMARY_WORDS: usize = 8;

    let line = source_lines.get(source_line_index)?;
    let summary = line
        .split_whitespace()
        .take(SUMMARY_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    (!summary.is_empty()).then_some(summary)
}

fn branch_anchor_tail_summary(
    source_lines: &[String],
    source_line_index: usize,
    source_byte_offset: usize,
) -> Option<String> {
    const SUMMARY_WORDS: usize = 6;

    let line = source_lines.get(source_line_index)?;
    let end = next_char_boundary(line, source_byte_offset.min(line.len()));
    let summary = line[..end]
        .split_whitespace()
        .rev()
        .take(SUMMARY_WORDS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");
    (!summary.is_empty()).then_some(summary)
}

fn branch_anchor_tail_summary_for_line(
    source_lines: &[String],
    source_line_index: usize,
) -> Option<String> {
    let line = source_lines.get(source_line_index)?;
    if line.is_empty() {
        return None;
    }
    branch_anchor_tail_summary(
        source_lines,
        source_line_index,
        line.len().saturating_sub(1),
    )
}

fn branch_anchor_selection_summary(snippets: &[NormalizedText]) -> Option<String> {
    let snippet = snippets.first()?.text.trim();
    if snippet.is_empty() {
        return None;
    }

    Some(snippet.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn previous_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut offset = byte_offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn next_char_boundary(text: &str, byte_offset: usize) -> usize {
    let offset = previous_char_boundary(text, byte_offset);
    if offset >= text.len() {
        return offset;
    }
    text[offset..]
        .chars()
        .next()
        .map_or(offset, |ch| offset + ch.len_utf8())
}

fn render_markdown_to_plain_lines(text: &str) -> Vec<String> {
    let mut rendered = Vec::new();
    crate::markdown::append_markdown(text, /*width*/ None, None, &mut rendered);
    rendered
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn segment_local_anchor(
    source_segments: &[String],
    absolute_anchor: usize,
) -> Option<(usize, usize)> {
    let mut segment_start = 0usize;
    for (segment_index, segment) in source_segments.iter().enumerate() {
        let segment_end = segment_start + segment.len();
        if absolute_anchor < segment_end {
            return Some((segment_index, absolute_anchor - segment_start));
        }
        segment_start = segment_end;
    }
    None
}

fn containing_list_item_start(source_lines: &[String], line_index: usize) -> Option<usize> {
    source_lines
        .iter()
        .enumerate()
        .take(line_index + 1)
        .rev()
        .take_while(|(_, line)| !line.trim().is_empty())
        .find_map(|(index, line)| is_list_item_start(line).then_some(index))
}

fn is_list_item_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }

    let Some((marker, rest)) = trimmed.split_once(['.', ')']) else {
        return false;
    };
    !rest.is_empty()
        && rest.starts_with(char::is_whitespace)
        && marker.chars().all(|ch| ch.is_ascii_digit())
}

struct NormalizedText {
    text: String,
    mapping: Vec<(usize, usize)>,
}

impl NormalizedText {
    fn new(lines: &[String]) -> Self {
        let mut text = String::new();
        let mut mapping = Vec::new();
        let mut pending_space: Option<(usize, usize)> = None;

        for (line_index, line) in lines.iter().enumerate() {
            for (byte_offset, ch) in line.char_indices() {
                if ch.is_whitespace() {
                    pending_space.get_or_insert((line_index, byte_offset));
                } else {
                    if !text.is_empty()
                        && let Some((space_line_index, space_byte_offset)) = pending_space.take()
                    {
                        text.push(' ');
                        mapping.push((space_line_index, space_byte_offset));
                    }
                    text.push(ch);
                    mapping.push((line_index, byte_offset));
                    pending_space = None;
                }
            }
            pending_space.get_or_insert((line_index, line.len()));
        }

        Self { text, mapping }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::history_cell::PlainHistoryCell;

    fn assistant_markdown_cell(text: &str) -> Arc<dyn HistoryCell> {
        Arc::new(AgentMarkdownCell::new(text.to_string(), Path::new("/")))
    }

    fn separator_cell() -> Arc<dyn HistoryCell> {
        Arc::new(PlainHistoryCell::new(Vec::new()))
    }

    #[test]
    fn searches_all_assistant_responses_and_returns_older_anchor() {
        let target = locate_branch_snippet(
            &[
                assistant_markdown_cell(
                    "QuickBooks does invoicing.\n- set invoice and due dates\n- track payments",
                ),
                separator_cell(),
                assistant_markdown_cell("newer response"),
            ],
            None,
            Some("newer response"),
            "- set invoice and due dates",
        )
        .expect("snippet should match an older response");

        assert_eq!(
            target.snapshot,
            ThreadForkSnapshot::AssistantReadAnchor {
                assistant_message_index: 0,
                source_line_index: 0,
                source_byte_offset: 53,
            }
        );
        assert_eq!(
            target.anchor_head_summary.as_deref(),
            Some("- set invoice and due dates")
        );
        assert_eq!(
            target.anchor_tail_summary.as_deref(),
            Some("- set invoice and due dates")
        );
    }

    #[test]
    fn searches_all_chunks_of_latest_streamed_assistant_response() {
        let target = locate_branch_snippet(
            &[],
            None,
            Some(
                "At a high level, a retainer consulting contract should pin down six things clearly:\n\n1. Scope and exclusions: exactly what you will do.\n2. Fee structure: monthly retainer amount, whether hours roll over, whether the\nretainer is earned on receipt or applied against time, overage rates,\ninvoicing cadence, payment terms, and late-fee rules.\n3. Capacity and service levels: how much availability the client is buying.\n4. Term and exit rules: start date, renewal, minimum term.",
            ),
            "2. Fee structure: monthly retainer amount, whether hours roll over, whether the\n     retainer is earned on receipt or applied against time",
        )
        .expect("snippet should match an earlier chunk of the latest response");

        assert_eq!(
            target.snapshot,
            ThreadForkSnapshot::LatestAssistantReadAnchor {
                source_line_index: 0,
                source_byte_offset: 338,
            }
        );
    }

    #[test]
    fn reports_ambiguous_snippet_across_assistant_responses() {
        let result = locate_branch_snippet(
            &[
                assistant_markdown_cell("repeat this"),
                separator_cell(),
                assistant_markdown_cell("repeat this"),
            ],
            None,
            Some("repeat this"),
            "repeat this",
        );

        assert_eq!(result, Err(BranchSnippetError::AmbiguousMatch));
    }

    #[test]
    fn returns_no_match_when_snippet_is_absent() {
        let result = locate_branch_snippet(
            &[assistant_markdown_cell("first reply"), separator_cell()],
            None,
            Some("latest reply"),
            "missing text",
        );

        assert_eq!(result, Err(BranchSnippetError::NoMatch));
    }

    #[test]
    fn anchors_against_latest_raw_markdown_when_visible_reply_spans_multiple_cells() {
        let result = locate_branch_snippet(
            &[
                assistant_markdown_cell(
                    "1. Define the user problem precisely and verify that AI is actually the right tool for it.\n2. Choose one narrow, high-value use case before expanding into broader capabilities.\n3. Identify the input data, output quality bar, and failure modes early.\n4. Decide what level of accuracy, latency, and cost the product must hit to be viable.\n5. Design the human workflow around the AI, not just the model output itself.\n6. Plan for trust: explainability, review steps, confidence signals, and safe fallbacks.",
                ),
                assistant_markdown_cell(
                    "7. Set clear boundaries for privacy, security, compliance, and data retention.\n8. Build evaluation from the start with realistic test cases and success metrics.\n9. Define the operating model: model/provider choice, pricing, monitoring, and iteration loop.\n10. Make the offer legible to buyers by stating outcome, scope, limitations, and ROI clearly.",
                ),
            ],
            None,
            Some(
                "1. Define the user problem precisely and verify that AI is actually the right tool for it.\n2. Choose one narrow, high-value use case before expanding into broader capabilities.\n3. Identify the input data, output quality bar, and failure modes early.\n4. Decide what level of accuracy, latency, and cost the product must hit to be viable.\n5. Design the human workflow around the AI, not just the model output itself.\n6. Plan for trust: explainability, review steps, confidence signals, and safe fallbacks.\n7. Set clear boundaries for privacy, security, compliance, and data retention.\n8. Build evaluation from the start with realistic test cases and success metrics.\n9. Define the operating model: model/provider choice, pricing, monitoring, and iteration loop.\n10. Make the offer legible to buyers by stating outcome, scope, limitations, and ROI clearly.",
            ),
            "7. Set clear boundaries for privacy, security, compliance, and data retention.",
        )
        .expect("snippet should anchor against continuation lines");

        assert_eq!(
            result.anchor_head_summary.as_deref(),
            Some("7. Set clear boundaries for privacy, security, compliance,")
        );
        assert_eq!(
            result.anchor_tail_summary.as_deref(),
            Some("privacy, security, compliance, and data retention.")
        );
        match result.snapshot {
            ThreadForkSnapshot::LatestAssistantReadAnchor {
                source_byte_offset, ..
            } => assert!(source_byte_offset > 0),
            other => panic!("expected latest assistant anchor, got {other:?}"),
        }
    }

    #[test]
    fn prefers_older_cell_match_to_latest_raw_markdown_when_snippet_unique_in_older() {
        let target = locate_branch_snippet(
            &[
                assistant_markdown_cell(
                    "That point is about deciding things like:\n- whether prompts, uploads, and outputs are stored\n- who can access them internally",
                ),
                separator_cell(),
                assistant_markdown_cell("unrelated second response"),
            ],
            None,
            Some("unrelated second response"),
            "- whether prompts, uploads, and outputs are stored",
        )
        .expect("snippet should match the older anchored response");

        assert_eq!(
            target.snapshot,
            ThreadForkSnapshot::AssistantReadAnchor {
                assistant_message_index: 0,
                source_line_index: 0,
                source_byte_offset: 91,
            }
        );
    }

    #[test]
    fn matches_quoted_wrapped_bullet_snippet() {
        let target = locate_branch_snippet(
            &[assistant_markdown_cell(
                "My practical advice if you stay unregistered for now:\n\n- every legitimate business expense reduces your net business income\n- the business-use percentage matters for mixed-use items\n- subscriptions like QuickBooks, OpenAI, Anthropic, domain, hosting, Zoom, and design tools are exactly the kind of expenses worth tracking from day one",
            )],
            None,
            Some(
                "My practical advice if you stay unregistered for now:\n\n- every legitimate business expense reduces your net business income\n- the business-use percentage matters for mixed-use items\n- subscriptions like QuickBooks, OpenAI, Anthropic, domain, hosting, Zoom, and design tools are exactly the kind of expenses worth tracking from day one",
            ),
            "\" - the business-use percentage matters for mixed-use\n  items\"",
        )
        .expect("quoted wrapped bullet snippet should match");

        assert_eq!(
            target.snapshot,
            ThreadForkSnapshot::LatestAssistantReadAnchor {
                source_line_index: 0,
                source_byte_offset: 180,
            }
        );
    }

    #[test]
    fn keeps_numbered_list_branch_summaries_on_the_same_item_block() {
        let target = locate_branch_snippet(
            &[],
            None,
            Some(
                "10. What happens when something goes wrong?\n   Check support responsiveness, SLAs/priority tiers, status history, and whether you can fail over to another provider.\n11. Does it help sales, not just delivery?\n   Enterprise buyers often care more about contracts, logging, auditability, and regional controls than benchmark scores.\n12. Are you accidentally creating CASL/privacy exposure?\n   If you use AI for outbound emails/texts, consent and message compliance matter too.",
            ),
            "11. Does it help sales, not just delivery?\n   Enterprise buyers often care more about contracts, logging, auditability, and regional controls than\nbenchmark\nscores.",
        )
        .expect("numbered list snippet should stay on the selected item block");

        assert_eq!(
            target.anchor_head_summary.as_deref(),
            Some("11. Does it help sales, not just delivery?")
        );
        assert_eq!(
            target.anchor_tail_summary.as_deref(),
            Some("and regional controls than benchmark scores.")
        );
    }

    #[test]
    fn matches_wrapped_older_cell_snippet() {
        let target = locate_branch_snippet(
            &[
                assistant_markdown_cell(
                    "If you want, I can give you a simple rule for deciding whether to book a software subscription as:\n\n1. fully business\n2. partially business\n3. not worth claiming",
                ),
                separator_cell(),
                assistant_markdown_cell("newer unrelated reply"),
            ],
            None,
            Some("newer unrelated reply"),
            " If you want, I can give you a simple rule for deciding whether to book a software\n  subscription as:\n\n  1. fully business\n  2. partially business\n  3. not worth claiming",
        )
        .expect("wrapped replay snippet should match authoritative cell source");

        match target.snapshot {
            ThreadForkSnapshot::AssistantReadAnchor {
                assistant_message_index,
                source_line_index: _,
                source_byte_offset,
            } => {
                assert_eq!(assistant_message_index, 0);
                assert!(source_byte_offset > 0);
            }
            ThreadForkSnapshot::LatestAssistantReadAnchor {
                source_line_index: _,
                source_byte_offset,
            } => {
                assert!(source_byte_offset > 0);
            }
            other => panic!("expected assistant anchor, got {other:?}"),
        }
    }
}
