-- Retry/backoff support for the push outbox.
--
-- Before this migration, failed rows were retried every push cycle forever
-- with no diagnostic. We now track:
--   * attempts        — how many times push has tried this row.
--   * last_error      — truncated error message from the most recent failure.
--   * next_attempt_at — epoch seconds; row is eligible when now >= this.
--
-- The status column gains a fourth terminal state:
--   0 = Pending, 1 = Done, 2 = Failed, 3 = DeadLetter.
-- DeadLetter rows are never retried automatically. They can be removed via
-- the "Clear dead letters" preferences action.

ALTER TABLE sync_outbox ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sync_outbox ADD COLUMN last_error TEXT;
ALTER TABLE sync_outbox ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;

-- Replace the status-only index with a composite that supports the new
-- fetch_pending predicate (status IN (0, 2) AND next_attempt_at <= now).
DROP INDEX IF EXISTS idx_sync_outbox_status;
CREATE INDEX idx_sync_outbox_status_next ON sync_outbox(status, next_attempt_at);
