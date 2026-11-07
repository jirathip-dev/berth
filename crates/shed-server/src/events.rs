//! The events stream: every terminal's starts, states, exits, kills and resource moves, numbered, the
//! last [`RING`] kept for replay.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use shed_proto::{Event, EventKind, EventsParams};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How many events are kept for `since`.
pub(crate) const RING: usize = 1_000;

type Labels = BTreeMap<String, String>;

/// One event's JSON line, serialised once and shared by the ring and every subscriber.
pub(crate) type Line = Arc<[u8]>;

/// The numbered events and the connections streaming them.
#[derive(Default)]
pub(crate) struct Events {
    seq: u64,
    ring: VecDeque<(Event, Line)>,
    /// Each subscriber's label filter and its stream of event lines.
    subs: Vec<(Labels, UnboundedSender<Line>)>,
}

fn matches(filter: &Labels, event: &Event) -> bool {
    filter.iter().all(|(k, v)| event.labels.get(k) == Some(v))
}

/// One event as a JSON line.
fn line(event: &Event) -> Line {
    // An event (strings, numbers and maps) always serialises.
    let mut line = serde_json::to_vec(event).unwrap_or_default();
    line.push(b'\n');
    line.into()
}

impl Events {
    /// Numbers, keeps and streams one event of terminal `name`.
    pub(crate) fn emit(&mut self, name: &str, labels: &Labels, kind: EventKind) {
        self.emit_as(name, labels, kind, false);
    }

    /// [`Self::emit`], flagged `replay` for a resumed ACP session's history.
    pub(crate) fn emit_as(&mut self, name: &str, labels: &Labels, kind: EventKind, replay: bool) {
        self.seq += 1;
        let event = Event {
            seq: self.seq,
            name: name.to_owned(),
            labels: labels.clone(),
            kind,
            replay,
        };
        let bytes = line(&event);
        self.subs
            .retain(|(filter, tx)| !matches(filter, &event) || tx.send(Arc::clone(&bytes)).is_ok());
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back((event, bytes));
    }

    /// The kept events after `p.since` that match `p.labels`, and the matching ones from now on.
    pub(crate) fn subscribe(&mut self, p: EventsParams) -> (Vec<u8>, UnboundedReceiver<Line>) {
        let replay = match p.since {
            Some(since) => self
                .ring
                .iter()
                .filter(|(e, _)| e.seq > since && matches(&p.labels, e))
                .flat_map(|(_, line)| line.iter().copied())
                .collect(),
            None => Vec::new(),
        };
        let (tx, rx) = unbounded_channel();
        self.subs.push((p.labels, tx));
        (replay, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_last_thousand_for_since() {
        let mut events = Events::default();
        let labels = Labels::from([("role".to_owned(), "impl".to_owned())]);
        for _ in 0..RING + 5 {
            events.emit("t", &labels, EventKind::Started);
        }
        let all = |events: &mut Events, since| {
            let (replay, _) = events.subscribe(EventsParams {
                labels: labels.clone(),
                since: Some(since),
            });
            let text = String::from_utf8(replay).expect("utf-8");
            text.lines()
                .map(|l| serde_json::from_str::<Event>(l).expect("event").seq)
                .collect::<Vec<_>>()
        };
        let seqs = all(&mut events, 0);
        assert_eq!(seqs.len(), RING);
        assert_eq!((seqs[0], seqs[RING - 1]), (6, 1_005));
        assert_eq!(all(&mut events, 1_003), [1_004, 1_005]);
    }

    #[test]
    fn every_subscriber_shares_the_one_serialised_line() {
        let mut events = Events::default();
        let mut rxs: Vec<_> = (0..3)
            .map(|_| events.subscribe(EventsParams::default()).1)
            .collect();
        events.emit("t", &Labels::new(), EventKind::Started);
        let lines: Vec<Line> = rxs
            .iter_mut()
            .map(|rx| rx.try_recv().expect("line"))
            .collect();
        let kept = &events.ring[0].1;
        assert!(lines.iter().all(|l| Arc::ptr_eq(l, kept)));
        // The ring and the three subscribers hold the one allocation.
        assert_eq!(Arc::strong_count(kept), 4);
    }
}

#[cfg(test)]
#[path = "../tests/bench/fanout.rs"]
mod bench;
