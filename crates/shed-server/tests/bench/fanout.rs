//! The event fan-out bench (`scripts/bench-fanout.sh` runs it at a PR's base and head): a burst of
//! events to N subscribers, then each subscriber's stream drained. One thread, so the time is the
//! fan-out's (serialising, copying or sharing, sending, freeing), not the scheduler's.

use std::time::Instant;

use super::*;

/// Events per burst.
const BURST: usize = 5_000;

#[test]
#[ignore = "a timing bench: scripts/bench-fanout.sh"]
fn fanout() {
    let labels = Labels::from(
        [("role", "impl"), ("fleet", "shed"), ("issue", "41")].map(|(k, v)| (k.into(), v.into())),
    );
    let mut total = 0;
    for n in [1, 16, 64, 256] {
        // The best of 3 bursts.
        let us = (0..3)
            .map(|_| {
                let mut events = Events::default();
                let subs: Vec<_> = (0..n)
                    .map(|_| events.subscribe(EventsParams::default()))
                    .collect();
                let start = Instant::now();
                for _ in 0..BURST {
                    events.emit("agent-terminal-1", &labels, EventKind::Started);
                }
                // Each subscriber takes what the burst left it.
                drop(events);
                let bytes: Vec<usize> = subs
                    .into_iter()
                    .map(|mut sub| sub.take().iter().map(|line| line.len()).sum())
                    .collect();
                let us = start.elapsed().as_micros();
                assert!(bytes.iter().all(|&b| b == bytes[0] && b > BURST));
                us
            })
            .min()
            .unwrap_or_default();
        println!("fanout n={n}: {us} us");
        total += us;
    }
    println!("fanout total: {total} us");
}
