//! Deterministic sorting for mixed-status records and events (Issue #284).
//!
//! When records and events span multiple lifecycle statuses (`Pending`, `Active`,
//! `Failed`, `Paused`, `Settled`, `Cancelled`, `Archived`) and arrive out of
//! order or share identical timestamps/ledger sequences, naive sorting can
//! produce unstable or non-deterministic results across nodes and retries.
//!
//! This module provides:
//! - Canonical [`RecordStatus::priority_rank`] ordering for mixed-status datasets.
//! - Total-order comparators (`compare_records`, `compare_events`) with a strict
//!   tie-breaking chain (`primary_key -> timestamp -> ledger_seq -> id/tx_index`).
//! - Bounded, `no_std`-compatible insertion sort (`MAX_SORT_ITEMS`) to prevent
//!   unbounded gas consumption.
//! - Visibility- and authorization-aware event sorting (`sort_events_for_viewer`)
//!   reusing [`crate::pagination::EventVisibility`].

use core::cmp::Ordering;
use soroban_sdk::{contracttype, Address, Env, Vec};

use crate::auth::require_admin;
use crate::errors::Error;
use crate::pagination::EventVisibility;

/// Maximum number of items accepted in a single deterministic sort call.
pub const MAX_SORT_ITEMS: u32 = 200;

/// Lifecycle status of a record or event for mixed-status sorting.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecordStatus {
    Failed,
    Pending,
    Active,
    Paused,
    Settled,
    Cancelled,
    Archived,
}

impl RecordStatus {
    /// Canonical operational priority rank (lower rank sorts first in `StatusPriorityFirst`).
    ///
    /// Order: `Failed (0) < Pending (1) < Active (2) < Paused (3) < Settled (4) < Cancelled (5) < Archived (6)`.
    pub fn priority_rank(self) -> u8 {
        match self {
            RecordStatus::Failed => 0,
            RecordStatus::Pending => 1,
            RecordStatus::Active => 2,
            RecordStatus::Paused => 3,
            RecordStatus::Settled => 4,
            RecordStatus::Cancelled => 5,
            RecordStatus::Archived => 6,
        }
    }
}

/// Sorting policy for mixed-status collections.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SortPolicy {
    /// Operational priority first (`Failed`, `Pending`, `Active`, ...), then timestamp asc, then id asc.
    StatusPriorityFirst,
    /// Chronological ascending (`timestamp` asc, `ledger_seq` asc, status rank asc, `id` asc).
    TimestampAsc,
    /// Reverse chronological (`timestamp` desc, `ledger_seq` desc, status rank asc, `id` asc).
    TimestampDesc,
    /// Ledger sequence ascending (`ledger_seq` asc, `timestamp` asc, status rank asc, `id` asc).
    LedgerSeqAsc,
}

/// A mixed-status record with deterministic tie-breaking fields.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SortableRecord {
    pub id: u64,
    pub status: RecordStatus,
    pub timestamp: u64,
    pub ledger_seq: u32,
    pub tie_breaker: u64,
}

/// A mixed-status event with visibility classification and deterministic tie-breakers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SortableEvent {
    pub event_id: u64,
    pub status: RecordStatus,
    pub timestamp: u64,
    pub ledger_seq: u32,
    pub tx_index: u32,
    pub visibility: EventVisibility,
}

/// Total-order comparator for [`SortableRecord`].
pub fn compare_records(a: &SortableRecord, b: &SortableRecord, policy: SortPolicy) -> Ordering {
    let primary = match policy {
        SortPolicy::StatusPriorityFirst => a
            .status
            .priority_rank()
            .cmp(&b.status.priority_rank())
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.ledger_seq.cmp(&b.ledger_seq)),
        SortPolicy::TimestampAsc => a
            .timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.ledger_seq.cmp(&b.ledger_seq))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
        SortPolicy::TimestampDesc => b
            .timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.ledger_seq.cmp(&a.ledger_seq))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
        SortPolicy::LedgerSeqAsc => a
            .ledger_seq
            .cmp(&b.ledger_seq)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
    };

    primary
        .then_with(|| a.tie_breaker.cmp(&b.tie_breaker))
        .then_with(|| a.id.cmp(&b.id))
}

