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
        let all = h.load_in_flight(None).unwrap();
        assert_eq!(all.len(), 3, "both hosts' rows are present");
        assert_eq!(&all[..2], &h0[..], "host 0's rows keep their order, ahead of host 1's");
        assert_eq!(&all[2..], &h1[..]);
        for r in h0.iter().chain(&h1) {
            assert!(all.contains(r));
        }
        // Host 0 recommits with nothing: only its own rows go.
        h.commit_cut(cut(3, 0, 12, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight(None).unwrap(), h1);
        // Host 1 replaces its rows; host 0's new ones stay.
        h.commit_cut(cut(4, 0, 13, h0.clone()), f.clone()).recv().unwrap().unwrap();
        h.commit_cut(cut(5, 1, 14, vec![]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight(None).unwrap(), h0);
        h.shutdown();
    });
}

/// A host adopts only its own in-flight rows (another host still re-commits its
/// own: adopting them too would deliver them twice); an all-in-one process adopts every
/// host's rows and its cut replaces them all.
#[test]
fn in_flight_adopted_per_host_or_all_in_one() {
    each_backend("inflight_adopt", false, |url| {
        let h = open(url);
        let f = fence(&h);
        let h0 = vec![row(A, false), row(B, true)];
        let h1 = vec![row(B, false)];
        h.commit_cut(cut(1, 0, 10, h0.clone()), f.clone()).recv().unwrap().unwrap();
        h.commit_cut(cut(2, 1, 11, h1.clone()), f.clone()).recv().unwrap().unwrap();
        assert_eq!(f.in_flight_scope(), Some(0));
        assert_eq!(h.load_in_flight(f.in_flight_scope()).unwrap(), h0, "host 0 adopts its own rows only");
        assert_eq!(h.load_in_flight(Some(1)).unwrap(), h1, "host 1 its own");
        // All-in-one adopted both hosts' rows: its cut (as host 0) replaces them all.
        let all = world_store::Fence { all_hosts: true, ..f.clone() };
        assert_eq!(all.in_flight_scope(), None);
        assert_eq!(h.load_in_flight(all.in_flight_scope()).unwrap().len(), 3);
        h.commit_cut(cut(3, 0, 12, h1.clone()), all).recv().unwrap().unwrap();
        assert_eq!(h.load_in_flight(None).unwrap(), h1, "only the all-in-one cut's rows are left");
        assert!(h.load_in_flight(Some(1)).unwrap().is_empty(), "host 1's old rows are gone");
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
