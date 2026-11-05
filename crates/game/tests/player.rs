//! Player identity: the token a client holds and the record a store keeps.
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
        fortress: Some(game::history::SiteId(4)),
        adventure: None,
    };
    let bytes = bincode::serialize(&(PlayerId(9), &record)).unwrap();
    let (id, back): (PlayerId, PlayerRecord) = bincode::deserialize(&bytes).unwrap();
    assert_eq!(id, PlayerId(9));
    assert_eq!(back, record);
}
