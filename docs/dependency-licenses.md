# Dependency license audit

This audit accompanies mdt 0.1.0, its checked-in `Cargo.lock`, Rust 1.98.1, and
the four macOS/Linux release targets. It records the source evidence used to
prepare redistribution notices. It does not change any dependency's license.

## Cargo dependencies

[`dependency-licenses.json`](dependency-licenses.json) was generated from actual
`cargo metadata --locked --offline` output, filtered for each release target.
Traversal includes normal and build dependencies, including procedural macros;
development-only dependencies are excluded from the release set.

- 108 registry packages appear in the complete resolved metadata.
- 52 external packages are in the union of the four release dependency graphs.
- The workspace's only first-party packages are `mdt` and `mdt-core`.
- Every release dependency has license text in its downloaded registry source.
- Each release dependency offers MIT or Apache-2.0 terms, or is Zlib-licensed.
  `unicode-ident` additionally requires Unicode-3.0. `similar` is Apache-2.0.
- The JSON records the selected permissive branch, original license expression,
  target membership, source URL, and SHA-256 of each included notice file.

The release graph has no required copyleft license. In particular, the optional
LGPL branch in the complete lock metadata is not selected. Packages used only
for tests or other architectures remain inventoried with an empty target list;
their source is fetched by Cargo under their own declared terms and is not
vendored or included in mdt's binary archives.

[`THIRD_PARTY_NOTICES.txt`](../THIRD_PARTY_NOTICES.txt) preserves the original
license and notice files for all 52 release dependencies. Alternative license
texts are retained even where one permitted branch is selected. Keep this file
and the first-party [`LICENSE`](../LICENSE) with redistributed binary archives.
Apache-2.0 includes a patent grant and its own conditions; the first-party MIT
license does not replace those terms.

## Native and language runtimes

Cargo metadata does not describe all code supplied by the compiler and linker.
The combined notices therefore also include:

- SQLite 3.51.3's source copyright disclaimer, from the exact amalgamation in
  `libsqlite3-sys` 0.37.0. Linux bundles SQLite; macOS uses system SQLite. The
  SQLCipher feature is disabled and SQLCipher source is not redistributed.
- Rust 1.98.1's complete `COPYRIGHT-library.html` text and the MIT, Apache-2.0,
  BSD-2-Clause, Unicode-3.0, and LLVM exception texts from that toolchain. The
  upstream inventory is deliberately broader than any one executable: it also
  lists optional components and unsupported targets, including the MPL-2.0
  Fortanix SGX ABI, which these mdt release targets do not use.
- The musl copyright and contributor notices from Zig 0.13.0's bundled musl
  tree, plus Zig's runtime MIT license and LLVM libunwind's Apache-2.0 license
  with LLVM exceptions. These are included conservatively for Linux builds
  using the documented cross-build toolchain.

Linux packages contain static executables. Rust supplies self-contained C and
unwinding archives for these targets; Zig also supplies compilation/linking
support. This audit preserves both toolchains' applicable runtime inventories
rather than claiming every listed runtime object is linked. macOS system
libraries are linked dynamically and are not copied into the archives.

No third-party implementation source is vendored in this repository. The
combined notices contain upstream authors' copyright statements and contact
details as required attribution, not project-user data.

## Refresh and review

After a dependency or toolchain change, fetch the lockfile dependencies and run:

```sh
python3 tools/update_notices.py --zig-root /path/to/zig-0.13.0
```

The generator fails when a release dependency has no root notice file or a
license expression outside the reviewed choices. Review changed native features
and runtime versions separately; metadata cannot detect a new bundled C library.
Regeneration is offline and writes no local paths into its output. Its lockfile
hash and original notice hashes make the snapshot auditable. Packaging includes
the exact checked-in notice file, and the artifact tests check its presence.
