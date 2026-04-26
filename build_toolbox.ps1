param(
    # Build release by default; pass -Release:$false for a debug binary.
    [switch]$Release = $true
)

# Keep Chinese build output readable in PowerShell.
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

# Stop immediately so copy/cleanup never runs after a failed cargo build.
$ErrorActionPreference = "Stop"

# The script lives in rust_toolbox, while the final renamed exe is copied to the
# project root for end-user distribution.
$projectRoot = Split-Path -Parent $PSScriptRoot
$cargoDir = Join-Path $projectRoot "rust_toolbox"
$targetDir = if ($Release) { "release" } else { "debug" }
$exeName = "boundary_toolbox.exe"
$finalName = "边境社区服工具箱.exe"

Push-Location $cargoDir
try {
    # Build inside the crate directory so Cargo finds the correct manifest.
    if ($Release) {
        cargo build --release
    }
    else {
        cargo build
    }
}
finally {
    Pop-Location
}

$builtExe = Join-Path $cargoDir "target\$targetDir\$exeName"
if (-not (Test-Path $builtExe)) {
    throw "未找到构建产物：$builtExe"
}

$finalExe = Join-Path $projectRoot $finalName
# Remove recent same-size experimental exe names left by previous manual builds,
# but keep the canonical output name intact.
Get-ChildItem -LiteralPath $projectRoot -Filter *.exe |
    Where-Object { $_.Name -ne $finalName -and $_.Length -eq (Get-Item -LiteralPath $builtExe).Length } |
    ForEach-Object {
        if ($_.LastWriteTime -gt (Get-Date).AddMinutes(-10)) {
            Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }
# Publish the freshly built binary with the Chinese display name.
Copy-Item -LiteralPath $builtExe -Destination $finalExe -Force
Write-Host "已生成：$finalExe"
