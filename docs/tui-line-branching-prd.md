# Line-Level Reading Progress and Branching (TUI PRD)

This document captures a proposed user experience for line-level reading
progress and conversational branching in the Codex TUI.

## Problem statement

Today, assistant replies are effectively consumed as monolithic scrollable
blocks. That makes two workflows awkward:

- A user cannot clearly mark "I have read up to here" while scanning a long
  response.
- A user cannot naturally interject from a precise point in an assistant reply
  and indicate that the remainder of the reply was not read.

This creates ambiguity in both the UX and the model context:

- The interface does not preserve where the user stopped reading.
- A follow-up triggered from the middle of a response can accidentally imply
  that the rest of that response was read and understood.
- There is no first-class navigation model for exploring alternate branches
  while still returning to the original thread.

## Goals

- Make assistant replies navigable at the line level while reading.
- Let the user place a cursor/highlight on a specific line in an assistant
  reply.
- Treat the highlighted line as both:
  - a read-progress marker
  - a branch point for follow-up input
- When branching from a line, treat the remainder of that assistant reply as
  unread and excluded from branch context.
- Let the user navigate back to the parent thread and continue reading the main
  branch.
- Make branch depth visible so the user can tell how far they are from the
  original conversation.

## Core principles

- A conversation branch is a real divergence in conversation state, not only a
  UI bookmark.
- A branch created from the middle of an assistant reply is semantically
  equivalent to a user interruption from that read point.
- The system should not assume the user read or accepted assistant content past
  the selected branch anchor.
- Branch context should follow the active ancestry path only, not sibling or
  descendant branches.
- Branch outcomes are isolated by default; carrying decisions back to a parent
  branch should be an explicit future action, not an implicit side effect.

## Non-goals

- Full document-style annotation, rich text selection, or arbitrary span-based
  editing inside transcript messages.
- Simultaneous multi-user branch collaboration.
- Changing the semantics of ordinary replies at the end of a fully read thread.

## Primary user story

A user is reading a long assistant response. Midway through, they notice a
mistake or want clarification on a specific detail. They move the cursor to the
line where they want to interject, branch from that point, and ask a follow-up.

That branch should preserve the fact that:

- the user has read the response up to the selected line
- the user has not read the remainder of the response
- the follow-up should be contextualized only with the content through the
  selected line, not the unread tail

Later, the user can return to the parent thread, resume reading from the branch
point, and continue down the original response stream.

## UX model

### 1. Line-addressable assistant output

Assistant messages should be navigable line-by-line while the user is reading.
The user can move a cursor/highlight through the visible reply using keyboard
navigation.

At any point, the highlighted line represents the user's current read position
within that assistant message.

### 2. Read-progress marker

The selected line means:

- the user has read up to and including this line
- content after this line is currently unread

This read marker should remain associated with the message so the user can
return to it later.

### 3. Branching from a line

If the user replies while a mid-message line is selected, the system should
create a child conversation branch rooted at that line.

Branch semantics:

- The branch inherits the conversation history up to that assistant message.
- The assistant message at the branch point is truncated to the selected line
  for branch-context purposes.
- The remainder of that assistant message is treated as unread and is not
  included in the branch context.
- The branch should record that the assistant reply was interrupted from the
  user's point of view, rather than merely appearing to end abruptly.

### 4. Default end-of-thread behavior

If the user replies at the natural end of the main thread, the existing
behavior remains:

- the immediately preceding assistant reply is assumed fully read
- the full reply is included in context
- no special branch semantics are needed unless the user explicitly branches

### 5. Parent/child navigation

The interface should let the user:

- enter a branch from a selected line
- return to the parent thread where the branch occurred
- resume reading from that point in the parent thread
- continue scrolling to the end of the parent thread independently of branch
  exploration

### 6. Branch depth visibility

The transcript UI should show how many levels deep the current thread is from
the original/main stream.

This can be represented through indentation, a breadcrumb, a thread path
indicator, or another clear lightweight mechanism. The exact presentation is
still open.

