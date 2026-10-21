//! 数据目录解析（V3）。
//!
//! 绿色包（zip/tar.gz 解压即用）习惯把 config.toml / reports / media 放 exe 同目录；
//! 但 MSI 装进 Program Files、deb 装进 /usr/bin 后该目录不可写——探测失败时
//! 退回系统应用数据目录（Windows %APPDATA%、Linux XDG_DATA_HOME），
//! 用户无感知。结果进程内缓存（探测涉及文件系统写测试）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DATA: OnceLock<PathBuf> = OnceLock::new();

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 可写数据目录（config.toml / reports / media 的父目录）。
pub fn data_dir() -> &'static Path {
    DATA.get_or_init(|| {
        let exe = exe_dir();
        if dir_writable(&exe) {
            exe
        } else {
            fallback_dir()
        }
    })
}

/// 目录可写探测：实际创建再删除探针文件（权限位不可靠，Windows 尤其）。
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".e02-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn fallback_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|v| PathBuf::from(v).join("e02-factory-rs"))
            .unwrap_or_else(exe_dir)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("e02-factory-rs"))
            .unwrap_or_else(exe_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_probe() {
        let dir = std::env::temp_dir();
        assert!(dir_writable(&dir));
        assert!(data_dir().is_absolute() || data_dir() == Path::new("."));
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_dir_detected() {
        assert!(!dir_writable(Path::new("/proc/self"))); // procfs 恒不可写
    }
}
