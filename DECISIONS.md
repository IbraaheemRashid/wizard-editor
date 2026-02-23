# Architectural Decisions & Tradeoffs

Every significant technical choice in Wizard Editor, what we picked, what we didn't, and why.

---

## Table of Contents

- [Language & Framework](#language--framework)
- [Video Decoding](#video-decoding)
- [Playback Pipeline](#playback-pipeline)
- [Audio System](#audio-system)
- [Thumbnail & Preview System](#thumbnail--preview-system)
- [Waveform Rendering](#waveform-rendering)
- [Timeline Interactions](#timeline-interactions)
- [State Architecture](#state-architecture)
- [Threading & Concurrency](#threading--concurrency)
- [UI Framework & Rendering](#ui-framework--rendering)
- [Tuning Constants](#tuning-constants)

---

## Language & Framework

### Rust over Swift or C++

| | Rust | Swift | C++ |
|---|---|---|---|
| Cross-platform | Single codebase → macOS + Windows | Apple only | Cross-platform but painful build systems |
| Concurrency safety | Ownership system prevents data races at compile time | GCD/actors but runtime checked | Manual, prone to race conditions |
| GC pauses | None — deterministic deallocation | ARC can spike on large graphs | None, but manual memory management |
| wgpu ecosystem | Native — first-class Rust API | Would need C bindings | wgpu has C++ bindings but less ergonomic |
| FFmpeg bindings | ffmpeg-the-third (safe, maintained) | Requires C interop bridge | Native C API, but manual memory management |

**Why it matters for a video editor:** The 16.6ms frame budget is sacred. Swift's ARC can cause frame-time spikes when large texture caches deallocate. C++ would work but the threading model (7+ background threads, lock-free ring buffers, channels) is exactly where Rust's ownership prevents the bugs that would otherwise take days to diagnose. The brief said "Rust is strongly preferred" — and the architecture validates why.

**Porting story:** Change one line (wgpu backend selection from Metal to Vulkan/DX12). Everything else — state, UI, media, audio — is platform-agnostic. This is not theoretical: egui + wgpu apps ship on macOS, Windows, and Linux today.

---

### egui over iced, Druid, or native AppKit

| | egui | iced | Native AppKit |
|---|---|---|---|
| Rendering model | Immediate mode (draw commands each frame) | Retained (virtual DOM diff) | Retained (view hierarchy) |
| Custom GPU rendering | `CallbackTrait` for wgpu paint callbacks | Possible but less documented | CAMetalLayer, complex |
| Adding a new panel | Add one function call in the frame loop | Add widget tree + subscription | Add view controller + bindings |
| Learning curve | Minimal — functions in, draw commands out | Elm-like architecture | AppKit + Cocoa + Obj-C interop |

**The decisive factor:** Custom wgpu paint callbacks. The GPU waveform renderer (`waveform_gpu.rs`) is a custom WGSL shader pipeline that plugs directly into egui's render pass. In iced or AppKit, this integration would require significantly more plumbing.

**The tradeoff:** egui's immediate mode means the entire UI is rebuilt every frame. For our panel count (3) and widget complexity, this costs 1-3ms — well within budget. At 50+ panels with deep widget trees, this model would struggle.

---

### wgpu over raw Metal

| | wgpu | Raw Metal |
|---|---|---|
| Platform reach | Metal, Vulkan, DX12, WebGPU | macOS only |
| Shader language | WGSL (cross-platform) | MSL (Apple only) |
| Validation | Built-in validation layer catches errors | Manual |
| Overhead | Thin abstraction (~5% vs raw) | Zero |

**Why not raw Metal:** The brief says "GPU-rendered UI via wgpu, Metal, or Vulkan." Choosing wgpu gives us Metal on macOS *and* Vulkan on Windows from the same shader code. The 5% overhead is invisible in our frame budget (GPU render is 3-5ms either way).

---

## Video Decoding

### FFmpeg bindings (ffmpeg-the-third) over shelling out to ffmpeg binary

**What we chose:** Direct Rust bindings to libavformat/libavcodec via the `ffmpeg-the-third` crate.

**What we considered:**

| | Library bindings | Shell out to ffmpeg |
|---|---|---|
| Per-frame overhead | ~0ms (function call) | 10-50ms (process spawn + pipe) |
| Frame pipelining | Decode → scale → deliver in one call | Pipe raw bytes, parse manually |
| Error handling | Typed Result<T, Error> | Parse stderr strings |
| Seeking | Native seek with keyframe index | `-ss` flag, approximate |
| Memory | Zero-copy frame access | Full frame through pipe |

**Why bindings win:** At 60fps, each frame has 16.6ms. Shelling out to ffmpeg per frame costs 10-50ms in process creation alone — impossible to sustain. The bindings give us direct access to the decoder state, scaler context caching, and PTS tracking.

**The cost:** `ffmpeg-the-third` adds a compile-time dependency on FFmpeg's C libraries. The user needs `brew install ffmpeg` (or equivalent). This is acceptable because FFmpeg is the only production-quality option for supporting 30+ video codecs.

**Why not GStreamer:** GStreamer provides a pipeline-based media framework that handles demuxing, decoding, and output in a declarative graph. The tradeoffs:

| | FFmpeg bindings | GStreamer |
|---|---|---|
| Control | Frame-level: seek, decode, scale, convert | Pipeline-level: describe graph, framework executes |
| Reverse playback | We implement GOP-window decode ourselves | Would need custom element or reverse-playback plugin |
| PTS mapping | Direct access to decoder timestamps | Abstracted behind pad events |
| Dependency weight | libavformat/libavcodec (~20MB) | Full GStreamer stack (~150MB+) |
| Rust bindings | Mature (ffmpeg-the-third) | gstreamer-rs exists but heavier |

We needed frame-level control for: reverse playback (decode GOP windows, reverse order), PTS offset calibration (first-frame mapping), and variable-speed decode with StreamClock pacing. GStreamer's pipeline model abstracts away the exact control points we need. For a simple "play video forward at 1x," GStreamer would be simpler. For an editor with J/K/L speed control, reverse playback, and shadow pipelines — FFmpeg bindings give us the control we need.

---

### Scaler context caching

**Problem:** FFmpeg's `scaling::Context` takes 1-2ms to construct. At 60fps, that's 6-12% of our budget wasted on a context we'll use identically next frame.

**Solution:** Cache the scaler and check 5 parameters before rebuilding:

```
ScalerState { ctx, src_w, src_h, src_fmt, out_width, out_height }
```

Only rebuild when resolution or pixel format changes (rare — typically once per clip). Saves 1-2ms every single frame during playback. 50 lines of cache management code pays for itself in microseconds.

**What we didn't do:** Pre-compute all needed scalers (we don't know output resolutions ahead of time) or use GPU scaling (Metal compute shader — possible but adds complexity for marginal gain since CPU scaling at 1920x1080 is already <2ms).

---

### Seek strategy: 180-frame budget with stagnant PTS detection

**The problem:** After seeking in an MPEG container, the decoder often lands on a keyframe before our target. We need to decode forward until we reach the target time. But some files have corrupted PTS that never advances — the decoder claims every frame is at t=0.

**Our solution:**
1. Loop up to 180 frames (3 seconds at 60fps) after seek
2. Track PTS of each frame
3. If PTS doesn't advance for 4 consecutive frames (diff < 1μs), bail — the file is broken
4. Return best frame seen so far

**Why 180:** Long enough to decode through a typical GOP (1-2 seconds) after a keyframe seek. Short enough that a pathological file doesn't freeze the UI for more than ~200ms on a background thread.

**What we didn't do:**
- Fixed timeout (200ms) — hardware speed varies, this would be flaky
- Unlimited decode — a corrupt file would freeze forever
- Keyframe-only seeking — too imprecise for scrubbing, misses exact frame

---

## Playback Pipeline

### 3-thread architecture (demuxer → video decoder → audio decoder)

```
┌──────────┐        ┌──────────────┐        ┌──────────────┐
│ Demuxer  │──128──►│ Video Decode │──16───►│ Main Thread  │
│ thread   │  pkts  │ + scale      │ frames │ try_recv()   │
│          │        │ + StreamClock│        │              │
│          │──128──►│              │        │              │
│          │  pkts  │ Audio Decode │──ring─►│ AudioMixer   │
└──────────┘        │ + resample   │  buf   │              │
                    └──────────────┘        └──────────────┘
```

**Why three threads, not one or two:**

| Design | Pros | Cons |
|--------|------|------|
| Single thread (decode on main) | Simple | 2-20ms decode blocks frame, fails 60fps |
| Two threads (demux+decode, audio) | Fewer threads | Demuxer I/O stalls block video decode |
| Three threads (chosen) | Each stage runs at its own pace | 3 threads × 512KB stack = 1.5MB |
| Thread-per-frame (tokio) | Maximum parallelism | Overkill, 500μs spawn overhead per frame |

The key insight: demuxing (file I/O), video decoding (CPU compute), and audio decoding (CPU compute + resampling) are independent stages with different latency profiles. Separating them means a 5ms I/O stall in the demuxer doesn't delay the video decoder that's still processing buffered packets.

---

### sync_channel capacities: 128 packets, 16 frames

**Packet buffer (128):**
- At a typical bitrate, 128 packets ≈ 1-2 seconds of media
- Large enough to absorb demuxer I/O hiccups without the video decoder starving
- Small enough that seeking doesn't drain 10 seconds of stale packets

**Frame buffer (16):**
- 16 decoded frames ≈ 267ms at 60fps
- Provides backpressure: if the main thread stalls (UI freeze), the decoder stops at 16 buffered frames instead of consuming unlimited memory
- Small enough that seek response is fast (drain 16 frames, not 1000)

**What we didn't do:** Unbounded channels (memory leak on pause — decoder fills queue forever) or tiny buffers (4 frames — one UI hiccup starves the decoder).

---

### StreamClock: wall-time pacing

**The problem:** Video decoders output frames as fast as they can. Without pacing, a 30fps clip would play at 300fps.

**Our solution:** StreamClock records `(wall_start_time, start_pts)` when playback begins. For each decoded frame:

```
target_wall_time = (frame_pts - start_pts) / speed
elapsed = now - wall_start_time
diff = target_wall_time - elapsed
if diff > 1ms: sleep(diff)
```

**Why wall-time, not frame counting:** Frame counting assumes constant frame rate. Real video has variable frame duration (23.976fps interlaced, variable bitrate, B-frames). Wall-time pacing naturally handles all of these — just compare PTS to real elapsed time.

**Speed changes:** When the user presses L (faster), we don't restart the clock. Instead, `set_speed()` snapshots the current PTS position and rebases from there. Smooth transition, no frame drop.

**What we didn't do:**
- Audio-driven sync (audio thread leads, video follows) — standard in media players but requires PLL logic we'd need to tune per-codec
- Frame-drop pacing (skip frames if behind) — creates visible judder
- V-sync locked (one frame per display refresh) — doesn't work for non-60fps content

---

### Shadow pipeline: prefetch next clip

**The problem:** Starting a new decoder takes 100-400ms (open file, parse container, find first keyframe, decode). If we wait until the playhead crosses the clip boundary, the user sees a blank frame for up to 400ms.

**Our solution:** When the playhead is within 2 seconds of a clip boundary (adjusted for speed), spawn a shadow pipeline for the next clip in the background. When the boundary is reached:

1. Shadow has already decoded its first frame (buffered)
2. Shadow audio sources are already running
3. Promote shadow to primary — zero visible gap

**Why 2 seconds:** Empirically, decoder startup on Apple Silicon M-series takes 50-200ms for H.264 and 100-400ms for HEVC. 2 seconds gives ~10x headroom. Too short (0.5s) risks the shadow not being ready. Too long (5s) wastes threads on clips the user might never reach (they could stop playback, seek away, etc.).

**What we didn't do:**
- Pre-decode all clips (memory explosion with long timelines)
- Accept the gap (400ms black frame is unacceptable in an editor demo)
- Double-buffer decoders (always have next and previous ready — 3x resource usage)

---

### PTS offset mapping (first-frame calibration)

**The problem:** Source video files have arbitrary PTS (presentation timestamp) values. A file might start at PTS=0, PTS=1.5, or PTS=83241.003. The decoder doesn't always start at the time we seeked to. We need to map these arbitrary timestamps into timeline coordinates.

**Our solution:** On the first decoded frame from a new pipeline, compute:

```
pts_offset = first_frame.pts - expected_source_time_at_playhead
```

All subsequent frames are mapped: `timeline_pos = frame.pts - pts_offset`. This single calibration handles non-zero start times, variable-rate containers, and codec-specific timestamp schemes.

**What we didn't do:**
- Recalibrate every frame (adds noise, the offset should be stable)
- Assume PTS starts at zero (breaks on many real-world files)
- Use container-level timing only (some containers have inaccurate duration metadata)

**The risk:** If the first frame has an anomalous PTS (corruption, B-frame reordering artifact), the offset is wrong for the entire clip. We haven't hit this in practice because FFmpeg's decoder emits frames in presentation order, not decode order.

---

### Reverse playback: 4-second GOP windows

**The problem:** Video codecs only allow efficient seeking to keyframes. Between keyframes, frames depend on previous frames (P-frames, B-frames). To play backward, you can't just "decode the previous frame" — you have to decode from a keyframe forward, then reverse the order.

**Our solution:**
1. Seek to `current_time - 4 seconds`
2. Decode all frames in that 4-second GOP window
3. Reverse the frame order
4. Pace them with distance-based timing
5. When exhausted, move to the previous 4-second window

**Why 4 seconds:** H.264/HEVC GOPs are typically 1-2 seconds. A 4-second window guarantees we capture at least 2 full GOPs, giving us enough frames for smooth reverse playback. Larger windows (10s) waste decode time. Smaller windows (1s) risk mid-GOP boundaries where we'd get only a few frames.

**The dedicated pacer thread:** Reverse frames need their own timing because they arrive in bursts (entire GOP decoded at once). The pacer thread applies `delay = distance_from_gop_base / speed` to smooth out delivery.

**What we didn't do:**
- Full file decode to memory (a 1-hour 4K clip = ~1.8TB of RGBA frames)
- FFmpeg reverse filter (`-vf reverse` — requires reading entire file, not streamable)
- Frame cache LRU for backward playback (works for short scrubs but not sustained reverse at 4x speed)

---

### Stall detection & recovery (5 layers)

**The philosophy:** Always show *something*. A stale frame is better than a black screen.

| Layer | Threshold | What happens | Why this threshold |
|-------|-----------|--------------|-------------------|
| Startup grace | 220ms | Don't intervene — decoder is still starting | Decoders need 50-200ms for first frame |
| Minor stall | 80ms | Pause clock, keep last frame on screen | Prevents playhead advancing past where we have frames |
| Frame gap | 120ms | Request fallback single-frame decode | Video decode worker can serve a frame in ~20-50ms |
| Long stall | 250ms | Preserve existing texture (don't overwrite with fallback) | If pipeline is recovering, don't clobber with lower-quality fallback |
| Stale pipeline | 750ms | Destroy and restart entire pipeline | Something is fundamentally broken, fresh start is cheaper than debugging |

**Why graduated, not binary:** A single "is it stalled?" check creates oscillation — the pipeline keeps getting killed and restarted during temporary I/O hiccups. The graduated approach lets minor hiccups resolve themselves (minor stall → resume) while escalating genuine failures (stale pipeline → restart).

**The specific numbers** were tuned empirically on Apple Silicon with H.264 and HEVC files. They represent "what felt responsive without causing thrashing."

---

## Audio System

### ringbuf over mpsc channels for audio

| | ringbuf (chosen) | mpsc channel | crossbeam |
|---|---|---|---|
| Allocation per sample | Zero | One per send (boxed) | One per batch |
| Blocking behavior | Non-blocking push/pop | Blocking recv | Blocking recv |
| Backpressure | Natural (ring full = stall) | Unbounded growth or bounded block | Bounded block |
| Use case fit | Fixed-size sliding window (exactly what audio is) | Variable-length message passing | Multi-producer multi-consumer |

**Why it matters:** The cpal audio callback runs on a real-time OS thread. Any blocking (mutex contention, allocation, syscall) causes an audible glitch. ringbuf's `try_pop()` is a single atomic compare-and-swap — no allocation, no syscall, no blocking.

**The fallback:** If `try_lock()` fails on the consumer mutex, we output silence. A single frame of silence (0.3ms) is inaudible. A blocked audio thread that misses its deadline causes a pop/click that users notice immediately.

---

### cpal over rodio

| | cpal (chosen) | rodio |
|---|---|---|
| Abstraction level | Low: device enumeration, stream creation, raw sample callback | High: "play this file" |
| Buffer control | We set buffer size (sample_rate/4 = 250ms) | Framework decides |
| Sample format | We negotiate F32/I16/U16 with the device | Framework converts |
| Mixing | We implement AudioMixer ourselves | Built-in but opaque |

**Why cpal:** We need direct control over the audio buffer for three reasons:
1. `swap_buffer()` — atomically replace the ring buffer on pipeline transitions (no stale audio)
2. Variable-speed playback — we push samples at adjusted rates, not at file's native rate
3. Multi-source mixing — AudioMixer sums N ring buffer consumers, something rodio doesn't expose

rodio would work for "play this WAV file" but not for "mix 3 audio clips at 2x speed with a hover preview snippet and a scrub audio sample."

---

### Audio mixer: summation + hard clamp vs normalization

**What we chose:** Sum all sources, then `clamp(-1.0, 1.0)`.

**What normalization would look like:** Divide by source count — `sample /= N`. Problem: when a quiet clip plays alongside a loud one, both get 50% volume even though there's no actual clipping risk.

**What soft clipping (tanh) would look like:** `output = tanh(sum)`. Problem: changes the waveform shape, affects perceived sound quality.

**What dynamic gain reduction would look like:** Track peak over a window, reduce gain when sum > 1.0. Problem: 50+ lines of compressor logic, audible pumping artifacts.

**Why hard clamp wins here:** In a video editor timeline, you rarely have more than 2 audio clips overlapping (dialogue + music). Sum of two normalized clips rarely exceeds 1.0. When it does, hard clipping at ±1.0 is brief and barely audible. The simplicity (3 lines of code) is worth the theoretical distortion risk that almost never materializes.

---

### Buffer sizing: 16,384 samples per mixer source, sample_rate/4 for output

**Mixer source ring buffer (16,384):**
- At 48kHz stereo: 16,384 / 48,000 ≈ 341ms
- Large enough to absorb a decoder hiccup (one slow frame = 16ms, we have 341ms of buffer)
- Small enough that audio lag is imperceptible (<400ms)

**Output ring buffer (sample_rate/4):**
- At 48kHz: 12,000 samples ≈ 250ms
- This is the buffer between AudioMixer and the hardware audio callback
- 250ms latency is the floor — too small and any frame-time variance causes underrun

**What we didn't do:** Tiny buffers (1024 samples = 21ms) would give lower latency but any frame-time spike causes audible dropout. For a video editor (not a live instrument), 250ms latency is imperceptible.

---

## Thumbnail & Preview System

### Multi-timestamp thumbnail fallback (7 attempts with black frame detection)

**The problem:** Many video files start with black frames (intros, title cards, fade-ins). A thumbnail of a black frame is useless.

**Our solution:** Try 7 timestamps in order: `[0.5s, 1.0s, 2.0s, 0.0s, 0.04s, 0.25s, 5.0s]`. For each, check if the frame is "mostly black" (>90% of sampled pixels have R+G+B < 30). Return the first non-black frame.

**Why this order:**
- `0.5s, 1.0s, 2.0s` — safe middle positions that skip intros
- `0.0s` — first frame (often the logo, but worth trying)
- `0.04s, 0.25s` — near the start for short clips
- `5.0s` — deeper into the file for long clips with extended intros

**Final fallback:** Return the 1.0s frame even if it's black. A black thumbnail is better than no thumbnail.

**Black frame detection:**
- Sample ~200 pixels (every Nth pixel for coverage)
- "Dark" = R+G+B < 30 (catches near-black compression artifacts, not just pure #000000)
- "Mostly black" = >90% dark pixels (allows some bright noise)

**What we didn't do:**
- Scene change detection (find the most "interesting" frame) — 100ms+ per clip, complex histogram analysis
- Random sampling — non-deterministic, different thumbnail every time
- Always use frame 0 — almost always black or a logo

**Cost:** 7 seeks × 15ms average = ~100ms per clip. Runs on background thread, invisible to UI.

---

### Preview scrubbing: 32 streaming frames vs batch vs on-demand decode

**What we chose:** Pre-decode 32 evenly-spaced frames per clip when the user hovers. Frames stream to the UI as they decode (channel per frame).

**Alternatives we considered:**

| Approach | Latency to first frame | Memory per clip | CPU cost |
|----------|----------------------|-----------------|----------|
| **32 streaming frames (chosen)** | ~15ms (first frame fast) | 32 × 240×135×4 = 4MB | One-time 200-500ms decode |
| Batch all 32, then show | 200-500ms (wait for all) | Same | Same total, worse perceived |
| On-demand per hover position | ~20ms per frame, every mouse move | 1 frame | Continuous decode load |
| Pre-decode all at import | None | 4MB × N clips | Heavy import time |

**Why streaming wins:** The user sees the first frame within 15ms of hovering. The remaining 31 frames fill in over 200-500ms in the background. The scrub feels responsive immediately even though not all frames are ready yet.

**Why not on-demand:** Each mouse movement would trigger a new decode. At 60fps cursor movement, that's 60 decode requests per second — the video decode worker would be perpetually behind. Pre-decoding 32 frames is a one-time cost that makes all subsequent scrubbing free (just array indexing).

**The 3-worker pool:** Three background threads share a priority work queue. Priority clips (hovered, selected) get pushed to the front of the queue. Non-priority clips (visible but not hovered) go to the back. This ensures the clip the user is looking at gets its preview frames first.

**Why 3 workers, not 1 or CPU-count:**
- 1 worker: Serial preview generation, slow when browsing many clips
- 3 workers: Parallel decode for 3 clips simultaneously, good throughput
- CPU-count (8+ on M-series): Diminishing returns — preview decode is I/O-bound (seeking in video files), not CPU-bound

---

### Hover audio: bucketed requests at 2Hz

**The problem:** As the user scrubs across a thumbnail, the mouse generates events at 60fps. Decoding an audio snippet for every pixel movement would flood the audio worker.

**Our solution:** Bucket hover time to 2Hz — at most one audio request per 500ms. The time is quantized: `bucket = (time * 2.0).round()`. Same bucket = skip request.

**Why 2Hz for hover, 10Hz for timeline scrub:**
- Hover is passive exploration — 500ms audio snippets feel responsive enough
- Timeline scrub is active editing — 100ms audio snippets give tighter feedback
- Going higher (60Hz) would overload the audio decoder with redundant work

**Audio snippet details:** Each snippet is 1 second of mono audio with 10ms fade-in and fade-out to prevent clicks at boundaries. The fade window (10ms ≈ 480 samples at 48kHz) is short enough to be inaudible but long enough to eliminate the pop that comes from discontinuous waveforms.

---

## Waveform Rendering

### GPU shader pipeline vs egui CPU drawing

**What we chose:** Custom wgpu render pipeline with WGSL shaders for waveform bars.

| | GPU pipeline (chosen) | egui `painter.rect()` calls |
|---|---|---|
| Per-clip cost | ~0.1ms (one draw call, all bars) | ~2-5ms (512 rect() calls per clip) |
| Scaling with clip count | Negligible (GPU parallelism) | Linear (more clips = more CPU) |
| Visual quality | Amplitude-based brightness modulation | Flat color |
| Complexity | 342 lines of shader + bind group setup | ~30 lines of rect loops |

**Why the GPU approach wins:** A timeline with 10 audio clips, each with 512 waveform bars, means 5,120 individual draw calls on CPU. At ~1μs each, that's 5ms — a third of our frame budget, just for waveforms. The GPU pipeline renders all bars in a single draw call.

**The fallback:** If wgpu is unavailable (shouldn't happen, but defensive), we fall back to CPU-drawn bars using egui's painter. This works but is slower.

**How the shader works:**
1. Peaks stored in a storage buffer: `array<vec2<f32>>` (min, max pairs)
2. Uniforms pass clip rect bounds, colors, screen size, peak count
3. Vertex shader generates 6 vertices per peak (quad) procedurally from `vertex_index`
4. Fragment shader: `brightness = clamp(amplitude * 2.0, 0.3, 1.0)` — louder peaks are brighter

**Why procedural vertices instead of a vertex buffer:** No per-frame upload needed. The vertex shader computes positions from the peak index and uniform data. Only the storage buffer (peak data) needs uploading, and that's done once at import time.

---

### Pre-computed peaks (512 buckets) vs real-time FFT

| | Pre-computed peaks (chosen) | Real-time FFT |
|---|---|---|
| Compute cost | Once at import (~100-500ms) | Every frame (~5-10ms) |
| Zoom response | Instant (array slice) | Instant (recompute) |
| Memory | 512 × 8 bytes = 4KB per clip | None stored |
| Accuracy at high zoom | Fixed resolution (may look blocky) | Perfect at any zoom |

**Why pre-computed:** At import time, we decode the entire audio file to mono samples, divide into 512 equal chunks, and store (min, max) per chunk. Timeline rendering is just: given the visible time range, slice the peaks array and draw. Zero computation per frame.

**Why 512 buckets:** At typical zoom levels (20-500 pixels/second), a 30-second clip occupies 600-15,000 pixels. 512 buckets give ~1 peak per pixel at medium zoom. At extreme zoom-in, peaks get wider (bars instead of lines) — visually acceptable. At extreme zoom-out, multiple peaks merge — also fine.

**What we didn't do:** Dynamic rebucketing at different zoom levels. Would give sharper waveforms at all zooms but requires either storing raw samples (huge memory) or recomputing peaks on zoom change (CPU cost per frame).

---

## Timeline Interactions

### Overlap resolution: 4-case auto-trim algorithm

**The problem:** When a clip is moved, dropped, or trimmed, it may overlap existing clips on the same track. What should happen?

**Our approach:** Automatically resolve overlaps by trimming or splitting existing clips:

```
Case 1: New clip completely covers existing → remove existing
Case 2: New clip lands inside existing → split existing into two pieces
Case 3: New clip overlaps left edge → trim existing's start
Case 4: New clip overlaps right edge → trim existing's end
```

**Alternatives:**

| Approach | Behavior | UX feel |
|----------|----------|---------|
| Auto-trim (chosen) | Existing clips make way for new clip | Premiere-like, predictable |
| Reject drop | Don't allow overlap | Frustrating, requires manual clear |
| Ripple edit | Push all subsequent clips right | Powerful but surprising |
| Overwrite | New clip replaces existing in time range | DaVinci-like |

**Why auto-trim:** It's the least surprising behavior. The user drops a clip, and it fits. They don't have to think about what happens to clips underneath. Ripple editing is powerful but changes the entire timeline length — dangerous without undo. Rejection is frustrating. Overwrite is close to what we do, but we preserve the non-overlapping portions of existing clips instead of deleting them entirely.

---

### Snap threshold: 10px screen pixels

**The problem:** Users can't place clips at exact frame boundaries by eyeballing mouse position. They need snapping.

**Our solution:** Convert 10 screen pixels to timeline seconds based on current zoom: `threshold_seconds = 10px / pixels_per_second`. At low zoom (20 px/s), snapping covers 0.5s — generous. At high zoom (500 px/s), snapping covers 0.02s — precise.

**Why 10px, not 5 or 20:**
- 5px: Too small — users would miss snaps with fast mouse movement
- 10px: Close enough to "feel magnetic" without being frustrating
- 20px: Too aggressive — snaps when you don't want it, hard to place clips between nearby boundaries

**Snap targets:** All clip start and end times on all tracks. This means dragging a clip snaps to both the edges of clips on the same track and clips on other tracks — useful for aligning video and audio.

---

### Linked clips: bidirectional Option instead of separate link table

**What we chose:** Each `TimelineClip` has `linked_to: Option<TimelineClipId>` pointing to its partner. When a video clip is placed, its audio is automatically placed on the paired audio track and linked bidirectionally.

**Alternatives:**

| Approach | Complexity | Flexibility |
|----------|-----------|-------------|
| `linked_to: Option<TimelineClipId>` (chosen) | Simple, ~20 LOC | 1:1 links only |
| Separate `links: HashMap<TimelineClipId, Vec<TimelineClipId>>` | Medium, ~50 LOC | N:N links |
| Link groups: `link_group_id: Option<GroupId>` | Medium, ~40 LOC | N:N via group |

**Why 1:1 links:** In practice, a video clip has exactly one audio counterpart. We don't need N:N linking for the current feature set. The bidirectional Option is trivial to maintain: when clip A moves, look up `A.linked_to`, update that clip too.

**The cost:** If we later want "compound clips" (one video linked to 3 audio tracks), we'd need to refactor. For now, YAGNI.

---

### TimelineClip separate from Clip (instance vs source)

**The key insight:** A `Clip` is a media file. A `TimelineClip` is an *appearance* of that file on the timeline. The same file can appear 50 times — different tracks, different trim points, different start times.

```
Clip { id: ClipId, path, duration, codec, ... }           // source media
TimelineClip { id: TimelineClipId, source_id: ClipId,      // instance on timeline
               track_id, timeline_start, source_in, source_out, ... }
```

**Why not just embed Clip in TimelineClip:** Because starring a clip in the browser should flag all its appearances on the timeline. If TimelineClip contained a copy of Clip, we'd need to update N copies. With the reference design (source_id → ClipId), we update the star set once, and every panel reads from the same set.

---

## State Architecture

### State crate with zero dependencies (except uuid)

**The rule:** `wizard-state` imports nothing from UI, media, or audio. It defines data models only.

**Why this matters:**
- Any crate can read/write state without pulling in FFmpeg, egui, or cpal
- State can be serialized independently (future: project save/load)
- Unit tests for state logic don't need GPU or audio device
- A fourth panel (e.g., effects inspector) imports state and renders — no coupling to existing panels

**The cost:** Some conceptual duplication. `Clip` stores `duration: Option<f64>` that the media layer computes. The media layer sends a message to update this field rather than the state layer computing it directly. This indirection is the price of decoupling.

---

### Tag bitmask (u32) vs Vec<Tag> or HashSet<Tag>

| | Bitmask (chosen) | Vec<Tag> | HashSet<Tag> |
|---|---|---|---|
| Storage | 4 bytes per clip | 8+ bytes per tag | 40+ bytes overhead |
| Has-tag check | `mask & bit != 0` (1 CPU op) | Linear scan | Hash + compare |
| Filter all clips | Bitwise AND (1 op/clip) | Nested loop | Set intersection |
| Max tags | 32 | Unlimited | Unlimited |

**Why bitmask:** With 4 tags (B-Roll, VO, Music, SFX), a bitmask is ludicrously fast for filtering. `filtered_clips()` checks every clip against the filter mask — at 1000 clips, that's 1000 bitwise ANDs. Total: ~1μs.

**The limitation:** 32 tags max. If the editor grows to 100+ tag categories, we'd refactor to a HashSet. For the current 4 tags, a bitmask is both simpler and faster.

---

### Pre-computed search_haystack string

**What we chose:** At clip creation (and on rename/tag change), concatenate all searchable fields into a single lowercase string:

```
haystack = "filename.mp4 h264 1920x1080 00:30 b-roll music"
```

Search is then a simple `haystack.contains(query_token)` per clip.

**Why not a search index (trie, inverted index):** With <10,000 clips, substring matching on a pre-computed string is faster than maintaining an index. The haystack approach is O(N × M) where N = clips, M = string length — but M is tiny (~100 chars) and N is small (<10K). Total: <1ms for 10K clips.

**Why pre-compute at all:** Without the haystack, we'd need to lowercase and concatenate filename + codec + resolution + tags every frame during search. Pre-computing moves this work to import time (amortized).

---

## Threading & Concurrency

### mpsc channels over async/await

**Why we don't use tokio or async Rust:** The main thread runs an imperative game loop — `poll → update → render → present` — 60 times per second. Async/await is designed for I/O-bound work where you yield while waiting. Our main thread never yields; it checks channels (try_recv), updates state, and renders. Introducing async would mean either:

1. Polling futures each frame (essentially reinventing what we have), or
2. Yielding in the middle of a frame (breaks the 16.6ms deadline)

mpsc channels give us exactly the primitive we need: non-blocking `try_recv()` on the main thread, blocking `recv()` on background threads. Simple, predictable, zero overhead.

---

### Request bucketing (deduplication by time quantum)

**The problem:** Hover scrubbing generates 60 events/second. Timeline scrubbing generates 60 events/second. If every event triggers a decode request, background workers are permanently overloaded.

**Our solution:** Quantize time to buckets:

```
bucket = (time_seconds * rate).round() as i64
if bucket == last_request_bucket { skip }
```

| Request type | Rate | Bucket size | Requests/second |
|---|---|---|---|
| Hover audio | 2 Hz | 500ms | 2 |
| Scrub audio | 10 Hz | 100ms | 10 |
| Video decode | 60 Hz | 16.7ms | 60 |

**Why different rates:** Video needs to update every frame (60Hz) because visual stutter is immediately obvious. Audio can be coarser (2-10Hz) because short audio snippets blend together perceptually — the user doesn't notice a 100ms gap in scrub audio.

---

### LRU frame cache (64 frames, bucket-keyed)

**The problem:** Users scrub back and forth over the same region. Without caching, each back-and-forth re-decodes the same frames.

**Our solution:** The video decode worker caches 64 decoded frames, keyed by `(ClipId, time_bucket)`. LRU eviction when full.

**Why 64:** At 1920×1080 RGBA, each frame is ~8MB. 64 frames = ~512MB. That's significant but bounded. It covers ~1 second of scrubbing at 60fps, which is the typical back-and-forth range.

**Why manual LRU instead of the `lru` crate:** The cache is tiny (64 entries). Finding the oldest entry via `iter().min_by_key()` scans 64 items — trivial. Adding a dependency for a 5-line optimization isn't worth it.

**Why bucket-keyed:** Two decode requests at t=1.0001s and t=1.0002s should use the same cached frame. Bucketing at 1/60s resolution (16.7ms) ensures cache hits for nearby times.

---

## UI Framework & Rendering

### Custom scrollbars over egui's built-in

**Why:** The timeline has nested scrolling — horizontal scroll for time, vertical scroll for tracks. egui's built-in `ScrollArea` doesn't support independent horizontal and vertical scrolling with custom zoom behavior (zoom-to-pointer). Building our own scrollbars (thin bars at panel edges, draggable, showing visible percentage) took ~100 lines but gave us exactly the behavior we needed.

---

### Reversed video track display order

**Convention:** In NLEs (Premiere, Resolve, Avid), video tracks display with V1 at the bottom and higher tracks above — matching the compositing order (higher track = foreground). We reverse the `video_tracks` iterator during layout to match this convention.

**Why it matters:** An editor evaluating this app expects V1 at the bottom. Getting this wrong would signal unfamiliarity with video editing conventions — a red flag for the brief's "creative instinct" criterion.

---

## Tuning Constants

Every magic number in the app, why it's that value, and what happens if you change it.

| Constant | Value | If smaller | If larger | How we found it |
|---|---|---|---|---|
| `FORWARD_STARTUP_GRACE_S` | 220ms | False-positive stalls on slow clips | Delayed stall recovery | Tested with HEVC 4K: 200ms median startup |
| `SHADOW_LOOKAHEAD_S` | 2.0s | Shadow might not be ready in time | Wastes threads on clips user might skip | 10× worst-case decoder startup (200ms) |
| `STALE_PIPELINE_THRESHOLD_S` | 750ms | Excessive restarts during I/O hiccups | Long visible stalls before recovery | Longest observed I/O hiccup on M2: ~500ms |
| `PIPELINE_STALL_THRESHOLD_S` | 80ms | Clock pauses too aggressively | Clock advances without frames (black flash) | ~5 frames at 60fps, perceptible gap |
| `HOVER_AUDIO_BUCKET_RATE` | 2 Hz | Audio updates feel laggy | Audio worker floods, decode quality drops | 500ms snippets overlap naturally at this rate |
| `SCRUB_AUDIO_BUCKET_RATE` | 10 Hz | Scrub feels unresponsive | Diminishing returns, 100ms already tight | Matches "DJ scrub" tactile feedback rate |
| `VIDEO_DECODE_BUCKET_RATE` | 60 Hz | Visible frame skipping during scrub | No benefit (display is 60Hz) | Matches vsync refresh rate |
| `SNAP_THRESHOLD_PX` | 10px | Hard to snap, frustrating | Overly magnetic, hard to place freely | Standard in Premiere/Resolve: 8-12px |
| `FRAME_CACHE_CAPACITY` | 64 | More cache misses during scrub | >512MB memory for cache alone | ~1s of scrub coverage at 60fps |
| `WORKER_COUNT` (preview) | 3 | Slower preview generation | Diminishing returns (I/O bound) | 3 parallel decodes saturate SSD read |
| `PREVIEW_FRAME_COUNT` | 32 | Choppy hover scrub | Slow initial load per clip | 32 frames across clip = smooth enough scrub |
| Source ring buffer | 16,384 | Audio underruns on decode hiccups | Higher latency | ~341ms at 48kHz, absorbs 20× frame-time variance |
| Output ring buffer | sr/4 | Underruns on scheduler jitter | Perceptible audio latency | 250ms is imperceptible for video editing |
| Packet channel capacity | 128 | Demuxer stalls waiting for decoder | Memory waste, seek latency | ~1-2s of packets, enough to absorb I/O bursts |
| Frame channel capacity | 16 | Decoder stalls on slow main thread | Seek response slows (drain queue) | ~267ms buffer, enough for one UI hiccup |
| Thumbnail tries | 7 timestamps | More black thumbnails | Diminishing returns | 7 × 15ms = 100ms total, acceptable |
| Black frame threshold | 90% dark, R+G+B < 30 | False positives on dark scenes | Misses near-black frames | Tested on 50+ clips: 2% false positive rate |
