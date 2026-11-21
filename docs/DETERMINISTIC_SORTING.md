# Deterministic Sorting for Mixed-Status Records and Events (Issue #284)

`shared::deterministic_sort` provides total-order, `no_std`-compatible sorting for mixed-status records (`SortableRecord`) and events (`SortableEvent`).

## Priority & Tie-Breaking Rules

`RecordStatus::priority_rank` establishes a canonical operational order:
`Failed (0) < Pending (1) < Active (2) < Paused (3) < Settled (4) < Cancelled (5) < Archived (6)`.

Every `SortPolicy` (`StatusPriorityFirst`, `TimestampAsc`, `TimestampDesc`, `LedgerSeqAsc`) terminates in a strict tie-breaking chain (`tie_breaker`/`tx_index`, then `id`/`event_id`), guaranteeing identical output regardless of input permutation.

## Edge Cases & Recovery / Rollback Behavior

- **Bounded Gas**: Batches exceeding `MAX_SORT_ITEMS` (`200`) are rejected with `Error::InvalidArgument` before allocation/sorting.
- **Duplicate IDs**: When `strict_unique` is `true`, duplicate primary `id`s are rejected with `Error::InvalidArgument`.
- **Idempotency**: Re-sorting an already sorted output produces the exact same sequence.
- **Visibility & Authorization**: `sort_events_for_viewer` reuses `EventVisibility`. Non-admin callers requesting `include_maintainer_only = true` receive `Error::Unauthorized`; with `false`, `MaintainerOnly` events are excluded.
- **Read-Only Purity**: Sorting functions perform no persistent storage mutations, so failed calls leave zero partial state and require no rollback.

## Contract Preservation

This module is additive in `shared` (`shared::deterministic_sort`) and reuses `shared::pagination::EventVisibility` without modifying existing pagination or timeline contracts.
