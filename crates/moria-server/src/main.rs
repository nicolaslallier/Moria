use moria_server::{Config, MAX_CONNECTIONS_PER_IP, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // Pas de couleurs hors terminal : Docker → Alloy → Loki garde un texte lisible.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
    let max_per_ip: usize = env("MORIA_MAX_PER_IP", &MAX_CONNECTIONS_PER_IP.to_string())
        .parse()
        .map_err(|e| anyhow::anyhow!("MORIA_MAX_PER_IP invalide : {e}"))?;
    anyhow::ensure!(max_per_ip > 0, "MORIA_MAX_PER_IP doit être au moins 1");
    let cfg = Config {
        data: env("MORIA_DATA", "/data/world.bin").into(),
        bind: env("MORIA_BIND", "0.0.0.0:7777").parse()?,
        nats_url: Some(env("NATS_URL", "nats://nats:4222")),
        invite: std::env::var("MORIA_INVITE").ok().filter(|s| !s.is_empty()),
        max_per_ip,
    };
    let metrics_bind: std::net::SocketAddr = env("MORIA_METRICS_BIND", "0.0.0.0:9464").parse()?;
    tokio::spawn(async move {
        if let Err(e) = moria_server::metrics::serve(metrics_bind).await {
            tracing::error!("metriques indisponibles : {e}");
        }
    });
    run(cfg, shutdown_signal()).await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
