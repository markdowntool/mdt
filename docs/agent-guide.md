# Using mdt as an agent

mdt is designed to let agents and humans use a potentially large Markdown file
as a shared space for communication and shared state. The file stays ordinary
Markdown that a human can edit with their preferred editor.

Check `command -v mdt` and `mdt --version` before using it. If installation is
needed, follow the [user-local installation instructions](../README.md#install-for-your-user):
install in `~/.local/bin` without `sudo`, identify the user's shell, update PATH
for the current session, and use the appropriate startup file for persistence.
Check existing configuration before adding an entry so it is not duplicated.
Verify the resolved executable path and version again after installation. A
shell's current-session PATH change does not automatically update an already
running agent process; that process may need the same PATH or the full binary path.

Start with `mdt --help`; use `mdt COMMAND -h` for syntax and `man mdt` for meanings.
Read saved source with `read`; compare versions with `diff --since REV`. Keep your
chosen baseline until all wanted pages are processed. Revisions never mean that
you have read every section.

Use `outline` for structure or `find FILE TEXT` for exact occurrences. Repeat
`--node LABEL` to disambiguate ancestry, or use a returned target with its `--rev`.
Use JSON for exact source; follow `next_cursor` with only `--cursor CURSOR` until
`complete` is true. A continuation pins source and output options even if the live
file changes. See [the complete CLI guide](cli.md) for fragment fields.

For a short correction, use `replace FILE --rev REV --old TEXT --new TEXT`.
To replace a whole section, first read its complete source, preserve its children,
then use `--node ... --body FILE|-`. Input is exact; supply all intended newlines.
`insert --before TARGET|--after TARGET --body FILE|-` adds exact source beside the
whole range; `D` is the document. There is no implicit replace-all or list renumbering.

Concurrent changes elsewhere can coexist with your edit. Receipts give the
immediate `previous` revision and resulting `revision`, plus a concise notice of
other changes. `diff --since BASE --rev PREVIOUS` inspects only intervening edits.
`--force` permits a changed target but never guesses its identity. A forced
pre-write source remains retained for inspection. `--dry-run` reserves nothing.

After a missing receipt, inspect with `read` or `diff` before retrying an insertion.
After `revision_unavailable`, read a fresh current view; do not pretend an expired
baseline had no differences. History is bounded local observation, not a backup.

Emm's `issue description` operations use this same engine. Description revisions
are separate from Emm's existing whole-issue title/description edit revision.
