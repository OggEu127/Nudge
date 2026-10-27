//! Packages verified Windows binaries, matching sources and license notices.
//!
//! Run with: `cargo run -p launcher-cli --example package_portable -- BIN_DIR OUTPUT.zip LICENSE_DIR`.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use zip::{ZipWriter, write::SimpleFileOptions};

const GUIDE: &str = "Nudge Windows x86-64 便携开发版\n\n解压整个文件夹后，双击 nudge-gui.exe 启动中文界面；nudge.exe 是命令行工具。\n需要匹配游戏版本的 Java，当前不附带或自动安装 Java。默认用户数据保存在系统用户目录，不是 EXE 旁。\n微软正版登录需要发布者自己的已注册并获准访问服务的应用 ID；本包未完成真实微软账号或 Windows 实机验收。离线账户不代表正版授权。\n此包是在 Linux 交叉编译的未签名开发构建，仍依赖 Windows 系统及 UCRT；SmartScreen 可能提示未知发布者。\nSHA256SUMS 可检查包内文件；source/ 是本次构建对应的工作区源码快照（含尚未提交的修改），不是仅凭旧 commit 重建的声明。\nTHIRD-PARTY-NOTICES.txt 和 licenses/ 包含依赖许可信息。分享日志前请检查账号隐私。\n";

struct Package {
    zip: ZipWriter<tempfile::NamedTempFile>,
    prefix: String,
    hashes: BTreeMap<String, String>,
}

impl Package {
    fn bytes(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        self.zip.start_file(
            format!("{}/{name}", self.prefix),
            SimpleFileOptions::default(),
        )?;
        self.zip.write_all(bytes)?;
        ensure!(
            self.hashes
                .insert(name.into(), hex::encode(Sha256::digest(bytes)))
                .is_none(),
            "duplicate package path: {name}"
        );
        Ok(())
    }

    fn file(&mut self, name: &str, path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "not a regular package input: {}",
            path.display()
        );
        self.zip.start_file(
            format!("{}/{name}", self.prefix),
            SimpleFileOptions::default(),
        )?;
        let mut input = fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            self.zip.write_all(&buffer[..count])?;
        }
        ensure!(
            self.hashes
                .insert(name.into(), hex::encode(hash.finalize()))
                .is_none(),
            "duplicate package path: {name}"
        );
        Ok(())
    }

    fn tree(&mut self, prefix: &str, directory: &Path) -> Result<()> {
        let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type()?;
            ensure!(
                !file_type.is_symlink(),
                "refusing linked package input: {}",
                entry.path().display()
            );
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-Unicode package input"))?;
            let stored = format!("{prefix}/{name}");
            launcher_minecraft::resolver::safe_relative_path(&stored)?;
            if file_type.is_dir() {
                self.tree(&stored, &entry.path())?;
            } else {
                self.file(&stored, &entry.path())?;
            }
        }
        Ok(())
    }
}

fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    ensure!(result.status.success(), "build metadata command failed");
    String::from_utf8(result.stdout).context("metadata is not UTF-8")
}

fn tracked_files(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let result = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--cached", "--full-name", "-z"])
        .output()?;
    ensure!(
        result.status.success(),
        "cannot list tracked package inputs"
    );
    let paths = std::str::from_utf8(&result.stdout).context("tracked path is not UTF-8")?;
    Ok(paths
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| root.join(path))
        .collect())
}

