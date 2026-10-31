//! Prometheus metrics, text format, on a tiny HTTP listener (`MORIA_METRICS_BIND`).
//! Process-wide atomics: one server per process in production.
// ponytail: hand-rolled exposition, no histogram; switch to the `prometheus` crate if
// quantiles are ever needed.

use std::fmt::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
pub struct Metrics {
    pub sessions: AtomicI64,
    pub connections: AtomicI64,
    pub accounts: AtomicI64,
    pub fortresses: AtomicI64,
    pub nats_connected: AtomicI64,
    pub ticks: AtomicU64,
    pub tick_nanos: AtomicU64,
    pub tick_overruns: AtomicU64,
    pub rejected_full: AtomicU64,
    pub views_published: AtomicU64,
    pub assignments_received: AtomicU64,
    pub assignments_rejected: AtomicU64,
    pub save_failures: AtomicU64,
}

pub static M: Metrics = Metrics {
    sessions: AtomicI64::new(0),
    connections: AtomicI64::new(0),
    accounts: AtomicI64::new(0),
    fortresses: AtomicI64::new(0),
    nats_connected: AtomicI64::new(0),
    ticks: AtomicU64::new(0),
    tick_nanos: AtomicU64::new(0),
    tick_overruns: AtomicU64::new(0),
    rejected_full: AtomicU64::new(0),
    views_published: AtomicU64::new(0),
    assignments_received: AtomicU64::new(0),
    assignments_rejected: AtomicU64::new(0),
    save_failures: AtomicU64::new(0),
};

pub fn inc(c: &AtomicU64) {
    c.fetch_add(1, Relaxed);
}

pub fn render(m: &Metrics) -> String {
    let mut s = String::new();
    let mut put = |name: &str, kind: &str, help: &str, v: String| {
        let _ = writeln!(s, "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {v}");
    };
    let g = |a: &AtomicI64| a.load(Relaxed).to_string();
    let c = |a: &AtomicU64| a.load(Relaxed).to_string();
    put(
        "moria_sessions",
        "gauge",
        "Joueurs connectes.",
        g(&m.sessions),
    );
    put(
        "moria_connections",
        "gauge",
        "Connexions TCP ouvertes.",
        g(&m.connections),
    );
    put("moria_accounts", "gauge", "Comptes crees.", g(&m.accounts));
    put(
        "moria_fortresses",
        "gauge",
        "Forteresses revendiquees.",
        g(&m.fortresses),
    );
    put(
        "moria_nats_connected",
        "gauge",
        "1 si le pont NATS est connecte.",
        g(&m.nats_connected),
    );
    put(
        "moria_ticks_total",
        "counter",
        "Ticks de simulation.",
        c(&m.ticks),
    );
    let secs = m.tick_nanos.load(Relaxed) as f64 / 1e9;
    put(
        "moria_tick_seconds_total",
        "counter",
        "Temps passe dans les ticks.",
        secs.to_string(),
    );
    put(
        "moria_tick_overruns_total",
        "counter",
        "Ticks plus longs que 100 ms.",
        c(&m.tick_overruns),
    );
    put(
        "moria_connections_rejected_total",
        "counter",
        "Connexions refusees (plein).",
        c(&m.rejected_full),
    );
    put(
        "moria_ai_views_published_total",
        "counter",
        "Vues publiees vers dwarf-ai.",
        c(&m.views_published),
    );
    put(
        "moria_ai_assignments_received_total",
        "counter",
        "Assignations acceptees de dwarf-ai.",
        c(&m.assignments_received),
    );
    put(
        "moria_ai_assignments_rejected_total",
        "counter",
        "Assignations rejetees.",
        c(&m.assignments_rejected),
    );
    put(
        "moria_save_failures_total",
        "counter",
        "Sauvegardes du monde echouees.",
        c(&m.save_failures),
    );
    s
}

/// Answers every request with the metrics; runs until the process exits.
pub async fn serve(bind: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tracing::info!("metriques sur http://{bind}/metrics");
    loop {
        let Ok((mut s, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await; // request ignored: one resource
            let body = render(&M);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes()).await;
            let _ = s.write_all(body.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_exposes_every_metric() {
        let m = Metrics::default();
        m.sessions.store(3, Relaxed);
        inc(&m.ticks);
        m.tick_nanos.store(250_000_000, Relaxed);
        let out = render(&m);
        assert!(out.contains("moria_sessions 3\n"));
        assert!(out.contains("moria_ticks_total 1\n"));
        assert!(out.contains("moria_tick_seconds_total 0.25\n"));
        assert_eq!(out.matches("# TYPE ").count(), 13);
    }
}
