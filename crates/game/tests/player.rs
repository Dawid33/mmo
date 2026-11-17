//! Player identity: an online account's token and the record a store keeps.
use game::{PlayerId, PlayerRecord, PlayerToken};

#[test]
fn token_validates_and_debug_redacts() {
    let token = PlayerToken::generate([0xab; 16]);
    assert_eq!(token.0, "ab".repeat(16));
    assert!(token.is_valid());
    let other = PlayerToken::generate(*b"0123456789abcdef");
    assert_eq!(other.0.len(), 32);
    assert!(other.is_valid());
    assert_ne!(token, other);

    for bad in ["", "abc", &"AB".repeat(16), &"g".repeat(32), &"a".repeat(33), &"é".repeat(16)] {
        assert!(!PlayerToken(bad.to_string()).is_valid(), "{bad:?} is not a token");
    }

    let shown = format!("{token:?}");
    assert!(shown.contains("ababab"), "{shown}");
    assert!(!shown.contains(&"ab".repeat(4)), "Debug must not print the secret: {shown}");
}

#[test]
fn player_record_round_trips_through_bincode() {
    let record = PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::math::Vec3::new(1.0, 2.0, 3.0)),
        region: game::RegionCoords::new(-2, 5),
        adventure: Some(game::history::SiteId(4)),
    };
    let bytes = bincode::serialize(&(PlayerId(9), &record)).unwrap();
    let (id, back): (PlayerId, PlayerRecord) = bincode::deserialize(&bytes).unwrap();
    assert_eq!(id, PlayerId(9));
    assert_eq!(back, record);
}

#[test]
fn parked_client_folds_the_cursor_into_the_inventory() {
    let stack = game::ItemStack { item: game::ItemId(6), count: 2 };
    let mut c = game::Client { inventory: game::Inventory::new(), cursor: Some(stack), ..game::Client::default() };
    let parked = game::parked_client(&c);
    assert_eq!(parked.cursor, None);
    assert_eq!(parked.inventory.slots[0], Some(stack));
    // A full inventory: what does not fit stays on the cursor.
    let other = game::ItemStack { item: game::ItemId(7), count: 1 };
    c.inventory.slots.iter_mut().for_each(|s| *s = Some(other));
    let parked = game::parked_client(&c);
    assert_eq!(parked.cursor, Some(stack), "kept on the cursor, not lost");
    assert!(parked.inventory.slots.iter().all(|s| *s == Some(other)));
}

#[test]
fn session_client_id_is_unique_per_join() {
    use game::{session_client_id, SESSION_EPOCH_BITS};
    let alice = PlayerId(1);
    assert_ne!(session_client_id(alice, 1), session_client_id(alice, 2), "two sessions of one player differ");
    assert_ne!(
        session_client_id(PlayerId(1), 1),
        session_client_id(PlayerId(2), 1),
        "two players in the same epoch differ"
    );
    // The epoch keeps its low 24 bits: 1 << 24 wraps onto epoch 0 of the same player only.
    assert_eq!(session_client_id(alice, 1 << SESSION_EPOCH_BITS), session_client_id(alice, 0));
    assert_ne!(session_client_id(alice, 1 << SESSION_EPOCH_BITS), session_client_id(PlayerId(2), 0));
    // A big player id stays above 32 bits (a ClientId is 64 bits on every target).
    assert_eq!(session_client_id(PlayerId(1), 0), 1u64 << SESSION_EPOCH_BITS);
}
