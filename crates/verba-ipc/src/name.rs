//! 本地 socket 命名与平台分支。
//!
//! 安全背景（架构审查 P0-1）：`GenericNamespaced` 在 macOS 落到写死的
//! `/tmp/{name}`（全局共享、粘滞位目录），Linux 为 abstract socket
//! （无 inode 权限，任何用户可连），Windows 为机器级全局管道
//! `\\.\pipe\{name}`——均可被其他用户预占（永久 DoS）或假冒 daemon
//! 窃取 API key / 提示词 / 截图 / 录音。因此：
//!
//! - Unix：socket 放**用户数据目录**（macOS `~/Library/Application Support/Verba`、
//!   Linux `~/.local/share/verba`），daemon 启动时创建目录并 chmod 0700，
//!   跨用户不可见不可连；命名用 `FilesystemUdSocket`（完整路径）。
//! - Windows：管道名带用户名后缀 `verba-ime-{USERNAME}`（per-user 隔离）。

use std::path::PathBuf;

/// 默认 socket 规格：
/// - Unix：用户数据目录下 socket 的完整路径（`GenericFilePath` 原样使用）
/// - Windows：`\\.\pipe\verba-ime-{USERNAME}-{token}`（per-user + 不可预测后缀管道）
pub fn default_socket_spec() -> String {
    #[cfg(unix)]
    {
        socket_path().display().to_string()
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME")
            .unwrap_or_else(|_| "default".into())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        format!(r"\\.\pipe\verba-ime-{user}-{}", ipc_token())
    }
}

/// 连接令牌：daemon 首次启动生成并写入用户数据目录（0700），client 读取后拼入
/// 管道名。管道名空间是机器级全局的，per-user 用户名不阻止其他用户预占
/// （`FILE_FLAG_FIRST_PIPE_INSTANCE` 下同名已存在 → daemon bind 失败 = 永久 DoS）；
/// 不可预测后缀使预占者无法提前创建目标管道（架构审查 P1）。文件缺失时
/// 返回固定后缀（首启兼容：client 连不到 → 拉 daemon → daemon 生成 token 后重试）。
#[cfg(windows)]
fn ipc_token() -> String {
    let data_dir = directories::ProjectDirs::from("dev", "verba", "Verba")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("verba-ime"));
    let token_path = data_dir.join("ipc-token");
    if let Ok(s) = std::fs::read_to_string(&token_path) {
        let t = s.trim().to_owned();
        if t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return t;
        }
    }
    "00000000000000000000000000000000".into()
}

/// 生成并持久化连接令牌（daemon 启动时调用；Unix 无管道名问题，返回空）。
#[cfg(windows)]
pub fn ensure_ipc_token() -> std::io::Result<()> {
    let data_dir = directories::ProjectDirs::from("dev", "verba", "Verba")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("verba-ime"));
    std::fs::create_dir_all(&data_dir)?;
    let token_path = data_dir.join("ipc-token");
    if token_path.exists() {
        return Ok(());
    }
    let mut bytes = [0u8; 16];
    // 无 rand 依赖：用系统时间 + pid + 地址熵（本地防预占足够；无需密码学强度）
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 32;
    let mut s = seed;
    for b in bytes.iter_mut() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        *b = (s.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u8;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&token_path, hex)
}

/// Unix 下用户数据目录中的 socket 完整路径（与 [`crate::name::default_socket_spec`] 对应）。
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    let data_dir = directories::ProjectDirs::from("dev", "verba", "Verba")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("verba-ime"));
    data_dir.join("verba-ipc.sock")
}
