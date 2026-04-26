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
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("missing CARGO_MANIFEST_DIR")?);
    let project_root = manifest_dir.parent().unwrap_or(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("missing OUT_DIR")?);
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

fn find_payload_root(project_root: &Path) -> Result<PathBuf> {
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

fn payload_root_has_all_items(candidate: &Path) -> bool {
    MANAGED_ITEMS
        .iter()
        .all(|item| candidate.join(item).exists())
}

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
        zip.finish()?;
        return Ok(());
    };

    for item in MANAGED_ITEMS {
        let path = payload_root.join(item);
        if path.is_dir() {
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

fn build_icon(manifest_dir: &Path, project_root: &Path, out_dir: &Path) -> Result<()> {
    let source = icon_asset_candidates(manifest_dir, project_root)
        .into_iter()
        .find(|path| path.exists());
    let icon_png = out_dir.join("app_icon.png");
    let icon_ico = out_dir.join("app_icon.ico");

    if let Some(source) = source {
        let image = image::open(&source).with_context(|| format!("open {}", source.display()))?;
        let square = image.resize_to_fill(256, 256, image::imageops::FilterType::Lanczos3);
        square
            .save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        square
            .save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    } else {
        let icon = image::RgbaImage::from_pixel(256, 256, image::Rgba([11, 14, 19, 255]));
        icon.save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        icon.save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    }
    Ok(())
}

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

fn embed_windows_icon(out_dir: &Path) -> Result<()> {
    let icon_ico = out_dir.join("app_icon.ico");
    let rc_path = out_dir.join("app_icon.rc");
    let mut rc_file =
        File::create(&rc_path).with_context(|| format!("create {}", rc_path.display()))?;
    writeln!(rc_file, "1 ICON \"{}\"", icon_ico.display())?;
    let _ = embed_resource::compile(rc_path, embed_resource::NONE);
    Ok(())
}
