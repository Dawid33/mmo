//! The dialer's failure log is rate-limited. Its own test binary: a logger is process-global.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use svc_link::{LinkDialer, LinkHello, PeerRole};

static DIAL_FAILED_LINES: AtomicUsize = AtomicUsize::new(0);

struct Counter;
impl log::Log for Counter {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        let line = r.args().to_string();
        if line.contains("dial") && line.contains("failed") {
            DIAL_FAILED_LINES.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn flush(&self) {}
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dial_failures_log_once_per_period() {
    log::set_logger(&Counter).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    // A listener the dialer's pin does not match: every attempt fails fast (a closed UDP
    // port instead times out, 5 s per attempt), so 3 s holds many attempts at the 250 ms minimum.
    let (listener, _from_peers, _senders) =
        svc_link::LinkListener::bind::<u64, u64>(([127, 0, 0, 1], 0).into(), "t".into(), String::new(), None).unwrap();
    let addr = listener.local_addr();
    let hello = LinkHello::new("t".to_string(), PeerRole::Coordinator, "");
    let (_tx, _rx) = LinkDialer::connect::<u64, u64>(addr, "00".repeat(32), hello);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(DIAL_FAILED_LINES.load(Ordering::SeqCst), 1, "one line for the whole down period");
}
