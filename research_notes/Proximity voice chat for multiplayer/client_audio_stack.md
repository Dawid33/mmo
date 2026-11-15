# Client-side audio stack for voice chat (native Bevy 0.19 + browser wasm), state of November 2026

Reading note: "Cited Findings" are things read on a fetched page or in a search result during this
research (2026-11-14). "Inferences" are my reasoning on top of them. "Gaps" lists what was not confirmed,
including several things I believe from background knowledge (training data up to mid-2026) but did not
re-verify — these are marked "unverified background".

## 1. LiveKit Rust SDK (`livekit` crate) — option A on native

### Takeaway
The `livekit` crate is actively released (0.11.0 on 2026-11-13) and, since May 2026, includes a real
device layer (`PlatformAudio`: microphone capture, speaker playout, device selection, AEC/NS/AGC through
libwebrtc), so a native client no longer has to feed PCM by hand. It does not target the browser: wasm has
to use the JS SDK (`livekit-client`) through JS interop, so option A means two different client
integrations. Its signalling TLS can be built with rustls + ring (no aws-lc-rs named in its manifest).

### Cited Findings
- Latest version is livekit 0.11.0, dated 2026-11-13; the release cadence in 2026 is roughly several
  releases per month (0.7.34 in March, 0.7.40 in May, 0.8.0 on 2026-07-27, 0.9.0 on 2026-09-08, 0.9.7 on
  2026-10-23) — [livekit CHANGELOG on docs.rs](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md)
