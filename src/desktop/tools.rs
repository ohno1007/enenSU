use anyhow::{Context, Result, ensure};
use enensu::device::PlatformTools;
use std::{
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::NamedTempFile;
use tinyfiledialogs::{MessageBoxIcon, YesNo};

const ARCHIVE: &[u8] = include_bytes!("../../desktop/dist/tools.zip");
const WEBVIEW_SETUP: &[u8] = include_bytes!("../../desktop/dist/webview-setup.exe");

pub fn root() -> Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("找不到 Windows 本地应用数据目录")?
        .join("enenSU"))
}

pub fn tool_dir(root: &Path) -> PathBuf {
    root.join("platform-tools-37.0.3")
}

fn write_verified(path: &Path, bytes: &[u8]) -> Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() == bytes.len() as u64) && fs::read(path)? == bytes {
        return Ok(());
    }
    let mut temporary = NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .with_context(|| format!("保存 {}", path.display()))?;
    Ok(())
}

pub fn prepare(root: &Path) -> Result<PlatformTools> {
    let dir = tool_dir(root);
    fs::create_dir_all(&dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("setup.lock"))?;
    lock.lock()?;
    let mut archive = zip::ZipArchive::new(Cursor::new(ARCHIVE))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let path = entry.enclosed_name().context("无效的工具包路径")?;
        ensure!(path.components().count() == 1, "unexpected embedded path");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        write_verified(&dir.join(path), &bytes)?;
    }
    Ok(PlatformTools::in_directory(&dir))
}

pub fn ensure_webview(root: &Path) -> Result<bool> {
    if wry::webview_version().is_ok_and(|v| !v.is_empty()) {
        return Ok(true);
    }
    if tinyfiledialogs::message_box_yes_no(
        "enenSU",
        "这台电脑还没有 Microsoft Edge WebView2。是否联网安装微软运行时并打开 enenSU？",
        MessageBoxIcon::Question,
        YesNo::Yes,
    ) == YesNo::No
    {
        return Ok(false);
    }
    let setup = root.join("MicrosoftEdgeWebview2Setup.exe");
    write_verified(&setup, WEBVIEW_SETUP)?;
    let status = Command::new(&setup)
        .args(["/silent", "/install"])
        .status()?;
    ensure!(
        status.success() && wry::webview_version().is_ok_and(|v| !v.is_empty()),
        "Microsoft Edge WebView2 安装未完成。请先安装微软的 Evergreen WebView2 Runtime，再打开 enenSU。"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_platform_tools_run_without_path_and_restore_a_damaged_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("驱动工具");
        prepare(&root).unwrap();
        let dir = tool_dir(&root);
        for (name, arg) in [("adb.exe", "version"), ("fastboot.exe", "--version")] {
            let output = Command::new(dir.join(name)).arg(arg).output().unwrap();
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("37.0.3"));
        }
        fs::write(dir.join("AdbWinApi.dll"), b"incomplete").unwrap();
        prepare(&root).unwrap();
        assert!(
            fs::read(dir.join("AdbWinApi.dll"))
                .unwrap()
                .starts_with(b"MZ")
        );
        assert!(dir.join("NOTICE.txt").is_file());
    }
}