## Functional requirements

### Transcript navigation

- The user can move a highlight/cursor across assistant reply lines with the
  keyboard.
- The current highlighted line is visually distinct from ordinary scroll state.
- The transcript can restore the read marker when revisiting a message/thread.

### Branch creation

- Branching is available from a selected line within an assistant response.
- A branch records:
  - parent thread identity
  - source assistant message identity
  - selected branch/read position
  - branch depth relative to the root thread

### Context construction

When constructing the next model input for a branch:

- include all prior turns up to the selected assistant message
- include only the read portion of the selected assistant message
- exclude the unread remainder of that assistant message
- exclude any future turns that only exist past the branch point in the parent
  thread
- include explicit machine-readable branch/interruption metadata so the model
  understands that the assistant reply was interrupted at the selected point

### Branch tree semantics

Each reply belongs to exactly one path through the conversation tree.

Recommended rule set:

- a turn should see the full ancestry of its active path
- a turn should not automatically see sibling branch turns
- a turn should not automatically see descendant branch turns
- parent/main-thread replies should not inherit child-branch discussion unless
  the user explicitly carries it back

Examples:

- replying at the end of the main path includes only main-path turns
- replying inside branch `A` includes the main path through the `A` split plus
  branch `A` turns
- replying inside branch `A1` includes the main path through `A`, then branch
  `A` through the `A1` split, then branch `A1` turns

Not included by default:

- sibling branches such as `B` while replying in `A`
- child branches while replying on a parent branch
- unread tail content after a branch anchor

### Branch merge semantics

Important branch conclusions may later need to influence the parent or main
thread. The recommended model is:

- branching is for divergence
- merging back is a separate explicit action

For v1:

- branch outcomes are isolated by default
- there is no implicit propagation of branch conclusions upward
- if a user wants a branch conclusion to affect a parent path, they must carry
  it back manually

Future direction:

- support an explicit "merge" or "promote decision to parent" action for
  carrying branch outcomes back into an ancestor path

### Parent-thread continuity

- Returning to the parent thread restores the user to the branch location.
- The parent thread still contains the full original assistant reply.
- The user can continue reading downward and later reply from the end of that
  main path with normal full-read semantics.

## Proposed data/behavior model

The core product concept is:

- `read_position`: where the user last marked progress in an assistant reply
- `branch_point`: a read position that was used to create a child thread

The cleanest implementation is likely to store a stable content anchor rather
than only a rendered row number.

Recommended internal model:

- Persist a stable text offset or logical line anchor in the assistant message.
- Map that anchor to rendered terminal rows at display time.

This avoids branch corruption when terminal width changes and wrapped visual
lines shift.

For forward compatibility, the stored branch metadata should be generic even if
v1 only exposes branching from assistant replies.

Recommended metadata shape:

- `branch_source_message_id`
- `branch_source_role`
- `branch_anchor`

This keeps the implementation open to future branching from user messages or
other transcript nodes without forcing assistant-only assumptions into the data
model.

## Product decisions for v1

The following decisions are considered settled for the first implementation.

### 1. Anchor model

- Use a stable text offset internally.
- Present that anchor to the user as a line-level highlight in the rendered
  transcript.

### 2. Resize behavior

- Read positions and branch points must survive terminal resize.
- Recompute rendered line mapping from the stable content anchor.

### 3. Read-progress persistence

- Read markers persist even when the user has not yet created a branch.
- Returning to a thread/message restores the saved reading position.

### 4. Multiple branches from one message

- A single assistant reply may have multiple child branches anchored at
  different positions.

### 5. Model awareness of branching

- The model should be explicitly informed that a branch was created from a
  partial read position.
- Branch context should include machine-readable branch semantics, not only raw
  transcript truncation.
- The branch metadata must state that content after the branch anchor was not
  read and is excluded from context.

Important constraint:

