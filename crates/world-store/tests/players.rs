//! Player ids are assigned by the database, safe under concurrent first
//! resolves (no in-process writer lock to lean on).

use std::collections::{BTreeMap, BTreeSet};

use world_store::{StoreHandle, StoreUrl};

use crate::handle::{each_backend, open};

const PER_THREAD: usize = 50;

#[test]
fn concurrent_resolve_assigns_distinct_ids() {
    each_backend("players", false, |url| {
        // Postgres: two handles (two pools — as two processes would be); SQLite
        // allows one writer process (the file lock), so one handle cloned.
        let h = open(url);
        let second = match url {
            StoreUrl::Postgres(_) => open(url),
            _ => h.clone(),
        };
        let run = |tag: &'static str, h: &StoreHandle| {
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
        let (a, b) = (run("a", &h), run("b", &second));
        let got: BTreeMap<String, _> = a.join().unwrap().into_iter().chain(b.join().unwrap()).collect();
        assert_eq!(got.len(), 2 * PER_THREAD);
        let ids: BTreeSet<_> = got.values().map(|id| id.0).collect();
        assert_eq!(ids.len(), 2 * PER_THREAD, "every new token gets its own id");
        for (key, id) in &got {
            assert_eq!(h.resolve_player(key).unwrap().0, *id, "{key} keeps its id");
        }
        // The same token resolved from many threads (both handles) at once agrees on one id.
        let same: BTreeSet<_> = (0..8)
            .map(|n| {
                let h = if n % 2 == 0 { h.clone() } else { second.clone() };
                std::thread::spawn(move || h.resolve_player("local:shared").unwrap().0.0)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert_eq!(same.len(), 1);
        second.shutdown();
        h.shutdown();
    });
}
