//! 工具箱的构建期资源流水线。
//!
//! 可执行文件会把原 Python 工具箱载荷打成 zip 内嵌，编译 Slint UI，
//! 并生成 Windows 图标。公开仓库或纯源码构建允许缺少私有载荷文件，
//! 这种情况下内嵌 zip 会保持为空，真正执行安装时会通过运行时校验快速失败。

use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const MANAGED_ITEMS: &[&str] = &[
    "BoundaryMetaServer-main",
    "nodejs",
    "commandlist.txt",
    "DT_ItemType.json",
    "dxgi.dll",
    "startgame.bat",
    "steam_appid.txt",
];

fn main() -> Result<()> {
    // 构建系统 Cargo 通过环境变量把路径传给构建脚本；生成物全部放进 OUT_DIR，
    // 避免普通构建污染仓库。
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("missing CARGO_MANIFEST_DIR")?);
    let project_root = manifest_dir.parent().unwrap_or(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("missing OUT_DIR")?);

    // 载荷发现同时兼容原 Python 项目旁边的本地开发，以及只有 Rust 源码的公开构建。
    let payload_root = find_payload_root(&manifest_dir)
        .or_else(|_| find_payload_root(project_root))
        .ok();

    slint_build::compile("ui/appwindow.slint").context("compile Slint UI")?;
    build_payload_zip(payload_root.as_deref(), &out_dir)?;
    build_icon(&manifest_dir, project_root, &out_dir)?;

    for asset in icon_asset_candidates(&manifest_dir, project_root) {
        println!("cargo:rerun-if-changed={}", asset.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("ui").join("appwindow.slint").display()
    );
    if let Some(payload_root) = &payload_root {
        for item in MANAGED_ITEMS {
            println!(
                "cargo:rerun-if-changed={}",
                payload_root.join(item).display()
            );
        }
    } else {
        println!(
            "cargo:warning=payload root not found; building source-only executable with empty embedded payload"
        );
    }

    embed_windows_icon(&out_dir)?;
    Ok(())
}

/// 查找包含全部受管旧载荷条目的目录。
fn find_payload_root(project_root: &Path) -> Result<PathBuf> {
    // 环境变量 BOUNDARY_PAYLOAD_ROOT 是发布构建时使用的显式覆盖路径。
    if let Ok(raw) = env::var("BOUNDARY_PAYLOAD_ROOT") {
        let candidate = PathBuf::from(raw);
        if payload_root_has_all_items(&candidate) {
            println!("cargo:rerun-if-changed={}", candidate.display());
            return Ok(candidate);
        }
        bail!(
            "BOUNDARY_PAYLOAD_ROOT does not contain all managed items: {}",
            candidate.display()
        );
    }

    // 本地开发可能从 Rust crate 或原项目根目录运行，因此检查根目录及其直接子目录。
    let mut candidates = vec![project_root.to_path_buf()];
    for entry in std::fs::read_dir(project_root)
        .with_context(|| format!("read {}", project_root.display()))?
    {
        let entry =
            entry.with_context(|| format!("read child under {}", project_root.display()))?;
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            candidates.push(entry.path());
        }
    }

    for candidate in &candidates {
        if payload_root_has_all_items(candidate) {
            println!("cargo:rerun-if-changed={}", candidate.display());
            return Ok(candidate.clone());
        }
    }

    let checked = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("unable to locate payload root containing all managed items. checked: {checked}")
}

/// 确认候选载荷目录包含完整安装集。
fn payload_root_has_all_items(candidate: &Path) -> bool {
    MANAGED_ITEMS
        .iter()
        .all(|item| candidate.join(item).exists())
}

/// 构建供 `src/core.rs` 使用的内嵌载荷压缩包。
fn build_payload_zip(payload_root: Option<&Path>, out_dir: &Path) -> Result<()> {
    let payload_zip = out_dir.join("payload.zip");
    let file =
        File::create(&payload_zip).with_context(|| format!("create {}", payload_zip.display()))?;
    let mut zip = ZipWriter::new(file);
    let file_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let dir_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o755);

    let Some(payload_root) = payload_root else {
        // 纯源码构建仍需要 payload.zip，因为 core.rs 使用 include_bytes!。
        // 空 zip 可以保证二进制可构建，真正安装时再由运行时校验报告缺失项。
        zip.finish()?;
        return Ok(());
    };

    for item in MANAGED_ITEMS {
        let path = payload_root.join(item);
        if path.is_dir() {
            // 保留相对载荷根目录的路径，解压时才能在 Binaries\Win64 下还原原布局。
            for entry in WalkDir::new(&path) {
                let entry = entry.with_context(|| format!("walk {}", path.display()))?;
                let entry_path = entry.path();
                let rel = entry_path.strip_prefix(payload_root).with_context(|| {
                    format!(
                        "strip prefix {} from {}",
                        payload_root.display(),
                        entry_path.display()
                    )
                })?;
                let rel_name = rel.to_string_lossy().replace('\\', "/");
                if entry.file_type().is_dir() {
                    if !rel_name.is_empty() {
                        zip.add_directory(format!("{rel_name}/"), dir_options)?;
                    }
                    continue;
                }
                zip.start_file(rel_name, file_options)?;
                let mut source = File::open(entry_path)
                    .with_context(|| format!("open {}", entry_path.display()))?;
                io::copy(&mut source, &mut zip)?;
            }
        } else {
            zip.start_file(item.replace('\\', "/"), file_options)?;
            let mut source =
                File::open(&path).with_context(|| format!("open {}", path.display()))?;
            io::copy(&mut source, &mut zip)?;
        }
    }

    zip.finish()?;
    Ok(())
}

/// 从可用游戏素材生成方形 PNG/ICO；没有素材时生成兜底图标。
fn build_icon(manifest_dir: &Path, project_root: &Path, out_dir: &Path) -> Result<()> {
    let source = icon_asset_candidates(manifest_dir, project_root)
        .into_iter()
        .find(|path| path.exists());
    let icon_png = out_dir.join("app_icon.png");
    let icon_ico = out_dir.join("app_icon.ico");

    if let Some(source) = source {
        // 在 Windows 上，图标使用正方形素材效果更稳定。
        let image = image::open(&source).with_context(|| format!("open {}", source.display()))?;
        let square = image.resize_to_fill(256, 256, image::imageops::FilterType::Lanczos3);
        square
            .save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        square
            .save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    } else {
        // 公开仓库不包含私有美术资源，因此生成中性图标，避免构建依赖二进制素材。
        let icon = image::RgbaImage::from_pixel(256, 256, image::Rgba([11, 14, 19, 255]));
        icon.save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        icon.save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    }
    Ok(())
}

/// 按优先级返回可用图标素材路径。
fn icon_asset_candidates(manifest_dir: &Path, project_root: &Path) -> Vec<PathBuf> {
    ["library_600x900_schinese.jpg", "Boundary.jpg"]
        .into_iter()
        .flat_map(|name| {
            [
                manifest_dir.join(name),
                manifest_dir.join("assets").join(name),
                project_root.join(name),
            ]
        })
        .collect()
}

/// 将生成的 ICO 嵌入为 Windows 可执行文件图标。
fn embed_windows_icon(out_dir: &Path) -> Result<()> {
    let icon_ico = out_dir.join("app_icon.ico");
    let rc_path = out_dir.join("app_icon.rc");
    let mut rc_file =
        File::create(&rc_path).with_context(|| format!("create {}", rc_path.display()))?;
    writeln!(rc_file, "1 ICON \"{}\"", icon_ico.display())?;
    let _ = embed_resource::compile(rc_path, embed_resource::NONE);
    Ok(())
}