/// Total-order comparator for [`SortableEvent`].
pub fn compare_events(a: &SortableEvent, b: &SortableEvent, policy: SortPolicy) -> Ordering {
    let primary = match policy {
        SortPolicy::StatusPriorityFirst => a
            .status
            .priority_rank()
            .cmp(&b.status.priority_rank())
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.ledger_seq.cmp(&b.ledger_seq)),
        SortPolicy::TimestampAsc => a
            .timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.ledger_seq.cmp(&b.ledger_seq))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
        SortPolicy::TimestampDesc => b
            .timestamp
            .cmp(&a.timestamp)
            .then_with(|| b.ledger_seq.cmp(&a.ledger_seq))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
        SortPolicy::LedgerSeqAsc => a
            .ledger_seq
            .cmp(&b.ledger_seq)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.status.priority_rank().cmp(&b.status.priority_rank())),
    };

    primary
        .then_with(|| a.tx_index.cmp(&b.tx_index))
        .then_with(|| a.event_id.cmp(&b.event_id))
}

/// Sort a bounded vector of mixed-status [`SortableRecord`]s deterministically.
///
/// - Rejects batches exceeding [`MAX_SORT_ITEMS`] with [`Error::InvalidArgument`].
/// - When `strict_unique` is `true`, rejects duplicate `id`s with [`Error::InvalidArgument`].
pub fn sort_records(
    env: &Env,
    records: &Vec<SortableRecord>,
    policy: SortPolicy,
    strict_unique: bool,
) -> Result<Vec<SortableRecord>, Error> {
    if records.len() > MAX_SORT_ITEMS {
        return Err(Error::InvalidArgument);
    }

    let mut out = Vec::new(env);
    for item in records.iter() {
        out.push_back(item);
    }

    // Stable insertion sort in Soroban Vec (bounded by MAX_SORT_ITEMS = 200).
    let len = out.len();
    let mut i = 1u32;
    while i < len {
        let key = out.get(i).unwrap();
        let mut j = i;
        while j > 0 {
            let prev = out.get(j - 1).unwrap();
            if compare_records(&prev, &key, policy) == Ordering::Greater {
                out.set(j, prev);
                j -= 1;
            } else {
                break;
            }
        }
        out.set(j, key);
        i += 1;
    }

    if strict_unique && len > 1 {
        let mut k = 1u32;
        while k < len {
            let a = out.get(k - 1).unwrap();
            let b = out.get(k).unwrap();
            if a.id == b.id {
                return Err(Error::InvalidArgument);
            }
            k += 1;
        }
    }

    Ok(out)
}

