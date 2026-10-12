# Savvy

Native desktop YouTube client: a Rust/[Tauri](https://tauri.app) backend with a
SvelteKit frontend.

## Requirements

- **Rust** 1.80+ (stable) and Cargo
- **Node.js** + npm
- **FFmpeg** (both `ffmpeg` and `ffprobe`) — see below; it is *not* bundled
- **WebView2** runtime (preinstalled on Windows 11 and most Windows 10)

## FFmpeg

Savvy shells out to `ffmpeg`/`ffprobe` for its internal muxer and audio
extractor. It is not shipped with the app, so install it yourself and make sure
both binaries are reachable. Savvy looks in this order:

1. `SAVVY_FFMPEG_DIR` — an environment variable pointing at the folder that
   contains `ffmpeg.exe` / `ffprobe.exe`.
2. Next to Savvy's own executable (a portable drop-in).
3. `PATH` — e.g. install with `winget install Gyan.FFmpeg`, or download a build
   from <https://www.gyan.dev/ffmpeg/builds/> and add its `bin` folder to `PATH`.
4. `C:\ffmpeg\bin` (conventional Windows install location).

## Build

Install frontend dependencies:

```
npm --prefix ui install
```

Produce the installers (Windows: NSIS `.exe` and `.msi`):

```
cargo tauri build
```

Artifacts land in `target/release/bundle/{nsis,msi}`. The frontend is built
automatically first (`beforeBuildCommand`).

## Development

See [`restart.md`](restart.md) for the local dev/restart workflow.

## License

GPL-3.0-or-later.
