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

## Code Conventions

- No comments unless explicitly asked
- Panel functions are pure render: `fn panel(ui: &mut egui::Ui, state: &mut AppState)`
- IDs are newtypes over Uuid (ClipId, TrackId)
- egui 0.31 API: use `CornerRadius` not `Rounding`, `rect_stroke` takes `StrokeKind`
- Dark theme defined in `ui/src/theme.rs` — use constants, don't hardcode colors
- Right-click to star clips, click to select
- J/K/L keyboard controls for playback

## Building

```bash
cargo run --release    # release mode for real perf numbers
cargo build            # dev mode for fast iteration
```

Rust installed via rustup. Only `ui` recompiles when changing UI code.
