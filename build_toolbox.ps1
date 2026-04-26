param(
    # 默认构建 release；需要 debug 二进制时传入 -Release:$false。
    [switch]$Release = $true
)

# 保证 PowerShell 里中文构建输出可读。
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

# 当 cargo 构建失败后立即停止，避免继续复制或清理文件。
$ErrorActionPreference = "Stop"

# 脚本位于 rust_toolbox 内，最终重命名后的 exe 会复制到项目根目录。
$projectRoot = Split-Path -Parent $PSScriptRoot
$cargoDir = Join-Path $projectRoot "rust_toolbox"
$targetDir = if ($Release) { "release" } else { "debug" }
$exeName = "boundary_toolbox.exe"
$finalName = "边境社区服工具箱.exe"

Push-Location $cargoDir
try {
    # 在 crate 目录内构建，确保 Cargo 找到正确的 manifest。
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
# 清理近期手动构建留下的同尺寸实验 exe，但保留正式输出文件名。
Get-ChildItem -LiteralPath $projectRoot -Filter *.exe |
    Where-Object { $_.Name -ne $finalName -and $_.Length -eq (Get-Item -LiteralPath $builtExe).Length } |
    ForEach-Object {
        if ($_.LastWriteTime -gt (Get-Date).AddMinutes(-10)) {
            Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }
# 用中文展示名发布刚构建出的二进制。
Copy-Item -LiteralPath $builtExe -Destination $finalExe -Force
Write-Host "已生成：$finalExe"
