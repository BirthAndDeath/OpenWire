use std::path::Path;

fn print_usage() {
    println!("OpenWire Relay Server v{}", env!("CARGO_PKG_VERSION"));
    println!("用法 / Usage: openwire-server-cli [data_dir] [port]");
    println!("示例 / Examples:");
    println!("  openwire-server-cli                      # 默认数据目录，默认端口 44909（成功后持久化）");
    println!("  openwire-server-cli /path/to/data 44909  # 指定数据目录和端口");
    println!("  openwire-server-cli /path/to/data        # 指定数据目录，使用持久化端口或默认 44909");
    println!("参数 / Args:");
    println!("  data_dir  数据目录，存放密钥/DB/配置 / data directory for keys, DB and config");
    println!("  port      监听端口，缺省时使用上次持久化端口，否则默认 44909 / listen port (default 44909)");
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        print_usage();
        return Ok(());
    }

    let dir = args.get(1).map(|s| Path::new(&s).to_path_buf());

    let port: Option<u16> = match args.get(2) {
        Some(raw) => Some(raw.parse().map_err(|_| {
            anyhow::anyhow!("端口无效 / invalid port '{}'", raw)
        })?),
        None => None,
    };

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        tokio::select! {
            r = openwire_server_common::relay(dir.as_deref(), port) => r,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("收到 SIGINT，正在关闭中继服务器...");
                Ok(())
            }
        }
    });
    tracing::info!("中继服务器已关闭");
    result
}