- The system should not fabricate a user motive for branching.
- If the user gives a reason in their follow-up, that reason is naturally part
  of the conversation.
- The system may communicate the structural reason: the user branched from a
  partial read position and unread content was excluded.
- The system may also communicate that the assistant reply was effectively
  interrupted from the user's point of view.

### 6. Branching scope in v1

- v1 supports branching from assistant replies only.
- The implementation should avoid boxing out future branching from user
  messages.

### 7. Streaming behavior

- v1 should support branching only from completed assistant messages.
- Branching during live streaming can be considered in a later phase.

## Recommended v1 UX

### Unread tail presentation

Start with:

- active line highlight
- branch marker or inline annotation on the selected line

Do not start with:

- dimming the entire unread tail below the read position

Rationale:

- This clearly communicates the branch point without making the parent thread
  visually heavy or harder to read.
- It keeps transcript readability high while still signaling branch semantics.
- Full unread-tail styling can be added later if users need stronger visual
  separation.

### Navigation model

Start with a transcript-first navigation model.

Recommended interaction shape:

- Up/Down moves through rendered transcript lines.
- A dedicated action replies/branches from the currently selected line.
- A dedicated action returns to the parent thread.
- Additional child-branch navigation can be added incrementally once the base
  interaction is stable.

Rationale:

- The user's primary task is reading the transcript.
- The branch point is line-addressable in the transcript itself.
- This avoids forcing the user into a separate "inspect mode" before branching.

### Copied-snippet branch command

Main-mode history is terminal scrollback, not a Codex-owned viewport. When the
user scrolls up in the terminal, Codex cannot reliably know which row the user
is viewing or where a visual cursor should be anchored.

For a low-risk prototype that fits the current architecture, support:

- user scrolls normally in the terminal
- user copies a sufficiently specific block of assistant text with the OS or
  terminal selection
- user runs `/branch-from <copied assistant text>` (with `/branch` retained as a short alias)
- Codex searches the latest assistant response for that text
- Codex maps the match to a semantic endpoint, starting with the containing
  paragraph or list item
- Codex immediately creates and switches into a persistent child branch rooted
  at that anchor, excluding unread assistant content after the anchor
- the composer is empty in the branch, and the user's next message submits
  normally into that child thread
- while the branch is active, the footer shows a compact dedicated `d<n>
  "...<anchor tail>"` row plus an `Esc to return` hint
- `/branch-list` shows a compact two-line branch navigator row: depth, Unicode
  relation glyph, compact last-active age, full thread id, then the anchor
  snippet on the second line
- the branch depth and anchor tail are persisted as thread metadata so the
  indicator can be reconstructed after `codex resume <branch-thread-id>`

This prototype does not require Codex to own normal scrollback and does not
own the visible terminal scrollback. It gives users a way to branch from the
content they were already reading while preserving the server-side branch
semantics and matching the existing `/side` habit of switching into a related
child thread before the user types the follow-up.

### Branch-depth indicator

Start with:

- a compact dedicated footer row showing the current branch depth and the last few
  words at the branch anchor
- compact local branch markers in the transcript at branch lines

Do not start with:

- transcript indentation for full branch depth
- a persistent sidebar tree

Rationale:

- Breadcrumbs provide global orientation cheaply in a narrow terminal.
- Local branch markers provide immediate context at the point where branching
  occurred.
- Avoiding indentation preserves horizontal space, which is especially
  important when line-level reading depends on wrapping behavior.

## Open design questions

These items remain open, but they no longer block a clean first
implementation.

### 1. What is a "line"?

Resolved for v1:

- Use a stable text offset internally, while presenting it to the user as a
  line-level highlight in the rendered transcript.

### 2. How should resize behave?

Resolved for v1:

- Read positions and branch points survive resize by reprojecting from a stable
  content anchor.

### 3. Should read markers persist without branching?

Resolved for v1:

- Yes. Reading progress persists independently from branch creation.

### 4. Can one message have multiple branches?

Resolved for v1:

