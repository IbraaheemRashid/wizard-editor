# Wizard Editor

Rust + egui + wgpu native macOS video editor. Apple Silicon, Metal backend.

## Project Structure

Cargo workspace with 5 crates:
- `app` — binary, window setup, run loop
- `state` — shared AppState, no UI or media deps
- `ui` — all egui panel code, depends on state
- `media` — import, decode, thumbnails, depends on state
- `audio` — cpal audio output, depends on state

Dependency graph: `app → ui → state`, `app → media → state`, `app → audio → state`. No circular deps.

## 60fps Frame Rules

Target: 16.6ms per frame. Every frame: handle input → update state → layout UI → render GPU → present.

### Rule 1: Never decode video on the main thread
Video decoding (2-20ms) MUST happen on background threads. Use double-buffered textures: "front" displayed, "back" being written. Main thread checks `mpsc::channel` with non-blocking `try_recv` each frame. If no new frame, show the previous one.

### Rule 2: Thumbnail generation is 100% async
Spawn background tasks at import time. Browser grid shows placeholders until thumbnails arrive via channel. Upload to GPU texture on main thread (<1ms).

### Rule 3: Waveform data is pre-computed
Compute peaks once at import as `Vec<(f32, f32)>` (min/max pairs). Timeline reads from this buffer — just array indexing + draw call.

### Rule 4: UI functions only read state and emit draw commands
No expensive work inside panel functions. They receive `&AppState` or `&mut AppState` and paint. egui's layout pass is 1-3ms for complex UI — fine as long as we don't block.

### Rule 5: Use Fifo present mode (vsync)
Caps at 60fps, prevents tearing, smoothest frame pacing.

### Rule 6: Request repaint only when needed
Only call `ctx.request_repaint()` when playing. Otherwise egui only repaints on input events.

### What drops frames — avoid these
- File I/O on main thread → always async/background
- Large texture uploads (4K = ~32MB) → use staging buffers
- Searching/filtering clips with linear scan → keep filtered index
- Allocations in hot path (`Vec::push` realloc) → pre-allocate, reuse buffers

## Threading Model

```
Main thread (60fps):          Background threads:
  poll channels (try_recv)      thumbnail extraction (per clip)
  update playback state         metadata extraction (per clip)
  egui layout + paint           frame decoding (1 thread)
  present frame                 waveform extraction (per clip)
                                audio decoding (1 thread)
```

Channel pattern used everywhere:
```rust
let (tx, rx) = std::sync::mpsc::channel();
// Background: tx.send((clip_id, data))
// Main loop:  while let Ok((id, data)) = rx.try_recv() { ... }
```

Frame budget: CPU ~2-4ms, GPU ~3-5ms, headroom ~8-11ms.

## Rust Conventions

- No `unwrap()` in production code — use `expect("reason")` or proper error handling with `Result`/`Option`
- Prefer `&str` over `String` in function parameters when ownership isn't needed
- Use `#[derive(Clone, Debug)]` on all public types
- Newtypes over Uuid for type safety: `ClipId`, `TrackId` — never pass raw Uuids
- No `unsafe` unless absolutely required for FFI or GPU interop, and document why
- Prefer `Vec::with_capacity()` over `Vec::new()` when size is known
- Use `std::sync::mpsc` for background→main communication, not `async`
- `Arc<Mutex<T>>` only when shared mutable state is unavoidable — prefer channels
- Run `cargo clippy` before committing — treat warnings as errors
- Run `cargo fmt` to keep formatting consistent

## Code Conventions

- No comments unless explicitly asked
- Panel functions are pure render: `fn panel(ui: &mut egui::Ui, state: &mut AppState)`
- IDs are newtypes over Uuid (ClipId, TrackId)
- egui 0.31 API: use `CornerRadius` not `Rounding`, `rect_stroke` takes `StrokeKind`
- Dark theme defined in `ui/src/theme.rs` — use constants, don't hardcode colors
- Constants in `ui/src/constants.rs` — sizing, spacing, layout values
- Right-click to star clips, click to select
- J/K/L keyboard controls for playback

## External Tools

- ffmpeg for video frame extraction: `ffmpeg -ss <time> -i <file> -frames:v 1 -f rawvideo -pix_fmt rgba pipe:1`
- ffprobe for metadata: `ffprobe -v quiet -print_format json -show_format -show_streams <file>`
- cpal for audio output
- Always shell out to ffmpeg/ffprobe on background threads, never main thread

## Building

```bash
cargo run --release    # release mode for real perf numbers
cargo build            # dev mode for fast iteration
cargo clippy           # lint — fix all warnings
cargo fmt              # format before committing
cargo test             # run all tests
```

Rust installed via rustup. Only `ui` recompiles when changing UI code.

## Current Status

See `plan.md` for the full implementation roadmap. Days 1-2 complete (foundation + media browser). Days 3-5 remaining (timeline interaction, playback engine, polish).
