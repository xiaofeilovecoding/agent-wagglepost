use crate::model::*;
use crate::store::Store;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Event bus: persists events with monotonic seq, fans out to SSE subscribers.
pub struct EventBus {
    pub store: Store,
    tx: broadcast::Sender<EventRow>,
    counter: Mutex<u64>,
    /// flight-triggering event kinds (production animation subscribes to these)
    pub flight_kinds: Vec<&'static str>,
    seq_padding: AtomicU64,
}

impl EventBus {
    pub fn new(store: Store) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(1024);
        let max = store.max_seq();
        Arc::new(EventBus {
            store,
            tx,
            counter: Mutex::new(max),
            flight_kinds: vec![
                "attempt_started",
                "reroute_started",
                "result_collected",
            ],
            seq_padding: AtomicU64::new(0),
        })
    }

    pub fn emit(
        &self,
        task_id: Option<&str>,
        attempt_id: Option<&str>,
        generation: u64,
        kind: &str,
        payload: serde_json::Value,
    ) -> EventRow {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let seq = *counter;
        drop(counter);
        let e = EventRow {
            seq,
            task_id: task_id.map(|s| s.to_string()),
            attempt_id: attempt_id.map(|s| s.to_string()),
            generation,
            kind: kind.to_string(),
            payload,
            ts: crate::store::timestamp(),
        };
        let _ = self.store.append_event(&e);
        // not sent to self; broadcast best-effort
        let _ = self.tx.send(e.clone());
        e
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventRow> {
        self.tx.subscribe()
    }

    pub fn current_seq(&self) -> u64 {
        *self.counter.lock().unwrap()
    }
}

/// SSE payload wire format
pub fn sse_line(e: &EventRow) -> String {
    let body = serde_json::json!({
        "seq": e.seq,
        "task_id": e.task_id,
        "attempt_id": e.attempt_id,
        "generation": e.generation,
        "kind": e.kind,
        "payload": e.payload,
        "ts": e.ts,
    });
    format!(
        "id: {}\nevent: {}\ndata: {}\n\n",
        e.seq,
        e.kind,
        serde_json::to_string(&body).unwrap_or_default()
    )
}
