//! Player ids are assigned by the database, safe under concurrent first
//! resolves (no in-process writer lock to lean on).

use std::collections::{BTreeMap, BTreeSet};

use crate::handle::{each_backend, open};

const PER_THREAD: usize = 50;

#[test]
fn concurrent_resolve_assigns_distinct_ids() {
    each_backend("players", false, |url| {
        // TODO(Task 2): two StoreHandles on one database once the advisory lock is gone.
        // One handle cloned across two threads: the world lock allows one open
        // store per database until the owner-epoch fence replaces it (Task 2).
        let h = open(url);
        let run = |tag: &'static str| {
            let h = h.clone();
            std::thread::spawn(move || {
                (0..PER_THREAD)
                    .map(|n| {
                        let key = format!("local:{tag}{n}");
                        let (id, rec) = h.resolve_player(&key).unwrap();
                        assert_eq!(rec, None);
                        (key, id)
                    })
                    .collect::<Vec<_>>()
            })
        };
        let (a, b) = (run("a"), run("b"));
        let got: BTreeMap<String, _> = a.join().unwrap().into_iter().chain(b.join().unwrap()).collect();
        assert_eq!(got.len(), 2 * PER_THREAD);
        let ids: BTreeSet<_> = got.values().map(|id| id.0).collect();
        assert_eq!(ids.len(), 2 * PER_THREAD, "every new token gets its own id");
        for (key, id) in &got {
            assert_eq!(h.resolve_player(key).unwrap().0, *id, "{key} keeps its id");
        }
        // The same token resolved from many threads at once agrees on one id.
        let same: BTreeSet<_> = (0..8)
            .map(|_| {
                let h = h.clone();
                std::thread::spawn(move || h.resolve_player("local:shared").unwrap().0.0)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert_eq!(same.len(), 1);
        h.shutdown();
    });
}
