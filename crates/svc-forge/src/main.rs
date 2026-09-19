use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(about = "Local web forge for semantic repositories")]
struct Args {
    /// JSON forge catalog produced by svc or maintained by another local integration.
    #[arg(long, default_value = ".svc/forge.json")]
    catalog: PathBuf,
    /// Loopback address to serve. Passing a non-loopback address requires --allow-remote.
    #[arg(long, default_value = "127.0.0.1:7742")]
    bind: SocketAddr,
    #[arg(long)]
    allow_remote: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !args.bind.ip().is_loopback() && !args.allow_remote {
        return Err("refusing a non-loopback bind without --allow-remote".into());
    }
    let catalog = svc_forge::Catalog::load(&args.catalog)?;
    let listener = tokio::net::TcpListener::bind(args.bind).await?;
    eprintln!("svc forge: http://{}", listener.local_addr()?);
    axum::serve(listener, svc_forge::app(catalog)).await?;
    Ok(())
}
