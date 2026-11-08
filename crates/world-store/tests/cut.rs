//! Cut commit rules that are not about round-tripping: in-flight rows belong to
//! the host that cut them, and the world tick only moves forward.

use game::{CutId, InFlightRow, RegionCoords, WorldCut};

use crate::codec::{intent, reply};
use crate::handle::{each_backend, fence, open};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

fn cut(id: u64, host: u32, tick: u64, in_flight: Vec<InFlightRow>) -> WorldCut {
    WorldCut { id: CutId(id), host, tick, regions: vec![], in_flight, players: vec![], history: None }
}

fn row(target: RegionCoords, reply_row: bool) -> InFlightRow {
    InFlightRow::Intent { target, msg: if reply_row { game::IntentMsg::Reply(reply()) } else { intent() } }
}

#[test]
fn in_flight_replace_is_per_host() {
    each_backend("inflight_host", false, |url| {
        let h = open(url);
        let f = fence(&h);
        let h0 = vec![row(A, false), row(B, true)];
        let h1 = vec![row(B, false)];
        h.commit_cut(cut(1, 0, 10, h0.clone()), f.clone()).recv().unwrap().unwrap();
        h.commit_cut(cut(2, 1, 11, h1.clone()), f.clone()).recv().unwrap().unwrap();
        let all = h.load_in_flight().unwrap();
        assert_eq!(all.len(), 3, "both hosts' rows are present");
        assert_eq!(&all[..2], &h0[..], "host 0's rows keep their order, ahead of host 1's");
        assert_eq!(&all[2..], &h1[..]);
        for r in h0.iter().chain(&h1) {
            assert!(all.contains(r));
        }
        // Host 0 recommits with nothing: only its own rows go.
        h.commit_cut(cut(3, 0, 12, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight().unwrap(), h1);
        // Host 1 replaces its rows; host 0's new ones stay.
        h.commit_cut(cut(4, 0, 13, h0.clone()), f.clone()).recv().unwrap().unwrap();
        h.commit_cut(cut(5, 1, 14, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight().unwrap(), h0);
        h.shutdown();
    });
}

#[test]
fn world_tick_never_goes_back() {
    each_backend("world_tick", false, |url| {
        let h = open(url);
        let f = fence(&h);
        h.commit_cut(cut(1, 0, 500, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.world_info().unwrap().world_tick, 500);
        h.commit_cut(cut(2, 1, 400, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.world_info().unwrap().world_tick, 500, "a slower host's older cut does not rewind it");
        h.commit_cut(cut(3, 1, 600, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.world_info().unwrap().world_tick, 600);
        h.shutdown();
    });
}
