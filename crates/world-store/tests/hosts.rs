//! The host registry and `world.generation`: a host that boots after a boot that never
//! left clean raises the world's generation, once; every other host adopts it.

use world_store::{Fence, HostRow, StoreError};

use crate::handle::{TEST_SUPER_REGIONS, each_backend, fence, open, raw_exec, register};

fn row(ordinal: u32, boot: u64) -> HostRow {
    HostRow {
        ordinal,
        boot,
        // Ignored by `register_host`.
        generation: 99,
        clean: true,
        state: 0,
        seen_ms: 0,
        reg: 0,
        peer_addr: None,
        peer_cert: None,
        quic: None,
        webtransport: None,
        wt_cert_hash: None,
    }
}

#[test]
fn first_boot_does_not_bump() {
    each_backend("hosts_first", false, |url| {
        let h = open(url);
        let f = fence(&h);
        assert_eq!(h.world_generation().unwrap(), 0);
        let boot = register(&h, row(0, u64::MAX - 5), &f).unwrap();
        assert_eq!((boot.generation, boot.bumped), (0, false));
        assert_eq!(h.world_generation().unwrap(), 0);
        let rows = h.hosts().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].ordinal, rows[0].boot, rows[0].generation, rows[0].clean), (0, u64::MAX - 5, 0, false));
        h.shutdown();
    });
}

#[test]
fn dirty_boot_bumps_once_and_peers_adopt() {
    each_backend("hosts_dirty", false, |url| {
        let h = open(url);
        let a = h.claim_super_regions(0, vec![TEST_SUPER_REGIONS[0]]).unwrap();
        let b = h.claim_super_regions(1, vec![TEST_SUPER_REGIONS[1]]).unwrap();
        assert!(!register(&h, row(0, 1), &a).unwrap().bumped);
        assert!(!register(&h, row(1, 2), &b).unwrap().bumped);
        // A crashed and reboots: its row was dirty at the world's generation.
        let again = register(&h, row(0, 3), &a).unwrap();
        assert_eq!((again.generation, again.bumped), (1, true));
        // B restarts because it saw generation 1: its row says 0, so it adopts.
        let adopted = register(&h, row(1, 4), &b).unwrap();
        assert_eq!((adopted.generation, adopted.bumped), (1, false));
        assert_eq!(h.world_generation().unwrap(), 1);
        // A genuine second crash of B bumps again.
        let again = register(&h, row(1, 5), &b).unwrap();
        assert_eq!((again.generation, again.bumped), (2, true));
        let rows = h.hosts().unwrap();
        assert_eq!(rows.iter().map(|r| (r.ordinal, r.generation)).collect::<Vec<_>>(), vec![(0, 1), (1, 2)]);
        h.shutdown();
    });
}

#[test]
fn clean_row_keeps_generation() {
    each_backend("hosts_clean", true, |url| {
        let h = open(url);
        let f = fence(&h);
        register(&h, row(0, 1), &f).unwrap();
        h.shutdown();
        raw_exec(url, "UPDATE hosts SET clean = 1 WHERE ordinal = 0");
        let h = open(url);
        assert!(h.hosts().unwrap()[0].clean);
        let f = fence(&h);
        let boot = register(&h, row(0, 2), &f).unwrap();
        assert_eq!((boot.generation, boot.bumped), (0, false));
        let rows = h.hosts().unwrap();
        assert!(!rows[0].clean, "a boot is dirty until a cut says otherwise");
        assert_eq!(rows[0].boot, 2);
        h.shutdown();
    });
}

#[test]
fn register_is_fenced() {
    each_backend("hosts_fenced", false, |url| {
        let h = open(url);
        let stale = h.claim_super_regions(0, TEST_SUPER_REGIONS.to_vec()).unwrap();
        let _newer = h.claim_super_regions(1, TEST_SUPER_REGIONS.to_vec()).unwrap();
        let err = register(&h, row(0, 1), &stale).unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        assert!(h.hosts().unwrap().is_empty(), "no row written");
        assert_eq!(h.world_generation().unwrap(), 0);
        // A fence that claimed nothing has nothing to lose.
        register(&h, row(2, 1), &Fence::default()).unwrap();
        h.shutdown();
    });
}

#[test]
fn stale_generation_names_both_numbers() {
    let text = StoreError::StaleGeneration { stored: 7, mine: 3 }.to_string();
    assert!(text.contains('7') && text.contains('3'), "{text}");
}

