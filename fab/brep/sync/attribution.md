
## References in comments

This crate is a copy of solidcore's source. Its comments cite material that lives in
solidcore's repository and is not part of this crate:

- `plan/modules.toml`, `plan/findings.md`, `plan/findings/`, `plan/upstream-issues.md` and
  `plan/upstream-issues/` (the port plan, findings and records of upstream defects);
- issue and pull request numbers (`#1023`, `#1440`) of solidcore's issue tracker;
- record ids (`WR-`, `UB-`, `HANG-`, `DOC-`) of those upstream-defect records;
- the goldens crate, the C++ harness and the corpora named above.

The tables above describe solidcore's repository, not the files shipped here. Tests that
need files or dependencies this copy does not carry were left out by the sync tool;
`fab/brep/SYNC.md` lists its rules.
