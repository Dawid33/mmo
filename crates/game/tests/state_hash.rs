use game::{state_hash, Rollback};

#[test]
fn state_hash_is_deterministic_and_sensitive() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut a = Rollback::new(Some(send.clone()));
    let b = Rollback::new(Some(send));
    assert_eq!(state_hash(&a), state_hash(&b), "identical fresh state hashes equal");

    a.new_transaction();
    a.create_player_safe(0);
    a.forget();
    assert_ne!(state_hash(&a), state_hash(&b), "a mutation changes the hash");
}
