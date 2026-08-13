# Orion User Guide

Orion is a live audio mixer and routing workspace for Linux, Windows, and
macOS. This guide covers the everyday surfaces: the mixer, routing, channels,
EQ, scenes, and settings — plus the platform-specific notes for virtual audio
cables.

- [The Mixer](#the-mixer)
- [Routing](#routing)
- [Channels](#channels)
- [EQ](#eq)
- [Scenes](#scenes)
- [Settings](#settings)
- [Windows & macOS: companion virtual cables](#windows--macos-companion-virtual-cables)

## The Mixer

The Mixer view has two resizable areas — **Sources** on the left, **Outputs**
on the right — with a drag handle between them (the split is remembered). Each
strip, top to bottom:

- **Name** — click to rename the channel.
- **Device selector** — click to assign or change the device the channel uses.
- **L/R meter** — live level per side, −90…0 dB, with a faint peak-hold mark
  and a dim RMS marker line. The strip across the top is the **clip LED**: it
  lights red for a moment when the signal hits full scale. The meter stretches
  with the window.
- **EQ** — HIGH / MID / LOW knobs (see [EQ](#eq)).
- **DELAY** — sync delay knob, 0–500 ms (see below).
- **Mode** — the channel-mode button (see [Channels](#channels)).
- **Route buttons** — one chip per output (`A1`, `B1`, …) on source strips;
  click to connect or disconnect that pair. An outlined chip means "wanted but
  not live yet" (the device is offline or still starting).
- **Fader** — −60…+10 dB. Drag vertically; hold **Shift** for fine adjustment;
  double-click to reset to 0 dB.
- **MUTE** — per-channel mute. The footer's **MUTE ALL** mutes every output at
  once (sources stay live) and is remembered across restarts.

Knobs work the same everywhere: drag vertically, **Shift** for fine steps,
double-click to reset to zero.

**Sync delay** delays that channel's audio everywhere it plays. Use it to align
a fast microphone against a delayed video capture, or to lip-sync an output to
a slow display.

## Routing

Routing is intent-based: toggle a connection once and Orion keeps it — across
device sleep, hot-plug, and reassignments — reconnecting as soon as both sides
are available. If a route ever stalls, Orion detects it and rebuilds it
automatically.

Two ways to connect a source to an output:

- **Route buttons** on each source strip (one chip per output).
- The **Routing** page's matrix: sources × outputs, with the same behavior.

Changing a channel's device migrates its connections to the new device — no
need to re-patch by hand.

## Channels

The **Channels** page manages the strips themselves:

- **Mixer Channels tab** — rename, recolor, assign/clear the device, or delete
  a channel; add sources and outputs. Adding a channel walks you through type
  (physical / application / virtual), an optional device pick, and details.
- **Virtual Devices tab** — Orion's virtual inputs and outputs. While Orion is
  running, **Virtual Inputs appear as playback targets** and **Virtual Outputs
  appear as microphones** in your system's sound settings. One of each always
  exists; add more for extra apps.

Every channel also has a **mode** (the button under DELAY): `AUTO` follows the
device's layout, `ST` stereo, `MN` mono downmix, `L`/`R` single channel, `SW`
swaps left and right. Switching mid-playback crossfades briefly instead of
clicking.

## EQ

Every source and output has a 3-band EQ on its strip — **LOW** (250 Hz shelf),
**MID** (1 kHz bell), **HIGH** (8 kHz shelf) — ±12 dB per band, live in the
real-time path. EQ applies to audio routed through Orion; direct hardware
monitoring is untouched.

## Scenes

Scenes capture the whole rig — channels, controls, routing, and device
bindings — and the **selected scene tracks the mixer live**: while a scene is
active, every change you make saves into it automatically. Switch scenes from
the header dropdown; switching applies the stored state (bindings included).

The **Scenes** page holds the cards: **NEW SCENE** (starts from the current
mixer), **UPDATE** (overwrite a card with the current mixer — useful for scenes
you are not currently on), and **DELETE** (with confirmation). A card without a
captured state shows an "Empty" hint; selecting it once fills it from the
current mixer.

## Settings

The **Settings** page holds the audio engine and the config file:

- **Sample rate** — the rate requested by Orion's route streams, 44.1 kHz up to
  768 kHz; PipeWire converts when a device can't run it natively.
- **Buffer size** — the latency hint for route streams, 32–16384 frames. The
  detected system quantum is the default.
- **RESET DEFAULTS** — back to the detected system clock.
- Changing rate or buffer **rebuilds active connections immediately** (a brief
  dropout).

### The config file

Everything lives in `~/.config/orion/settings.json` — channels, routing,
scenes, engine settings, and layout. Orion **watches it live**: edit or replace
the file and the change applies without a restart. Use **OPEN** to edit it in
your editor, **IMPORT** to load a file from elsewhere, **EXPORT** to save a
copy (backups, or syncing rigs between machines).

A JSON Schema sits next to the file (`settings.schema.json`) for editor
autocomplete, and the canonical copy lives in the repository at
[`schema/settings.schema.json`](../schema/settings.schema.json) — once the repo
is public, external tools can reference it by URL:

```text
https://raw.githubusercontent.com/gardun0/orion/main/schema/settings.schema.json
```

While Orion is running it rewrites the file on internal changes, so export
before editing by hand.

## Windows & macOS: companion virtual cables

On Linux, Orion creates its own virtual devices and sees application streams
natively through PipeWire. Windows and macOS have no built-in user-space
virtual audio device, so Orion's virtual-device management is hidden there —
instead, Orion works with the free companion drivers below. Once installed,
**the cable appears in Orion like any other device**: route it, meter it, EQ
it, exactly as on Linux.

### Windows: VB-CABLE (or any virtual audio cable)

1. Install [VB-CABLE](https://vb-audio.com/Cable/) (free; the paid Hi-Fi
   variant also works). It adds a playback device `CABLE Input` and a
   recording device `CABLE Output`.
2. To mix an app's audio: **Settings → System → Sound → Volume mixer** and set
   that app's **Output device** to `CABLE Input` (per-app, no restart needed
   for most apps). In Orion, add a source channel on the `CABLE Output`
   recording device.
3. To use an Orion mix as a microphone: route sources to a bus assigned to
   `CABLE Input`, then pick `CABLE Output` as the microphone in the target
   app. (Windows exposes one direction per cable; two cables give you both.)

### macOS: BlackHole

1. Install [BlackHole](https://existential.audio/blackhole/) (free, signed;
   2ch is enough for most setups). It adds a device macOS sees as both input
   and output.
2. To mix an app's audio: set that app's output device to `BlackHole 2ch` (in
   the app's own audio settings, or for the whole system in System Settings →
   Sound → Output). In Orion, add a source channel on BlackHole's input side.
3. To use an Orion mix as a microphone: route to a bus assigned to BlackHole,
   then choose `BlackHole 2ch` as the mic in the target app.

### Notes

- Everything else in this guide — routing, scenes, EQ, delay, modes, meters —
  works identically on all three platforms; persistence lives at
  `%APPDATA%\gardun0\orion` (Windows) or `~/Library/Application Support/…`
  (macOS) instead of `~/.config/orion`.
- Capturing audio from a *specific* application without a cable is a separate
  feature (Windows process loopback, macOS audio taps) tracked on the
  roadmap — cables are the reliable story today.
