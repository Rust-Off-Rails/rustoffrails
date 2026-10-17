//! With the realtime sample installed (optional wiring: `pub mod ai_realtime;` at `dashboard mods`
//! and `Box::new(…::Realtime)` at `dashboard ai sources`): what Ask AI may know about
//! `/dev/sockets`. Without realtime, no module declares this file, so it isn't built.
//!
//! Counts, rates, transports and topic names (redacted); never people's names, connection keys
//! or labels, or frames.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::ai_context::{Source, SourceFuture, clean};
use crate::state::AppState;

/// The hub's snapshot, as shapes.
pub struct Realtime;

impl Source for Realtime {
    fn tab(&self) -> &'static str {
        "sockets"
    }

    fn context<'a>(&'a self, state: &'a AppState) -> SourceFuture<'a> {
        Box::pin(async move {
            let snap = state.hub.snapshot();
            let mut s = format!(
                "## Realtime hub (node {}, {} nodes)\n{} connections; frames in {} ({:.1}/s), out {} ({:.1}/s); restarts {}; frame logging {}\n",
                clean(&snap.node, 60),
                snap.nodes.len(),
                snap.connections,
                snap.frames_in,
                snap.in_per_sec,
                snap.frames_out,
                snap.out_per_sec,
                snap.restarts,
                if snap.logging { "on" } else { "off" }
            );
            let mut transports: BTreeMap<&str, usize> = BTreeMap::new();
            for c in &snap.conns {
                *transports.entry(c.transport).or_default() += 1;
            }
            let _ = writeln!(
                s,
                "Transports: {}",
                transports
                    .iter()
                    .map(|(t, n)| format!("{t} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for n in snap.nodes.iter().filter(|n| !n.this) {
                let _ = writeln!(s, "- node seen {} s ago", n.seen_secs_ago);
            }
            let mut topics = snap.topics.clone();
            topics.sort_by_key(|t| std::cmp::Reverse(t.connections));
            let _ = writeln!(s, "\n### Topics ({}), busiest first", topics.len());
            for t in topics.iter().take(20) {
                let _ = writeln!(
                    s,
                    "- {}: {} connections, {} people present, {} broadcasts{}",
                    clean(&t.topic, 80),
                    t.connections,
                    t.people.len(),
                    t.broadcasts,
                    if t.room_running { ", room running" } else { "" }
                );
            }
            Some(s)
        })
    }
}
