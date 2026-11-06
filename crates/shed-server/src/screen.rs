//! `screen.watch`: a terminal's screen as a full update, then the rows that changed, at most 20 a second.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use shed_core::{Color, Snapshot, Terminal, Wake};
use shed_proto::{self as proto, Line, Run, ScreenUpdate};

/// The least time between two updates to one watcher: 20 a second.
const EVERY: Duration = Duration::from_millis(50);

fn color(c: Option<Color>) -> Option<proto::Color> {
    c.map(|c| match c {
        Color::Index(i) => proto::Color::Index(i),
        Color::Rgb(r, g, b) => proto::Color::Rgb([r, g, b]),
    })
}

fn run(r: &shed_core::Run) -> Run {
    let pen = r.pen;
    Run {
        text: r.text.clone(),
        fg: color(pen.fg),
        bg: color(pen.bg),
        bold: pen.bold,
        italic: pen.italic,
        underline: pen.underline,
        inverse: pen.inverse,
    }
}

/// The update from `last` (`None`: the client has nothing) to `now`, at change count `seq`; `None` when
/// nothing differs. A first update or a resize carries every row.
fn diff(last: Option<&Snapshot>, now: &Snapshot, seq: u64) -> Option<ScreenUpdate> {
    if last == Some(now) {
        return None;
    }
    let full = last.is_none_or(|l| l.size != now.size);
    let lines = (0..now.size.rows)
        .zip(&now.rows)
        .filter(|&(i, row)| full || last.and_then(|l| l.rows.get(usize::from(i))) != Some(row))
        .map(|(row, runs)| Line {
            row,
            runs: runs.iter().map(run).collect(),
        })
        .collect();
    Some(ScreenUpdate {
        seq,
        full,
        rows: now.size.rows,
        cols: now.size.cols,
        cursor: [now.cursor.0, now.cursor.1],
        scrollback: now.scrollback,
        lines,
    })
}

/// The update lines for `term` from now on; the channel closes once the program has exited.
pub(crate) fn watch(term: Arc<Terminal>, since: Option<u64>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || stream(&term, since, &tx));
    rx
}

fn stream(term: &Terminal, since: Option<u64>, tx: &Sender<Vec<u8>>) {
    let (mut seq, mut now) = term.snapshot();
    let mut last = (since == Some(seq)).then(|| now.clone());
    let mut exited = false;
    loop {
        if let Some(update) = diff(last.as_ref(), &now, seq) {
            // An update (numbers, strings and flags) always serialises.
            let mut line = serde_json::to_vec(&update).unwrap_or_default();
            line.push(b'\n');
            if tx.send(line).is_err() {
                return;
            }
            std::thread::sleep(EVERY);
        }
        if exited {
            return;
        }
        exited = !matches!(
            term.wait_until(|t| t.changes() != seq, None),
            Ok(Wake::Matched)
        );
        last = Some(now);
        (seq, now) = term.snapshot();
    }
}