- Yes. Multiple child branches from the same assistant message are allowed.

### 5. How should unread tail state be shown?

Recommended v1 choice:

- only show the active line highlight
- add a branch marker on the selected line

Chosen direction for v1:

- active line highlight plus branch marker/annotation
- no unread-tail dimming initially

Future expansion:

- dimming or annotating unread lines below the saved read position can be added
  later if needed

### 6. How explicit should the branch context be to the model?

Resolved for v1:

- Prefer explicit branch metadata in the constructed context so the model knows
  the user intentionally interrupted reading and unread content was excluded.

### 7. Which messages can be branched from?

Resolved for v1:

- Branching is allowed from assistant replies only.

Future-proofing requirement:

- Keep the stored metadata generic enough to support future branching from user
  messages or other transcript nodes.

### 8. How should live streaming behave?

Resolved for v1:

- Start with completed assistant messages only.

### 9. What is the navigation model?

Recommended v1 choice:

- transcript-first navigation
- dedicated actions for branch creation and parent-thread return

Still open:

- exact keybindings
- whether left/right should later be used for ancestry navigation
- how child-branch discovery should work when multiple child branches exist

### 10. What is the thread-depth indicator?

Recommended v1 choice:

- breadcrumb/header plus compact local branch markers

Still open:

- exact visual form of the breadcrumb
- exact transcript marker styling
- whether deeper tree visualization is needed later

## Success criteria

The feature is successful if:

- Users can stop mid-response and clearly mark where they are.
- Users can ask a follow-up from the middle of a response without implying they
  read the rest.
- Branched follow-ups exclude unread assistant content from model context.
- Users can move between branch and parent thread without losing their place.
- The current branch depth/path is understandable from the interface.

## Suggested implementation phases

### Phase 1: UX scaffolding

- Add line-level transcript cursor/highlight for assistant messages.
- Persist read-progress markers.
- Show a simple branch-depth/path indicator.
- Add local transcript markers for saved branch points.

### Phase 2: Branch semantics

- Allow reply-from-line to create child threads.
- Truncate selected assistant content in branch context.
- Add explicit branch metadata to model context construction.
- Restore parent-thread location on navigation back.

### Phase 3: Refinement

- Improve visual distinction for read vs unread regions if needed.
- Add richer branch navigation affordances.
- Evaluate streaming-time branching.

## Engineering plan

This section maps the product direction onto the existing codebase and
identifies the minimum viable implementation slices.

### Current code seams

The main implementation seams already exist, but they do not yet support
partial-message branching.

#### TUI transcript and input

- `codex-rs/tui/src/chatwidget.rs`
  - owns input handling and current thread/session state
  - currently handles normal submission and whole-thread fork flows
- `codex-rs/tui/src/history_cell.rs`
  - defines transcript rendering units via `HistoryCell`
  - currently renders cells, but does not expose stable per-line anchors for
    assistant messages
- `codex-rs/tui/src/wrapping.rs`
  - already contains source-text-to-rendered-line mapping helpers such as
    `wrap_ranges` and `wrap_ranges_trim`
  - this is the most natural place to build stable anchor-to-row mapping
- `codex-rs/tui/src/app.rs`
  - owns top-level thread replacement, overlay coordination, and session
    transitions

#### Thread lifecycle / server integration

- `codex-rs/tui/src/app_server_session.rs`
  - already wraps `thread/read`, `thread/start`, `thread/resume`, and
    `thread/fork`
- `codex-rs/app-server-protocol/src/protocol/v2.rs`
  - `ThreadForkParams` currently supports whole-thread forking only
  - `ThreadReadResponse` and `Turn` / `ThreadItem` already expose enough
    history detail to identify assistant messages
- `codex-rs/app-server/src/codex_message_processor.rs`
  - implements `thread/read` and `thread/fork`
  - current fork behavior copies a thread from an existing rollout, but does
    not support truncating an assistant message at an anchor

### Architectural implication

This feature is not TUI-only.

