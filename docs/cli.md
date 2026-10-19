# mdt — Markdown tool

`mdt` 0.1.0 reads, compares and edits saved Markdown without starting Emm.
Linux and macOS packages contain the headless command only. Emm's
`issue description` commands reuse the same parser and engine for saved descriptions.

```
mdt read FILE [--rev REV] [--node LABEL]... [--target TARGET]
mdt diff FILE --since REV [--rev REV] [--node LABEL]... [--context-words N]
mdt find FILE TEXT [--rev REV] [--node LABEL]... [--context-words N]
mdt outline FILE [--rev REV] [--node LABEL]...
mdt replace FILE --rev REV [--node LABEL]... --old TEXT --new TEXT
mdt replace FILE --rev REV (--node LABEL... | --target TARGET) --body FILE|-
mdt insert FILE --rev REV (--before TARGET | --after TARGET) --body FILE|-
mdt history FILE
mdt forget FILE [--dry-run]
```

All operations accept `--max-bytes N` (default 8192) and `--json`. Reads, diffs,
find results, outlines and history continue with `mdt COMMAND FILE --cursor C`.
The cursor retains JSON/text mode, budget, selection, position and both revision
endpoints. Supply no other options with a continuation. A changed live file cannot
change the comparison being delivered. An expired required version or cursor is
`revision_unavailable`, never a comparison against guessed source.

`mdt -h` and `mdt COMMAND -h` document syntax without touching files or state.
`--help` is identical. `--version` identifies the independent tool version and
source build. `feedback` retains the existing explicit feedback service and its
permissions; see `mdt feedback -h`. There is no pager or prompt.

## Versions and exact source

A revision binds the source identity, Markdown profile and exact UTF-8 bytes.
It records neither an agent identity nor a reading position. `--since` always
means a revision, never a datetime. The caller retains its baseline until all
wanted source or differences have been delivered and processed.

`read` returns `revision`, `scope`, `target`, exact `content`, `complete`, and
`next_cursor`. Concatenate content across pages. A section read includes its
heading, children, indentation, whitespace and final newline exactly as saved.
A partial source preview is not a complete replacement body.

```sh
mdt read note.md --node Plan --json |
  jq -ej 'if .complete then .content else error("more source pages remain") end' > plan.md
```

Only use `plan.md` after that pipeline succeeds. No raw/export command is needed.

`--node` selects a literal heading or list-item label. Repeat labels for successive
ancestry. Missing or duplicate selections are errors. `outline` returns `nodes`
with paths and revision-bound targets; `D` names the complete document. `find`
returns `match_count` and `matches`; each match's `content` and target describe
only the exact occurrence, with separate `context_before` and `context_after`.
Find includes overlapping matches, is case-sensitive, and succeeds with no matches.
A target must be used with its `--rev`; it is not a permanent ID in Markdown.

## Differences and bounded delivery

`diff --since A --rev B` reports net additions, deletions and replacements. Omitting
`--rev` observes current saved source. A named scope is selected on both sides so
removed sections remain readable. A→B→A has no net difference; history may still
show the intermediate observed version.

JSON differences include `from`, `revision`, `total`, `changes`, `complete`, and
`next_cursor`. Each change has exact `before`/`current` excerpts, a path, absolute
`before_start`/`current_start` byte offsets, and flags for omitted unchanged context.
Large changed source is fragmented, never discarded as context. `item_index`,
`before_offset`, `current_offset` and `change_complete` identify fragments of a
single change; concatenate each side in delivery order. The same fragment fields
apply to long find results. `complete` says whether the query was delivered, not
whether anyone read or understood it.

Short blocks are supplied whole when combined before/current source fits 1 KiB.
Otherwise the default is up to 12 unchanged words either side with a byte cap.
`--context-words N` requests a different word allowance; the byte caps still hold.
Labels and omission notices are presentation; JSON source has no invented ellipses.
Exceptionally long paths are visibly truncated as metadata; their targets still
address exact source. Use a target rather than a truncated path as a selector.

The response cap includes serialized JSON escapes, metadata, newline and text
notices. Oversized single lines, paragraphs, URLs and code blocks cannot bypass it.
Too-small envelopes fail with `budget_too_small`. Write receipts are compact and
never paginated; the largest possible receipt is checked before a write.

## Writes and concurrency

