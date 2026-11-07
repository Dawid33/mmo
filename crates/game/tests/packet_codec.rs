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

fn tile(landing: Option<[u32; 2]>) -> game::TileSummary {
    game::TileSummary {
        ground_min_m: -3.0,
        ground_mean_m: 4.5,
        ground_max_m: 20.0,
        relief_m: 9.0,
        slope: [0.1, -0.2],
        sea_fraction: 0.25,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 2,
        river_max_discharge_m2: 12.0,
        river_max_width_m: 3.0,
        surface_rock: Some(1),
        landing,
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 10.0,
        precip_mm: 700.0,
        savagery: 50,
        alignment: 0,
        volcano: false,
    }
}

fn map_2x1() -> game::WorldMap {
    game::WorldMap {
        origin: RegionCoords::new(-1, 0),
        width: 2,
        height: 1,
        tiles: vec![tile(Some([3, 4])), tile(None)],
        rock_names: vec!["granite".into(), "basalt".into()],
        region_types: vec![game::RegionType::Grassland],
    }
}

#[test]
fn world_map_packet_round_trips() {
    let packet = ServerPacket::WorldMap(map_2x1());
    let back: ServerPacket = decode_packet(&encode_packet(&packet).unwrap()).unwrap();
    match back {
        ServerPacket::WorldMap(map) => assert_eq!(map, map_2x1()),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn play_region_packets_round_trip() {
    let rc = RegionCoords::new(-2, 5);
    let back: ClientPacket =
        decode_packet(&encode_packet(&ClientPacket::PlayRegion(rc)).unwrap()).unwrap();
    assert!(matches!(back, ClientPacket::PlayRegion(r) if r == rc));
    let packet = ServerPacket::PlayRefused(rc, game::PlayRefusal::NoDryLand);
    let back: ServerPacket = decode_packet(&encode_packet(&packet).unwrap()).unwrap();
    assert!(matches!(back, ServerPacket::PlayRefused(r, game::PlayRefusal::NoDryLand) if r == rc));
}

#[test]
fn world_map_tile_lookup() {
    let tiles: Vec<_> = (0..6).map(|i| {
        let mut t = tile(None);
        t.rivers = i;
        t
    }).collect();
    let map = game::WorldMap {
        origin: RegionCoords::new(-1, -1),
        width: 3,
        height: 2,
        tiles,
        rock_names: vec![],
        region_types: vec![],
    };
    assert_eq!(map.tile(RegionCoords::new(1, 0)).unwrap().rivers, 5);
    assert!(map.tile(RegionCoords::new(2, 0)).is_none());
    assert!(map.tile(RegionCoords::new(-2, 0)).is_none());
    assert_eq!(map.coords(4), RegionCoords::new(0, 0));
}

/// Water plan Task 8: `LodData` carries the key's `LodWater` surface layer
/// next to its terrain grids, through the shared wire framing.
#[test]
fn lod_data_round_trips_with_water() {
    use game::lod::{LodKey, LodWater, PackedLodGrid};
    let key = LodKey { factor: 16, coords: [-2, 3] };
    let side = 4u16;
    let mut tops = vec![(game::NO_WATER, 0u8); usize::from(side) * usize::from(side)];
    tops[5] = (63, 12);
    tops[6] = (63, 12);
    tops[11] = (70, 255);
    let water = LodWater::encode(&tops, side);
    assert!(water.is_well_formed());
    assert_eq!(water.decode().as_deref(), Some(&tops[..]));
    let payload = vec![([0, 1, 0], PackedLodGrid(vec![(32 * 32 * 32, game::BlockId(2))]))];
    let wire = encode_packet(&ServerPacket::LodData(key, payload.clone(), water.clone())).unwrap();
    match decode_packet::<ServerPacket>(&wire).unwrap() {
        ServerPacket::LodData(k, p, w) => {
            assert_eq!(k, key);
            assert_eq!(p, payload);
            assert_eq!(w, water);
        }
        other => panic!("wrong variant {other:?}"),
    }
}
