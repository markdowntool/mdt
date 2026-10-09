# mdt — Markdown tool

mdt is designed to let agents and humans use a potentially large Markdown file
as a shared space for communication and shared state.
It shows changes relative to a retained revision, helping agents avoid
unnecessary rereading, token use, and distraction. We created it because agents
we asked reported that rereading large files was their biggest frustration.

Read, compare, and safely edit saved Markdown from a terminal or an agent.
`mdt` preserves exact source text and gives each observed version a revision.
Reads and comparisons can be paged within an output budget. Edits identify their
baseline so unrelated concurrent changes can survive while ambiguous or changed
targets are rejected.

```sh
mdt read notes.md --json
mdt outline notes.md --json
mdt find notes.md "a phrase" --json
mdt diff notes.md --since REVISION --json
mdt replace notes.md --rev REVISION --old "a phrase" --new "a correction" --json
```

Use the revision returned by a read as `REVISION`. A continuation needs only
`mdt read notes.md --cursor CURSOR` (or the original command with its cursor).
The cursor pins the source versions, selections, and output choices. Use
`--dry-run` to inspect a proposed edit and `history` to inspect retained versions.
Retained history is bounded local observation, not a backup.

## Documentation

- [CLI guide](docs/cli.md): commands, selectors, revisions, paging, and errors.
- [Agent guide](docs/agent-guide.md): safe read, compare, and edit workflows.
- [Manual](docs/mdt.1): install as `mdt.1` to use `man mdt`.
- [Release guide](docs/releasing.md): build and package the four supported targets.
- [Dependency licenses](docs/dependency-licenses.md): resolved dependency and runtime notices.

Run `mdt --help` or `mdt COMMAND --help` for command syntax. The command set is
`read`, `diff`, `find`, `outline`, `replace`, `insert`, `history`, and `forget`.
`feedback` explicitly sends supplied feedback to the existing feedback service;
all other commands stay local. See `mdt feedback --help` for what is sent.

## Install for your user

Choose the archive for your operating system and CPU, extract it, and run these
commands from the extracted directory. No administrator access or `sudo` is needed.

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 ./mdt "$HOME/.local/bin/mdt"
```

Before changing shell configuration, identify the shell you use:

```sh
printf 'Login shell: %s\n' "$SHELL"
```

The configured login shell can differ from the shell running now. In bash or
zsh, `ps -p $$ -o comm=` identifies the current shell process; in fish, use
`status fish-path`.

For **zsh or bash**, make the directory available in the current session without
adding a duplicate PATH entry:

```sh
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) export PATH="$HOME/.local/bin:$PATH" ;;
esac
```

To keep it available in new terminals, put that block in the appropriate shell
startup file only if an equivalent entry is not already there: `~/.zshrc` for
interactive zsh, `~/.bashrc` for interactive bash, or `~/.bash_profile` when your
terminal starts a bash login shell. If the login profile already loads
`~/.bashrc`, keep the PATH block in `~/.bashrc` alone. For **fish**, run
`fish_add_path "$HOME/.local/bin"`; it updates the current session and persists
the entry without duplicates. Other shells need their own PATH syntax.

Verify that your shell resolves the installed command and its version:

```sh
command -v mdt
mdt --version
```

The path should be your user's `.local/bin/mdt`. If another installation appears
first, inspect your PATH order before continuing. When building from source,
use `target/release/mdt` instead of `./mdt` in the installation command.

## Build and test

Install [Rust](https://rustup.rs/). The checked-in toolchain file pins the compiler.

```sh
cargo build --locked --release
cargo test --workspace --locked
cargo fmt --all --check
python3 -m unittest discover -s script -p 'test_*.py'
python3 script/smoke_cli.py target/release/mdt
```

The executable is `target/release/mdt`. Release targets are macOS and Linux on
ARM64 and x86-64. Linux release builds use musl and bundle SQLite; macOS links
the system SQLite library. Cross-build prerequisites and artifact checks are in
the release guide. Windows is not currently supported.

GitHub Actions runs the checks and release builds on native macOS and Linux
runners for both architectures. Its downloadable artifacts are unsigned test
builds; signing credentials are not used in CI.

Feedback tests bind an isolated loopback HTTP server without contacting the
production service. One macOS full-disk test is ignored by default because it
mounts a temporary disk image; its source documents the explicit invocation.

The workspace contains the `mdt` command and reusable engine, plus `mdt-core`
with the Markdown parsing and atomic file helpers it needs. The public engine
accepts a `Source` adapter for saved text held somewhere other than a file.

## Local state

For compatibility with existing observations, default state is
`$HOME/.emm/notes/updates.sqlite3`. `EMM_HOME` changes the parent to
`$EMM_HOME/notes`. Set `QM_STATE_DIR` to an absolute directory to isolate all CLI
history and file gates; that environment variable retains its established name.
Revision hash domains and shared database formats also retain their established
values. No account is needed for local operations.

## License

The first-party source in this repository is available under the [MIT license](LICENSE).
Dependencies retain their own licenses and notices. Release archives include
[THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt), covering the resolved release
dependency graph and the runtime notices described in the dependency audit.
