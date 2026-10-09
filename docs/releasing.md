# Building and releasing mdt

Build from this repository and keep its Cargo target directory local to this
checkout. The Rust toolchain is pinned in `rust-toolchain.toml`. Release builds
use the committed lockfile and identify this repository's exact Git commit.

## Four supported targets

| Download platform | Rust target | Runtime linkage |
| --- | --- | --- |
| macos-arm64 | aarch64-apple-darwin | Apple system libraries |
| macos-x86_64 | x86_64-apple-darwin | Apple system libraries |
| linux-arm64 | aarch64-unknown-linux-musl | Static musl and SQLite |
| linux-x86_64 | x86_64-unknown-linux-musl | Static musl and SQLite |

The macOS targets need macOS and an Xcode SDK. Linux cross-builds use Zig 0.13.0
and cargo-zigbuild 0.23.4. They are build tools and are not needed to run mdt.
A local Python environment can supply them without a system installation:

```sh
uv venv /path/to/cli-tools
uv pip install --python /path/to/cli-tools/bin/python \
  ziglang==0.13.0 cargo-zigbuild==0.23.4
export PATH="/path/to/cli-tools/bin:$PATH"
export CARGO_ZIGBUILD_CACHE_DIR=/path/to/cli-tools/cache
export ZIG_GLOBAL_CACHE_DIR=/path/to/cli-tools/zig-cache
rustup target add aarch64-apple-darwin x86_64-apple-darwin \
  aarch64-unknown-linux-musl x86_64-unknown-linux-musl
script/build-cli.sh
```

The script accepts one or more platform names, or builds all four when none are
named. It writes `target/<Rust target>/release/mdt` and logs under
`logs/cli-builds/`. Set `CARGO_NET_OFFLINE=true` to use only cached dependencies.
A native Linux builder with the matching musl C toolchain can set
`CLI_NATIVE_LINUX=1`.

The Linux downloads require no particular glibc version or SQLite shared
library. `mdt feedback` additionally uses the external `curl` command. macOS
minimum versions are recorded from the actual Mach-O load commands; running on
a current Mac does not establish compatibility with the oldest declared OS.

## Test the actual binaries

```sh
cargo test --locked
python3 -m unittest discover -s script -p 'test_package_cli.py'
python3 script/smoke_cli.py target/aarch64-apple-darwin/release/mdt
```

The smoke test uses temporary source and state folders. It checks exact CRLF and
Unicode reads, changed and deleted text in a diff, retained revision reads,
refusal of a stale section replacement, and a successful guarded replacement.
It performs no network operation or credential lookup.

Use `--runner /path/to/wrapper` for a compatible runner. A Rosetta wrapper can
execute `arch -x86_64 "$@"`; a Blink wrapper can execute `blink -jm "$@"` for
Linux x86-64 emulation on macOS. Blink's memory-safe interpreter was used in
development because its default JIT failed the read probe. Always distinguish
emulated execution from native Linux tests. ARM64 Linux header/linkage checks
alone do not establish execution coverage.

## Package a committed release

Commit the final source before building the release binaries. Packaging refuses
a dirty checkout, a binary from another source commit, a wrong architecture,
a non-system macOS library, or a Linux dynamic interpreter/shared-library
dependency. It also runs `--version` and `--help` where a compatible host or
explicit runner is available.

```sh
python3 script/package_cli.py --version 0.1.0 \
  --sign-identity "$SIGN_IDENTITY" --notary-profile "$NOTARY_PROFILE"
```

`SIGN_IDENTITY` names an existing Developer ID identity and `NOTARY_PROFILE`
names an existing macOS keychain profile. Supply them explicitly; the scripts
do not read local deployment configurations, create credentials, export keys,
or infer authentication settings. Signing and notarization are authenticated
release operations and require authorization. The packager signs staging copies
with hardened runtime and timestamps, verifies their signatures, submits ZIPs
to Apple's notary service, and accepts only an `Accepted` result. It does not
upload downloads or deploy a website.

The final tarballs contain the signed macOS commands. A bare executable cannot
carry a stapled notarization ticket, so first Gatekeeper verification needs
internet access. Linux executables are static ELF files. Both package types
include the exact project license and dependency notices.

For unsigned local inspection, use `--allow-dirty --output dist/review`; this
writes no publishable manifest. `--platform` can limit this inspection to one
platform. An optional `--linux-runner linux-x86_64=/absolute/path/to/runner`
records emulated smoke execution in the build report.

Each archive has exactly these flat members:

- `mdt` with executable mode 0755
- `README.txt` with user-local installation instructions
- `build-info.json` with provenance and verification coverage
- `mdt.1`
- `LICENSE`
- `THIRD_PARTY_NOTICES.txt`

Output defaults to `dist/cli/`:

- `mdt-cli-<version>-<commit10>-<platform>.tar.gz`
- `mdt-verification.json`, including archive and binary SHA-256 checksums
- `mdt-cli.json`, the complete release manifest, only after all four packages pass

The manifest uses schema version 1, `product: "mdt"`, `version`, `source_commit`,
`source_commit_timestamp`, and an `artifacts` object keyed by the four platform
names. Each entry contains `artifact_url` and `sha256`. The default URL base is
the download host used by emmflow.com; override it with `--artifact-base-url`.
Immutable object keys include the version, source commit, archive SHA-256 and
platform. No mutable `latest` URL is embedded.

## Publish and verify separately

Publication needs its own authorized hosting workflow. Prepared manifests do
not prove an upload or a live download. Before activating a release, upload the
archives to the immutable keys in `mdt-verification.json`, fetch each URL, and
compare the downloaded SHA-256 to the manifest. Then activate the complete
manifest on the download website and verify its live source commit and all four
download routes. Report source, uploads and live publication separately.
