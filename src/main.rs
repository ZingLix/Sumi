use sqlx::postgres::PgPoolOptions;
use sumi::{auth::Auth, config::PublicUrl, server, store::Store};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sumi=info,tower_http=info".into()),
        )
        .init();
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    if std::env::args().nth(1).as_deref() == Some("migrate") {
        sqlx::migrate!().run(&pool).await?;
        tracing::info!("Migrations applied");
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("restore") {
        let path = std::env::args()
            .nth(2)
            .ok_or_else(|| anyhow::anyhow!("Usage: sumi restore export.json"))?;
        let document = serde_json::from_slice(&tokio::fs::read(path).await?)?;
        let ledger_id = sumi::store::restore_export(&pool, document).await?;
        tracing::info!(%ledger_id,"Personal ledger restored");
        return Ok(());
    }
    let store = Store::new(pool);
    store.verify_runtime_role().await?;
    let jwks =
        serde_json::from_slice(&tokio::fs::read(std::env::var("SUMI_OAUTH_JWKS_FILE")?).await?)?;
    let public_url = PublicUrl::from_env()?;
    let auth = Auth::new(
        std::env::var("SUMI_OAUTH_ISSUER")?,
        public_url.resource_url.clone(),
        jwks,
    )?;
    let bind = std::env::var("SUMI_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(address=%bind, resource=%public_url.resource_url,"Sumi MCP listening; terminate HTTPS at the reverse proxy");
    axum::serve(listener, server::router(store, auth))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Cannot install SIGTERM handler");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
