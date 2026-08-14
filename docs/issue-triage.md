# Issue Triage

This project does **manual, on-demand** issue triage — no scheduled/automated pass. Run
one whenever a milestone is being cut, or whenever the open-issue backlog feels stale
(a good sign: several milestones with due dates in the past, or a cluster of issues
with zero comments sitting untouched for months).

## What a triage pass checks

1. **Obsolete/already-done issues.** Issues can be filed against a design or a piece of
   code that later changed underneath them. For each candidate:
   - Grep the codebase for the specific type/function/file the issue names. If it no
     longer exists (renamed, deleted, refactored away), the issue is likely obsolete.
   - Check whether the issue's proposed approach was actually the one adopted — some
     issues describe an alternate plan that was superseded by a different design
     (e.g. a phased refactor proposal that a later, different refactor made moot).
   - Check the relevant `docs/design-*.md` doc's phase/status table, if one exists —
     it often shows a feature is already finished through the phase the issue targets.

2. **Duplicates.** Two issues chasing the same gap, filed at different times or by
   different review passes (e.g. a manual issue and an automated-review issue covering
   the same thing). Keep the one with more detail/labels, close the other pointing at it.

3. **Stale milestones.** Check `gh api repos/<owner>/<repo>/milestones` for due dates in
   the past — a sign the roadmap hasn't been touched in a while. Re-baseline the
   *next* milestone with a realistic near-term date; clear (don't re-guess) due dates on
   milestones further out — plan those for real when you get closer.

4. **Prioritization.** Group open issues thematically (grep titles/labels for
   patterns) rather than reading all of them individually. Ask two things:
   - Is there a real bug affecting users that isn't in the next milestone?
   - Does the next milestone contain large *new* feature work when the actual need is
     finishing something already in flight (e.g. closing out the last phase of a
     multi-phase design)? If so, swap them.

## Precedent

Closing an issue as obsolete/not-planned should always include a comment with the
evidence (what changed, why the issue no longer applies) — see issues closed with
`stateReason: NOT_PLANNED` for the existing style (e.g. #39, #146, #494).

## History

- 2026-08-14: first full pass. Closed 12 issues (obsolete EventBus-era refactor
  issues, already-implemented items, one superseded design, one duplicate). Reshaped
  `v0.5.0` around bug fixes + finishing the photo-editing MVP (Phase 7) instead of new
  large features. Parked the GNOME Circle-readiness cluster and the "pro editor"
  wishlist (HSL, tone curves, GPU rendering, etc.) in `vBacklog` since Circle
  submission is shelved and editing MVP completion is the near-term priority.
  Re-baselined `v0.5.0`'s due date; cleared stale due dates on `v0.6.0`–`v1.0.0`.
