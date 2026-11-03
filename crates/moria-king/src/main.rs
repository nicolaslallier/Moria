use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        // Pas de couleurs hors terminal : Docker → Alloy → Loki garde un texte lisible.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
    let url = env("AMQP_URL", "amqp://moria:moria@rabbitmq:5672/moria");
    let llm = moria_king::Ollama {
        url: env("OLLAMA_URL", "http://192.168.2.35:11434"),
        model: env("OLLAMA_MODEL", "gemma4:26b-a4b-it-qat"),
    };
    tracing::info!("roi : modèle {} sur {}", llm.model, llm.url);
    moria_king::run(url, llm, shutdown_signal()).await
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
