# The limpet philosophy

One sentence governs everything:

> **A memory about code is only worth keeping if the system can tell you when
> it stops being true.**

Storage is trivial; everything stores. Knowing when what you stored went bad
is the product. Every principle below earned its place by catching a real
bug, killing a bad design, or surviving an audit of this very codebase. A
principle without a scar is a slogan, and slogans do not review pull
requests.

## 1. Not all knowledge deserves equal trust

Retrieval by similarity alone is dangerous: a memory that is 95% relevant
and six months wrong is worse than no memory at all. limpet ranks recall by
trust signals (confidence, freshness, proximity, evidence), flags what it
cannot vouch for, and drops confidence the moment anchored code changes.
Trust is earned continuously, never declared.

*Scar:* the stale-confidence penalty was found compounding on every tool
call, collapsing stale memories to the floor within minutes. Trust must
decay for a reason, exactly once per reason.

## 2. Honesty is architecture, not tone

Every response carries the honesty envelope: what matched, what was
returned, what was omitted and why, how fresh the index is, how much of the
answer is stale or contradicted. Negative savings render as negatives. A
failed sweep says `sweep_failed` instead of pretending freshness. The
ledger ships its methodology in every payload so the number can be checked
rather than believed.

*Scar:* the benchmark's regression gate killed our own ledger feature when
a 2-int receipt on the recall wire dropped savings below the gate. A system
that will fail its own features on principle is one you can trust with your
knowledge. And the relevance floor was rewritten when an audit caught it
contradicting its own doc comment: flagged items are never score-hidden,
now enforced, not promised.

## 3. Degrade, never drop

A grammar can only upgrade a file, never remove it from the index. Source
that cannot be decoded gets a file-level anchor instead of vanishing. A
memory that loses one anchor of several degrades to `anchor_lost` instead
of dying. Files that disappear in a branch switch resurrect their memories
when they return; only a deliberate human supersession is final.

*Scar:* v0.4.0 silently skipped 73% of a real template-and-styles-heavy web project and killed
multi-anchor memories over one unresolved anchor. Every "X silently
disappears" audit finding since has been a violation of this principle.

## 4. Guards, not promises

"Restart your client" printed to a terminal is a wish; a version-stamped
store that refuses writes from a stale binary image is a guarantee.
"Don't commit secrets" is advice; a detector that runs on every write path,
including imports from teammates, is a property. Every anchor must resolve
at write time or the write fails loudly; no memory is ever born dead.

*Scar:* `import` was discovered to be a second, unguarded write path that
skipped the secret check, the confidence clamp, and the size bounds that
`remember` enforced. Any invariant enforced on one path and assumed on
another is not an invariant.

## 5. Every claim carries its receipt

The 4.1x benchmark is reproducible and states its own biases. The ledger
prices every recall against its file-reading counterfactual with the same
arithmetic, undercounting on purpose: anchorless memories claim zero
savings, negatives are shown, repeat queries are disclosed as gross versus
distinct. Verified facts carry the command that proved them, and hand it
back when their code changes.

*Scar:* none yet, by design. The receipt discipline exists so that the day
someone challenges the numbers, the answer is "run it yourself" rather
than a defense.

## 6. The tool lives under its own law

limpet's knowledge about limpet is stored in limpet. Its memories went
stale when we edited the functions they anchor to, were caught by
`affected` before commits, and were superseded with evidence, release
after release. The staleness engine has flagged its own development
dozens of times, including edits made while fixing the staleness engine.

*Scar:* the version guard was built after limpet's own updater created the
exact stale-image race it now refuses, and the guard's first catch was our
own session.

## The decision test

Every proposed feature answers one question:

> Does this make stored knowledge more trustworthy, or make its value more
> visible?

Snippet retrieval and rework-avoided metrics pass. Stability contracts
pass. Becoming a code search engine or a call-graph oracle fails, no matter
how adjacent it looks; the "what limpet is not" section of the README is
this test applied to whole categories.

## The humility clause

Code-anchored trust scoring is a hypothesis, not a fact: the claim is that
knowing when knowledge went stale improves long-lived AI-assisted
development. The benchmark, the ledger, and the eval gates exist to test
that hypothesis against reality, and any part of the model that reality
contradicts gets superseded the same way a stale memory does, with the
history kept.

Features are copyable. This discipline is the moat.