/// Filter by authorization/visibility and deterministically sort [`SortableEvent`]s.
///
/// - If `include_maintainer_only` is `true`, verifies `caller` is admin via [`require_admin`],
///   returning [`Error::Unauthorized`] otherwise.
/// - If `include_maintainer_only` is `false`, `MaintainerOnly` events are excluded.
pub fn sort_events_for_viewer(
    env: &Env,
    caller: &Address,
    events: &Vec<SortableEvent>,
    policy: SortPolicy,
    include_maintainer_only: bool,
) -> Result<Vec<SortableEvent>, Error> {
    if events.len() > MAX_SORT_ITEMS {
        return Err(Error::InvalidArgument);
    }
    if include_maintainer_only {
        require_admin(env, caller)?;
    }

    let mut out = Vec::new(env);
    for ev in events.iter() {
        if include_maintainer_only || ev.visibility.is_user_facing() {
            out.push_back(ev);
        }
    }

    let len = out.len();
    let mut i = 1u32;
    while i < len {
        let key = out.get(i).unwrap();
        let mut j = i;
        while j > 0 {
            let prev = out.get(j - 1).unwrap();
            if compare_events(&prev, &key, policy) == Ordering::Greater {
                out.set(j, prev);
                j -= 1;
            } else {
                break;
            }
        }
        out.set(j, key);
        i += 1;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::initialize_admin;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{contract, contractimpl};

    #[contract]
    pub struct MockContract;

    #[contractimpl]
    impl MockContract {
        pub fn ping() -> u32 {
            1
        }
    }

    fn rec(id: u64, status: RecordStatus, ts: u64, seq: u32, tie: u64) -> SortableRecord {
        SortableRecord {
            id,
            status,
            timestamp: ts,
            ledger_seq: seq,
            tie_breaker: tie,
        }
    }

    fn ev(
        id: u64,
        status: RecordStatus,
        ts: u64,
        seq: u32,
        tx: u32,
        vis: EventVisibility,
    ) -> SortableEvent {
        SortableEvent {
            event_id: id,
            status,
            timestamp: ts,
            ledger_seq: seq,
            tx_index: tx,
            visibility: vis,
        }
    }

    #[test]
    fn mixed_status_priority_and_tie_breakers_are_deterministic() {
        let env = Env::default();
        let mut input = Vec::new(&env);
        input.push_back(rec(5, RecordStatus::Settled, 100, 10, 2));
        input.push_back(rec(2, RecordStatus::Failed, 200, 20, 1));
        input.push_back(rec(4, RecordStatus::Pending, 100, 10, 2));
        input.push_back(rec(3, RecordStatus::Pending, 100, 10, 1));
        input.push_back(rec(1, RecordStatus::Active, 50, 5, 0));

        let sorted = sort_records(&env, &input, SortPolicy::StatusPriorityFirst, true).unwrap();
        assert_eq!(sorted.get(0).unwrap().id, 2); // Failed
        assert_eq!(sorted.get(1).unwrap().id, 3); // Pending, tie=1
        assert_eq!(sorted.get(2).unwrap().id, 4); // Pending, tie=2
        assert_eq!(sorted.get(3).unwrap().id, 1); // Active
        assert_eq!(sorted.get(4).unwrap().id, 5); // Settled

        // Retry / idempotency: sorting already-sorted output yields identical result.
        let resorted = sort_records(&env, &sorted, SortPolicy::StatusPriorityFirst, true).unwrap();
        assert_eq!(sorted, resorted);
    }

    #[test]
    fn permutation_invariance_under_timestamp_desc() {
        let env = Env::default();
        let mut p1 = Vec::new(&env);
        p1.push_back(rec(1, RecordStatus::Active, 100, 10, 0));
        p1.push_back(rec(2, RecordStatus::Failed, 100, 10, 0));
        p1.push_back(rec(3, RecordStatus::Settled, 200, 11, 0));

        let mut p2 = Vec::new(&env);
        p2.push_back(rec(3, RecordStatus::Settled, 200, 11, 0));
        p2.push_back(rec(2, RecordStatus::Failed, 100, 10, 0));
        p2.push_back(rec(1, RecordStatus::Active, 100, 10, 0));

        let s1 = sort_records(&env, &p1, SortPolicy::TimestampDesc, true).unwrap();
        let s2 = sort_records(&env, &p2, SortPolicy::TimestampDesc, true).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(s1.get(0).unwrap().id, 3);
        assert_eq!(s1.get(1).unwrap().id, 2); // Failed beats Active on tie
        assert_eq!(s1.get(2).unwrap().id, 1);
    }

    #[test]
    fn rejects_oversized_batches_and_duplicate_ids() {
        let env = Env::default();
        let mut dup = Vec::new(&env);
        dup.push_back(rec(7, RecordStatus::Pending, 100, 1, 0));
        dup.push_back(rec(7, RecordStatus::Active, 100, 1, 0));
        assert_eq!(
            sort_records(&env, &dup, SortPolicy::TimestampAsc, true),
            Err(Error::InvalidArgument)
        );

        let mut huge = Vec::new(&env);
        for i in 0..(MAX_SORT_ITEMS + 1) {
            huge.push_back(rec(i as u64, RecordStatus::Active, i as u64, i, 0));
        }
        assert_eq!(
            sort_records(&env, &huge, SortPolicy::LedgerSeqAsc, false),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn event_sorting_enforces_visibility_and_admin_authorization() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, MockContract);
        let admin = Address::generate(&env);
        let user = Address::generate(&env);

        let mut events = Vec::new(&env);
        events.push_back(ev(10, RecordStatus::Settled, 300, 3, 1, EventVisibility::Public));
        events.push_back(ev(
            11,
            RecordStatus::Failed,
            100,
            1,
            0,
            EventVisibility::MaintainerOnly,
        ));
        events.push_back(ev(
            12,
            RecordStatus::Pending,
            200,
            2,
            0,
            EventVisibility::Authorized,
        ));

        env.as_contract(&contract_id, || {
            initialize_admin(&env, &admin).unwrap();

            // Non-admin requesting MaintainerOnly events is rejected.
            assert_eq!(
                sort_events_for_viewer(&env, &user, &events, SortPolicy::TimestampAsc, true),
                Err(Error::Unauthorized)
            );

            // Non-admin with include_maintainer_only = false gets only user-facing events, sorted.
            let user_view =
                sort_events_for_viewer(&env, &user, &events, SortPolicy::TimestampAsc, false)
                    .unwrap();
            assert_eq!(user_view.len(), 2);
            assert_eq!(user_view.get(0).unwrap().event_id, 12);
            assert_eq!(user_view.get(1).unwrap().event_id, 10);

            // Admin with include_maintainer_only = true sees all 3 in canonical order.
            let admin_view =
                sort_events_for_viewer(&env, &admin, &events, SortPolicy::TimestampAsc, true)
                    .unwrap();
            assert_eq!(admin_view.len(), 3);
            assert_eq!(admin_view.get(0).unwrap().event_id, 11);
        });
    }
}
