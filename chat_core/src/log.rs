use crate::CoreConfig;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::Level;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, fmt};
//✅稳定中
// 标记是否已初始化
static LOGGER_INIT: OnceLock<()> = OnceLock::new();
// 文件日志 guard（仅在文件日志模式下持有）
static LOGGER_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

const DEFAULT_LOG_LEVEL: Level = Level::WARN;
const DEBUG_LOG_LEVEL: Level = Level::DEBUG;
const MAX_LOG_FILES: usize = 7; // 保留最近7个日志文件
/// 初始化日志系统
///
/// 根据配置决定是否记录文件日志或控制台日志。
/// 可安全多次调用，后续调用会被忽略。
///
/// # 参数
/// * `cfg` - 核心配置，包含日志级别和日志路径信息
///
/// # 返回值
/// * `Ok(())` - 初始化成功或已经初始化
/// * `Err(anyhow::Error)` - 初始化失败（仅在第一次调用时可能失败）
pub fn init_logger(cfg: &CoreConfig) -> anyhow::Result<()> {
    // 防止重复初始化
    if LOGGER_INIT.set(()).is_err() {
        tracing::debug!("Logger already initialized, skipping");
        return Ok(());
    }

    let env_filter = build_filter(cfg.log_level.as_deref())?;

    match &cfg.path_to_log {
        Some(path) => {
            let guard = init_file_logger(path, &env_filter)?;
            LOGGER_GUARD.set(guard).ok();
        }
        None => init_console_logger(&env_filter)?,
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
            let default = level.unwrap_or({
                if cfg!(debug_assertions) {
                    DEBUG_LOG_LEVEL.as_str()
                } else {
                    DEFAULT_LOG_LEVEL.as_str()
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
/// * `path` - 日志文件存储路径（将被规范化）
/// * `filter` - 环境过滤器
///
/// # 返回值
/// * `Ok(WorkerGuard)` - 工作线程守卫，确保日志写入完成
/// * `Err(anyhow::Error)` - 初始化失败
fn init_file_logger(path: &Path, filter: &EnvFilter) -> anyhow::Result<WorkerGuard> {
    std::fs::create_dir_all(path).expect("Failed to create log directory");
    let safe_path = validate_log_path(path)?; // 规范化后的安全路径

    // 使用 tracing_appender 的滚动文件写入器，按日期轮转，保留 7 天日志
    // 日志文件格式: chat.log.YYYY-MM-DD
    // 当日志文件达到指定日期时自动创建新文件
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY) // 按天轮转
        .filename_prefix("chat") // 文件名前缀
        .max_log_files(MAX_LOG_FILES)
        .build(&safe_path)
        .map_err(|e| anyhow::anyhow!("Failed to create rolling file appender: {}", e))?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    fmt()
        .with_ansi(false)
        .with_writer(non_blocking)
        .with_env_filter(filter.clone())
        .try_init()
        .expect("Failed to init File logger");

    tracing::info!(
        filter = %filter,
        path = %safe_path.display(),
        "File logger initialized with daily rotation"
    );

    Ok(guard)
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
    fmt()
        .with_env_filter(filter.clone())
        .try_init()
        .map_err(|e| anyhow::anyhow!("Logger init failed: {}", e))?;

    tracing::info!("Console logger initialized");
    Ok(())
}

/// 验证日志路径的安全性
///
/// 检查路径中是否包含父目录遍历组件，并将路径规范化为绝对路径。
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

    // 尝试规范化为绝对路径
    // 如果路径本身不存在，则尝试规范化其父目录
    let canonical = path.canonicalize().or_else(|_| {
        path.parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.join(path.file_name().unwrap_or_default()))
            .ok_or_else(|| anyhow::anyhow!("Invalid log path: cannot resolve"))
    })?;

    Ok(canonical)
}
