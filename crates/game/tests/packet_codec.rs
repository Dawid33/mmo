//! Wire framing shared by every transport edge (server, native client, wasm
//! client): `game::encode_packet` / `game::decode_packet`.

use game::{
    decode_packet, encode_packet, ClientPacket, PacketError, RegionCoords, ServerPacket,
    MAX_DECOMPRESSED_PACKET_BYTES, PACKET_COMPRESS_THRESHOLD,
};

#[test]
fn small_packets_stay_raw_and_round_trip() {
    let packet = ClientPacket::RequestRegionConnection(RegionCoords::new(-3, 7));
    let wire = encode_packet(&packet).unwrap();
    assert_eq!(wire[0], 0, "tick-sized traffic must not pay for compression");
    assert_eq!(&wire[1..], &bincode::serialize(&packet).unwrap()[..]);
    let back: ClientPacket = decode_packet(&wire).unwrap();
    assert!(matches!(back, ClientPacket::RequestRegionConnection(rc) if rc == RegionCoords::new(-3, 7)));
}

#[test]
fn large_redundant_packets_are_compressed_and_round_trip_exactly() {
    // A region snapshot in miniature: a big, highly repetitive payload.
    let payload: Vec<u16> = (0..200_000u32).map(|i| (i / 5000) as u16).collect();
    let raw = bincode::serialize(&payload).unwrap();
    assert!(raw.len() > PACKET_COMPRESS_THRESHOLD);

    let wire = encode_packet(&payload).unwrap();
    assert_eq!(wire[0], 1, "above the threshold the payload is lz4-compressed");
    assert!(wire.len() * 10 < raw.len(), "{} wire bytes for {} raw", wire.len(), raw.len());
    let back: Vec<u16> = decode_packet(&wire).unwrap();
    assert_eq!(back, payload);
}

#[test]
fn incompressible_payloads_fall_back_to_raw() {
    // xorshift noise: lz4 cannot shrink it, so framing must not grow it either.
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let payload: Vec<u64> = (0..4096)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        })
        .collect();
    let wire = encode_packet(&payload).unwrap();
    assert_eq!(wire[0], 0);
    let back: Vec<u64> = decode_packet(&wire).unwrap();
    assert_eq!(back, payload);
}

#[test]
fn hostile_or_corrupt_input_is_an_error_never_a_panic() {
    assert!(matches!(decode_packet::<ServerPacket>(&[]), Err(PacketError::Empty)));
    assert!(matches!(decode_packet::<ServerPacket>(&[9, 1, 2, 3]), Err(PacketError::UnknownFormat(9))));
    // Compressed frame too short to hold its length prefix.
    assert!(matches!(decode_packet::<ServerPacket>(&[1, 0, 0]), Err(PacketError::Truncated)));
    // A length prefix claiming more than the cap must be rejected before any
    // allocation happens (wire data is untrusted).
    let mut bomb = vec![1u8];
    bomb.extend_from_slice(&((MAX_DECOMPRESSED_PACKET_BYTES as u32).saturating_add(1)).to_le_bytes());
    bomb.extend_from_slice(&[0u8; 16]);
    assert!(matches!(decode_packet::<ServerPacket>(&bomb), Err(PacketError::TooLarge(_))));
    // A plausible length over garbage: lz4 must fail cleanly.
    let mut garbage = vec![1u8];
    garbage.extend_from_slice(&100_000u32.to_le_bytes());
    garbage.extend_from_slice(&[0xFFu8; 64]);
    assert!(decode_packet::<Vec<u16>>(&garbage).is_err());
    // A valid frame around bytes that are not the expected type.
    let wire = encode_packet(&vec![1u8, 2, 3]).unwrap();
    assert!(decode_packet::<ServerPacket>(&wire).is_err());
}