fn verify_source_tree(directory: &Path, tracked: &BTreeSet<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "not a regular source directory: {}",
        directory.display()
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "linked package input: {}",
            entry.path().display()
        );
        if kind.is_dir() {
            verify_source_tree(&entry.path(), tracked)?;
        } else {
            ensure!(
                kind.is_file(),
                "not a regular package input: {}",
                entry.path().display()
            );
            ensure!(
                tracked.contains(&entry.path()),
                "untracked package input: {} (stage or remove it before packaging)",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn verify_source_inputs(root: &Path) -> Result<()> {
    let tracked = tracked_files(root)?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "rustfmt.toml",
        ".gitattributes",
        ".gitignore",
        "README.md",
        "LICENSE",
    ] {
        ensure!(
            tracked.contains(&root.join(name)),
            "untracked package input: {name}"
        );
    }
    for name in ["crates", "docs", "assets", "scripts", ".github"] {
        verify_source_tree(&root.join(name), &tracked)?;
    }
    Ok(())
}

fn notices(root: &Path) -> Result<String> {
    let metadata: Value = serde_json::from_str(&output(
        Command::new(env!("CARGO")).current_dir(root).args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            "x86_64-pc-windows-gnullvm",
        ]),
    )?)?;
    let packages = metadata["packages"]
        .as_array()
        .context("missing Cargo packages")?;
    let mut text = String::from(
        "Nudge 第三方依赖许可清单\n\n依据 Cargo.lock 的完整工作区解析生成，包含构建、测试和其他平台的依赖，不表示它们全部链接进 Windows EXE。\n以下列出 SPDX 声明及包内随附的许可证、版权和 NOTICE 文本；工具链运行库许可另见 licenses/。\n",
    );
    for package in packages {
        writeln!(
            text,
            "\n================ {} {} ================\nSPDX: {}\n",
            package["name"].as_str().unwrap_or("unknown"),
            package["version"].as_str().unwrap_or("unknown"),
            package["license"]
                .as_str()
                .unwrap_or("see accompanying files")
        )?;
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .context("missing manifest path")?,
        );
        let directory = manifest
            .parent()
            .context("manifest has no directory")?
            .canonicalize()?;
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let upper = name.to_ascii_uppercase();
            if entry.file_type()?.is_file()
                && ["LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            {
                files.insert(name, entry.path());
            }
        }
        if let Some(license) = package["license_file"].as_str() {
            let path = directory.join(license).canonicalize()?;
            ensure!(path.starts_with(&directory), "license escapes its package");
            files.insert(license.into(), path);
        }
        for (name, path) in files {
            ensure!(
                fs::metadata(&path)?.len() <= 1024 * 1024,
                "license file too large"
            );
            writeln!(text, "\n--- {name} ---")?;
            text.push_str(&fs::read_to_string(path)?);
        }
    }
    Ok(text)
}

