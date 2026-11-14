//! # tauri-plugin-rootcell
//!
//! 包装 [`rootcell`] 身份/密钥环能力的 Tauri 插件：
//!
//! - **命令**：`is_keyring_available`（经 capabilities 授权）。
//!
//! Android 平台的 NDK 上下文（JVM/Context）由 Tauri 运行时（tao）在启动时注入
//! `ndk_context`，rootcell 的 Android KeyStore 存储直接读取该全局上下文，无需宿主
//! 自行初始化。

use tauri::{plugin::Builder, Runtime};

/// 密钥环是否可用（前端据此提示用户安装系统密钥环）
#[tauri::command]
fn is_keyring_available() -> bool {
    rootcell::identity::PrivateKeyHandle::check_keyring_available()
}

/// 注册插件：命令 `is_keyring_available` 需在 capabilities 中授权
/// （`rootcell:default` → `allow-is-keyring-available`）。
pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    Builder::new("rootcell")
        .invoke_handler(tauri::generate_handler![is_keyring_available])
        .build()
}
