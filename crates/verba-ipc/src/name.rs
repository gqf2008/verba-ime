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

#[cfg(unix)]
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

/// 用户数据目录（三处共用统一实现与降级路径；daemon/前端同源）。
fn verba_data_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("dev", "verba", "Verba")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("verba-ime"))
}

/// 令牌合法性：32 位十六进制。
#[cfg(windows)]
fn is_valid_token(t: &str) -> bool {
    t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// 本地熵（无 rand 依赖）：系统时间 + pid 异或后过 xorshift64*——仅供「不可
/// 预测命名 / 会话盐」这类本地防预占、防碰撞用途，不作密码学强度承诺。
/// 前端（IMK/TSF）进程盐与本模块令牌共用此实现（复用评审：三处内联合一）。
pub fn local_entropy_u64() -> u64 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 32;
    let mut s = seed;
    s ^= s >> 12;
    s ^= s << 25;
    s ^= s >> 27;
    s.wrapping_mul(0x2545F4914F6CDD1D)
}

/// 连接令牌：daemon 首次启动生成并写入用户数据目录（0700），client 读取后拼入
/// 管道名。管道名空间是机器级全局的，per-user 用户名不阻止其他用户预占
/// （`FILE_FLAG_FIRST_PIPE_INSTANCE` 下同名已存在 → daemon bind 失败 = 永久 DoS）；
/// 从本地熵源生成 16 字节 xorshift 流并序列化为 32 位十六进制（两个
/// 调用点共用；抽取前为逐字重复块，曾修一处漏一处——复审 D7）。
#[cfg(windows)]
fn random_token_hex() -> String {
    let mut s = local_entropy_u64();
    let mut bytes = [0u8; 16];
    for b in bytes.iter_mut() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        *b = (s.wrapping_mul(0x2545F4914F6CDD1D) >> 32) as u8;
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 不可预测后缀使预占者无法提前创建目标管道（架构审查 P1）。文件缺失时
/// 返回**一次性随机后缀**（每次调用不同）：首启期间客户端每次 connect 落到
/// 不同名称上，全部失败即拉起 daemon（其生成真 token 后重试读到）。固定
/// 全零后缀会把所有首启连接钉在可预占的固定名称上：一旦被预占，Windows 下
/// connect 永不失败（握手无 I/O 超时），UI 线程随之无限阻塞（复审发现）。
#[cfg(windows)]
fn ipc_token() -> String {
    let token_path = verba_data_dir().join("ipc-token");
    if let Ok(s) = std::fs::read_to_string(&token_path) {
        let t = s.trim().to_owned();
        if is_valid_token(&t) {
            return t;
        }
    }
    // 一次性随机后缀（不可预测、不持久化）：语义同真实 token。
    random_token_hex()
}

/// 生成并持久化连接令牌（daemon 启动时调用；Unix 无管道名问题，返回空）。
#[cfg(windows)]
pub fn ensure_ipc_token() -> std::io::Result<()> {
    let data_dir = verba_data_dir();
    std::fs::create_dir_all(&data_dir)?;
    let token_path = data_dir.join("ipc-token");
    // 仅当文件存在**且内容合法**（32 位十六进制）才复用。崩溃/断电留下 0 字节或
    // 截断文件时，`ipc_token()` 会拒绝该内容并退回随机后缀（管道名不可预测 → 防预占
    // 失效）；此处若只看 exists() 会永久卡住不重建（复审 sweep）。内容非法则重建。
    if let Ok(s) = std::fs::read_to_string(&token_path) {
        if is_valid_token(s.trim()) {
            return Ok(());
        }
        log::warn!(
            "ipc-token 内容非法（截断/损坏），重新生成: {}",
            token_path.display()
        );
    }
    let hex = random_token_hex();
    std::fs::write(&token_path, hex)
}

/// Unix 下用户数据目录中的 socket 完整路径（与 [`crate::name::default_socket_spec`] 对应）。
#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    verba_data_dir().join("verba-ipc.sock")
}
