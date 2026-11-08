//! `Credential` in the `Hello` packet: wire round trip, redacted `Debug`, and
//! the player-name rules of local servers.

use game::{decode_packet, encode_packet, ClientPacket, Credential, PlayerName, PLAYER_NAME_MAX_CHARS};

fn round_trip(credential: Credential) -> Credential {
    let bytes = encode_packet(&ClientPacket::Hello { credential }).unwrap();
    match decode_packet::<ClientPacket>(&bytes).unwrap() {
        ClientPacket::Hello { credential } => credential,
        other => panic!("not a Hello: {other:?}"),
    }
}

#[test]
fn hello_round_trips_both_kinds() {
    let local = Credential::Local(PlayerName::parse("Urist").unwrap());
    assert_eq!(round_trip(local.clone()), local);
    let jwt = Credential::Jwt("a.b.c".into());
    assert_eq!(round_trip(jwt.clone()), jwt);
}

#[test]
fn debug_never_prints_secrets() {
    let shown = format!("{:?}", Credential::Jwt("header.SECRETPAYLOAD.sig".into()));
    assert!(!shown.contains("SECRETPAYLOAD") && !shown.contains("sig"), "{shown}");
    // A name is not a secret: it is shown.
    let shown = format!("{:?}", Credential::Local(PlayerName::parse("bob").unwrap()));
    assert!(shown.contains("bob"), "{shown}");
}

#[test]
fn player_name_parse() {
    assert_eq!(PlayerName::parse("  Urist McMiner \n").unwrap().0, "Urist McMiner", "trimmed");
    assert_eq!(PlayerName::parse("Żółw 🐢").unwrap().0, "Żółw 🐢", "unicode is fine");
    let longest = "é".repeat(PLAYER_NAME_MAX_CHARS);
    assert_eq!(PlayerName::parse(&longest).unwrap().0, longest, "the limit counts characters, not bytes");
    assert!(PlayerName::parse(&"a".repeat(PLAYER_NAME_MAX_CHARS + 1)).is_err());
    assert!(PlayerName::parse("").is_err());
    assert!(PlayerName::parse("   \t").is_err());
    assert!(PlayerName::parse("a\u{0}b").is_err());
    assert!(PlayerName::parse("a\nb").is_err());
    assert!(PlayerName::parse("a\u{7f}b").is_err());
    // `:` is allowed: store keys namespace names (`local:<name>`), they do not parse them.
    assert!(PlayerName::parse("a:b").is_ok());
}
