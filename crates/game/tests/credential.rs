//! `Credential` in the `Hello` packet: wire round trip and redacted `Debug`.

use game::{decode_packet, encode_packet, ClientPacket, Credential, PlayerToken, PLAYER_TOKEN_DEBUG_CHARS};

fn round_trip(credential: Credential) -> Credential {
    let bytes = encode_packet(&ClientPacket::Hello { credential }).unwrap();
    match decode_packet::<ClientPacket>(&bytes).unwrap() {
        ClientPacket::Hello { credential } => credential,
        other => panic!("not a Hello: {other:?}"),
    }
}

#[test]
fn hello_round_trips_both_kinds() {
    let token = Credential::Token(PlayerToken::generate([7; 16]));
    assert_eq!(round_trip(token.clone()), token);
    let jwt = Credential::Jwt("a.b.c".into());
    assert_eq!(round_trip(jwt.clone()), jwt);
}

#[test]
fn debug_never_prints_secrets() {
    let t = PlayerToken::generate([0xab; 16]);
    let shown = format!("{:?}", Credential::Token(t.clone()));
    assert!(!shown.contains(&t.0), "{shown}");
    assert!(shown.contains(&t.0[..PLAYER_TOKEN_DEBUG_CHARS]), "{shown}");
    let shown = format!("{:?}", Credential::Jwt("header.SECRETPAYLOAD.sig".into()));
    assert!(!shown.contains("SECRETPAYLOAD") && !shown.contains("sig"), "{shown}");
}
