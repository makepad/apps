# Staging the solidcore kernel for Fab

From the lane worktree root, run the tool against a local SolidCraft clone.
The first kernel pin is `88c4e8eb9e548034e1af2a76f120a5727f2ef5ee`.

```sh
makepad run --unattended makepad-solidsync -- sync --dry-run --no-fetch --pin <sha> --clone <dir>
```

The tool resolves the pin to a commit on the clone's local `origin/main`.
All objects must already be present in the clone. The raw stage lives in
`target/solidsync/<full-commit>/raw/` and uses the upstream directory names.
A repeat run checks the files there against a fresh archive of the same pin.

| Rule | Operation | Expected result |
| --- | --- | --- |
| C1 | The kernel's `src/`, `Cargo.toml` and `build.rs`, plus `Cargo.toml`, `LICENSE`, `NOTICE` and `ATTRIBUTION.md` from the `solidcore/` workspace root. The crate-local licence files are symlinks. | Each selected path exists. Every selected Git file appears in the archive exactly once. A symlink or unexpected entry type stops staging. |
| C2 | Check each path for `assets/` (including `assets/branding` and `assets/icons/app`), `crates/look/`, `LICENSE-BRAND` and files ending in `.svg`, `.png`, `.ico`, `.icns`, `.ttf` or `.otf`. | Zero matches. An error identifies the rule and path. |
| D1 | When the record crate is enabled, drop the top-level line `pub mod native;` in its `src/lib.rs`. Pass that declaration to S2 as a removed edge. | Exactly one declaration. |
| S1 | Strip the construct attached to an outer `#[cfg(test)]` or `#[cfg(all(test, …))]`. | At least one removal per crate and zero surviving positive test-cfg tokens. |
| S2 | Trace module declarations from `src/lib.rs`, following both the kept and removed declarations. Check the targets of `include_str!` and `include_bytes!`. | Print the deleted files, files kept for review, and the disposition of include targets. A missing include fails the run. |
| S3 | Keep the kernel build script's inputs matching `src/kernel/fixes/*.md`. | At least one match. |

## Stripping test code

The tool reads each crate's edition from its manifest, resolving workspace
inheritance against the staged workspace manifest. It lexes each Rust file
once. S1 uses the tokens and their delimiters to identify items, associated
items, statements, fields, enum variants, match arms and parameters. A
removed element takes its outer attributes, outer doc comments, and any
terminating comma or semicolon that belongs to it. A block-bodied match arm
can end at its closing brace. The count is the number of separate spans
removed. Removing an entire test module counts once.

S1 copies the bytes between those spans verbatim, including whitespace,
comments, literals, and code guarded by `cfg(not(test))` or
`cfg_attr(not(test), …)`. An unsupported test predicate, a test attribute in
a macro token tree or an unclassified attachment stops the run. Diagnostics
give the source location and surrounding lines for each refusal.

## Finding test-only files

S2 uses the original declarations and S1's byte ranges. Ordinary children
of `x.rs` live below `x/`. Children of `mod.rs` live beside it. A literal
`#[path]` uses the declaring file's directory, or the directory of its
enclosing inline module. Paths must stay inside the crate's `src/`.
When a removed declaration was the only way into a module, its descendants
are test-only too. Any kept declaration protects the file. A Rust file with
no known route from the crate root stays in the review list.

For data files, a kept include or an explicit keep pattern protects the
target. A file referenced only by removed code can be deleted. Other data
files stay for review. The kernel's tests also use `concat!` with
`env!("CARGO_MANIFEST_DIR")` to name fixtures outside the stage. S2 checks
those exact paths in the pinned Git tree and reports them separately.
All source changes and deletions in the dry run take place in memory.
