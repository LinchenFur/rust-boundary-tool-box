param(
    [switch]$Release = $true
)

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8

$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
$cargoDir = Join-Path $projectRoot "rust_toolbox"
$targetDir = if ($Release) { "release" } else { "debug" }
$exeName = "boundary_toolbox.exe"
$finalName = "边境社区服工具箱.exe"

Push-Location $cargoDir
try {
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
Get-ChildItem -LiteralPath $projectRoot -Filter *.exe |
    Where-Object { $_.Name -ne $finalName -and $_.Length -eq (Get-Item -LiteralPath $builtExe).Length } |
    ForEach-Object {
        if ($_.LastWriteTime -gt (Get-Date).AddMinutes(-10)) {
            Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue
        }
    }
Copy-Item -LiteralPath $builtExe -Destination $finalExe -Force
Write-Host "已生成：$finalExe"
