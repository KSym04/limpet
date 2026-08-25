# Contributing to limpet

Thanks for helping. A few terms keep the project clean for everyone,
including a future you.

## License of contributions (inbound = outbound)

By submitting a contribution (a pull request, patch, or any code, docs, or
other material) you agree that your contribution is licensed under the same
[MIT License](LICENSE) that covers the project, and that you have the right
to license it that way. You retain copyright to your contribution; you grant
the project and its users the MIT permissions over it.

This is the standard GitHub inbound-equals-outbound rule
([GitHub Terms of Service, section D.6](https://docs.github.com/en/site-policy/github-terms-of-service#6-contributions-under-repository-license)).
There is no separate contributor license agreement to sign. Do not submit
code you do not have the right to license, code under an incompatible license,
or machine-generated code whose license you cannot vouch for.

## What the bar is

limpet is a trust tool, so the contribution bar is correctness and honesty,
not volume:

- **Every change ships with a test.** New behavior, bug fixes, and edge cases
  all get a test that fails before the change and passes after. `cargo test
  --locked` must be green.
- **No feature ships that cannot show its value or flag its own staleness.**
  See [PHILOSOPHY.md](PHILOSOPHY.md); the decision test there is the review
  test here.
- **Never widen a claim the code cannot back.** The README is guarded by
  `tests/docs_in_sync.rs`; if you add a tool, grammar, or command, the docs
  update in the same PR or the build fails.
- **No secrets, ever**, including in test fixtures. The detector's own
  fixtures are split with `concat!` so scanners do not flag them; keep that
  pattern.
- **Security-relevant changes** (the write/import paths, path validation, the
  updater, the UI server) get extra scrutiny and, where practical, an
  adversarial test.

## When the audit gate blocks a release

CI runs `cargo audit` against a live checkout of the RustSec advisory
database (`rustsec/advisory-db` at `main`, not pinned to a commit). That is
deliberate: a frozen database would have read green straight through the
advisory that put the job there. The cost is that an advisory published
upstream against any crate in `Cargo.lock` can turn `main` and every open
pull request red without anyone here changing a line of code.

Unblock it in this order:

1. **Upgrade out of it.** Bump the dependency, run `cargo test --locked`,
   and commit the `Cargo.lock` change. This is the right answer in most
   cases, and it is the only one that actually removes the vulnerability.
2. **If there is nothing to upgrade to yet**, add the advisory to the
   `ignore` list in [`.cargo/audit.toml`](.cargo/audit.toml), following the
   rules at the top of that file: the advisory ID, one line on why limpet's
   use of the crate is not exploitable (or why the fix cannot land yet), and
   the condition that removes the entry again. An entry with no removal
   condition does not pass review.

Do not reach for `.github/workflows/ci.yml` to skip, soften, or
`continue-on-error` the audit step. That disarms the gate for every future
advisory rather than today's one, and it does it in a diff that reads like a
routine CI tweak. The audit config exists so that an exception arrives as a
narrow, reviewable change with an expiry attached.

## Reporting security issues

Do not open a public issue for a vulnerability. Follow
[SECURITY.md](SECURITY.md).

## Practical

- Discuss anything large in an issue first; a rejected 2,000-line PR helps
  no one.
- Match the surrounding style; the code is deliberately dependency-light and
  free of `unsafe`.
- If you add a file the build reads (a new `include_str!` target, a new
  source directory), add it to the `include` allowlist in `Cargo.toml` in the
  same change. That list is the published crate's whole contents, and no CI
  job packages the crate, so a missing entry does not surface until
  `cargo publish` runs on a release tag. Check with
  `cargo package --list --allow-dirty`.
- Keep commits focused and messages plain about the "why".