fn main() -> Result<()> {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    if arguments.len() == 2 && arguments[0] == "--verify" {
        return verify_archive(Path::new(&arguments[1]));
    }
    ensure!(
        arguments.len() == 3,
        "usage: package_portable BIN_DIR OUTPUT.zip LICENSE_DIR"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let binaries = PathBuf::from(&arguments[0]);
    let archive = PathBuf::from(&arguments[1]);
    let licenses = PathBuf::from(&arguments[2]);
    verify_source_inputs(&root)?;
    ensure!(
        !archive.exists(),
        "output already exists: {}",
        archive.display()
    );
    // Generate metadata first so missing cached manifests cannot leave a
    // plausible-looking but incomplete application archive behind.
    let notices = notices(&root)?;
    let info = serde_json::to_vec_pretty(&serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"), "target": "x86_64-pc-windows-gnullvm",
        "source_commit": output(Command::new("git").current_dir(&root).args(["rev-parse", "HEAD"]))?.trim(),
        "working_tree_dirty": !output(Command::new("git").current_dir(&root).args(["status", "--porcelain"]))?.is_empty(),
        "rustc": output(Command::new("rustc").arg("--version"))?.trim(),
        "link_flags": "-Ctarget-feature=+crt-static", "windows_runtime_tested": false,
        "source": "source/ contains the corresponding current working-tree snapshot"
    }))?;
    let temporary = tempfile::NamedTempFile::new_in(
        archive
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let mut package = Package {
        zip: ZipWriter::new(temporary),
        prefix: format!("Nudge-{}-windows-x86_64", env!("CARGO_PKG_VERSION")),
        hashes: BTreeMap::new(),
    };
    for name in ["nudge.exe", "nudge-gui.exe"] {
        package.file(name, &binaries.join(name))?;
    }
    for name in ["README.md", "LICENSE"] {
        package.file(name, &root.join(name))?;
    }
    for name in ["docs", "assets", "scripts"] {
        package.tree(name, &root.join(name))?;
    }
    package.tree("licenses", &licenses)?;
    package.bytes("使用说明.txt", GUIDE.as_bytes())?;
    package.bytes("BUILD-INFO.json", &info)?;
    package.bytes("THIRD-PARTY-NOTICES.txt", notices.as_bytes())?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "rustfmt.toml",
        ".gitattributes",
        ".gitignore",
        "README.md",
        "LICENSE",
    ] {
        package.file(&format!("source/{name}"), &root.join(name))?;
    }
    for name in ["crates", "docs", "assets", "scripts", ".github"] {
        package.tree(&format!("source/{name}"), &root.join(name))?;
    }
    let mut checksums = String::new();
    for (name, digest) in &package.hashes {
        writeln!(checksums, "{digest}  {name}")?;
    }
    package.bytes("SHA256SUMS", checksums.as_bytes())?;
    let temporary = package.zip.finish()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&archive)
        .map_err(|error| error.error)
        .context("cannot commit portable archive")?;
    let digest = digest_file(&archive)?;
    let mut sidecar_name = archive.as_os_str().to_os_string();
    sidecar_name.push(".sha256");
    let mut sidecar = fs::File::create_new(PathBuf::from(sidecar_name))?;
    writeln!(
        sidecar,
        "{digest}  {}",
        archive
            .file_name()
            .context("no archive filename")?
            .to_string_lossy()
    )?;
    sidecar.sync_all()?;
    println!("{}  {}", digest, archive.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify_source_tree;
    use std::collections::BTreeSet;

    #[test]
    fn refuses_untracked_source_files() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("crates");
        std::fs::create_dir(&source)?;
        let tracked_file = source.join("lib.rs");
        std::fs::write(&tracked_file, "pub fn safe() {}")?;
        let tracked = BTreeSet::from([tracked_file]);
        verify_source_tree(&source, &tracked)?;
        std::fs::write(source.join(".env"), "SECRET=fixture")?;
        assert!(verify_source_tree(&source, &tracked).is_err());
        Ok(())
    }
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn verify_archive(path: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
    let prefix = format!("Nudge-{}-windows-x86_64", env!("CARGO_PKG_VERSION"));
    let manifest_name = format!("{prefix}/SHA256SUMS");
    let mut manifest = String::new();
    archive
        .by_name(&manifest_name)?
        .take(1024 * 1024)
        .read_to_string(&mut manifest)?;
    let mut checked = std::collections::BTreeSet::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    for line in manifest.lines() {
        let (expected, relative) = line.split_once("  ").context("invalid checksum entry")?;
        ensure!(
            expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid digest"
        );
        launcher_minecraft::resolver::safe_relative_path(relative)?;
        ensure!(
            checked.insert(relative.to_owned()),
            "duplicate checksum entry"
        );
        let mut entry = archive.by_name(&format!("{prefix}/{relative}"))?;
        let mut hash = Sha256::new();
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        ensure!(
            hex::encode(hash.finalize()) == expected,
            "checksum mismatch: {relative}"
        );
    }
    ensure!(
        checked.len() + 1 == archive.len(),
        "archive contains unchecked entries"
    );
    for name in [
        "nudge.exe",
        "nudge-gui.exe",
        "source/Cargo.lock",
        "source/Cargo.toml",
        "LICENSE",
        "README.md",
        "BUILD-INFO.json",
        "THIRD-PARTY-NOTICES.txt",
        "使用说明.txt",
    ] {
        ensure!(
            checked.contains(name),
            "missing required package entry: {name}"
        );
    }
    for name in ["nudge.exe", "nudge-gui.exe"] {
        let mut signature = [0_u8; 2];
        archive
            .by_name(&format!("{prefix}/{name}"))?
            .read_exact(&mut signature)?;
        ensure!(&signature == b"MZ", "invalid executable signature");
    }
    println!(
        "Verified ZIP CRCs and SHA-256 for {} package files",
        checked.len()
    );
    Ok(())
}