There are two distinct layers:

1. TUI-only reading UX
   - line cursor
   - persisted read markers
   - breadcrumb/header and branch markers

2. Branch semantics
   - new branch metadata
   - partial assistant-message truncation
   - context construction that excludes unread content

Phase 1 can begin in the TUI without protocol changes.
Phase 2 requires protocol and app-server work.

### Recommended module strategy

Do not grow existing central files substantially.

Recommended new TUI modules:

- `codex-rs/tui/src/transcript_read_cursor.rs`
  - state model for the selected transcript line, saved read positions, and
    anchor restoration
- `codex-rs/tui/src/transcript_branching.rs`
  - branch metadata, parent/child navigation state, and branch creation intent
- `codex-rs/tui/src/history_cell/line_anchors.rs` or similar extracted helper
  - stable mapping between assistant message text offsets and rendered rows

Recommended protocol/server additions:

- extend thread fork inputs with a partial-history branch descriptor rather than
  creating an entirely separate ad hoc branch API
- add explicit branch metadata types instead of overloading plain strings

This keeps the implementation scoped and avoids turning `chatwidget.rs` /
`app.rs` into even larger orchestration files.

### Phase 1 engineering slice: transcript cursor and saved read positions

Goal:

- let the user navigate assistant output line-by-line
- persist read progress
- show branch-related UI scaffolding before true branch semantics land

#### TUI state changes

Add transcript-reading state that can represent:

- currently focused transcript message
- selected rendered line within that message
- stable anchor for the selected position
- saved read position per assistant message
- current thread path/depth metadata for display

Recommended anchor shape:

- thread id
- turn id
- thread item id
- byte offset within agent message text

This is more durable than storing only a rendered row index.

#### Rendering changes

Add assistant-message-specific rendering support so the TUI can:

- resolve the selected stable anchor to a rendered line at the current width
- draw an active line highlight
- draw a saved branch marker/annotation on a line
- leave the rest of the transcript visually unchanged in v1

`wrapping.rs` already provides most of the low-level mapping primitives needed
to convert text offsets into rendered row ranges. The new work is mostly about
binding those mappings to specific assistant transcript cells.

#### Input changes

Extend transcript navigation so that, when the transcript is the active reading
surface:

- Up/Down moves across rendered lines
- branch action captures the selected anchor
- parent-thread action returns to the parent branch location

Exact keybindings remain open, but the state model should support them without
assuming specific keys.

#### Persistence

Persist branch context on the forked thread:

- `forked_from_id` preserves the parent thread id
- `branch_depth` preserves the current nesting depth
- `branch_anchor_head_summary` preserves a short recognition label from the
  beginning of the selected anchor
- `branch_anchor_summary` preserves the short footer label for the selected
  cutoff point

Saved read positions that do not create a branch are still a follow-up. They may
remain TUI-local until the product needs restart-stable reading progress without
branch creation.

The persisted branch context is UI/session metadata only. It should not add the
unread suffix back into model context and should not inject synthetic messages
into the conversation.

### Phase 2 engineering slice: true branch creation and context truncation

Goal:

- branching from a selected line creates a child thread whose visible and model
  history ends at the selected assistant anchor

#### Protocol changes

`ThreadForkParams` should gain a structured optional branch descriptor.

Recommended shape:

- `sourceMessageId: String`
- `sourceRole: enum/string`
- `anchorByteOffset: u32` or `usize`-equivalent wire type
- `includeSelectedLine: bool`
- optional `branchMetadata` payload for machine-readable semantics

This should be modeled as a dedicated type, for example
`ThreadBranchPointParams`, rather than sprinkling new top-level optional
scalars across `ThreadForkParams`.

The wire payload should communicate:

- this is a partial-read branch
- which message the branch comes from
- where truncation occurs
- that unread content after the anchor is excluded

#### App-server changes

`thread/fork` in `codex_message_processor.rs` should:

