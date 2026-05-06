# orangecat

A cross-platform audio routing and virtual I/O application built in Rust.

Orangecat presents a click-to-toggle routing matrix: rows are inputs, columns are outputs, and each cell enables or disables a route between them. Physical devices discovered at startup are merged with four built-in virtual channels (2 in, 2 out) so you can route audio even with no hardware attached.

## Features

- **Routing matrix UI** — click any cell to enable/disable a route; per-route gain (linear) stored in the model
- **Virtual I/O channels** — two virtual inputs and two virtual outputs available by default; add/remove at runtime
- **Physical device enumeration** — discovers CPAL-compatible devices on startup (sample rate and channel count captured)
- **Audio engine** — background thread drives CPAL streams; matrix snapshots are pushed via lock-free ArcSwap
- **Native GUI** — powered by [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (Wayland and X11 on Linux, Metal on macOS, Direct3D on Windows)

## Building

```sh
cargo build
```

To enable Linux virtual device support via PipeWire:

```sh
cargo build --features virtual-devices
```

## Running

```sh
cargo run
```

The window opens at 1280×720. If no audio devices are found, the app falls back to virtual channels only and logs a warning.

## Project layout

```
src/
  device/      — hardware enumeration and DeviceDescriptor
  engine/      — audio thread, CPAL streams, lock-free ring buffers
  model/       — RoutingMatrix, ChannelId, Route, VirtualIoManager
  platform/    — per-OS stubs (Linux, macOS, Windows)
  ui/          — GPUI components: RootView, MatrixView, MatrixCell, HeaderBar, FooterBar
  state.rs     — top-level AppState (wires model ↔ engine)
  main.rs      — app entry point
```

## Dependencies

| Crate | Purpose |
|---|---|
| [gpui](https://github.com/zed-industries/zed) | Native GPU-accelerated UI |
| [cpal](https://github.com/RustAudio/cpal) | Cross-platform audio I/O |
| [rtrb](https://github.com/mgeier/rtrb) | Real-time lock-free ring buffer |
| [arc-swap](https://github.com/vorner/arc-swap) | Wait-free matrix snapshot swap |
| [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam) | Engine command/event channels |

## License

MIT