Writes require `--rev`, exact literal `--old/--new`, or a selected range with
`--body FILE|-`. stdin and files are accepted without trimming, dedenting, newline
insertion or list renumbering. Empty replacement deletes; empty insertion is a
successful no-op. `before D` is the start; `after D` is the end. A section anchor
includes its entire subtree.

```sh
mdt replace note.md --rev REV --old 'Cap' --new 'Cape'
mdt insert note.md --rev REV --after D --body appendix.md
```

Resolve the intention at the supplied baseline. An unchanged uniquely located
target can be edited while surrounding text changes. A newly duplicated literal
is refused. Named or exact targets are never rebound to an unrelated section.
A changed target returns `conflict`; `--force` permits overwriting a changed
uniquely located target or inserting beside that anchor. It does not restore
missing literals or choose among duplicate headings. Ambiguous moves are refused.

<!-- Reporting revision: Codex · GPT-6 (exact variant unavailable) · session 01a15578-ff67-77e2-90a9-ffccd1c679b3 · 2026-10-19 -->
If a target was valid at the baseline but cannot be resolved in changed source,
the error says the source changed and includes `current_revision` for the retained
bytes. Missing and ambiguous targets keep their usual exit codes (3 and 4).
An invalid baseline selection remains a selection error without that context.
Both plain-text and JSON errors show `current_revision` when available; inspect
it with `mdt read FILE --rev REV` or `mdt diff FILE --since BASE --rev REV`.

Receipts contain `base`, `previous`, `revision`, `matched`/`changed` counts (or
`anchors`/`inserted`), and bounded `other_changes` paths. Plain-text notices go to
stderr; JSON notices are fields. `previous` is retained before any mutation,
including a forced overwrite. Inspect intervening changes with the ordinary diff:

```sh
mdt diff note.md --since BASE --rev PREVIOUS
```

`--dry-run` reports `would_change` and changes no Markdown; it reserves nothing.
The actual write rechecks against the live source and refuses contention. A no-op
keeps the revision. A lost write receipt is an unknown outcome; inspect current
source before retrying an insertion. A post-save retention failure returns a
successful write receipt with `retention_warning`, never a misleading failed save.

Cooperating Emm Notes and mdt writers share the same source gate. If the final
pre-write check sees different bytes, mdt refuses the write (exit 9), retains
that exact observed version, and reports its `current_revision`. If retention
fails, the error says so and omits the revision; no source write committed.
That version may already have been superseded by another save when the error
arrives. MDT reports observed bytes, not which editor or person wrote them.

Tools which ignore the gate can still race a file's final read and atomic rename;
unobserved changes cannot be detected or retained. Writers using different state
directories also do not coordinate. Issue-description adapters use an atomic
database compare-and-swap instead of the file gate.

## Local retention and configuration

Emm observations, mdt versions and continuations share `~/.emm/notes/updates.sqlite3`.
There is no separate snapshot directory, `--cache-dir` or agent checkpoint.
`history` lists retained observations newest first, with human UTC timestamps;
these are observation times, not guaranteed edit times. Unobserved intermediate
saves may be absent. Use `read --rev` for source and `diff` for comparisons.

Retention is bounded: 30 days plus an earlier baseline where capacity permits,
2048 observations per document, 16384 total and 256 MiB compressed source.
Continuations retain only query metadata, capped at 4096 records/16 MiB/30 days.
Retention is local convenience, not a backup promise. `forget` removes this
source's versions and continuations, never the source; `forget --dry-run` reports
without removing them. A normal read establishes a fresh view after expiry.

`EMM_HOME` replaces `~/.emm`; relative values resolve from the working directory
and an empty value means that directory. Its `notes` subdirectory matches Emm.
`QM_STATE_DIR` is the retained compatibility name for an absolute isolated CLI
state directory. Empty or relative values are refused. It overrides `EMM_HOME`
for mdt but does not relocate Emm Notes. Use isolated note copies for isolated tests.

## Exit status

| Code | Meaning |
|---|---|
| 0 | Success, including no matches in find, no changes, and no-op writes |
| 2 | Invalid arguments or response budget too small |
| 3 | Source, selection or literal replacement match not found |
| 4 | Ambiguous selection or edit |
| 5 | Selected target changed; no write committed |
| 6 | Required retained revision, target or continuation unavailable |
| 9 | Write/storage contention; no source write committed |
| 10 | Storage or I/O failure |

Failed JSON commands emit a bounded `{"error":{"code":...,"message":...}}`
object on stderr and no success result on stdout. JSON continuations preserve
that format even after their query metadata expires.
