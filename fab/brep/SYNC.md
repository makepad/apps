# Staging the solidcore kernel for Fab

From the repository root, run the tool against a local clone of the upstream solidcore repository.
The first kernel pin is `88c4e8eb9e548034e1af2a76f120a5727f2ef5ee`.

```sh
makepad run --unattended makepad-solidsync -- sync --dry-run --no-fetch --pin <sha> --clone <dir>
```

The tool resolves the pin to a commit on the clone's local `origin/main`.
All objects must already be present in the clone. The raw stage lives in
`target/solidsync/<full-commit>/raw/` and uses the upstream directory names.
A repeat run checks the files there against a fresh archive of the same pin.

Pin-specific paths, expectations and shim mappings live in `sync/rules.toml`. Each crate's
`rust_roots` lists its copied Rust source directories. For the kernel, S1 and its
denied-token check cover every Rust file under `src/`. I2 inventories the production
code in those same files. The build script belongs to B1. Its content hash guards
every line, and the dry run lists its capability sites for review.

S1 removes a test function that uses an excluded dependency, fixture, filesystem
read or serde API. It checks each child module the same way, including modules in
separate files. Siblings keep their original source. If a module's own import or
shared helper needs an excluded capability, the whole module needs an entry in
`sync/skip.txt`. Other test-gated items with denied tokens leave individually.
A denied token in a test-gated statement or macro argument stops the sync at the
original location. The run also stops if a denied token survives under `src/`.

S2 follows module declarations, including literal `#[path]` and inline module directories.
A file loses its place in the tree when every route to it goes through a removed declaration.
Unresolved macro-generated modules stay on the review list. A surviving include or an
explicit keep pattern protects a data file. Fixtures outside the copy set that were used
only by removed code are checked against the pinned Git tree.

C1 stages the selected crate sources and manifests, any declared build script and the workspace
files named in the rules. It checks the archive against the pinned Git tree and rejects symlinks.
C2 rejects brand paths and media before they enter the stage. C3 checks each resolved crate
licence and the staged workspace LICENSE and NOTICE.

The denied tokens for S1 are `serde_json`, `flate2`, `include_str!`, `include_bytes!`,
`CARGO_MANIFEST_DIR`, `std::fs`, `fs::read*` and `serde::` paths other than imports or derives of
`Serialize` and `Deserialize`. The dry run names every surviving production occurrence.
Filesystem access also appears in I2. Any other production occurrence stops the run.
When a kept item names a removed test helper, record that caller in a `[[remove]]` entry with
its qualified name, reason and expected count of one. Run again to find any further callers.

When a removal leaves a helper or import unused, record it in an `[[orphan]]` entry, and
name the removed caller in its reason. A `use` item is named by its path up to its leaf or
its group, plus the imported `name`. For example, `tests::convert` with `name =
"draft_rebuild"` stands for `use a::convert::{draft_rebuild, ..}`, and `tests::super` with
`name = "*"` stands for `use super::*`. O1 then looks for a surviving identifier of that
name wherever a reference could still resolve:
- for a private item or a plain `use`, its module and the modules under it;
- for a `pub(super)` item, its parent module and the modules under it;
- for any other `pub` form, the whole crate.

A glob needs its module to be empty after the cuts. A private item outside test code needs
`scope = "production"`; a public one is never an orphan.

Each line of `sync/skip.txt` names the upstream crate, qualified item, kind and reason.
Use `module` for a whole test module and name its import or helper, source line and denied token.
Use `slow` with the measured time and profile, `rustc` with the implicated shim or patch,
`platform` with the affected OS or architecture, or `relax-bug` with the pin for a JIT-only
failure. For `feature`, start the reason with the Cargo feature name. The test or an
enclosing module must require it through `cfg`, and the generated manifest must leave
it off by default. These test skips and `#[ignore]` each remove one test. Every skip entry must match
once. Each named caller removal in `rules.toml` must also give the removed test or helper
path in `uses`. A stale entry stops the sync. An upstream rustc failure on the unpatched
source calls for another pin.

D1 drops the declared native module when the record crate is enabled. S3 keeps the build
script's Markdown inputs. B1 checks those inputs, the script's SHA-256, the output basename
and the generated include in the consumer. For a changed script, read the diff against the
installed script, or inspect the entire pinned script on a first sync. Review any additional
`include!` or `env!("OUT_DIR")` sites. To accept a reviewed change for this sync, pass
`--accept-build-rs <full SHA-256>`. The tool prints the accepted hash. Changing the tracked
hash in `sync/rules.toml` is a separate review decision.

M1 prints the generated Cargo manifest. It resolves inherited workspace settings and checks each
dependency's version, path, features and default-feature setting. Local dependency keys retain
their upstream names so feature forwarding continues to work. The kernel's dependency on the
spade shim leads to `makepad-csg-boolean`, which is independent of the kernel. The serde shim
provides the derives for the in-memory types. The generated library enables its unit tests and
disables doctests. Both production and test configurations must build warning-free when the
kernel is installed.

F1 requires exactly one crate-level `#![forbid(unsafe_code)]`. I1 counts the serde helpers and
thiserror forms in all kept library code, including the tests. I2 counts capability sites in
production library code. The summary shows up to ten original source locations per kind and
writes every site to `target/solidsync/<full-commit>/i2.txt`. A newly observed kind stops the
run for review. The dry run builds the transformed source and manifest in memory.

## Install and verify

Run the dry run first and read its counts, inventory changes and private-scan result.
Remove `--dry-run` to install that result. The clone must contain the requested commit and
its local `origin/main`. Run these commands from the root of a real checkout, not one served through a virtual
filesystem, so the tool and the git processes it starts see the same stage.

