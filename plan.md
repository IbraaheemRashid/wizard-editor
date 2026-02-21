# Wizard Editor — Implementation Plan

## Status

- [X] Day 1 — Foundation: Window + Panels + State
- [X] Day 2 — Media Browser: Thumbnails + Search + Hover Preview
- [ ] Day 3 — Timeline: Tracks + Clips + Drag & Drop + Playhead
- [ ] Day 4 — Playback Engine + Waveforms + Preview
- [ ] Day 5 — Polish + Integration + Bonus Features

---

## Day 1 (Done)

Cargo workspace with 5 crates. Three-panel dark-themed app running on wgpu/Metal.

- AppState with clips, tracks, playback, selection
- Browser panel: import folder, search, star (right-click), select, thumbnail placeholders
- Timeline panel: 3 tracks (V1, V2, A1), time ruler, playhead with scrub
- Preview panel: selected clip info or empty state
- J/K/L + Space keyboard controls
- Compiles clean, release build verified

---

## Day 2 — Media Browser: Thumbnails + Search + Hover Preview

| Task                                       | Details                                                                                                                                                                                                                                                           |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Thumbnail extraction via ffmpeg subprocess | Shell out to `ffmpeg -ss 1 -i <file> -frames:v 1 -f rawvideo -pix_fmt rgba pipe:1`. Background threads via `std::thread::spawn`, send `(ClipId, RgbaImage)` via `mpsc::channel`. Main thread polls with `try_recv`, uploads to `egui::TextureHandle`. |
| Thumbnail grid layout                      | Replace placeholder rects with actual textures. Handle aspect ratio. Show loading spinner or color while pending.                                                                                                                                                 |
| Search bar filtering                       | Already wired up —`state.filtered_clips()` filters by filename. Verify it works with real clips.                                                                                                                                                               |
| Star/favorite toggle                       | Already wired — right-click toggles. Add visual filter button "Show starred only".                                                                                                                                                                               |
| Metadata extraction                        | Shell out to `ffprobe -v quiet -print_format json -show_format -show_streams <file>`. Parse duration, resolution, codec. Store on Clip struct. Show below thumbnail.                                                                                            |
| Hover preview (stretch goal)               | On hover: decode frames at 0.5s intervals, cycle through them as a "scrub preview". Audio on hover via cpal. Complex — skip if time-pressed.                                                                                                                     |

### Key files to modify

- `crates/media/src/thumbnail.rs` — actual ffmpeg subprocess
- `crates/media/src/metadata.rs` — ffprobe parsing
- `crates/app/src/lib.rs` — spawn background tasks, poll channels
- `crates/ui/src/browser.rs` — render real thumbnails
- `crates/state/src/clip.rs` — ensure metadata fields populated

---

## Day 3 — Timeline: Drag & Drop + Clip Interaction

| Task                               | Details                                                                                                                                    |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Drag clip from browser to timeline | egui drag source on browser thumbnails. Detect drop target: which track + time position. Create `TimelineClip` on drop.                  |
| Clip rendering with thumbnails     | Show thumbnail texture on video track clips instead of solid color.                                                                        |
| Clip rendering on audio tracks     | Colored rectangle with filename for now. Waveforms come day 4.                                                                             |
| Playhead scrub improvements        | Already working. Add snap-to-clip-edge behavior.                                                                                           |
| Drag-to-reorder clips              | Drag clips within timeline. Detect source clip, remove from old position, insert at new position. Handle same-track and cross-track moves. |
| Star indicator on timeline clips   | Already rendering — verify with real clips.                                                                                               |
| Zoom/scroll on timeline            | Mouse wheel to zoom (change PIXELS_PER_SECOND). Horizontal scroll to pan.                                                                  |

### Key files to modify

- `crates/ui/src/browser.rs` — drag source
- `crates/ui/src/timeline.rs` — drop target, clip interaction, zoom
- `crates/state/src/timeline.rs` — add/remove/move clip operations

---

## Day 4 — Playback Engine + Waveforms + Preview

| Task                              | Details                                                                                                                                                                                                                                                |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Video frame decode pipeline       | Background thread: given playhead time + active clip, decode frame via ffmpeg, convert to RGBA, send via channel. Main thread:`try_recv`, upload as texture, display in preview panel. Double-buffer: keep showing last frame until new one arrives. |
| Preview panel shows current frame | Map playhead time → which clip is under it → request frame decode → show texture.                                                                                                                                                                   |
| J-K-L playback                    | Already wired. Verify smooth playhead advance. Handle end-of-timeline (stop or loop).                                                                                                                                                                  |
| Audio playback via cpal           | Open cpal output stream. Decode audio samples for current clip at playhead position. Feed samples to stream callback. Sync to playhead — if playhead jumps, flush and re-seek audio.                                                                  |
| Waveform peak extraction          | At import: spawn background task to decode audio via ffmpeg, downsample to min/max peaks per bucket (e.g., 1 peak per 10ms). Send `Vec<(f32, f32)>` via channel. Store on state.                                                                     |
| Waveform rendering                | In timeline, for audio tracks: read peaks for visible time range, draw as filled polygon using `ui.painter()`. Color: `CLIP_AUDIO` with alpha gradient.                                                                                            |

### Key files to modify

- `crates/media/src/decode.rs` — frame decoding pipeline
- `crates/media/src/waveform.rs` — peak extraction
- `crates/audio/src/output.rs` — cpal stream, sample feeding
- `crates/ui/src/preview.rs` — show decoded frame texture
- `crates/ui/src/timeline.rs` — waveform drawing
- `crates/app/src/lib.rs` — orchestrate decode requests, poll results

---

## Day 5 — Polish + Integration + Bonus

| Task                              | Details                                                                                                                                   |
| --------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Selection sync                    | Click clip in timeline → highlights in browser. Click in browser → highlights on timeline. Use `state.selection.selected_clip`.       |
| Basic trim on clip edges          | Draw drag handles on clip left/right edges. Dragging adjusts `in_point`/`out_point`. Visual feedback: dimmed region for trimmed area. |
| GPU-accelerated waveforms (bonus) | Upload peaks as wgpu storage buffer. Compute shader re-buckets at current zoom level. Render via custom paint callback.                   |
| UI polish                         | Spacing, hover states, panel resize handles, clip labels truncation, empty state illustrations.                                           |
| Edge cases                        | Empty timeline, no clips imported, very long clips, very short clips, many clips (100+).                                                  |
| Test with real media              | Import real footage, demo all features, verify 60fps under load.                                                                          |

---

## Architecture Reminders

### Threading model

```
Main thread (60fps):          Background threads:
  poll channels (try_recv)      thumbnail extraction (per clip)
  update playback state         metadata extraction (per clip)
  egui layout + paint           frame decoding (1 thread)
  present frame                 waveform extraction (per clip)
                                audio decoding (1 thread)
```

### Channel pattern (used everywhere)

```rust
let (tx, rx) = std::sync::mpsc::channel();
// Background: tx.send((clip_id, data))
// Main loop:  while let Ok((id, data)) = rx.try_recv() { ... }
```

### Frame budget

CPU: ~2-4ms (input + state + layout + submit)
GPU: ~3-5ms (render + present)
Headroom: ~8-11ms — don't waste it on decode or I/O
