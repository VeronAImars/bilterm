use super::limits::{LimitError, MAX_ATTACH_CLIENTS, MAX_ATTACH_COLS, MAX_ATTACH_ROWS};
use super::size_owner::*;
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn first_rw_client_owns_and_applies_immediately() {
    let mut s = SizeOwner::default();
    let o = s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    assert_eq!(o.apply, Some((100, 30)));
    assert_eq!(o.owner_changed, Some(Some(1)));
    assert_eq!(s.owner(), Some(1));
}

#[test]
fn ro_never_owns_or_resizes() {
    let mut s = SizeOwner::default();
    let o = s.register(1, Role::Ro, 100, 30, ms(0)).unwrap();
    assert_eq!(o, Outcome::default());
    assert_eq!(s.owner(), None);
    assert_eq!(s.resize(1, 120, 40, ms(1000)), Err(SizeError::ReadOnly));
    assert_eq!(s.activity(1, ms(1000)).unwrap(), Outcome::default());
    assert_eq!(s.applied_size(), None);
    s.register(2, Role::Rw, 80, 24, ms(1000)).unwrap();
    assert_eq!(s.activity(1, ms(2000)).unwrap().owner_changed, None);
    assert_eq!(s.owner(), Some(2));
    assert_eq!(s.applied_size(), Some((80, 24)));
}

#[test]
fn latest_rw_wins_after_debounce() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    let o = s.register(2, Role::Rw, 80, 24, ms(500)).unwrap();
    assert_eq!(o.apply, Some((80, 24)));
    assert_eq!(s.owner(), Some(2));
    // Client 1 types: becomes owner again, its stored size wins.
    let o = s.activity(1, ms(1000)).unwrap();
    assert_eq!(o.owner_changed, Some(Some(1)));
    assert_eq!(o.apply, Some((100, 30)));
}

#[test]
fn debounce_defers_then_trailing_poll_applies_latest() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    let o = s.resize(1, 101, 31, ms(10)).unwrap();
    assert_eq!(o.apply, None);
    assert_eq!(o.retry_at, Some(ms(150)));
    // Further resizes inside the window coalesce: only the latest is applied.
    let o = s.resize(1, 102, 32, ms(60)).unwrap();
    assert_eq!((o.apply, o.retry_at), (None, Some(ms(150))));
    assert_eq!(s.poll(ms(149)).apply, None);
    assert_eq!(s.poll(ms(150)).apply, Some((102, 32)));
    assert_eq!(s.poll(ms(400)), Outcome::default());
}

#[test]
fn ping_pong_between_two_clients_is_rate_limited() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    s.register(2, Role::Rw, 80, 24, ms(0)).unwrap();
    let mut applies = 0;
    for t in 1..100u64 {
        let id = if t % 2 == 0 { 1 } else { 2 };
        let o = s.activity(id, ms(t)).unwrap();
        applies += u32::from(o.apply.is_some());
        applies += u32::from(s.poll(ms(t)).apply.is_some());
    }
    // 100 ms of flapping with a 150 ms debounce: at most the initial apply happened.
    assert!(applies <= 1, "applies={applies}");
}

#[test]
fn same_size_is_not_reapplied() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    assert_eq!(s.resize(1, 100, 30, ms(1000)).unwrap().apply, None);
    assert_eq!(s.activity(1, ms(2000)).unwrap(), Outcome::default());
}

#[test]
fn owner_detach_falls_back_to_next_most_recent_rw() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    s.register(2, Role::Rw, 90, 28, ms(1000)).unwrap();
    s.register(3, Role::Rw, 80, 24, ms(2000)).unwrap();
    s.register(4, Role::Ro, 60, 20, ms(2000)).unwrap();
    let o = s.detach(3, ms(3000));
    assert_eq!(o.owner_changed, Some(Some(2)));
    assert_eq!(o.apply, Some((90, 28)));
}

#[test]
fn last_rw_detach_keeps_size() {
    let mut s = SizeOwner::default();
    s.register(1, Role::Rw, 100, 30, ms(0)).unwrap();
    s.register(2, Role::Ro, 60, 20, ms(0)).unwrap();
    let o = s.detach(1, ms(5000));
    assert_eq!(o.apply, None);
    assert_eq!(o.owner_changed, Some(None));
    assert_eq!(s.applied_size(), Some((100, 30)));
    assert_eq!(s.poll(ms(9000)), Outcome::default());
    // A later rw attach takes over again.
    assert_eq!(s.register(3, Role::Rw, 70, 20, ms(9000)).unwrap().apply, Some((70, 20)));
}

#[test]
fn dims_validated_against_cap_and_state_untouched() {
    let mut s = SizeOwner::default();
    assert_eq!(
        s.register(1, Role::Rw, MAX_ATTACH_COLS + 1, 10, ms(0)),
        Err(SizeError::BadDims(LimitError::TooWide(MAX_ATTACH_COLS + 1)))
    );
    assert_eq!(s.client_count(), 0);
    s.register(1, Role::Rw, MAX_ATTACH_COLS, MAX_ATTACH_ROWS, ms(0)).unwrap();
    assert_eq!(s.resize(1, 0, 10, ms(1000)), Err(SizeError::BadDims(LimitError::ZeroDimension)));
    assert_eq!(s.resize(1, 10, MAX_ATTACH_ROWS + 1, ms(1000)).is_err(), true);
    assert_eq!(s.applied_size(), Some((MAX_ATTACH_COLS, MAX_ATTACH_ROWS)));
    assert!(s.resize(1, u16::MAX, u16::MAX, ms(1000)).is_err());
}

#[test]
fn unknown_duplicate_and_capacity() {
    let mut s = SizeOwner::default();
    assert_eq!(s.resize(9, 10, 10, ms(0)), Err(SizeError::UnknownClient));
    assert_eq!(s.activity(9, ms(0)), Err(SizeError::UnknownClient));
    s.register(1, Role::Rw, 10, 10, ms(0)).unwrap();
    assert_eq!(s.register(1, Role::Rw, 10, 10, ms(0)), Err(SizeError::DuplicateClient));
    for id in 2..=MAX_ATTACH_CLIENTS as u64 {
        s.register(id, Role::Ro, 10, 10, ms(0)).unwrap();
    }
    assert_eq!(s.register(999, Role::Ro, 10, 10, ms(0)), Err(SizeError::TooManyClients));
}