- Supported platforms per the README: Windows, macOS, Linux, iOS, Android. No wasm32/browser target is
  listed. Crates in the repo: `livekit-api`, `livekit`, `livekit-ffi`, `livekit-protocol`. Licence
  Apache-2.0 — [livekit/rust-sdks README](https://github.com/livekit/rust-sdks)
- A LiveKit community forum reply states the Rust SDK does not support WASM and points to a GitHub issue
  closed as "not planned" (a later poster says a bot auto-closed it; the thread is undated and not an
  official statement) — [community.livekit.io thread](https://community.livekit.io/t/does-the-rust-sdk-support-wasm/1369)
- The crate source has `cfg(not(target_arch = "wasm32"))` gating around the platform audio module, but
  nothing found confirms a working browser build — [livekit 0.7.45 lib.rs](https://docs.rs/crate/livekit/0.7.45/source/src/lib.rs)
- 0.7.40 (2026-05-14) "Get WebRTC ADM into Rust": added `AdmProxy` and the high-level `PlatformAudio` API
  covering microphone capture, speaker playout, device enumeration and selection, with echo cancellation,
  noise suppression and auto gain control configurable with platform-specific defaults. Later: Android
  context init (0.7.42), `set_recording_device` returns `DeviceNotFound` (0.7.46), ADM created lazily on
  first use (0.9.0), index-based device selection (0.9.6, 2026-10-20) — [livekit CHANGELOG](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md)
- Two audio paths exist side by side: "manual audio" (default) where you push frames into a
  `NativeAudioSource`, and "platform audio" where `PlatformAudio::new()` makes WebRTC capture the selected
  recording device itself ("You do NOT push frames manually"); a `LocalAudioTrack` is created from
  `mic.rtc_source()`. Multiple `PlatformAudio` handles share one reference-counted ADM —
  [livekit 0.7.45 platform_audio/mod.rs](https://docs.rs/crate/livekit/0.7.45/source/src/platform_audio/mod.rs);
  [libwebrtc crate audio_source.rs](https://docs.rs/crate/libwebrtc/latest/source/src/audio_source.rs)
- Platform processing notes in the source: Apple platforms use the Voice Processing IO unit for
  AEC/AGC/NS; on Android the software APM is always used because hardware AEC is unreliable on many
  devices — [livekit 0.8.1 platform_audio/processing.rs](https://docs.rs/crate/livekit/0.8.1/source/src/platform_audio/processing.rs)
- Received audio is exposed as PCM frames through `NativeAudioStream` (README example), i.e. the app can
  take decoded per-participant PCM and route it into its own (spatial) mixer —
  [livekit/rust-sdks README](https://github.com/livekit/rust-sdks)
- libwebrtc versions bundled: M125 (Dec 2024) → M144 (0.7.34, 2026-03-22) → M150 (0.8.4, 2026-08-25, a
  build that lets TURN/TLS use the OS trust store); 0.9.7 bumped to a build dropping compromised eMudhra
  roots — [livekit CHANGELOG](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md)
- The README's Linux build notes refer to "the hermetic libc++ shipped inside the libwebrtc artifact",
  i.e. a prebuilt libwebrtc artifact is used rather than a from-source build — [livekit/rust-sdks README](https://github.com/livekit/rust-sdks)
- `webrtc-sys` binds libwebrtc with cxx.rs and adds its own C++ glue — [webrtc-sys README](https://raw.githubusercontent.com/livekit/rust-sdks/main/webrtc-sys/README.md)
- Cargo features of livekit 0.11.0: `default = ["native"]`, `tokio`, `glib-main-loop`, `native-tls`,
  `native-tls-vendored`, `rustls-tls-native-roots`, `rustls-tls-webpki-roots` (all TLS ones pass through to
  `livekit-signaling`) — [livekit 0.11.0 features](https://docs.rs/crate/livekit/latest/features)
- Since 0.8.0 (2026-07-27) TLS features delegate to a new `livekit-net` crate. Its manifest: `native`
  alone enables tokio-tungstenite + reqwest with no TLS; `rustls-tls-native-roots` adds `tokio-rustls`
  with features `logging, tls12, ring`; `ring` is the only provider named, `aws-lc-rs` does not appear in
  the manifest — [livekit-net Cargo.toml](https://raw.githubusercontent.com/livekit/rust-sdks/main/livekit-net/Cargo.toml)
- Other 2026 additions: "data tracks" (0.7.36, 2026-04-02 onward) — a non-media low-latency channel, with
  E2EE fixes in 0.7.37/0.7.52 — [livekit CHANGELOG](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md)
- Examples listed in the repo: `basic_room`, `mobile`, `play_from_disk`, `save_to_disk`; the wgpu/egui
  `rust-dev-client` lives in a separate repository; no Bevy example is mentioned —
  [livekit/rust-sdks README](https://github.com/livekit/rust-sdks)

### Inferences
- Option A cannot share one Rust code path between native and web. Native = `livekit` crate (tokio +
  libwebrtc C++); web = `livekit-client` JS on the page (main thread), driven from wasm via wasm-bindgen
  or plain JS glue, with the browser's own RTCPeerConnection doing capture, AEC, Opus, jitter buffering
  and playout. The game's "work in Web Workers" model does not apply to it — RTCPeerConnection lives on the page.
- The ring-only constraint looks satisfiable for signalling: pick `rustls-tls-native-roots` (ring is
  explicitly set on tokio-rustls). The risk is transitive: tokio-tungstenite/reqwest rustls features may
  default to aws-lc-rs in some versions; the manifest read here does not settle that. Check with
  `cargo tree -i aws-lc-rs` before committing. The media path (DTLS-SRTP) uses libwebrtc's own BoringSSL
  inside the static C++ library, which is outside rustls entirely — a second crypto stack in the binary,
  but not an `aws-lc-rs` crate.
- The SDK needs a tokio runtime (`tokio` feature is default). The game keeps tokio at the networking edge,
  so the LiveKit room would sit on that same edge thread and hand PCM/control to Bevy over channels.
- For proximity voice, spatialization in option A on native means: subscribe to remote tracks, pull PCM
  from `NativeAudioStream`, and mix/pan in the game's audio engine — not letting the ADM play out directly
  (ADM playout is a flat mix). If the ADM does not play the far-end audio, libwebrtc's AEC has no render
  reference unless the app feeds one; whether `PlatformAudio`'s AEC works with app-side playout was not
  confirmed (see Gaps).
- LiveKit's per-version churn (0.7 → 0.11 within 2026, new crates `livekit-net`/`livekit-signaling`)
  means upgrades are a recurring cost; pin exactly.

### Gaps
- Binary size and build-time impact of linking libwebrtc were not found in any fetched page. Unverified
  background: the prebuilt static libwebrtc download is in the hundreds of MB per target and adds roughly
  tens of MB to a release binary; `webrtc-sys` build.rs downloads it from GitHub releases and
  `LK_CUSTOM_WEBRTC` points to a local build. Measure before deciding.
- No `bevy_livekit` crate or official Bevy example was found; not searched exhaustively.
- Whether AEC in `PlatformAudio` can use an app-provided render reference (needed when the game spatializes
  playback itself) is not confirmed.
- libwebrtc's own licence (BSD-3-Clause plus patent grant, unverified background) was not re-checked.
- No official LiveKit statement on wasm for the Rust SDK was found; only the forum reply.

## 2. Pure-Rust WebRTC stacks: `webrtc-rs` and `str0m`

### Takeaway
Both are protocol stacks only (ICE/DTLS/SRTP/RTP/SDP): neither captures, encodes, decodes, jitter-buffers
or plays audio, so against an SFU they replace only libwebrtc's transport half and leave the whole audio
pipeline as DIY. Neither helps in the browser, where the browser's own RTCPeerConnection is the WebRTC
stack. webrtc-rs finished its sans-IO rewrite in 2026 (webrtc 0.20.0 on 2026-07-31); str0m is at 0.20 and
defaults to aws-lc-rs crypto.

### Cited Findings
- webrtc-rs: the sans-IO core crate `rtc` was first announced 2026-01-04 (rtc 0.3.0; 0.7.0 on 2026-01-10
  added mDNS) — [webrtc.rs blog, rtc 0.3.0](https://webrtc.rs/blog/2026/01/04/announcing-rtc-v0.3.0);
  [rtc 0.7.0](https://webrtc.rs/blog/2026/01/10/announcing-rtc-v0.7.0.html)
- webrtc 0.17.0 (end of January 2026) was the final feature release of the Tokio-coupled line; it is now
  bug-fix only — [webrtc v0.17.0 feature freeze](https://webrtc.rs/blog/2026/01/31/webrtc-v0.17.0-feature-freeze-sansio-shift.html)
- webrtc 0.20.0 (announced 2026-07-31) is the first non-prerelease of the new design: an async layer over
  sans-IO `rtc`, runtime-agnostic with Tokio and smol backends; alpha.1 was March 2026, rc.1 2026-06-30.
  0.20.3 exists and its docs tell new projects to start on 0.20 —
  [announcing webrtc v0.20.0](https://webrtc.rs/blog/2026/07/31/announcing-webrtc-v0.20.0);
  [webrtc 0.20.3 on docs.rs](https://docs.rs/crate/webrtc/0.20.3)
- A "Path to webrtc 1.0" post (2026-08-01) and a sans-IO SFU crate (v0.20.0-rc.3, 2026-07-13) exist —
  [path to 1.0](https://webrtc.rs/blog/2026/08/01/the-path-to-webrtc-1.0.html);
  [SFU rc.3](https://webrtc.rs/blog/2026/07/13/announcing-sfu-v0.20.0-rc.3)
- str0m describes itself as "A Sans I/O WebRTC implementation in Rust": no network I/O, no internal
  threads or async tasks. Supported: SDP, ICE, data channels, sender/receiver reports, TWCC, bandwidth
  estimation, simulcast, NACK, packetization, a fixed depacketize buffer, UDP and TCP. Explicitly NOT
  supported: the standard PeerConnection API, an adaptive jitter buffer, audio/video capture, encoding,
  decoding, audio rendering, TURN, network interface enumeration — [algesten/str0m README](https://github.com/algesten/str0m)
- str0m crypto backends are feature flags: `aws-lc-rs` (default), `rust-crypto`, `openssl` (DTLS 1.2
  only), `openssl-dimpl`, `apple-crypto`, `wincrypto`; with default features off a backend must be
  configured explicitly — [algesten/str0m README](https://github.com/algesten/str0m)
- str0m was developed by Martin Algesten at Lookback, which uses it as a server SFU; it is primarily aimed
  at server use (SFUs, recording), peer-to-peer "has been tested less"; BitWHIP is cited as a real-world
  user. Licence MIT OR Apache-2.0 — [algesten/str0m README](https://github.com/algesten/str0m)
- str0m 0.20.0 is published as a multi-crate workspace (`str0m-proto` plus crypto backend crates); no
  release date was visible — [str0m 0.20.0 RELEASE.md](https://docs.rs/crate/str0m/0.20.0/source/docs/RELEASE.md)

### Inferences
- str0m with default features would pull `aws-lc-rs` into the workspace, violating the ring-only rule;
  it would have to be built with `default-features = false` and the `rust-crypto` (or OpenSSL) backend.
  That is a str0m-internal DTLS/SRTP crypto choice, not a rustls provider, but the crate would still be in
  the tree.
- A native client built on str0m or webrtc-rs talking to a LiveKit/other SFU still needs: the SFU's
  signalling protocol reimplemented, cpal capture, APM/AEC, Opus, an adaptive jitter buffer with PLC. That
  is the full DIY audio pipeline plus WebRTC interop work — strictly more work than DIY-over-QUIC, with
  the only gain being SFU compatibility.
- For wasm, both crates are irrelevant: UDP sockets do not exist in the browser; WebRTC there is
  `RTCPeerConnection` via `web-sys` or the SFU's JS SDK.

### Gaps
- webrtc-rs crypto stack (it used ring/rustls-style pure-Rust DTLS historically — unverified background)
  was not checked for 0.20.
- No production reports of str0m or webrtc-rs 0.20 used as a game voice client were found.
- str0m wasm support: the README does not mention it.

## 3. DIY path on native (cpal + Opus + processing + jitter buffer)

### Takeaway
Every building block exists as a crate in late 2026 — including, new this cycle, pure-Rust ports of both
libopus 1.6 (`opus-rs`, `ruopus`) and the WebRTC audio processing module (`sonora`, `aec3`) — but there is
no off-the-shelf NetEq-class adaptive jitter buffer in Rust, and the pure-Rust ports are young (0.1.x).
Push-to-talk + headphones-friendly DIY (no AEC) is a modest project; open-mic-on-speakers quality is where
the work and risk concentrate.

### Cited Findings
Capture/playback
- cpal 0.17.0 was released 2025-12-20; latest seen is 0.17.3. 0.17 added stable device IDs
  (`DeviceTrait::id`, `HostTrait::device_by_id`), JACK on macOS/Windows, `SampleRate` became a `u32`
  alias, error enums became `#[non_exhaustive]`, and `BufferSize::Default` now defers to the host/device
  default — "which can change buffer sizes and latency" — per the changelog/upgrade guide (read via
  mirrors) — [cpal CHANGELOG mirror](https://vat.pages.dev/RustAudio/cpal/blob/master/CHANGELOG.md);
  [cpal 0.17.3 on docs.rs](https://docs.rs/crate/cpal/latest/source/src/host/mod.rs)

Opus
- libopus 1.6 was released 2025-12-15. It adds a wideband-to-fullband bandwidth extension (BWE) model,
  also used with the FARGAN vocoder for deep PLC and DRED decoding. DRED (Deep REDundancy) was introduced
  as experimental in 1.5 (2024-03-04); the 1.6 DRED model is bitstream-incompatible with 1.5's, and the
  authors are "hoping (but not yet promising)" it will not change again before the final DRED standard —
  [libopus 1.6 release](https://opus-codec.org/release/stable/2025/12/15/libopus-1_6.html);
  [Opus 1.6 demo page](https://opus-codec.org/demo/opus-1.6)
- Deep PLC is a build-time option and at run time needs decoder complexity ≥ 5; because it only affects
  the decoder it has no compatibility implications — [Opus 1.5 demo page](https://opus-codec.org/demo/opus-1.5)
- `opus-rs` (restsend), latest seen 0.1.32: a pure-Rust port of libopus 1.6 with no C dependency; docs
  claim performance close to C libopus on x86-64/aarch64, no unsafe in the codec core, and a `no_std`
  build verified on wasm32-unknown-unknown; 0.1.28 fixed a high-bitrate interop bug with libopus —
  [opus-rs on docs.rs](https://docs.rs/crate/opus-rs/0.1.32); [restsend/opus-rs](https://github.com/restsend/opus-rs)
- `ruopus` 0.1.2: a separate from-scratch pure-Rust Opus; builds on wasm32; decoder is `no_std + alloc`
  and passes the official conformance vectors, encoder needs `std`; unsafe limited to SIMD kernels —
  [ruopus on docs.rs](https://docs.rs/crate/ruopus/latest)
- `unsafe-libopus` 0.2.0: a c2rust transpilation of libopus 1.3.1 (so no 1.5/1.6 deep PLC/DRED), still
  mostly unsafe, intended for building without a C toolchain — [unsafe-libopus on docs.rs](https://docs.rs/crate/unsafe-libopus/latest)
- A crate `opus1` 0.3.0 documents `set_complexity(5)` for deep PLC and references libopus 1.6.1 built
  with DRED (the 1.6.1 version is not confirmed from opus-codec.org) — [opus1 on docs.rs](https://docs.rs/crate/opus1/0.3.0)

Echo cancellation / noise suppression / AGC
- `sonora` 0.1.0: "Pure Rust implementation of WebRTC audio processing — the full pipeline with echo
  cancellation (AEC3), noise suppression, automatic gain control (AGC2), and high-pass filtering";
  BSD-3-Clause; requires Rust 1.91+; `sonora-aec3` is published separately; a `sonora-ffi` C API exists —
  [sonora on docs.rs](https://docs.rs/crate/sonora/0.1.0); [sonora-aec3](https://docs.rs/crate/sonora-aec3/0.1.0)
- `aec3` (rubybit/aec3-rs), versions 0.1.x → 0.4.0: a separate community Rust port of AEC3; 0.2.0 dropped
  its special-purpose "voip wrapper" for a generic event-driven DAG; "noise suppression 2" listed as
  planned — [aec3 0.4.0 on docs.rs](https://docs.rs/crate/aec3/0.4.0); [rubybit/aec3-rs](https://github.com/rubybit/aec3-rs)
- The typical WebRTC capture-side order is high-pass filter → echo cancellation → noise suppression →
  gain control → Opus encode, with NetEQ as the receive-side jitter buffer —
  [Fora Soft, WebRTC audio pipeline (2026-06-06)](https://www.forasoft.com/learn/audio-for-video/articles-audio/webrtc-audio-pipeline-end-to-end)

VAD
- `earshot` 1.2.2: pure-Rust neural VAD (0.1.0 was a port of WebRTC VAD; 1.x is a new model), 16 ms mono
  frames at 16 kHz, streaming, returns a 0–1 score (0.5 suggested threshold), ~40 KiB model, ~8 KiB per
  detector, `no_std` with `libm`; the author claims ~40x faster than Silero VAD v6 and more accurate
  (self-reported) — [earshot 1.2.2 on docs.rs](https://docs.rs/crate/earshot/1.2.2)
- Silero VAD's model is ~2 MiB and needs ONNX Runtime, which adds ~8 MB to a binary (per earshot's docs) —
  [earshot 1.2.2 on docs.rs](https://docs.rs/crate/earshot/1.2.2)
- `rlx-vad` 0.2.5 offers Earshot and Silero backends in pure Rust with embedded weights —
  [rlx-vad README](https://docs.rs/crate/rlx-vad/latest/source/README.md)
- A third-party benchmark of `fast-vad` (logistic regression, no NN) reports Silero with a clear accuracy
  edge on messy real-world audio (F1 0.738 vs 0.680) — [themenonlab blog](https://themenonlab.blog/blog/fast-vad-721x-realtime-voice-activity-detection)

What others built
- Simple Voice Chat (Minecraft): Opus codec, RNNoise noise suppression, automatic microphone gain, voice
  activity detection, OpenAL playback with 3D sound, and its own separate UDP port (default 24454/udp)
  beside the game connection, AES-encrypted (the authors do not guarantee its security) —
  [Modrinth: Simple Voice Chat](https://modrinth.com/plugin/simple-voice-chat)
- Mumble: resamples everything to 48 kHz in its audio chain; echo cancellation follows the Speex model
  (the far-end frame must reach the canceller before its echo appears in the mic input) with "Mixed"
  (cheap, works only if speakers are equally loud) and "Multichannel" (better, more CPU) modes; a
  user-tunable jitter buffer, and guidance that 20 ms audio-per-packet gives low delay without device
  driver problems — [Mumble audio settings](https://www.mumble.info/documentation/user/audio-settings/);
  [Mumble docs/dev/AudioInputDebug.md](https://sources.debian.org/src/mumble/1.5.857-1/docs/dev/AudioInputDebug.md)
- Bevy community crates found: `bevy_talk` ("Voice chat while in game"; 0.19.0 dated 2026-06-20 with
  bevy_ecs ^0.19 and cpal ^0.17; a 0.20.0 exists; essentially undocumented, not hosted on GitHub);
  `bevy_audio_stream` 0.1.0 (2024-03-23, streaming audio "designed specifically for use in voice chat
  systems", a building block); `bevy_mod_audio` 0.1.0 ("streaming spatial audio and microphone support",
  cpal mic input) — [bevy_talk on docs.rs](https://docs.rs/crate/bevy_talk/latest);
  [bevy_audio_stream](https://docs.rs/bevy_audio_stream);
  [bevy_mod_audio microphone.rs](https://docs.rs/crate/bevy_mod_audio/latest/source/src/microphone.rs)

### Inferences
- A workable native DIY chain: cpal input (device rate, often 44.1/48 kHz) → resample to 48 kHz mono →
  [AEC with render reference] → NS → AGC → VAD/PTT gate → Opus encode 20 ms frames (in-band FEC on, DTX on)
  → datagram. Receive: per-speaker jitter buffer → Opus decode (PLC on gaps, FEC recovery from the next
  packet, optionally deep PLC at complexity ≥ 5) → per-speaker gain/pan → mix → cpal output or the game
  audio engine.
- The piece nobody ships as a Rust crate is the adaptive jitter buffer with time-stretching (NetEq). str0m
  explicitly excludes it; a DIY one is typically a fixed/slowly adapting playout delay (e.g. 40–80 ms)
  keyed on sequence number, with Opus PLC for holes. This is the main quality gap against option A on bad
  networks, and the main piece of bespoke code.
- DRED is attractive for a datagram path (redundancy for bursts of loss at low bitrate) but the 1.6 model
  is not yet a frozen standard — both ends must run the same libopus build. In this game both ends are
  the game's own clients, so that is controllable on native; a browser's built-in WebCodecs Opus will not
  encode or decode DRED, so DRED implies wasm Opus in the browser too.
- Using a pure-Rust Opus (`opus-rs` or `ruopus`) would give one codec implementation for native and wasm
  (a worker can run it), removing the C toolchain/emscripten question. Both are 0.1.x with one known
  interop bug already fixed in `opus-rs`; a conformance/interop test against C libopus should gate adoption.
- `sonora` is the first credible way to get WebRTC-grade AEC3/NS/AGC2 without linking C++; at 0.1.0 it
  is unproven. The conservative native choice remains the C++ APM (via the `webrtc-audio-processing` crate
  or via LiveKit).
- AEC complicates architecture: the canceller needs the exact mixed far-end signal that goes to the
  speakers as its render reference, time-aligned. If game audio and voice are mixed by the game's audio
  engine, the reference must be tapped after that mix (or voice-only before it, accepting that game sound
  is not cancelled). That coupling is why PTT-first designs skip AEC.
- Work estimate (my judgement, not sourced): PTT, headphones, fixed jitter buffer, Opus PLC+FEC, distance
  attenuation — roughly 2–4 engineer-weeks for both targets including UI and settings; adding open mic
  with NS+VAD — another 1–2 weeks; adding robust AEC, device hot-plug handling, adaptive jitter and
  loss tuning — open-ended, months of long-tail tuning. Simple Voice Chat shows the first two tiers are
  achievable by a small team and good enough for a large player base.

### Gaps
- Not checked this session (unverified background): `opus` crate (0.3.x, bindings via `audiopus_sys`,
  bundles/builds libopus with cmake), `audiopus`, `rubato` (resampler; widely used, current major
  version not confirmed), `nnnoiseless` (pure-Rust RNNoise port, 48 kHz, 10 ms/480-sample frames, also
  outputs a VAD probability), DeepFilterNet (`deep_filter`/libDF, heavier and higher quality; maintenance
  status in 2026 unknown), `webrtc-audio-processing` crate (tonarino; wraps the PulseAudio fork of the
  WebRTC APM, needs a C++ toolchain/meson; version in 2026 not confirmed — the search surfaced only
  sonora/aec3), `webrtc-vad` crate, Speex DSP bindings, Silero via `ort`.
- cpal pitfalls were not found in a citable source this session. Unverified background: input and output
  streams run on separate callbacks with independent clocks (drift → needs a ring buffer plus drift
  compensation or resampling); device sample rates/formats vary (must resample to 48 kHz); callbacks are
  real-time (no allocation/locks); default-device changes and hot-unplug need explicit stream rebuilds;
  WASAPI shared-mode and PipeWire/ALSA buffer sizes vary widely (0.17's `BufferSize::Default` change makes
  this more host-dependent); macOS needs the microphone usage entitlement/Info.plist key.
- No write-ups from Veloren, Lightyear or other open-source Rust games about shipping voice were found;
  `bevy_voice` / `bevy_mod_voip` were not found under those names.
- Simple Voice Chat's and Mumble's internal jitter-buffer designs and Opus settings were not retrieved.
- Release dates of sonora, aec3, opus-rs, ruopus were not visible; maturity is judged from version
  numbers only.

## 4. DIY path in the browser

### Takeaway
As of 2026 the browser has all the pieces natively: `getUserMedia` gives AEC/NS/AGC by constraint,
WebCodecs `AudioEncoder`/`AudioDecoder` with Opus is in Chrome (94+), Firefox desktop (130+) and Safari
(26.0+), and WebTransport with datagrams became Baseline in March 2026 when Safari 26.4 shipped it. The
remaining holes are Firefox for Android (no WebCodecs audio encoder) and older Safari, for which a wasm
Opus fallback is the usual answer. MoQ is still an IETF draft heading for RFC in 2027.

### Cited Findings
- `getUserMedia` audio constraints: `echoCancellation` (boolean or string type — "whether or not echo
  cancellation is preferred and/or required, and if supported, what type"), `noiseSuppression`,
  `autoGainControl`, plus `latency`, `sampleRate`, `channelCount`; each takes a plain value or
  `ideal`/`exact`/`min`/`max`; some settings may be absent where a browser does not support them —
  [MDN MediaTrackConstraints](https://developer.mozilla.org/en-US/docs/Web/API/MediaTrackConstraints)
- `AudioEncoder` support (caniuse): Chrome 94, Edge 94, Firefox desktop 130, Safari macOS/iOS 26.0;
  Firefox for Android has no supporting version (160 marked unsupported); global usage 92.36% —
  [caniuse: AudioEncoder](https://caniuse.com/mdn-api_audioencoder)
- Opus in `AudioEncoder.configure`: Safari unsupported through 18.7, supported from 26.0; MDN recommends
  Opus for most WebCodecs audio encoding and recommends runtime `AudioEncoder.isConfigSupported()` checks —
  [caniuse: AudioEncoder opus](https://caniuse.com/mdn-api_audioencoder_configure_opus);
  [MDN WebCodecs codec selection](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API/Codec_selection)
- Conflict on Firefox: caniuse's top-level `AudioEncoder` row says Firefox 130, while a caniuse sub-row for
  the `opus` configure options appeared to show support only from ~158 in a fragmentary snippet, and
  Mozilla's WebCodecs meta bug is still open — [caniuse: AudioEncoder](https://caniuse.com/mdn-api_audioencoder);
  contradicted/qualified by [caniuse opus options](https://caniuse.com/mdn-api_audioencoder_configure_opus_opus_application)
  and [Bugzilla 1746557](https://bugzilla.mozilla.org/show_bug.cgi?id=1746557)
- WebTransport: Safari 26.4 shipped it by default in March 2026, making it Baseline ("Newly available")
  across major browsers; MDN lists `WebTransport.datagrams` as Baseline 2026 —
  [MDN WebTransport.datagrams](https://developer.mozilla.org/docs/Web/API/WebTransport/datagrams);
  [webrtc.ventures, April 2026](https://webrtc.ventures/2026/04/webtransport-is-now-baseline-what-it-means-for-real-time-media/)
- An MDN content PR notes that the older `datagrams.writable` path for outgoing datagrams is not supported
  in Safari (the newer `createWritable()`-style API is) — [mdn/content PR 44317 (mirror)](https://webproxy.poorya-velaei-d67.workers.dev/https://github.com/mdn/content/pull/44317)
- Operational caveats reported: no browser devtools show WebTransport traffic WebSocket-style, and a
  WebSocket fallback is advised for locked-down enterprise networks (UDP blocked) —
  [webrtc.ventures, April 2026](https://webrtc.ventures/2026/04/webtransport-is-now-baseline-what-it-means-for-real-time-media/)
- A page claiming Safari WebTransport since Safari 17, and one claiming Safari silently falls back to
  HTTP/2 without datagrams, both conflict with MDN and should be treated as unreliable —
  [videosdk.live](https://www.videosdk.live/developer-hub/webtransport/safari-webtransport);
  [echo.semantic-ui.com](https://echo.semantic-ui.com/compat)
- MoQ: `draft-ietf-moq-transport` reached revision 18 on 2026-05-12, with Working Group Last Call targeted
  and an RFC most likely in 2027; the LOC (Low Overhead Media Container, aligned with WebCodecs) draft is
  at -04 (July 2026), with publication targeted for March 2027; the working group is active (IETF 126
  session 2026-07-20) — [draft-ietf-moq-loc](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04);
  [IETF MoQ WG](https://datatracker.ietf.org/wg/moq/about/);
  [Fora Soft MoQ 2026 (vendor blog)](https://www.forasoft.com/blog/article/media-over-quic-moq-streaming-2026)
- moq.dev's `hang` library (JS, published on JSR as `@moq/hang` 0.3.x) uses WebCodecs with Opus and loads
  a libav wasm polyfill when the WebCodecs audio classes are missing; the maintainers chose to stay on
  Opus rather than add AAC for Safari, and a PR notes the polyfill check wrongly skipped Safari versions
  where `AudioEncoder` exists but lacks Opus — [moq.dev blog 2025-09-04](https://moq.dev/blog/latest-features-00);
  [@moq/hang on JSR](https://jsr.io/@moq/hang/doc/util/)
- A vendor blog's view: 2026 is the MoQ pilot year, 2027 the production year for most operators —
  [Fora Soft MoQ 2026](https://www.forasoft.com/blog/article/media-over-quic-moq-streaming-2026)

### Inferences
- Browser DIY pipeline: page calls `getUserMedia({audio:{echoCancellation:true, noiseSuppression:true,
  autoGainControl:true, channelCount:1}})` after a user gesture → `MediaStreamTrackProcessor` or an
  `AudioWorklet` to get PCM → `AudioEncoder` (Opus, 20 ms) → bytes posted to the worker/page that owns the
  WebTransport session → `datagrams` writer. Receive: datagram → jitter buffer → `AudioDecoder` → PCM into
  an `AudioWorklet` ring buffer → per-speaker `GainNode`/`PannerNode` → destination.
- The browser gives AEC for free only when the far-end audio is played through the page's normal audio
  output (Web Audio/`<audio>`), which the browser's canceller observes; this holds for Web Audio playback
  in Chrome. So the browser target gets open-mic-on-speakers quality with no extra work, unlike native DIY.
- The game's "no shared-memory threads" rule matters for audio: without `SharedArrayBuffer` (which needs
  cross-origin isolation), PCM must move between the worklet and workers through `MessagePort`
  `postMessage` with transferable buffers. That works but adds a few ms of jitter and garbage; frames of
  20 ms keep message rates low (50/s per stream).
- Media capture and `AudioContext` must be created/resumed from the page (main thread) in response to a
  user gesture; `AudioEncoder`/`AudioDecoder` are available in dedicated workers, so codec work can sit in
  the existing worker model while capture/playout nodes stay on the page.
- Fallback matrix: Firefox Android and Safari < 26 need wasm Opus (libopus via emscripten, or a pure-Rust
  Opus inside the game's own wasm); Safari < 26.4 has no WebTransport at all, so the game itself does not
  run there and voice need not either. If a pure-Rust Opus proves sound, using it everywhere (native and
  web) removes the WebCodecs support matrix entirely at a small CPU cost.
- MoQ solves relay fan-out/CDN-scale broadcast and tracks/subscriptions; a game that already owns a
  QUIC/WebTransport session per host and its own interest management (proximity) gains little from adopting
  a still-moving draft. Its codebase (moq-dev) is a useful reference for the browser WebCodecs+WebTransport
  audio path, not a dependency to take.

### Gaps
- Not fetched this session (unverified background, all long-standing platform behaviour): `getUserMedia`
  requires a secure context (HTTPS or localhost) and shows a permission prompt; `AudioContext` starts
  suspended until a user gesture (autoplay policy) and must be `resume()`d; Safari may re-prompt for the
  microphone per session and iOS routes/ducks audio when the mic is open; `AudioWorklet` is supported in
  all current browsers; `MediaStreamTrackProcessor` for audio is Chromium-only (Safari/Firefox need an
  `AudioWorkletNode` tap); browser AEC quality varies (Chrome AEC3 strongest; Firefox and Safari weaker
  and constraint handling differs).
- The exact Firefox version where Opus `AudioEncoder` configure options work (130 vs ~158) is unresolved;
  test with `isConfigSupported`.
- `AudioDecoder` Opus support matrix was not fetched separately (expected to match or exceed the encoder).
- No public Discord or other large-vendor write-up on "Opus over WebTransport datagrams" was found; the
  `moq-rs`/moq-dev Rust crate versions were not retrieved.
- Typical achieved mouth-to-ear latency for a WebCodecs + AudioWorklet + datagram path was not found.

## 5. QUIC datagrams in quinn and WebTransport

### Takeaway
QUIC datagrams (RFC 9221) are congestion-controlled and share the connection's congestion window with the
game's reliable streams; quinn's `send_datagram` handles a full send buffer by discarding the oldest unsent
datagrams, which is the right behaviour for voice. A 20 ms Opus frame is far below the ~1200-byte safe
datagram size, so fragmentation is not a concern; the open question is priority against bulk stream data.

### Cited Findings
- quinn (docs.rs latest shown: 0.11.12) `Connection::send_datagram`: "Previously queued datagrams which
  are still unsent may be discarded to make space"; `send_datagram_wait`: "Unlike send_datagram(), this
  method will wait for buffer space during congestion conditions"; `max_datagram_size` returns `None` if
  datagrams are unsupported by the peer or disabled locally; `datagram_send_buffer_space`: when greater
  than zero, sending a datagram of at most that size is guaranteed (not to drop older ones) —
  [quinn Connection docs](https://docs.rs/quinn/latest/quinn/struct.Connection.html)
- quinn-proto `Datagrams::max_size`: the limit "may change over the lifetime of a connection according to
  variation in the path MTU estimate" — [quinn_proto::Datagrams](https://docs.rs/quinn-proto/0.10.6/quinn_proto/struct.Datagrams.html)
- `TransportConfig`: `datagram_receive_buffer_size` — "Maximum number of incoming application datagram
  bytes to buffer, or None to disable incoming datagrams"; "The peer is forbidden to send single datagrams
  larger than this size"; `datagram_send_buffer_size` — "Maximum number of outgoing application datagram
  bytes to buffer"; `initial_mtu` and `min_mtu` default to 1200 ("known to be safe for typical internet
  applications"); MTU discovery is enabled by default; the congestion controller is pluggable
  (`congestion_controller_factory`) — [quinn_proto::TransportConfig](https://docs.rs/quinn-proto/latest/quinn_proto/struct.TransportConfig.html)
- A real-world failure mode: a GStreamer quinn element died with "Sending data failed: datagram too large"
  when sending without checking the limit — [gst-plugins-rs issue 561](https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs/-/issues/561)
- RFC 9221: QUIC datagrams are subject to QUIC congestion control; the RFC argues one congestion controller
  for reliable and unreliable data can be more effective and efficient; implementations SHOULD offer an API
  to prioritise DATAGRAM frames relative to each other and to streams; sharing a connection shares one
  handshake and authentication context; real-time audio/video and gaming are named use cases —
  [RFC 9221](https://rfc-editor.org/rfc/rfc9221)

### Inferences
- Size budget: Opus voice at 24–32 kbit/s with 20 ms frames is ~60–80 bytes per frame; with a small header
  (speaker id, sequence, timestamp/position) ~100 bytes — over ten times under the 1200-byte floor, even
  through WebTransport's HTTP/3 datagram framing (a quarter-stream-id varint of overhead). Always clamp to
  `max_datagram_size()` anyway and never bundle many frames to near the limit.
- Datagrams have no head-of-line blocking against streams at the delivery level (a lost stream packet does
  not delay a datagram), but they do compete for the same congestion window and pacer. During a large
  reliable burst (region/LOD terrain streaming, a join) voice datagrams may sit in the send queue; with
  `send_datagram` the queue then drops the oldest, so voice degrades to gaps (covered by PLC) rather than
  growing latency. Keep `datagram_send_buffer_size` small (a few frames' worth) so stale audio is dropped
  early instead of delivered late.
- Whether quinn schedules DATAGRAM frames ahead of stream data in a packet was not confirmed from docs
  (see Gaps). If it does not, a bulk download can starve voice; this is the concrete reason to consider a
  second connection for voice — it gets its own congestion window, at the cost of a second handshake, a
  second auth ticket, and (in the browser) a second WebTransport session. The codebase already opens
  per-host and observer connections, so the mechanism exists.
- Counter-argument for one connection: two connections on the same path compete with each other anyway
  (two controllers each probing the same bottleneck), and voice at ~4 KB/s is tiny; prioritising datagrams
  within one connection is the cleaner fix if the stack allows it. Recommendation shape: start on the
  existing connection, measure voice datagram send-queue age during heavy terrain streaming, and keep the
  separate-connection option as a fallback.
- In the multi-host world (a player's home connection plus observer connections), proximity voice across
  a host seam has to be relayed server-side or routed over whichever connection covers the speaker; that
  is a server-side routing question outside this note, but the client must accept voice datagrams from any
  connection in its `ConnectionSet`.
- Datagrams are encrypted and authenticated by the QUIC connection, so DIY needs no extra voice
  encryption (unlike Simple Voice Chat's separate UDP port with its own AES).

### Gaps
- quinn's internal frame scheduling order (datagrams vs streams vs stream priorities) was not confirmed;
  read `quinn-proto` `Connection::populate_packet` or test. Default `datagram_send_buffer_size` /
  `datagram_receive_buffer_size` values were not shown on the docs page (unverified background: about
  1 MiB send, ~1.25 MiB receive — far too large for voice; lower them).
- `wtransport` and `web-transport-quinn` datagram APIs and limits were not fetched; unverified background:
  both expose send/receive datagram calls that map to HTTP/3 datagrams (RFC 9297) on a quinn connection and
  inherit quinn's size limit minus the session-id prefix.
- Browser-side behaviour: the WebTransport `datagrams` object has `maxDatagramSize`,
  `incomingHighWaterMark`/`outgoingHighWaterMark` and `incomingMaxAge`/`outgoingMaxAge` knobs (unverified
  background from the W3C spec) which are the browser's equivalent of "drop stale datagrams"; per-browser
  support for the age knobs was not checked.
- No measurement of real-time audio sharing a QUIC connection with bulk streams was found.

## 6. Spatial playback

### Takeaway
On native, the practical 2026 choice for mixing live per-speaker PCM with spatialization inside Bevy 0.19
is either `bevy_seedling` 0.8 (Firewheel; has an optional HRTF feature) or a small custom mixer on cpal;
stock `bevy_audio` is still the default engine in 0.19 and is not designed around live streams. In the
browser, Web Audio's panner/gain nodes do the job and keep the browser AEC effective. Distance attenuation
plus stereo pan is a sufficient minimum; HRTF is optional polish.

### Cited Findings
- Bevy 0.19 still ships `bevy_audio`: the 0.19.0-rc.2 cargo features doc lists the `audio` feature set as
  `bevy_audio, vorbis` — [bevy 0.19.0-rc.2 cargo_features.md](https://docs.rs/crate/bevy/0.19.0-rc.2/source/docs/cargo_features.md)
- Firewheel's README says it is "currently planned to be upstreamed into the Bevy game engine where it will
  become the default audio engine"; latest seen firewheel 0.12.1. A Bevy-side design note assessed
  bevy_seedling/Firewheel for upstreaming, with a minimal feature-parity `bevy_audio` replacement as one
  option — [firewheel 0.12.1 README](https://docs.rs/crate/firewheel/0.12.1/source/README.md);
  [hackmd: firewheel and seedling](https://hackmd.io/@bevy/firewheel_and_seedling)
- `bevy_seedling` 0.8.0 targets Bevy 0.19 (0.7 ↔ 0.18, 0.6 ↔ 0.17). Features: `cpal` backend (default),
  `rtaudio` backend, `web_audio` ("multi-threading web backend", off by default), `hrtf` and
  `hrtf_subjects` (HRTF spatialization with embedded HRTF data, off by default), `resample_inputs` (audio
  input resampling). Users must disable `bevy_audio` to avoid name clashes —
  [bevy_seedling 0.8.0 README](https://docs.rs/crate/bevy_seedling/latest/source/README.md);
  [bevy_seedling on docs.rs](https://docs.rs/crate/bevy_seedling/0.8.0)
- Earlier bevy_seedling versions documented a `stream` feature enabling "CPAL input and output stream
  nodes" (not listed in the 0.8.0 feature table read here; `resample_inputs` suggests input streams are
  still present) — [bevy_seedling features on lib.rs](https://lib.rs/crates/bevy_seedling/features)
- `firewheel-web-audio`: a multi-threaded wasm32-unknown-unknown Web Audio backend for Firewheel; it
  relies on Wasm multi-threading and therefore needs cross-origin isolation headers ("credentialless" may
  not work on Safari); currently stereo inputs and outputs only —
  [firewheel-web-audio on lib.rs](https://lib.rs/crates/firewheel-web-audio)
- Community building blocks for live audio in Bevy: `bevy_audio_stream` 0.1.0 (2024) and `bevy_mod_audio`
  0.1.0 ("streaming spatial audio and microphone support") — [bevy_audio_stream](https://docs.rs/bevy_audio_stream);
  [bevy_mod_audio](https://docs.rs/crate/bevy_mod_audio/latest/source/src/microphone.rs)
- Simple Voice Chat plays voice through OpenAL with 3D sound, i.e. it reuses the game's positional audio
  engine for voice — [Modrinth: Simple Voice Chat](https://modrinth.com/plugin/simple-voice-chat)

### Inferences
- Firewheel's web backend needs wasm shared-memory threads + cross-origin isolation, which this project's
  wasm build explicitly does not have. So `bevy_seedling` cannot be the web voice mixer here unless it has
  a single-threaded web path (not confirmed). On the web the voice mixer should be plain Web Audio nodes
  driven from JS glue: one `AudioWorkletNode` (or one worklet with N outputs) per speaker feeding
  `GainNode` → `PannerNode` (`panningModel: "equalpower"` or `"HRTF"`) → destination, with listener pose
  updated from the game camera each frame.
- Since the two targets need different playout back ends anyway, the shared abstraction should be thin:
  "voice sink: push 20 ms mono PCM for speaker S; set S's position/gain", implemented by (native) a
  lock-free ring buffer per speaker read by an audio-thread mixer, and (web) a worklet message port.
- Minimum viable spatialization, engine-independent: gain = clamp(1 − (d − d_near)/(d_far − d_near))
  (or inverse-distance with a hard cutoff equal to the server's proximity radius) and constant-power stereo
  pan from the speaker's azimuth relative to the listener; ~30 lines in a custom mixer. In a top-down
  commander camera (fortress mode) the "listener" is ambiguous — camera focus point vs. a controlled unit —
  and that is a design decision, not a technical one.
- If the game adopts `bevy_seedling` for game audio generally (it tracks Bevy 0.19 and is the declared
  future default), voice on native can enter the Firewheel graph as a custom stream node per speaker and get
  its spatial/HRTF nodes for free. If the game stays on `bevy_audio`, a dedicated cpal output stream for
  voice is simpler than forcing live PCM through rodio `Decodable` sources.
- A separate cpal output stream for voice (rather than mixing in the game engine) also makes the AEC render
  reference easy to tap on native, at the cost of game sound not being echo-cancelled.

### Gaps
- Whether `bevy_audio` in 0.19 can play an endless live PCM source well was not confirmed. Unverified
  background: a custom `Decodable` asset whose decoder is an `Iterator<Item = f32>` pulling from a channel
  works (returning silence when empty rather than `None`), spatial playback is plain stereo
  panning/attenuation via `SpatialListener` with no HRTF, and latency/buffering are not tunable.
- `bevy_kira_audio` (Bevy 0.19 support, streaming sources), `oddio` (has a `Stream` signal and
  `Spatial`/`SpatialScene`; maintenance status), `audionimbus`/`steam-audio` bindings and
  `bevy_steam_audio` were not checked this session.
- Whether bevy_seedling 0.8 still has stream writer/reader nodes for pushing external PCM, and whether a
  single-threaded web backend exists, were not confirmed.
- Final Bevy 0.19 release notes were not read for audio changes (only the rc.2 feature doc).

## 7. Push-to-talk vs open mic, and the AEC requirement

### Takeaway
AEC is only needed when an open microphone can hear the speakers; push-to-talk (or headphones) removes
most of the need. The browser and LiveKit both give AEC for free, so the AEC question is specifically a
cost of native DIY — which makes "PTT by default, open mic as an opt-in labelled for headphones" the cheap
default there.

### Cited Findings
- Simple Voice Chat ships with voice activity detection plus RNNoise and automatic gain, and no echo
  canceller in its listed feature set — [Modrinth: Simple Voice Chat](https://modrinth.com/plugin/simple-voice-chat)
- Mumble offers echo cancellation as a setting with two modes and notes the cheap mode only works well when
  speakers are equally loud — [Mumble audio settings](https://www.mumble.info/documentation/user/audio-settings/)
- Browser: `echoCancellation`, `noiseSuppression`, `autoGainControl` are per-track `getUserMedia`
  constraints — [MDN MediaTrackConstraints](https://developer.mozilla.org/en-US/docs/Web/API/MediaTrackConstraints)
- LiveKit native: AEC/NS/AGC configurable in `PlatformAudio` with platform defaults (Apple VPIO; Android
  software APM) — [livekit CHANGELOG](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md);
  [platform_audio/processing.rs](https://docs.rs/crate/livekit/0.8.1/source/src/platform_audio/processing.rs)
- Native DIY AEC options that exist: pure-Rust `sonora` (AEC3 + NS + AGC2, 0.1.0) and `aec3` (0.4.0) —
  [sonora](https://docs.rs/crate/sonora/0.1.0); [aec3](https://docs.rs/crate/aec3/0.4.0)

### Inferences
- With PTT, the mic is closed while the player listens most of the time; echo only leaks while the key is
  held, and half-duplex ducking (lower voice playback volume while transmitting) is a cheap mitigation.
  With open mic + VAD on speakers and no AEC, other players' voices re-trigger the VAD and are retransmitted
  — the classic feedback complaint — so open mic without AEC should be gated behind a "using headphones"
  toggle or shipped only where AEC exists.
- Resulting matrix: browser DIY → open mic is safe (browser AEC); option A → open mic is safe on both
  targets; native DIY → PTT default, open mic for headphone users, AEC (sonora or C++ APM) as a later
  milestone. Noise suppression (RNNoise-class) and a VAD/noise gate are worth having even with PTT
  (keyboard clatter).
- For a DF-style commander game, voice is social/co-op rather than twitch-tactical; PTT default also
  limits moderation exposure and bandwidth (DTX/silence = no datagrams).

### Gaps
- No sourced data on what share of PC players use speakers vs headsets, or on player preference for PTT vs
  open mic in proximity-chat games.
- No independent quality evaluation of `sonora`/`aec3` against the C++ AEC3 was found.

## 8. Decision-relevant summary: (A) SFU SDK vs (B) Opus over the game's QUIC/WebTransport

### Takeaway
(A) buys mature capture/AEC/jitter handling at the price of two separate client integrations (Rust+libwebrtc
natively, JS SDK in the browser), a second transport and auth domain, and a large C++ dependency; (B) reuses
the existing connections, auth and interest management and in 2026 is feasible on both targets with
browser-native pieces, but the team owns the jitter buffer and native echo cancellation.

### Cited Findings
- (A) native: `livekit` 0.11.0, device capture/playout and AEC/NS/AGC included since 0.7.40; platforms
  Windows/macOS/Linux/iOS/Android; Apache-2.0; bundles libwebrtc M150 —
  [livekit CHANGELOG](https://docs.rs/crate/livekit/latest/source/CHANGELOG.md);
  [README](https://github.com/livekit/rust-sdks)
- (A) web: the Rust SDK is not a browser SDK (forum reply; no wasm in the platform list) —
  [community thread](https://community.livekit.io/t/does-the-rust-sdk-support-wasm/1369)
- (B) web: WebTransport datagrams Baseline since March 2026 (Safari 26.4); WebCodecs Opus encoder in Chrome
  94+, Firefox desktop 130+, Safari 26+; not in Firefox Android —
  [MDN datagrams](https://developer.mozilla.org/docs/Web/API/WebTransport/datagrams);
  [caniuse AudioEncoder](https://caniuse.com/mdn-api_audioencoder)
- (B) native: quinn datagrams drop oldest-unsent on overflow and are congestion-controlled with streams —
  [quinn Connection docs](https://docs.rs/quinn/latest/quinn/struct.Connection.html);
  [RFC 9221](https://rfc-editor.org/rfc/rfc9221)
- (B) codec: libopus 1.6 (2025-12-15) with deep PLC and (still non-final) DRED; pure-Rust ports exist —
  [libopus 1.6](https://opus-codec.org/release/stable/2025/12/15/libopus-1_6.html);
  [opus-rs](https://docs.rs/crate/opus-rs/0.1.32); [ruopus](https://docs.rs/crate/ruopus/latest)
- Prior art for (B)-style designs in games: Simple Voice Chat (own UDP channel, Opus, RNNoise, VAD,
  positional playback through the game's audio engine) — [Modrinth](https://modrinth.com/plugin/simple-voice-chat)

### Inferences
- Crypto constraint: (B) adds no TLS stack at all (voice rides the existing ring-backed QUIC). (A) adds
  BoringSSL inside libwebrtc (not a Rust crate, so it does not violate "keep `aws-lc-rs` out" literally)
  and a rustls signalling client that can be configured with ring; verify transitive features. str0m-based
  variants default to `aws-lc-rs` and need non-default features.
- Proximity fit: with (B) the server already knows positions and regions and can forward a speaker's
  datagrams only to nearby listeners (and attach positions); with (A) proximity must be expressed as SFU
  subscriptions/permissions managed by the game server through the LiveKit server API, or everyone in a
  room receives everyone and the client attenuates — a privacy and bandwidth difference.
- Spatialization is DIY in both options on native (pull PCM, pan in the game mixer); in the browser (A)
  gives `MediaStreamTrack`s that plug into Web Audio panners, and (B) gives PCM into worklets feeding the
  same panners.
- Risk profile: (A)'s risk is integration/operations (SFU deployment, token service, two SDKs, SDK churn,
  binary size, a tokio + C++ thread world beside Bevy); (B)'s risk is audio quality engineering (jitter
  buffer, loss behaviour, native AEC, device handling) — reducible by PTT-first scope.
- A hybrid is possible but mostly gets the costs of both: e.g. (B) transport with the WebRTC APM (C++ or
  `sonora`) for native processing is the natural upgrade path of (B) rather than a hybrid with an SFU.

### Gaps
- Server-side cost and behaviour (SFU capacity, relaying datagrams across hosts, moderation/recording) is
  outside this note's scope and was not researched here.
- No measured comparison of perceived quality between a NetEq-based stack and a simple fixed jitter buffer
  with Opus PLC/FEC under realistic loss was found.
- LiveKit SDK binary-size and compile-time figures remain unmeasured (see section 1 Gaps).
