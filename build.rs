//! Build-time asset pipeline for the toolbox.
//!
//! The executable embeds the original Python toolbox payload as a zip, compiles
//! the Slint UI, and generates a Windows icon. Public/source-only builds are
//! allowed to compile without private payload files; in that case the embedded
//! zip is intentionally empty and runtime installation will fail fast with a
//! clear validation error.

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
    // Cargo gives build scripts paths through environment variables. Keep all
    // generated artifacts inside OUT_DIR so normal builds never dirty the repo.
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("missing CARGO_MANIFEST_DIR")?);
    let project_root = manifest_dir.parent().unwrap_or(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("missing OUT_DIR")?);

    // Payload discovery supports local development beside the original Python
    // project and CI/public builds where only the Rust source is present.
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

/// Finds a directory containing every managed legacy payload item.
fn find_payload_root(project_root: &Path) -> Result<PathBuf> {
    // BOUNDARY_PAYLOAD_ROOT is the explicit override used by release builds.
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

    // Local development often runs from either the Rust crate or the original
    // project root, so inspect the root and its direct children.
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

/// Confirms that a payload candidate contains the complete install set.
fn payload_root_has_all_items(candidate: &Path) -> bool {
    MANAGED_ITEMS
        .iter()
        .all(|item| candidate.join(item).exists())
}

/// Builds the embedded payload archive consumed by `src/core.rs`.
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
        // Source-only builds still need payload.zip because core.rs uses
        // include_bytes!. An empty zip keeps the binary buildable and lets the
        // runtime validator report the missing items when install is attempted.
        zip.finish()?;
        return Ok(());
    };

    for item in MANAGED_ITEMS {
        let path = payload_root.join(item);
        if path.is_dir() {
            // Preserve paths relative to the payload root so extraction can
            // recreate the original toolbox layout under Binaries\Win64.
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

/// Generates a square PNG/ICO from available game artwork, or a fallback icon.
fn build_icon(manifest_dir: &Path, project_root: &Path, out_dir: &Path) -> Result<()> {
    let source = icon_asset_candidates(manifest_dir, project_root)
        .into_iter()
        .find(|path| path.exists());
    let icon_png = out_dir.join("app_icon.png");
    let icon_ico = out_dir.join("app_icon.ico");

    if let Some(source) = source {
        // Windows icons look better when source art is normalized to a square.
        let image = image::open(&source).with_context(|| format!("open {}", source.display()))?;
        let square = image.resize_to_fill(256, 256, image::imageops::FilterType::Lanczos3);
        square
            .save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        square
            .save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    } else {
        // Public repos do not include private artwork; generate a neutral icon
        // instead of making build success depend on binary assets.
        let icon = image::RgbaImage::from_pixel(256, 256, image::Rgba([11, 14, 19, 255]));
        icon.save(&icon_png)
            .with_context(|| format!("write {}", icon_png.display()))?;
        icon.save(&icon_ico)
            .with_context(|| format!("write {}", icon_ico.display()))?;
    }
    Ok(())
}

/// Returns icon locations in preference order.
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

/// Embeds the generated ICO as the Windows executable icon.
fn embed_windows_icon(out_dir: &Path) -> Result<()> {
    let icon_ico = out_dir.join("app_icon.ico");
    let rc_path = out_dir.join("app_icon.rc");
    let mut rc_file =
        File::create(&rc_path).with_context(|| format!("create {}", rc_path.display()))?;
    writeln!(rc_file, "1 ICON \"{}\"", icon_ico.display())?;
    let _ = embed_resource::compile(rc_path, embed_resource::NONE);
    Ok(())
}
