# Wizard Editor

A native video editor built in Rust. Three-panel layout (media browser, timeline, preview) with real-time playback, audio mixing, and GPU-accelerated rendering via egui + wgpu.

## Prerequisites

- [Rust](https://rustup.rs/) (install via rustup)
- GStreamer and plugins:

```bash
brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav
```

## Build & Run

```bash
cargo run --release
```

For development (faster compile, slower runtime):

```bash
cargo run
```

## Usage

1. Click the import button in the browser panel to open a folder of video/audio files
2. Drag clips from the browser onto timeline tracks
3. Use J/K/L keys for reverse/stop/forward playback, Space to toggle play/pause
4. Scrub by clicking and dragging on the timeline ruler
5. Trim clips by dragging their edges on the timeline

## Project Structure

```
crates/
  app/     - Binary entry point, run loop, playback engine, workers
  state/   - Shared data models (clips, timeline, playback, selection)
  ui/      - egui panel rendering (browser, timeline, preview)
  media/   - GStreamer decoding, thumbnails, waveform extraction
  audio/   - cpal audio device output
```
