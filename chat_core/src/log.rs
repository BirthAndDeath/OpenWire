use crate::CoreConfig;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt};

static LOGGER_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// 初始化日志系统
///
/// 根据配置决定是否记录文件日志或控制台日志
///
/// # 参数
/// * `cfg` - 核心配置，包含日志级别和日志路径信息
///
/// # 返回值
/// * `Ok(())` - 初始化成功
/// * `Err(anyhow::Error)` - 初始化失败
pub fn init_logger(cfg: &CoreConfig) -> anyhow::Result<()> {
    let env_filter = build_filter(cfg.log_level.as_deref())?;

    if let Some(path) = &cfg.path_to_log {
        let guard = LOGGER_GUARD.get_or_init(|| init_file_logger(path, &env_filter));
        // 确保 guard 被引用，避免 Drop
        let _ = guard;
    } else {
        init_console_logger(&env_filter)?;
    }

    Ok(())
}

/// 构建环境过滤器
///
/// 优先从环境变量获取过滤规则，如果环境变量未设置则使用默认级别
///
/// # 参数
/// * `level` - 可选的日志级别字符串
///
/// # 返回值
/// * `Ok(EnvFilter)` - 成功构建的环境过滤器
/// * `Err(anyhow::Error)` - 过滤器构建失败
fn build_filter(level: Option<&str>) -> anyhow::Result<EnvFilter> {
    EnvFilter::try_from_default_env()
        .or_else(|_| {
            let default = level.unwrap_or_else(|| {
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "info"
                }
            });
            EnvFilter::try_new(default)
        })
        .map_err(|e| anyhow::anyhow!("Invalid log filter: {}", e))
}

/// 初始化文件日志记录器
///
/// 创建日志目录并设置非阻塞文件写入器
///
/// # 参数
/// * `path` - 日志文件存储路径
/// * `filter` - 环境过滤器
///
/// # 返回值
/// * `WorkerGuard` - 工作线程守卫，确保日志写入完成
fn init_file_logger(path: &Path, filter: &EnvFilter) -> WorkerGuard {
    validate_log_path(path).expect("Invalid log path");

    std::fs::create_dir_all(path).expect("Failed to create log directory");

    let log_file = path.join("app.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .expect("Failed to open log file");

    let (non_blocking, guard) = tracing_appender::non_blocking(file);

    fmt()
        .with_writer(non_blocking)
        .with_env_filter(filter.clone())
        .try_init()
        .expect("logger init error");

    tracing::info!(
        filter = %filter,
        path = %path.display(),
        "File logger initialized"
    );

    guard
}

/// 初始化控制台日志记录器
///
/// 设置控制台输出格式和过滤器
///
/// # 参数
/// * `filter` - 环境过滤器
///
/// # 返回值
/// * `Ok(())` - 初始化成功
/// * `Err(anyhow::Error)` - 初始化失败
fn init_console_logger(filter: &EnvFilter) -> anyhow::Result<()> {
    // try_init 避免重复初始化 panic
    fmt()
        .with_env_filter(filter.clone())
        .try_init()
        .map_err(|e| anyhow::anyhow!("Logger init failed: {}", e))?;

    tracing::info!("Console logger initialized");
    Ok(())
}

/// 验证日志路径的安全性
///
/// 检查路径中是否包含父目录遍历组件，防止路径穿越攻击
///
/// # 参数
/// * `path` - 待验证的路径
///
/// # 返回值
/// * `Ok(PathBuf)` - 验证通过的规范路径
/// * `Err(anyhow::Error)` - 路径验证失败
fn validate_log_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("Path traversal detected in log path");
    }

    // 验证父目录
    let canonical_base = path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_else(|| path.to_path_buf());

    Ok(canonical_base)
}
