# **震撼美味**

# Rust Boundary Tool Box

Native Windows toolbox for Boundary community/PVE setup, written in Rust with Slint.

## Build

```powershell
cargo check
cargo build --release
```

The public source tree can build without the private runtime payload. In that mode the embedded payload is empty, so installer actions that require bundled files will report a missing payload at runtime.

To build a functional installer, provide a payload directory containing:

- `BoundaryMetaServer-main`
- `nodejs`
- `commandlist.txt`
- `DT_ItemType.json`
- `dxgi.dll`
- `startgame.bat`
- `steam_appid.txt`

Then build with:

```powershell
$env:BOUNDARY_PAYLOAD_ROOT="D:\path\to\payload"
cargo build --release
```

## Notes

- UI is native Slint, no WebView.
- VNT v2 source is vendored under `vendor/vnt` and used for the native multiplayer platform.
- ProjectRebound runtime files are downloaded from the configured Nightly release URL during install.
