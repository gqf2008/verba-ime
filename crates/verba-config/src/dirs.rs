//! 平台标准目录。

use std::path::PathBuf;

use directories::ProjectDirs;

/// Verba 数据/配置/日志目录。
#[derive(Debug, Clone)]
pub struct VerbaDirs {
    config_dir: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
}

impl VerbaDirs {
    /// 依据平台约定计算目录（`ProjectDirs("dev","verba","Verba")`）：
    /// - Windows: `%APPDATA%\verba\Verba\data`
    /// - macOS: `~/Library/Application Support/dev.verba.Verba`
    /// - Linux: `~/.config/verba` / `~/.local/share/verba`
    pub fn locate() -> Result<Self, std::io::Error> {
        let dirs = ProjectDirs::from("dev", "verba", "Verba")
            .ok_or_else(|| std::io::Error::other("无法解析用户目录"))?;
        let config_dir = dirs.config_dir().to_path_buf();
        let data_dir = dirs.data_dir().to_path_buf();
        let log_dir = data_dir.join("logs");
        Ok(Self {
            config_dir,
            data_dir,
            log_dir,
        })
    }

    pub fn config_dir(&self) -> &PathBuf {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }

    pub fn log_dir(&self) -> &PathBuf {
        &self.log_dir
    }

    /// 确保目录存在。
    pub fn ensure(&self) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.log_dir)?;
        Ok(())
    }
}
impl VerbaDirs {
    /// 测试/自定义目录（从给定根目录派生）。
    pub fn from_paths(root: std::path::PathBuf) -> Self {
        Self {
            config_dir: root.clone(),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
        }
    }
}
