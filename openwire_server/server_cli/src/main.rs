use std::path::Path;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).map(|s| Path::new(&s).to_path_buf());
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(44909);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(openwire_server_common::relay(dir.as_deref(), port))
}
