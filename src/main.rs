mod bytecode;
mod extract;
mod index;
mod server;

use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "ijdocs-server")]
#[command(about = "IntelliJ-style javadoc server for nvim")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:63343")]
    listen: SocketAddr,

    #[arg(long)]
    extra_root: Vec<PathBuf>,

    #[arg(long, default_value_t = false)]
    no_default_roots: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ijdocs_server=info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();

    let mut roots = Vec::new();
    if !args.no_default_roots {
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".m2/repository"));
            roots.push(home.join(".gradle/caches/modules-2/files-2.1"));
        }
    }
    roots.extend(args.extra_root);
    roots.retain(|p| p.exists());

    tracing::info!(?roots, "discovering javadoc jars");
    let idx = index::Index::build(&roots)?;
    tracing::info!(jars = idx.jar_count(), classes = idx.class_count(), "index built");

    let state = Arc::new(server::AppState { index: idx });
    let app = server::router(state);

    tracing::info!(listen = %args.listen, "starting server");
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
