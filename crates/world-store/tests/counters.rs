//! The store thread's wait / codec-CPU counters (`StoreHandle::take_counters`), on every backend.

use game::RegionCoords;

use crate::handle::{each_backend, fence, first_cut, open};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };

#[test]
fn wait_and_cpu_are_recorded_and_reset() {
    each_backend("counters", false, |url| {
        let h = open(url);
        let f = fence(&h);
        h.commit_cut(first_cut(&h), f.clone()).recv().unwrap().unwrap();
        // The setup's own requests are not the ones under test.
        let _ = h.take_counters();

        for _ in 0..10 {
            assert!(h.load_region(A, &f).unwrap().is_some());
        }
        let c = h.take_counters();
        assert_eq!(c.requests, 10, "{c:?}");
        assert!(c.cpu_ms_sum > 0, "decoding ten regions took no CPU time: {c:?}");
        assert!(c.cpu_ms_max > 0 && c.cpu_ms_max <= c.cpu_ms_sum, "{c:?}");
        assert!(c.wait_ms_max <= c.wait_ms_sum, "{c:?}");

        assert_eq!(h.take_counters(), world_store::StoreCounters::default(), "a take resets");
        h.shutdown();
    });
}
