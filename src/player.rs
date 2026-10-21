//! 视频验证（M4）：ffmpeg sidecar 抽帧解码。
//!
//! 方案取舍见 docs/MIGRATION_PLAN.md：本机/产线无需链接 libmpv，
//! `ffmpeg` 全程解码成功即证明录像文件有效；后续如需有声平滑播放，
//! 可升级为 libmpv render API 嵌入（接口保持不变）。
//!
//! ffmpeg/ffprobe 查找顺序：环境变量 `FFMPEG` / `FFPROBE` →
//! exe 同目录（随包分发形态）→ PATH。

use crate::exec;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// exe 所在目录（随包分发工具的查找位置）。
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn ffmpeg_path() -> String {
    resolve_tool(exe_dir().as_deref(), "FFMPEG", "ffmpeg")
}

fn ffprobe_path() -> String {
    resolve_tool(exe_dir().as_deref(), "FFPROBE", "ffprobe")
}

/// 查找顺序：环境变量 `env_key` → `dir` 下同名可执行 → PATH（裸名字）。
fn resolve_tool(dir: Option<&Path>, env_key: &str, name: &str) -> String {
    if let Ok(p) = std::env::var(env_key) {
        return p;
    }
    let plain = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Some(d) = dir {
        let bundled = d.join(&plain);
        if bundled.is_file() {
            return bundled.to_string_lossy().into_owned();
        }
    }
    plain
}

/// 用 ffmpeg 以 `fps` 帧率抽取视频帧到 `frames_dir`（PNG，宽度缩放到 `scale_w`）。
///
/// 返回解出的帧数；ffmpeg 解码失败（文件损坏/非视频）返回 Err。
/// 成功但 0 帧（如空文件）也视为失败。
pub fn extract_frames(video: &Path, frames_dir: &Path, fps: u32, scale_w: u32) -> Result<usize> {
    // 清空旧帧，防止上一轮残帧冒充本轮结果
    if frames_dir.exists() {
        std::fs::remove_dir_all(frames_dir)
            .with_context(|| format!("清理 {}", frames_dir.display()))?;
    }
    std::fs::create_dir_all(frames_dir)?;

    let out = exec::output(
        Command::new(ffmpeg_path())
            .args(["-nostdin", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(video)
            .arg("-vf")
            .arg(format!("fps={fps},scale={scale_w}:-2"))
            .arg(frames_dir.join("f%04d.png")),
        exec::default_timeout(),
    )
    .context("执行 ffmpeg 失败（ffmpeg 是否在 PATH？）")?;
    if !out.status.success() {
        bail!(
            "ffmpeg 解码失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let count = std::fs::read_dir(frames_dir)
        .with_context(|| format!("读取 {}", frames_dir.display()))?
        .filter(|e| {
            e.as_ref()
                .map(|e| e.file_name().to_string_lossy().ends_with(".png"))
                .unwrap_or(false)
        })
        .count();
    if count == 0 {
        bail!("ffmpeg 成功退出但未解出任何帧");
    }
    Ok(count)
}

/// 探测视频是否含音频流（产线顺带校验录音通道）。
pub fn probe_has_audio(video: &Path) -> Result<bool> {
    let out = exec::output(
        Command::new(ffprobe_path())
            .args([
                "-v",
                "error",
                "-select_streams",
                "a",
                "-show_entries",
                "stream=index",
                "-of",
                "csv=p=0",
            ])
            .arg(video),
        exec::default_timeout(),
    )
    .context("执行 ffprobe 失败")?;
    if !out.status.success() {
        bail!(
            "ffprobe 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

#[cfg(all(unix, test))] // ffmpeg 预装于 ubuntu/macos CI；Windows CI 跳过
mod tests {
    use super::*;

    /// 生成确定性测试视频（ffmpeg lavfi 测试源），失败则跳过。
    fn make_clip(path: &Path, with_audio: bool) -> bool {
        let mut cmd = Command::new(ffmpeg_path());
        cmd.args(["-nostdin", "-loglevel", "error", "-y"])
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("testsrc=duration=2:size=320x240:rate=10");
        if with_audio {
            cmd.arg("-f")
                .arg("lavfi")
                .arg("-i")
                .arg("sine=frequency=440:duration=2");
        }
        let st = cmd.arg(path).status();
        st.map(|s| s.success()).unwrap_or(false)
    }

    #[test]
    fn bundled_tool_resolution() {
        // exe 同目录有同名文件 → 用随包的
        let dir = std::env::temp_dir().join(format!("e02-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ffmpeg"), b"").unwrap();
        assert_eq!(
            resolve_tool(Some(&dir), "E02_UNSET_ENV", "ffmpeg"),
            dir.join("ffmpeg").display().to_string()
        );
        // 目录里没有 → 落回 PATH
        let empty = std::env::temp_dir().join(format!("e02-tool-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            resolve_tool(Some(&empty), "E02_UNSET_ENV", "ffmpeg"),
            "ffmpeg"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn extract_and_probe_with_audio() {
        let dir = std::env::temp_dir().join(format!("e02-ffmpeg-a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        if !make_clip(&clip, true) {
            eprintln!("ffmpeg 不可用，跳过");
            return;
        }
        let frames = extract_frames(&clip, &dir.join("frames"), 2, 320).unwrap();
        assert!(frames >= 3, "2 秒 2fps 应至少 3 帧，实际 {frames}");
        assert!(probe_has_audio(&clip).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reject_garbage_video() {
        let dir = std::env::temp_dir().join(format!("e02-ffmpeg-b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("fake.mp4");
        std::fs::write(&clip, b"not a video").unwrap();
        assert!(extract_frames(&clip, &dir.join("frames"), 2, 320).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_no_audio() {
        let dir = std::env::temp_dir().join(format!("e02-ffmpeg-c-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("silent.mp4");
        if !make_clip(&clip, false) {
            eprintln!("ffmpeg 不可用，跳过");
            return;
        }
        assert!(!probe_has_audio(&clip).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