```sh
makepad run --unattended makepad-solidsync -- sync --no-fetch --pin <sha> --clone <dir>
makepad run --unattended makepad-solidsync -- verify
```

For a trial install, add `--dest target/solidsync/dest-test` to both commands.
An empty trial destination takes its initial patch queue from `sync/patches/`.
Subsequent trial runs use that destination's own queue.
The tool copies each crate's `src/`, declared `build.rs`, generated `Cargo.toml` and the
workspace's root `LICENSE`, `NOTICE` and `ATTRIBUTION.md`. Other crate files, including
`ci.toml`, belong to Fab. A1 appends the section from `sync/attribution.md` once per crate.
If upstream already has its heading, review the attribution before continuing.

After applying the patches, N1 compares each surviving file with its raw-stage bytes.
Every changed Rust, TOML or Markdown file ends in a single comment naming its rules and
patch numbers. A Rust line uses `//`, TOML uses `#` and Markdown uses an HTML comment.
The notice goes below the existing content to preserve source line numbers. LICENSE
and NOTICE keep their upstream bytes.

The tool private-scans the diff with paths relative to the apps repository. A hit stops
installation. After a clean scan it records the destination, staged replacements and file
hashes in `target/solidsync/<pin>/install.journal`. If interrupted, repeat the install
command. The tool resumes the journal for that destination, checks the result, writes
`summary.txt` beside the stage and removes the journal.

`sync/manifest.txt` records the SHA-256, byte count and destination-relative path of each
owned crate file and patch, sorted by path. Run `verify` to find changed, missing or extra
files. `sync/pin.toml` records the commit, committer date, subject, tool version and inventory
counts. The summary compares inventories, fix flags, manifest settings and file and line counts.

Land a tool or rule change with a re-sync at the installed pin. A same-pin sync installs the
current rules and restores owned source files to the derived bytes. A pin move first derives
the old pin with the current rules and patches, compares it with the installed manifest and
checks the installed files. Resolve any difference at the installed pin before advancing it.

## Behaviour patches

Keep one behaviour fix or hook per `sync/patches/NNNN-name.patch`. The queue runs in filename
order after A1 and before N1. Each header gives its reason, guard test, upstreamability and last
applied pin. Each file entry names a crate-relative path and its before and after hashes. A
dash denotes an absent file. The tool checks applicability, applies the diff and checks both
the changed files and their result hashes.

To prepare a patch, dry-run the pin first. The first save prepares an editable tree under
`target/solidsync/<pin>/work/` with the preceding patches applied. Edit the crate files there
and repeat the save with the review information. Supply `--pin <full-sha>` before the first
install. Later saves default to the installed pin. Move a previous patch's work directory aside
before starting a different patch.

```sh
makepad run --unattended makepad-solidsync -- patch save <NNNN> <name> --pin <full-sha>
makepad run --unattended makepad-solidsync -- patch save <NNNN> <name> --pin <full-sha> \
  --why "<reason>" --guard "<test>" --upstreamable "yes: <note>"
```

When upstream changes a patch's base, the tool takes the installed files from the lane's HEAD,
removes their notices and reverses the installed patches newest first. It checks the recovered
bases and results against the headers, then merges the old change into the new staged files.
A clean refresh is staged with the install. Read each refreshed diff and run its guard test.
Conflicting files are left under `target/solidsync/<pin>/conflicts/` for review.

## Rule table

Run `makepad run --unattended makepad-solidsync -- doc` after changing the rules.

<!-- solidsync doc: begin -->

| Rule | Operation | Expectation |
|---|---|---|
| C1 | Archive the configured root files and crate copy sets. Reject symlinks and compare archive paths to the pinned tree. | At least 1 file per copy path across 1 configured crate(s) |
| C2 | Reject the configured directory names, paths and media extensions. | 0 denied paths |
| C3 | Resolve the crate license and require the legal files in the stage. | Apache-2.0 or MIT OR Apache-2.0 |
| B1 | Check the build script, its inputs and its generated include. | One consumer of `upstream_fixes.rs` and the reviewed script hash `03d5f435fc971cd42b3917bfba3aa7df82d0249380c25eec014fca6699911949` |
| D1 | Remove the configured module declaration in a copied crate. | 1 × `pub mod native;` when `solidcore/crates/solidcore-record` is copied |
| S1 | Strip test items that use excluded capabilities, ignored tests and named callers. Check the surviving references. | At least 1 cut(s) per crate, 42 configured caller(s) each matched once, zero surviving denied tokens |
| O1 | Remove reviewed items and import names left unused by earlier removals. | 11 configured orphan(s), each matched once with zero surviving uses in scope |
| S2 | Remove files whose module or include edges were all removed. | At least 0 deleted files per crate |
| S3 | Preserve files matching the explicit keep patterns. | At least 1 file per pattern |
| S4 | Remove tests named in the skip list for a reviewed runtime, platform, test failure or default-off feature. | Each entry matches once |
| M1 | Generate the local manifest from the crate and workspace manifests. Check sections and dependency mappings. | One manifest per crate, 4 dependency mappings |
| F1 | Check the crate-level prohibition on unsafe code. | Exactly one per library |
| I1 | Count serde helpers and thiserror forms across kept production and test code. | Only the 14 reviewed kinds |
| I2 | Count production capability sites and record their upstream locations. | Only the 3 reviewed kinds |
| A1 | Append the attribution section. Refuse a heading already present upstream. | 1 section per crate from `sync/attribution.md` |
| N1 | Append a language-appropriate last-line notice naming the rules and patches that changed the bytes. | One per changed, surviving file |

<!-- solidsync doc: end -->