- load the source rollout/history as it does today
- identify the target assistant message by stable item/message id
- truncate that message to the selected anchor
- discard later items/turns beyond the branch point
- materialize the new forked thread from the truncated history
- preserve branch provenance metadata on the new thread, including the parent id,
  branch depth, short anchor-head summary used by branch navigation, and short
  anchor summary used by the TUI footer

This likely requires a reusable rollout/turn-history truncation helper rather
than embedding branch logic directly inside `thread_fork`.

Recommended extraction:

- add a dedicated history truncation module close to thread-history / rollout
  reconstruction code rather than bloating `codex_message_processor.rs`

#### Context construction

When a user submits inside the already-created branch:

- the app-server should already consider the thread's copied history truncated
  at the branch point
- additional branch metadata should be added so the model explicitly knows:
  - the user branched from a partial read position
  - the assistant reply was interrupted from the user's point of view
  - content after that point was unread and excluded

This avoids relying on transcript shape alone to imply the semantics.

### Phase 3 engineering slice: parent/child navigation and branch affordances

Goal:

- let users move between parent and child threads without losing their place

#### Required state

Track:

- current thread id
- parent thread id
- branch source anchor
- child branches known for a message / anchor

The TUI already knows about fork ancestry via `forked_from_id`, but this is too
coarse for line-level branch navigation by itself. Additional branch-point
metadata is needed in the TUI state layer.

#### Rendering

Add:

- a breadcrumb/header showing current path depth from root
- compact branch markers on transcript lines that have child branches

Avoid transcript indentation for v1 so line wrapping remains stable and the
available reading width stays large.

### Suggested implementation order

1. Add stable assistant-message anchor mapping in the TUI.
2. Add transcript read cursor state and active-line rendering.
3. Add saved read-position persistence and simple branch markers.
4. Add breadcrumb/header thread-path display.
5. Introduce protocol branch-point types on `thread/fork`.
6. Implement app-server history truncation for partial assistant-message forks.
7. Wire TUI branch action to true child-thread creation.
8. Add parent-thread return and branch-location restoration.
9. Add explicit branch metadata to model context construction.

### Testing plan

#### TUI

- Add unit tests for offset-to-rendered-line mapping using `wrapping.rs`
  helpers.
- Add snapshot coverage for:
  - active line highlight
  - branch marker rendering
  - breadcrumb/header depth indicator
- Add key-handling tests in `chatwidget/tests` or `app` tests for:
  - moving the transcript cursor
  - saving/restoring read position
  - entering/exiting a branch

#### Protocol / app-server

- Add protocol serialization tests for the new branch-point params.
- Add app-server tests covering:
  - forking at an assistant message boundary
  - forking mid-message with truncation
  - excluding later turns after the branch point
  - preserving fork ancestry metadata

### Main risks

#### 1. Rendered-line stability

If anchor logic accidentally depends on wrapped row numbers instead of stable
offsets, branch points will drift on terminal resize.

Mitigation:

- make the stable content anchor the canonical source of truth

#### 2. Overgrowth of central TUI files

`chatwidget.rs` and `app.rs` are already large, high-churn modules.

Mitigation:

- extract new branching/read-cursor state into dedicated modules

#### 3. Ambiguity between transcript UI and actual model context

If the transcript visually branches but the server still forks whole-thread
history, the product semantics will be wrong.

Mitigation:

- treat TUI UX and server truncation as separate tracked milestones
- do not claim full partial-read semantics until protocol/app-server work lands

#### 4. Multiple child branches from one message

A single message can accumulate several saved anchors and child branches.

Mitigation:

- keep the first version's UI simple: current-line highlight plus compact local
  branch markers, with richer child-branch discovery deferred

## Questions to resolve with product/design

- What exact keybinding creates a branch from the selected line?
- How should users discover branch navigation and branch depth?
- Is the branch point inclusive of the selected line or exclusive?
- How should copy/select interactions coexist with line highlighting?
- What should happen if the user branches from a message and then the terminal
  width changes before they revisit the thread?
