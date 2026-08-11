use std::collections::HashMap;

use orion::domain::{
    AudioEndpoint, AudioRoute, BackendCapabilities, ChannelMode, EndpointId, EndpointState,
    EndpointType, MeterFrame, RouteId, RouteState, VirtualDeviceId,
};

pub const SAMPLE_RATE_OPTIONS: [u32; 10] = [
    44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000, 705_600, 768_000,
];
pub const BUFFER_SIZE_OPTIONS: [u32; 10] =
    [32, 64, 128, 256, 512, 1_024, 2_048, 4_096, 8_192, 16_384];
/// Maximum per-channel sync delay in milliseconds (knob range).
pub const MAX_DELAY_MS: f32 = 500.0;

/// Per-channel 3-band EQ gains in dB (low shelf / mid bell / high shelf).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EqBands {
    pub low_db: f32,
    pub mid_db: f32,
    pub high_db: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqBand {
    Low,
    Mid,
    High,
}

impl EqBand {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Mid => "Mid",
            Self::High => "High",
        }
    }
}

impl EqBands {
    /// Clamp and store a band gain; returns the stored value when it changed.
    pub fn set(&mut self, band: EqBand, db: f32) -> Option<f32> {
        let db = db.clamp(orion_dsp::EQ_MIN_DB, orion_dsp::EQ_MAX_DB);
        let slot = match band {
            EqBand::Low => &mut self.low_db,
            EqBand::Mid => &mut self.mid_db,
            EqBand::High => &mut self.high_db,
        };
        if (*slot - db).abs() > f32::EPSILON {
            *slot = db;
            return Some(db);
        }
        None
    }
}

/// Clamp and store a strip delay; returns the stored value when it changed.
fn set_strip_delay(slot: &mut f32, delay_ms: f32) -> Option<f32> {
    let delay_ms = delay_ms.clamp(0.0, MAX_DELAY_MS);
    if (*slot - delay_ms).abs() > f32::EPSILON {
        *slot = delay_ms;
        return Some(delay_ms);
    }
    None
}

fn delay_status(name: &str, delay_ms: f32) -> String {
    if delay_ms <= 0.0 {
        format!("'{name}' delay off")
    } else {
        format!("'{name}' delayed {delay_ms:.1} ms")
    }
}
pub fn format_sample_rate(sample_rate: u32) -> String {
    if sample_rate.is_multiple_of(1_000) {
        format!("{} kHz", sample_rate / 1_000)
    } else {
        format!("{:.1} kHz", sample_rate as f64 / 1_000.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppView {
    Mixer,
    Routing,
    Devices,
    Scenes,
    Settings,
}

impl AppView {
    pub const ALL: [Self; 5] = [
        Self::Mixer,
        Self::Routing,
        Self::Devices,
        Self::Scenes,
        Self::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mixer => "Mixer",
            Self::Routing => "Routing",
            Self::Devices => "Channels",
            Self::Scenes => "Scenes",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Physical,
    Desktop,
    Application,
    Virtual,
}

/// Preset accent colors for mixer channels. Raw hex values mirror `ui::theme`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelColor {
    Cyan,
    Blue,
    Purple,
    Teal,
    Amber,
    Red,
    Green,
    Pink,
}

impl ChannelColor {
    pub const ALL: [Self; 8] = [
        Self::Cyan,
        Self::Blue,
        Self::Purple,
        Self::Teal,
        Self::Amber,
        Self::Red,
        Self::Green,
        Self::Pink,
    ];

    pub const fn value(self) -> u32 {
        match self {
            Self::Cyan => 0x22D3EE,
            Self::Blue => 0x60A5FA,
            Self::Purple => 0xA78BFA,
            Self::Teal => 0x2DD4BF,
            Self::Amber => 0xF59E0B,
            Self::Red => 0xF87171,
            Self::Green => 0x34D399,
            Self::Pink => 0xF472B6,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Cyan => Self::Blue,
            Self::Blue => Self::Purple,
            Self::Purple => Self::Teal,
            Self::Teal => Self::Amber,
            Self::Amber => Self::Red,
            Self::Red => Self::Green,
            Self::Green => Self::Pink,
            Self::Pink => Self::Cyan,
        }
    }
}

/// What a channel captures or where it plays, chosen at creation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorRole {
    Physical,
    Application,
    Virtual,
}

impl EditorRole {
    pub const SOURCES: [Self; 3] = [Self::Physical, Self::Application, Self::Virtual];
    pub const OUTPUTS: [Self; 2] = [Self::Physical, Self::Virtual];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Physical => "PHYSICAL",
            Self::Application => "APPLICATION",
            Self::Virtual => "VIRTUAL",
        }
    }
}

/// Steps of the channel creation wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorStep {
    Type,
    Endpoint,
    Details,
}

/// In-progress channel/bus creation form.
#[derive(Clone, Debug)]
pub struct ChannelEditor {
    pub is_source: bool,
    pub step: EditorStep,
    pub role: EditorRole,
    pub name: String,
    pub color: ChannelColor,
    pub endpoint: Option<EndpointId>,
}

/// Virtual I/O page tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HubTab {
    Channels,
    VirtualDevices,
}

#[derive(Clone, Debug)]
pub struct SourceStrip {
    pub endpoint_id: Option<EndpointId>,
    /// Last known device name; survives restarts so an offline selection can
    /// still show what it is waiting for.
    pub endpoint_name: Option<String>,
    pub name: String,
    pub detail: String,
    pub kind: SourceKind,
    pub color: ChannelColor,
    /// When true, inventory refresh will not replace the binding automatically.
    pub user_bound: bool,
    pub gain_db: f32,
    pub muted: bool,
    /// Sync delay applied to this source's audio in every route, in ms.
    pub delay_ms: f32,
    /// 3-band EQ applied to this source's audio in every route.
    pub eq: EqBands,
    /// Channel mapping mode (mono downmix, single channel, swap...).
    pub mode: ChannelMode,
    pub routes: Vec<bool>,
    pub meter_l: f32,
    pub meter_r: f32,
    pub online: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusKind {
    Physical,
    Virtual,
}

#[derive(Clone, Debug)]
pub struct OutputBus {
    pub endpoint_id: Option<EndpointId>,
    /// Last known device name; survives restarts so an offline selection can
    /// still show what it is waiting for.
    pub endpoint_name: Option<String>,
    pub code: String,
    pub name: String,
    pub detail: String,
    pub kind: BusKind,
    pub color: ChannelColor,
    pub user_bound: bool,
    pub gain_db: f32,
    pub muted: bool,
    /// Sync delay applied to every route playing into this bus, in ms.
    pub delay_ms: f32,
    /// 3-band EQ applied to every route playing into this bus.
    pub eq: EqBands,
    /// Channel mapping mode (mono downmix, single channel, swap...).
    pub mode: ChannelMode,
    pub meter_l: f32,
    pub meter_r: f32,
    pub online: bool,
}

pub const MAX_PHYSICAL_INPUT_CHANNELS: usize = 2;

#[derive(Clone, Debug)]
pub struct Scene {
    pub name: String,
    pub description: String,
    /// Captured mixer state; None until the user saves into the scene.
    pub snapshot: Option<SceneSnapshot>,
}

/// A full mixer snapshot captured into a scene: channels, controls and the
/// active route matrix by stable endpoint ids.
#[derive(Clone, Debug, Default)]
pub struct SceneSnapshot {
    pub sources: Vec<ChannelSnapshot>,
    pub outputs: Vec<ChannelSnapshot>,
    pub routes: Vec<(EndpointId, EndpointId)>,
}

#[derive(Clone, Debug)]
pub struct ChannelSnapshot {
    pub name: String,
    pub color: ChannelColor,
    pub gain_db: f32,
    pub muted: bool,
    /// Per-channel sync delay in milliseconds.
    pub delay_ms: f32,
    /// 3-band EQ gains in dB.
    pub eq: EqBands,
    /// Channel mapping mode.
    pub mode: ChannelMode,
    pub endpoint_id: Option<EndpointId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaderTarget {
    Source(usize),
    Output(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointPickerTarget {
    Source(usize),
    Output(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct FaderDrag {
    pub target: FaderTarget,
    pub last_y: gpui::Pixels,
}

/// Which knob parameter a drag is adjusting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnobParam {
    Delay,
    EqLow,
    EqMid,
    EqHigh,
}

impl KnobParam {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Delay => "DELAY",
            Self::EqLow => "LOW",
            Self::EqMid => "MID",
            Self::EqHigh => "HIGH",
        }
    }
}

/// A knob on a channel strip: which strip, which parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnobTarget {
    pub strip: FaderTarget,
    pub param: KnobParam,
}

#[derive(Clone, Copy, Debug)]
pub struct KnobDrag {
    pub target: KnobTarget,
    pub last_y: gpui::Pixels,
}

#[derive(Clone, Copy, Debug)]
pub struct DividerDrag {
    pub last_x: gpui::Pixels,
}

/// Which settings selector is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSelect {
    SampleRate,
    BufferSize,
}

impl SettingsSelect {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SampleRate => "rate",
            Self::BufferSize => "buffer",
        }
    }
}

/// An open settings dropdown: which selector and where the click landed.
#[derive(Clone, Copy, Debug)]
pub struct SettingsDropdown {
    pub select: SettingsSelect,
    pub anchor: gpui::Point<gpui::Pixels>,
}

/// Default width of the mixer outputs panel in pixels.
pub const DEFAULT_OUTPUTS_PANEL_WIDTH: f32 = 584.0;
/// Minimum width of the outputs panel (about three strips).
pub const MIN_OUTPUTS_PANEL_WIDTH: f32 = 320.0;
/// The sources panel never gets squeezed below this while resizing.
pub const MIN_SOURCES_PANEL_WIDTH: f32 = 400.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceMonitorStatus {
    Connecting,
    Connected,
    Error(String),
}

pub struct AppState {
    pub active_view: AppView,
    pub devices: Vec<AudioEndpoint>,
    pub device_monitor_status: DeviceMonitorStatus,
    pub sources: Vec<SourceStrip>,
    pub outputs: Vec<OutputBus>,
    pub scenes: Vec<Scene>,
    pub selected_scene: usize,
    pub dirty: bool,
    pub master_muted: bool,
    pub sample_rate: u32,
    pub buffer_size: u32,
    /// Global audio delay added to every output, for A/V lip sync.
    pub status_message: String,
    /// Detected graph clock from PipeWire's `settings` metadata. None until
    /// the backend reports it; the UI then shows these instead of presets.
    pub detected_audio: Option<(u32, u32)>,
    /// True once the user picks a rate/buffer explicitly; detection stops
    /// overwriting their choice.
    pub audio_overridden: bool,
    pub drag: Option<FaderDrag>,
    pub knob_drag: Option<KnobDrag>,
    pub divider_drag: Option<DividerDrag>,
    pub active_routes: HashMap<(EndpointId, EndpointId), RouteId>,
    /// Last route connection failure per matrix cell `(source, output)`.
    pub route_errors: HashMap<(usize, usize), String>,
    pub endpoint_picker: Option<EndpointPickerTarget>,
    /// `(is_source, index)` of the channel being renamed, plus the edit buffer.
    pub rename_target: Option<(bool, usize)>,
    pub rename_buffer: String,
    /// Virtual device pending delete confirmation.
    pub confirm_delete_virtual: Option<VirtualDeviceId>,
    /// Channel/bus pending delete confirmation: `(is_source, index)`.
    pub confirm_delete_channel: Option<(bool, usize)>,
    /// Scene index awaiting delete confirmation.
    pub confirm_delete_scene: Option<usize>,
    /// In-progress channel/bus creation form.
    pub channel_editor: Option<ChannelEditor>,
    /// New-scene creation name buffer (modal).
    pub scene_editor: Option<String>,
    /// Whether the header scene dropdown is open.
    pub scene_dropdown_open: bool,
    /// Width of the mixer outputs panel in pixels (user-resizable).
    pub outputs_panel_width: f32,
    /// Open settings selector dropdown (sample rate / buffer size).
    pub settings_dropdown: Option<SettingsDropdown>,
    /// Selected tab on the Channels page.
    pub hub_tab: HubTab,
    /// Features the active backend reports at runtime. Virtual-device
    /// affordances are gated on this: unsupported platforms (Windows without
    /// a driver, macOS without a plugin) must not offer creation.
    pub backend_capabilities: BackendCapabilities,
}

impl AppState {
    pub fn new(devices: Vec<AudioEndpoint>) -> Self {
        let outputs = default_output_template();
        let route_count = outputs.len();
        let sources = default_source_template(route_count);
        let mut state = Self {
            active_view: AppView::Mixer,
            devices: Vec::new(),
            device_monitor_status: DeviceMonitorStatus::Connecting,
            sources,
            outputs,
            scenes: vec![Scene {
                name: "Studio".into(),
                description: "Default mixer state".into(),
                snapshot: None,
            }],
            selected_scene: 0,
            dirty: false,
            master_muted: false,
            sample_rate: 48_000,
            buffer_size: 512,
            status_message: "Connecting to PipeWire…".into(),
            detected_audio: None,
            audio_overridden: false,
            drag: None,
            knob_drag: None,
            divider_drag: None,
            active_routes: HashMap::new(),
            route_errors: HashMap::new(),
            endpoint_picker: None,
            rename_target: None,
            rename_buffer: String::new(),
            confirm_delete_virtual: None,
            confirm_delete_channel: None,
            confirm_delete_scene: None,
            channel_editor: None,
            scene_editor: None,
            scene_dropdown_open: false,
            outputs_panel_width: DEFAULT_OUTPUTS_PANEL_WIDTH,
            settings_dropdown: None,
            hub_tab: HubTab::Channels,
            backend_capabilities: BackendCapabilities::default(),
        };
        if !devices.is_empty() {
            state.set_devices(devices);
        }
        state
    }

    pub fn adjust_fader(&mut self, target: FaderTarget, delta: f32) {
        let gain = match target {
            FaderTarget::Source(index) => self
                .sources
                .get_mut(index)
                .map(|source| &mut source.gain_db),
            FaderTarget::Output(index) => self
                .outputs
                .get_mut(index)
                .map(|output| &mut output.gain_db),
        };
        if let Some(gain) = gain {
            *gain = (*gain + delta).clamp(-60.0, 10.0);
            self.dirty = true;
        }
    }

    // ---- Channel editor (creation modal) ----

    pub fn open_channel_editor(&mut self, is_source: bool) {
        let virtual_supported = self.backend_capabilities.virtual_devices;
        let default_role = if is_source {
            if self
                .sources
                .iter()
                .filter(|source| source.kind == SourceKind::Physical)
                .count()
                < MAX_PHYSICAL_INPUT_CHANNELS
            {
                EditorRole::Physical
            } else {
                EditorRole::Application
            }
        } else if virtual_supported {
            EditorRole::Virtual
        } else {
            EditorRole::Physical
        };
        let name = default_channel_name(self, is_source, default_role);
        let color = next_free_color(self, is_source);
        self.channel_editor = Some(ChannelEditor {
            is_source,
            step: EditorStep::Type,
            role: default_role,
            name,
            color,
            endpoint: None,
        });
    }

    pub fn cancel_channel_editor(&mut self) {
        self.channel_editor = None;
    }

    pub fn editor_next_step(&mut self) {
        if let Some(editor) = self.channel_editor.as_mut() {
            editor.step = match editor.step {
                EditorStep::Type => EditorStep::Endpoint,
                EditorStep::Endpoint => EditorStep::Details,
                EditorStep::Details => EditorStep::Details,
            };
        }
    }

    pub fn editor_prev_step(&mut self) {
        if let Some(editor) = self.channel_editor.as_mut() {
            editor.step = match editor.step {
                EditorStep::Type => EditorStep::Type,
                EditorStep::Endpoint => EditorStep::Type,
                EditorStep::Details => EditorStep::Endpoint,
            };
        }
    }

    pub fn editor_choose_role(&mut self, role: EditorRole) {
        self.editor_set_role(role);
    }

    pub fn editor_set_role(&mut self, role: EditorRole) {
        let physical_full = self.physical_source_count() >= MAX_PHYSICAL_INPUT_CHANNELS;
        if let Some(editor) = self.channel_editor.as_mut() {
            if editor.is_source && role == EditorRole::Physical && physical_full {
                return;
            }
            // Virtual devices only exist if the backend reports the capability.
            if role == EditorRole::Virtual && !self.backend_capabilities.virtual_devices {
                return;
            }
            editor.role = role;
            editor.endpoint = None;
        }
    }

    pub fn editor_set_color(&mut self, color: ChannelColor) {
        if let Some(editor) = self.channel_editor.as_mut() {
            editor.color = color;
        }
    }

    pub fn editor_name_input(&mut self, text: &str) {
        if let Some(editor) = self.channel_editor.as_mut() {
            if editor.name.chars().count() < 24 {
                editor.name.push_str(text);
            }
        }
    }

    pub fn editor_name_backspace(&mut self) {
        if let Some(editor) = self.channel_editor.as_mut() {
            editor.name.pop();
        }
    }

    pub fn editor_select_endpoint(&mut self, endpoint_id: EndpointId) {
        if let Some(editor) = self.channel_editor.as_mut() {
            editor.endpoint = Some(endpoint_id);
        }
    }

    /// Returns the finished editor when the form is valid, consuming it.
    pub fn take_channel_editor(&mut self) -> Option<ChannelEditor> {
        let editor = self.channel_editor.as_ref()?;
        if editor.name.trim().is_empty() {
            return None;
        }
        self.channel_editor.take()
    }

    pub fn physical_source_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.kind == SourceKind::Physical)
            .count()
    }

    pub fn push_source_channel(
        &mut self,
        name: String,
        kind: SourceKind,
        color: ChannelColor,
    ) -> usize {
        self.sources.push(SourceStrip {
            endpoint_id: None,
            endpoint_name: None,
            name,
            detail: empty_source_detail(kind).into(),
            kind,
            color,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            routes: vec![false; self.outputs.len()],
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        });
        self.dirty = true;
        self.sources.len() - 1
    }

    pub fn push_output_bus(&mut self, name: String, kind: BusKind, color: ChannelColor) -> usize {
        let code = next_bus_code(&self.outputs, kind);
        self.outputs.push(OutputBus {
            endpoint_id: None,
            endpoint_name: None,
            code,
            name,
            detail: empty_output_detail(kind).into(),
            kind,
            color,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        });
        for source in &mut self.sources {
            source.routes.push(false);
        }
        self.dirty = true;
        self.outputs.len() - 1
    }

    // ---- Channel deletion ----

    /// Active routes touching the channel's endpoint.
    pub fn channel_route_count(&self, source: bool, index: usize) -> usize {
        let endpoint = if source {
            self.sources
                .get(index)
                .and_then(|channel| channel.endpoint_id)
        } else {
            self.outputs
                .get(index)
                .and_then(|channel| channel.endpoint_id)
        };
        let Some(endpoint) = endpoint else {
            return 0;
        };
        self.active_routes
            .keys()
            .filter(|(route_source, route_destination)| {
                *route_source == endpoint || *route_destination == endpoint
            })
            .count()
    }

    pub fn remove_source(&mut self, index: usize) {
        if index < self.sources.len() {
            self.sources.remove(index);
            self.dirty = true;
        }
    }

    pub fn remove_output_bus(&mut self, index: usize) {
        if index < self.outputs.len() {
            self.outputs.remove(index);
            for source in &mut self.sources {
                if index < source.routes.len() {
                    source.routes.remove(index);
                }
            }
            self.dirty = true;
        }
    }

    /// Delete a scene, keeping the selection in range. Refuses to delete the
    /// last one: the mixer always has a scene.
    pub fn delete_scene(&mut self, index: usize) {
        if self.scenes.len() <= 1 || index >= self.scenes.len() {
            return;
        }
        self.scenes.remove(index);
        if self.selected_scene >= self.scenes.len() {
            self.selected_scene = self.scenes.len() - 1;
        }
        self.dirty = true;
    }

    pub fn set_hub_tab(&mut self, tab: HubTab) {
        self.hub_tab = tab;
    }

    /// Create a scene named by the modal and capture the current mixer as its
    /// initial snapshot.
    pub fn commit_scene_editor(&mut self) {
        let Some(name) = self.scene_editor.take() else {
            return;
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.scenes.push(Scene {
            name,
            description: "Custom scene based on the current mixer".into(),
            snapshot: None,
        });
        self.selected_scene = self.scenes.len() - 1;
        self.capture_scene(self.selected_scene);
        self.dirty = true;
    }

    pub fn scene_editor_input(&mut self, text: &str) {
        if let Some(buffer) = self.scene_editor.as_mut() {
            if buffer.chars().count() < 24 {
                buffer.push_str(text);
            }
        }
    }

    pub fn scene_editor_backspace(&mut self) {
        if let Some(buffer) = self.scene_editor.as_mut() {
            buffer.pop();
        }
    }

    pub fn set_source_color(&mut self, index: usize, color: ChannelColor) {
        if let Some(source) = self.sources.get_mut(index) {
            source.color = color;
            self.dirty = true;
        }
    }

    pub fn set_output_color(&mut self, index: usize, color: ChannelColor) {
        if let Some(output) = self.outputs.get_mut(index) {
            output.color = color;
            self.dirty = true;
        }
    }

    // ---- Rename modal ----

    pub fn open_rename(&mut self, source: bool, index: usize) {
        let current = if source {
            self.sources.get(index).map(|channel| channel.name.clone())
        } else {
            self.outputs.get(index).map(|channel| channel.name.clone())
        };
        if let Some(name) = current {
            self.rename_target = Some((source, index));
            self.rename_buffer = name;
        }
    }

    pub fn cancel_rename(&mut self) {
        self.rename_target = None;
        self.rename_buffer.clear();
    }

    pub fn rename_input(&mut self, text: &str) {
        if self.rename_target.is_some() && self.rename_buffer.chars().count() < 24 {
            self.rename_buffer.push_str(text);
        }
    }

    pub fn rename_backspace(&mut self) {
        self.rename_buffer.pop();
    }

    pub fn commit_rename(&mut self) {
        if let Some((source, index)) = self.rename_target.take() {
            let name = std::mem::take(&mut self.rename_buffer);
            self.rename_channel(source, index, name);
            self.status_message = "Channel renamed".into();
        } else {
            self.rename_buffer.clear();
        }
    }

    pub fn rename_channel(&mut self, source: bool, index: usize, name: impl Into<String>) {
        let name = name.into();
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if source {
            if let Some(channel) = self.sources.get_mut(index) {
                channel.name = name.to_owned();
                self.dirty = true;
            }
        } else if let Some(channel) = self.outputs.get_mut(index) {
            channel.name = name.to_owned();
            self.dirty = true;
        }
    }

    /// Apply the detected graph clock: rate/quantum become the effective
    /// values, unless the user has already picked their own.
    pub fn apply_audio_settings(&mut self, rate: u32, quantum: u32) {
        if self.detected_audio == Some((rate, quantum)) {
            return;
        }
        self.detected_audio = Some((rate, quantum));
        if !self.audio_overridden {
            self.sample_rate = rate;
            self.buffer_size = quantum;
        }
        self.status_message = format!(
            "PipeWire clock detected: {} · {quantum} frames",
            format_sample_rate(rate)
        );
    }

    /// Orion-local sample rate for new route streams (PipeWire converts at
    /// node boundaries when a device cannot run it).
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        if SAMPLE_RATE_OPTIONS.contains(&sample_rate) && self.sample_rate != sample_rate {
            self.sample_rate = sample_rate;
            self.audio_overridden = true;
            self.dirty = true;
            self.status_message = format!(
                "Route sample rate set to {}",
                format_sample_rate(sample_rate)
            );
        }
    }

    /// Orion-local buffer hint (`node.latency`) for new route streams.
    pub fn set_buffer_size(&mut self, buffer_size: u32) {
        if BUFFER_SIZE_OPTIONS.contains(&buffer_size) && self.buffer_size != buffer_size {
            self.buffer_size = buffer_size;
            self.audio_overridden = true;
            self.dirty = true;
            self.status_message =
                format!("Route buffer set to {buffer_size} frames for new routes");
        }
    }

    /// Return to automatic audio settings: the detected PipeWire clock
    /// becomes the effective rate/quantum again.
    pub fn reset_audio_settings(&mut self) {
        self.audio_overridden = false;
        if let Some((rate, quantum)) = self.detected_audio {
            self.sample_rate = rate;
            self.buffer_size = quantum;
        }
        self.dirty = true;
        self.status_message = "Audio engine reset to the detected system clock".into();
    }

    /// Per-source sync delay in milliseconds (0 disables). Applied to this
    /// source's audio in every route — e.g. aligning a fast mic against a
    /// delayed video capture.
    pub fn set_source_delay(&mut self, index: usize, delay_ms: f32) {
        let Some(source) = self.sources.get_mut(index) else {
            return;
        };
        let name = source.name.clone();
        if let Some(delay_ms) = set_strip_delay(&mut source.delay_ms, delay_ms) {
            self.dirty = true;
            self.status_message = delay_status(&name, delay_ms);
        }
    }

    /// Set one EQ band on a source strip (clamped to ±12 dB).
    pub fn set_source_eq(&mut self, index: usize, band: EqBand, db: f32) {
        if let Some(source) = self.sources.get_mut(index) {
            let name = source.name.clone();
            if let Some(db) = source.eq.set(band, db) {
                self.dirty = true;
                self.status_message = format!("'{name}' {} {db:+.1} dB", band.label());
            }
        }
    }

    /// Set one EQ band on an output bus (clamped to ±12 dB).
    pub fn set_output_eq(&mut self, index: usize, band: EqBand, db: f32) {
        if let Some(output) = self.outputs.get_mut(index) {
            let name = output.name.clone();
            if let Some(db) = output.eq.set(band, db) {
                self.dirty = true;
                self.status_message = format!("'{name}' {} {db:+.1} dB", band.label());
            }
        }
    }

    /// Cycle a channel's mapping mode (AUTO -> ST -> MN -> L -> R -> SW).
    pub fn cycle_channel_mode(&mut self, source: bool, index: usize) {
        let (name, next) = if source {
            match self.sources.get(index) {
                Some(source) => (source.name.clone(), source.mode.next()),
                None => return,
            }
        } else {
            match self.outputs.get(index) {
                Some(output) => (output.name.clone(), output.mode.next()),
                None => return,
            }
        };
        self.set_channel_mode(source, index, next);
        self.status_message = format!("'{name}' channel mode: {}", next.label());
    }

    pub fn set_channel_mode(&mut self, source: bool, index: usize, mode: ChannelMode) {
        let slot = if source {
            self.sources.get_mut(index).map(|source| &mut source.mode)
        } else {
            self.outputs.get_mut(index).map(|output| &mut output.mode)
        };
        if let Some(slot) = slot {
            if *slot != mode {
                *slot = mode;
                self.dirty = true;
            }
        }
    }

    /// Per-output sync delay in milliseconds (0 disables). Applied to every
    /// route playing into that bus.
    pub fn set_output_delay(&mut self, index: usize, delay_ms: f32) {
        let Some(output) = self.outputs.get_mut(index) else {
            return;
        };
        let name = output.name.clone();
        if let Some(delay_ms) = set_strip_delay(&mut output.delay_ms, delay_ms) {
            self.dirty = true;
            self.status_message = delay_status(&name, delay_ms);
        }
    }

    /// Resize the outputs panel, keeping both mixer sections usable.
    pub fn set_outputs_panel_width(&mut self, width: f32, window_width: f32) {
        let max = (window_width - MIN_SOURCES_PANEL_WIDTH).max(MIN_OUTPUTS_PANEL_WIDTH);
        let width = width.clamp(MIN_OUTPUTS_PANEL_WIDTH, max);
        if (self.outputs_panel_width - width).abs() > f32::EPSILON {
            self.outputs_panel_width = width;
            self.dirty = true;
        }
    }

    pub fn set_device_monitor_connected(&mut self) {
        self.device_monitor_status = DeviceMonitorStatus::Connected;
        self.status_message = "Connected to PipeWire — discovering audio devices…".into();
    }

    pub fn set_device_monitor_error(&mut self, error: String) {
        self.device_monitor_status = DeviceMonitorStatus::Error(error.clone());
        self.status_message = error;
    }

    pub fn set_devices(&mut self, mut devices: Vec<AudioEndpoint>) {
        devices.sort_by(|left, right| {
            right
                .is_default
                .cmp(&left.is_default)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        self.devices = devices;
        self.reconcile_channel_bindings();
    }

    fn reconcile_channel_bindings(&mut self) {
        let physical_inputs = online_endpoints(&self.devices, EndpointType::PhysicalInput);
        let physical_outputs = online_endpoints(&self.devices, EndpointType::PhysicalOutput);
        let virtual_inputs = online_endpoints(&self.devices, EndpointType::VirtualInput);
        let virtual_outputs = online_endpoints(&self.devices, EndpointType::VirtualOutput);

        // Auto-bind up to two physical mic channels from system defaults first.
        let mut mic_index = 0usize;
        for source in &mut self.sources {
            if source.kind != SourceKind::Physical {
                continue;
            }
            if !source.user_bound {
                if let Some(endpoint) = physical_inputs.get(mic_index) {
                    source.endpoint_id = Some(endpoint.id);
                } else if source.endpoint_id.is_some()
                    && !endpoint_still_present(&self.devices, source.endpoint_id)
                {
                    source.endpoint_id = None;
                }
            }
            update_source_binding(source, &self.devices, empty_source_detail(source.kind));
            mic_index += 1;
        }

        // Physical outs A1/A2: default sink first, then next physical.
        let mut physical_out_index = 0usize;
        for output in &mut self.outputs {
            if output.kind != BusKind::Physical {
                continue;
            }
            if !output.user_bound {
                if let Some(endpoint) = physical_outputs.get(physical_out_index) {
                    output.endpoint_id = Some(endpoint.id);
                } else if output.endpoint_id.is_some()
                    && !endpoint_still_present(&self.devices, output.endpoint_id)
                {
                    output.endpoint_id = None;
                }
            }
            update_output_binding(output, &self.devices, empty_output_detail(output.kind));
            physical_out_index += 1;
        }

        // Virtual outs B1/B2… bind to Orion virtual outputs in stable order.
        let mut virtual_out_index = 0usize;
        for output in &mut self.outputs {
            if output.kind != BusKind::Virtual {
                continue;
            }
            if !output.user_bound {
                if let Some(endpoint) = virtual_outputs.get(virtual_out_index) {
                    output.endpoint_id = Some(endpoint.id);
                } else if output.endpoint_id.is_some()
                    && !endpoint_still_present(&self.devices, output.endpoint_id)
                {
                    output.endpoint_id = None;
                }
            }
            // Keep user channel name; only refresh detail/online from endpoint.
            update_output_binding(output, &self.devices, empty_output_detail(output.kind));
            virtual_out_index += 1;
        }

        // Optional virtual-in sources and the Desktop channel follow virtual inputs.
        for source in &mut self.sources {
            let follows_virtual_input =
                matches!(source.kind, SourceKind::Virtual | SourceKind::Desktop);
            if follows_virtual_input && !source.user_bound {
                if source.endpoint_id.is_none() {
                    if let Some(endpoint) = virtual_inputs.first() {
                        source.endpoint_id = Some(endpoint.id);
                    }
                } else if !endpoint_still_present(&self.devices, source.endpoint_id) {
                    source.endpoint_id = None;
                }
            }
            if matches!(
                source.kind,
                SourceKind::Desktop | SourceKind::Application | SourceKind::Virtual
            ) {
                update_source_binding(source, &self.devices, empty_source_detail(source.kind));
            }
            source.routes.resize(self.outputs.len(), false);
        }

        self.status_message = format!(
            "Ready · {} inputs · {} outputs · {} virtual in · {} virtual out",
            physical_inputs.len(),
            physical_outputs.len(),
            virtual_inputs.len(),
            virtual_outputs.len()
        );
    }

    pub fn upsert_device(&mut self, endpoint: AudioEndpoint) {
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|existing| existing.id == endpoint.id)
        {
            *existing = endpoint;
        } else {
            self.devices.push(endpoint);
        }
        let devices = std::mem::take(&mut self.devices);
        self.set_devices(devices);
    }

    pub fn remove_device(&mut self, endpoint_id: EndpointId) {
        self.devices.retain(|endpoint| endpoint.id != endpoint_id);
        let devices = std::mem::take(&mut self.devices);
        self.set_devices(devices);
    }

    pub fn upsert_route(&mut self, route: AudioRoute) {
        let pair = (route.source, route.destination);
        if matches!(route.state, RouteState::Connecting | RouteState::Active) {
            self.active_routes.insert(pair, route.id);
        } else {
            self.active_routes.remove(&pair);
        }
        self.dirty = true;
    }

    pub fn remove_route(&mut self, route_id: RouteId) {
        self.active_routes.retain(|_, id| *id != route_id);
        self.dirty = true;
    }

    // ---- Routing intent ----
    //
    // `SourceStrip::routes` flags are the DESIRED matrix (user intent); they
    // survive disconnects, rebinds and device replugs. The engine-facing code
    // reconciles desired pairs against `active_routes`.

    /// Whether a matrix cell is marked for routing.
    pub fn route_desired(&self, source_index: usize, output_index: usize) -> bool {
        self.sources
            .get(source_index)
            .and_then(|source| source.routes.get(output_index))
            .copied()
            .unwrap_or(false)
    }

    /// Whether a matrix cell currently has a live route.
    pub fn route_connected(&self, source_index: usize, output_index: usize) -> bool {
        let Some(pair) = self.cell_pair(source_index, output_index) else {
            return false;
        };
        self.active_routes.contains_key(&pair)
    }

    /// Endpoint pair for a matrix cell, when both sides are bound.
    pub fn cell_pair(
        &self,
        source_index: usize,
        output_index: usize,
    ) -> Option<(EndpointId, EndpointId)> {
        let source = self.sources.get(source_index)?.endpoint_id?;
        let destination = self.outputs.get(output_index)?.endpoint_id?;
        Some((source, destination))
    }

    /// Flip a matrix cell's intent.
    pub fn toggle_route_intent(&mut self, source_index: usize, output_index: usize) {
        if let Some(source) = self.sources.get_mut(source_index) {
            if let Some(flag) = source.routes.get_mut(output_index) {
                *flag = !*flag;
                self.dirty = true;
            }
        }
    }

    /// Every desired route as endpoint pairs (both sides bound).
    pub fn desired_route_pairs(&self) -> Vec<(EndpointId, EndpointId)> {
        let mut pairs = Vec::new();
        for (source_index, _source) in self.sources.iter().enumerate() {
            for (output_index, _) in self.outputs.iter().enumerate() {
                if self.route_desired(source_index, output_index) {
                    if let Some(pair) = self.cell_pair(source_index, output_index) {
                        pairs.push(pair);
                    }
                }
            }
        }
        pairs
    }

    /// Replace the desired matrix with the given endpoint pairs (scene apply
    /// and session restore). Pairs whose endpoints are not currently bound to
    /// a strip are dropped, matching the name-matching semantics of controls.
    pub fn set_desired_routes_from_pairs(&mut self, pairs: &[(EndpointId, EndpointId)]) {
        for source in &mut self.sources {
            source.routes.fill(false);
        }
        for (source_id, destination_id) in pairs {
            let cell = self
                .sources
                .iter()
                .position(|source| source.endpoint_id == Some(*source_id))
                .zip(
                    self.outputs
                        .iter()
                        .position(|output| output.endpoint_id == Some(*destination_id)),
                );
            if let Some((source_index, output_index)) = cell {
                self.sources[source_index].routes[output_index] = true;
            }
        }
        self.dirty = true;
    }

    // ---- Scenes ----

    /// Capture the current mixer state into the selected scene's snapshot.
    pub fn capture_scene(&mut self, index: usize) {
        self.capture_scene_impl(index, true);
    }

    /// Silent capture for live scenes: the active scene tracks the mixer on
    /// every autosave, without spamming the status bar.
    pub fn capture_scene_silent(&mut self, index: usize) {
        self.capture_scene_impl(index, false);
    }

    fn capture_scene_impl(&mut self, index: usize, announce: bool) {
        let snapshot = SceneSnapshot {
            sources: self
                .sources
                .iter()
                .map(|source| ChannelSnapshot {
                    name: source.name.clone(),
                    color: source.color,
                    gain_db: source.gain_db,
                    muted: source.muted,
                    delay_ms: source.delay_ms,
                    eq: source.eq,
                    mode: source.mode,
                    endpoint_id: source.endpoint_id,
                })
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|output| ChannelSnapshot {
                    name: output.name.clone(),
                    color: output.color,
                    gain_db: output.gain_db,
                    muted: output.muted,
                    delay_ms: output.delay_ms,
                    eq: output.eq,
                    mode: output.mode,
                    endpoint_id: output.endpoint_id,
                })
                .collect(),
            routes: self.desired_route_pairs(),
        };
        let description = format!(
            "{} sources · {} outputs · {} routes",
            snapshot.sources.len(),
            snapshot.outputs.len(),
            snapshot.routes.len()
        );
        let Some(scene) = self.scenes.get_mut(index) else {
            return;
        };
        scene.snapshot = Some(snapshot);
        scene.description = description;
        self.dirty = true;
        if announce {
            self.status_message = format!("Scene '{}' saved", scene.name);
        }
    }

    /// Apply a scene's controls (gain/mute/delay matched by channel name) and
    /// replace the desired route matrix. Engine reconciliation is handled by
    /// the caller, which owns the engine.
    pub fn apply_scene_controls(&mut self, index: usize) -> Option<()> {
        let snapshot = self.scenes.get(index)?.snapshot.clone()?;
        for saved in &snapshot.sources {
            if let Some(source) = self
                .sources
                .iter_mut()
                .find(|source| source.name == saved.name)
            {
                source.gain_db = saved.gain_db;
                source.muted = saved.muted;
                source.delay_ms = saved.delay_ms;
                source.eq = saved.eq;
                source.mode = saved.mode;
                if saved.endpoint_id.is_some() {
                    source.endpoint_id = saved.endpoint_id;
                    source.user_bound = true;
                }
            }
        }
        for saved in &snapshot.outputs {
            if let Some(output) = self
                .outputs
                .iter_mut()
                .find(|output| output.name == saved.name)
            {
                output.gain_db = saved.gain_db;
                output.muted = saved.muted;
                output.delay_ms = saved.delay_ms;
                output.eq = saved.eq;
                output.mode = saved.mode;
                if saved.endpoint_id.is_some() {
                    output.endpoint_id = saved.endpoint_id;
                    output.user_bound = true;
                }
            }
        }
        self.set_desired_routes_from_pairs(&snapshot.routes);
        self.selected_scene = index;
        self.dirty = true;
        Some(())
    }

    /// Apply a meter frame; returns whether any visible level changed (the UI
    /// only repaints then, so silence does not churn the render loop).
    pub fn apply_meter(&mut self, frame: MeterFrame) -> bool {
        let mut changed = false;
        let Some(endpoint) = self
            .devices
            .iter()
            .find(|endpoint| endpoint.id == frame.endpoint_id)
        else {
            return false;
        };
        let left = endpoint
            .channels
            .first()
            .and_then(|channel| frame.levels.get(channel))
            .map_or(0.0, |level| level.value());
        let right = endpoint
            .channels
            .get(1)
            .and_then(|channel| frame.levels.get(channel))
            .map_or(left, |level| level.value());
        for source in &mut self.sources {
            if source.endpoint_id == Some(frame.endpoint_id) {
                changed |= update_meter_pair(&mut source.meter_l, &mut source.meter_r, left, right);
            }
        }
        for output in &mut self.outputs {
            if output.endpoint_id == Some(frame.endpoint_id) {
                changed |= update_meter_pair(&mut output.meter_l, &mut output.meter_r, left, right);
            }
        }
        changed
    }

    pub fn assign_source_endpoint(&mut self, index: usize, endpoint_id: EndpointId) -> bool {
        let compatible = self.devices.iter().any(|endpoint| {
            endpoint.id == endpoint_id
                && self
                    .sources
                    .get(index)
                    .is_some_and(|source| source_accepts(source.kind, endpoint.endpoint_type))
        });
        if !compatible {
            return false;
        }
        let source = &mut self.sources[index];
        source.endpoint_id = Some(endpoint_id);
        source.user_bound = true;
        update_source_binding(source, &self.devices, empty_source_detail(source.kind));
        self.endpoint_picker = None;
        self.dirty = true;
        true
    }

    pub fn assign_output_endpoint(&mut self, index: usize, endpoint_id: EndpointId) -> bool {
        let compatible = self.devices.iter().any(|endpoint| {
            endpoint.id == endpoint_id
                && self
                    .outputs
                    .get(index)
                    .is_some_and(|output| output_accepts(output.kind, endpoint.endpoint_type))
        });
        if !compatible {
            return false;
        }
        let output = &mut self.outputs[index];
        output.endpoint_id = Some(endpoint_id);
        output.user_bound = true;
        update_output_binding(output, &self.devices, empty_output_detail(output.kind));
        self.endpoint_picker = None;
        self.dirty = true;
        true
    }

    pub fn clear_source_binding(&mut self, index: usize) {
        if let Some(source) = self.sources.get_mut(index) {
            source.endpoint_id = None;
            source.user_bound = true;
            source.detail = empty_source_detail(source.kind).into();
            source.online = false;
            self.dirty = true;
        }
    }

    pub fn clear_output_binding(&mut self, index: usize) {
        if let Some(output) = self.outputs.get_mut(index) {
            output.endpoint_id = None;
            output.user_bound = true;
            output.detail = empty_output_detail(output.kind).into();
            output.online = false;
            self.dirty = true;
        }
    }

    // ---- Virtual delete confirmation ----

    /// Endpoints (channel names) currently bound to this virtual device, plus
    /// whether active routes touch it.
    pub fn virtual_usage(&self, virtual_device_id: VirtualDeviceId) -> (Vec<String>, usize) {
        let endpoint_ids: Vec<EndpointId> = self
            .devices
            .iter()
            .filter(|endpoint| endpoint.virtual_device_id == Some(virtual_device_id))
            .map(|endpoint| endpoint.id)
            .collect();
        let mut users = Vec::new();
        for source in &self.sources {
            if source
                .endpoint_id
                .is_some_and(|id| endpoint_ids.contains(&id))
            {
                users.push(source.name.clone());
            }
        }
        for output in &self.outputs {
            if output
                .endpoint_id
                .is_some_and(|id| endpoint_ids.contains(&id))
            {
                users.push(format!("{} ({})", output.name, output.code));
            }
        }
        let routes = self
            .active_routes
            .keys()
            .filter(|(source, destination)| {
                endpoint_ids.contains(source) || endpoint_ids.contains(destination)
            })
            .count();
        (users, routes)
    }
}

pub fn source_accepts(kind: SourceKind, endpoint_type: EndpointType) -> bool {
    match kind {
        SourceKind::Physical => endpoint_type == EndpointType::PhysicalInput,
        SourceKind::Desktop => matches!(
            endpoint_type,
            EndpointType::ApplicationOutput | EndpointType::VirtualInput
        ),
        SourceKind::Application => endpoint_type == EndpointType::ApplicationOutput,
        SourceKind::Virtual => endpoint_type == EndpointType::VirtualInput,
    }
}

pub fn output_accepts(kind: BusKind, endpoint_type: EndpointType) -> bool {
    match kind {
        BusKind::Physical => endpoint_type == EndpointType::PhysicalOutput,
        BusKind::Virtual => endpoint_type == EndpointType::VirtualOutput,
    }
}

fn default_source_template(route_count: usize) -> Vec<SourceStrip> {
    vec![
        SourceStrip {
            endpoint_id: None,
            endpoint_name: None,
            name: "Mic 1".into(),
            detail: "System default input".into(),
            kind: SourceKind::Physical,
            color: ChannelColor::Cyan,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            routes: vec![false; route_count],
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        },
        SourceStrip {
            endpoint_id: None,
            endpoint_name: None,
            name: "Desktop".into(),
            detail: "Orion Virtual Input 1".into(),
            kind: SourceKind::Desktop,
            color: ChannelColor::Blue,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            routes: vec![false; route_count],
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        },
        SourceStrip {
            endpoint_id: None,
            endpoint_name: None,
            name: "Game".into(),
            detail: "Select application".into(),
            kind: SourceKind::Application,
            color: ChannelColor::Green,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            routes: vec![false; route_count],
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        },
    ]
}

fn default_output_template() -> Vec<OutputBus> {
    vec![
        OutputBus {
            endpoint_id: None,
            endpoint_name: None,
            code: "A1".into(),
            name: "Speakers".into(),
            detail: "System default output".into(),
            kind: BusKind::Physical,
            color: ChannelColor::Cyan,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        },
        OutputBus {
            endpoint_id: None,
            endpoint_name: None,
            code: "B1".into(),
            name: "Stream".into(),
            detail: "Orion Virtual Output 1".into(),
            kind: BusKind::Virtual,
            color: ChannelColor::Purple,
            user_bound: false,
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            eq: EqBands::default(),
            mode: ChannelMode::Auto,
            meter_l: 0.0,
            meter_r: 0.0,
            online: false,
        },
    ]
}

fn empty_source_detail(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Physical => "Select a microphone",
        SourceKind::Desktop => "Select desktop or app audio",
        SourceKind::Application => "Select application",
        SourceKind::Virtual => "Select virtual input",
    }
}

pub fn editor_role_accepts_source(role: EditorRole, endpoint_type: EndpointType) -> bool {
    match role {
        EditorRole::Physical => endpoint_type == EndpointType::PhysicalInput,
        EditorRole::Application => endpoint_type == EndpointType::ApplicationOutput,
        EditorRole::Virtual => endpoint_type == EndpointType::VirtualInput,
    }
}

pub fn editor_role_accepts_output(role: EditorRole, endpoint_type: EndpointType) -> bool {
    match role {
        EditorRole::Physical => endpoint_type == EndpointType::PhysicalOutput,
        EditorRole::Virtual => endpoint_type == EndpointType::VirtualOutput,
        _ => false,
    }
}

pub fn editor_role_source_kind(role: EditorRole) -> SourceKind {
    match role {
        EditorRole::Physical => SourceKind::Physical,
        EditorRole::Application => SourceKind::Application,
        EditorRole::Virtual => SourceKind::Virtual,
    }
}

pub fn editor_role_bus_kind(role: EditorRole) -> BusKind {
    match role {
        EditorRole::Physical => BusKind::Physical,
        EditorRole::Application | EditorRole::Virtual => BusKind::Virtual,
    }
}

fn default_channel_name(state: &AppState, is_source: bool, role: EditorRole) -> String {
    match (is_source, role) {
        (true, EditorRole::Physical) => format!("Mic {}", state.physical_source_count() + 1),
        (true, EditorRole::Application) => format!("Source {}", state.sources.len() + 1),
        (true, EditorRole::Virtual) => format!("Virtual {}", state.sources.len() + 1),
        (false, EditorRole::Physical) => format!("Output {}", state.outputs.len() + 1),
        (false, _) => format!("Bus {}", state.outputs.len() + 1),
    }
}

fn next_free_color(state: &AppState, is_source: bool) -> ChannelColor {
    let used: Vec<ChannelColor> = if is_source {
        state.sources.iter().map(|channel| channel.color).collect()
    } else {
        state.outputs.iter().map(|channel| channel.color).collect()
    };
    ChannelColor::ALL
        .into_iter()
        .find(|color| !used.contains(color))
        .unwrap_or(ChannelColor::ALL[used.len() % ChannelColor::ALL.len()])
}

fn empty_output_detail(kind: BusKind) -> &'static str {
    match kind {
        BusKind::Physical => "Select a physical output",
        BusKind::Virtual => "Select a virtual output",
    }
}

fn online_endpoints(devices: &[AudioEndpoint], endpoint_type: EndpointType) -> Vec<AudioEndpoint> {
    devices
        .iter()
        .filter(|device| {
            device.endpoint_type == endpoint_type && device.state != EndpointState::Disconnected
        })
        .cloned()
        .collect()
}

fn endpoint_still_present(devices: &[AudioEndpoint], endpoint_id: Option<EndpointId>) -> bool {
    endpoint_id.is_some_and(|id| devices.iter().any(|endpoint| endpoint.id == id))
}

fn next_bus_code(outputs: &[OutputBus], kind: BusKind) -> String {
    let prefix = match kind {
        BusKind::Physical => 'A',
        BusKind::Virtual => 'B',
    };
    let count = outputs.iter().filter(|bus| bus.kind == kind).count() + 1;
    format!("{prefix}{count}")
}

fn update_source_binding(source: &mut SourceStrip, devices: &[AudioEndpoint], fallback: &str) {
    let endpoint = source
        .endpoint_id
        .and_then(|id| devices.iter().find(|endpoint| endpoint.id == id));
    if let Some(endpoint) = endpoint {
        source.endpoint_name = Some(endpoint.name.clone());
        source.detail = endpoint.name.clone();
        source.online = !matches!(
            endpoint.state,
            EndpointState::Disconnected | EndpointState::Error
        );
    } else if source.endpoint_id.is_some() {
        // Bound to a device that is not present right now: keep the binding
        // and show what we are waiting for until it comes back.
        source.detail = source
            .endpoint_name
            .clone()
            .map(|name| format!("{name} — offline"))
            .unwrap_or_else(|| fallback.to_string());
        source.online = false;
    } else {
        source.detail = fallback.to_string();
        source.online = false;
    }
}

fn update_output_binding(output: &mut OutputBus, devices: &[AudioEndpoint], fallback: &str) {
    let endpoint = output
        .endpoint_id
        .and_then(|id| devices.iter().find(|endpoint| endpoint.id == id));
    if let Some(endpoint) = endpoint {
        output.endpoint_name = Some(endpoint.name.clone());
        output.detail = endpoint.name.clone();
        output.online = !matches!(
            endpoint.state,
            EndpointState::Disconnected | EndpointState::Error
        );
    } else if output.endpoint_id.is_some() {
        output.detail = output
            .endpoint_name
            .clone()
            .map(|name| format!("{name} — offline"))
            .unwrap_or_else(|| fallback.to_string());
        output.online = false;
    } else {
        output.detail = fallback.to_string();
        output.online = false;
    }
}

/// Update a strip's meter pair; returns whether the rendered levels changed.
fn update_meter_pair(meter_l: &mut f32, meter_r: &mut f32, left: f32, right: f32) -> bool {
    let new_l = left.clamp(0.0, 1.0);
    let new_r = right.clamp(0.0, 1.0);
    let changed = (*meter_l - new_l).abs() > 1.0e-4 || (*meter_r - new_r).abs() > 1.0e-4;
    *meter_l = new_l;
    *meter_r = new_r;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion::domain::{ChannelId, EndpointIdentity, GainDb, MeterLevel, NormalizedBalance};

    fn endpoint(endpoint_type: EndpointType) -> AudioEndpoint {
        AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: None,
            device_id: None,
            virtual_device_id: None,
            identity: EndpointIdentity::new("test"),
            name: "test endpoint".into(),
            description: "test endpoint".into(),
            endpoint_type,
            state: EndpointState::Available,
            channel_count: 2,
            sample_rate: Some(48_000),
            is_default: false,
            channels: vec![ChannelId::new(), ChannelId::new()],
            gain: GainDb::default(),
            muted: false,
            balance: NormalizedBalance::default(),
        }
    }

    #[test]
    fn faders_are_clamped_to_the_supported_range() {
        let mut state = AppState::new(Vec::new());

        state.adjust_fader(FaderTarget::Source(0), 500.0);
        assert_eq!(state.sources[0].gain_db, 10.0);

        state.adjust_fader(FaderTarget::Output(0), -500.0);
        assert_eq!(state.outputs[0].gain_db, -60.0);
    }

    #[test]
    fn new_sources_have_a_route_slot_for_every_output() {
        let mut state = AppState::new(Vec::new());

        state.push_source_channel("Radio".into(), SourceKind::Application, ChannelColor::Pink);

        let source = state.sources.last().expect("source should be added");
        assert_eq!(source.routes.len(), state.outputs.len());
        assert!(source.routes.iter().all(|active| !active));
    }

    #[test]
    fn channel_editor_validates_name_and_commits() {
        let mut state = AppState::new(Vec::new());
        state.open_channel_editor(true);
        let len = state
            .channel_editor
            .as_ref()
            .map(|editor| editor.name.chars().count())
            .unwrap_or(0);
        for _ in 0..len {
            state.editor_name_backspace();
        }
        assert!(
            state.take_channel_editor().is_none(),
            "empty name must not commit"
        );
        assert!(state.channel_editor.is_some());

        state.editor_name_input("Radio");
        let editor = state.take_channel_editor().expect("valid name commits");
        assert_eq!(editor.name, "Radio");
        assert!(state.channel_editor.is_none());

        let index = state.push_source_channel(
            editor.name.clone(),
            editor_role_source_kind(editor.role),
            editor.color,
        );
        assert_eq!(state.sources[index].name, "Radio");
    }

    #[test]
    fn physical_role_is_capped_in_editor() {
        let mut state = AppState::new(Vec::new());
        state.push_source_channel("Mic A".into(), SourceKind::Physical, ChannelColor::Cyan);
        state.push_source_channel("Mic B".into(), SourceKind::Physical, ChannelColor::Blue);
        state.open_channel_editor(true);
        state.editor_set_role(EditorRole::Application);
        state.editor_set_role(EditorRole::Physical);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.role),
            Some(EditorRole::Application),
            "physical role must stay unavailable once two mic channels exist"
        );
    }

    #[test]
    fn editor_stepper_walks_type_endpoint_details() {
        let mut state = AppState::new(Vec::new());
        // The test drives the Virtual role: model a backend that supports it.
        state.backend_capabilities.virtual_devices = true;
        state.open_channel_editor(false);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.step),
            Some(EditorStep::Type)
        );

        state.editor_choose_role(EditorRole::Virtual);
        let editor = state.channel_editor.as_ref().expect("editor open");
        assert_eq!(editor.step, EditorStep::Type, "cards select, NEXT advances");
        assert_eq!(editor.role, EditorRole::Virtual);

        state.editor_next_step();
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.step),
            Some(EditorStep::Endpoint)
        );

        state.editor_next_step();
        state.editor_name_input(" Discord");
        state.editor_set_color(ChannelColor::Teal);
        let editor = state.channel_editor.as_ref().expect("editor open");
        assert_eq!(editor.step, EditorStep::Details);
        assert_eq!(editor.color, ChannelColor::Teal);

        state.editor_prev_step();
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.step),
            Some(EditorStep::Endpoint)
        );

        // Role change clears a previously chosen endpoint.
        state.editor_prev_step();
        state.editor_choose_role(EditorRole::Physical);
        assert_eq!(
            state
                .channel_editor
                .as_ref()
                .and_then(|editor| editor.endpoint),
            None
        );
    }

    #[test]
    fn output_editor_only_offers_physical_and_virtual() {
        assert_eq!(
            EditorRole::OUTPUTS,
            [EditorRole::Physical, EditorRole::Virtual]
        );
        assert_eq!(
            EditorRole::SOURCES,
            [
                EditorRole::Physical,
                EditorRole::Application,
                EditorRole::Virtual
            ]
        );
    }

    #[test]
    fn removing_a_bus_drops_its_routing_column() {
        let mut state = AppState::new(Vec::new());
        let index = state.push_output_bus("Discord".into(), BusKind::Virtual, ChannelColor::Teal);
        assert_eq!(state.sources[0].routes.len(), 3);

        state.remove_output_bus(index);

        assert_eq!(state.outputs.len(), 2);
        assert!(state.sources.iter().all(|source| source.routes.len() == 2));
    }

    #[test]
    fn formats_fractional_and_integer_sample_rates() {
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(352_800), "352.8 kHz");
        assert_eq!(format_sample_rate(768_000), "768 kHz");
    }

    #[test]
    fn truncates_long_labels_with_ellipsis() {
        assert_eq!(
            crate::ui::channel_strip::truncate_label("short", 16),
            "short"
        );
        assert_eq!(
            crate::ui::channel_strip::truncate_label("ORION VIRTUAL INPUT 1", 16),
            "ORION VIRTUAL I…"
        );
    }

    #[test]
    fn scene_capture_and_apply_restores_controls_by_name() {
        let mut state = AppState::new(Vec::new());
        state.sources[0].gain_db = -12.0;
        state.sources[0].muted = true;
        state.capture_scene(0);

        // Change the live mixer, then apply the scene back.
        state.sources[0].gain_db = 0.0;
        state.sources[0].muted = false;
        let applied = state.apply_scene_controls(0);

        assert_eq!(state.sources[0].gain_db, -12.0);
        assert!(state.sources[0].muted);
        assert_eq!(state.selected_scene, 0);
        assert!(applied.is_some());
        assert!(state.scenes[0].snapshot.is_some(), "scene holds a snapshot");
    }

    #[test]
    fn live_capture_silently_tracks_the_selected_scene() {
        let mut state = AppState::new(Vec::new());
        state.scene_editor = Some("Live".into());
        state.commit_scene_editor();
        let scene = state.scenes.len() - 1;
        assert_eq!(state.selected_scene, scene);

        // Mixer changes flow into the scene snapshot without a status flash.
        state.sources[0].gain_db = -9.0;
        let before = state.status_message.clone();
        state.capture_scene_silent(scene);
        assert_eq!(state.status_message, before, "silent capture stays silent");
        assert!(
            (state.scenes[scene].snapshot.as_ref().unwrap().sources[0].gain_db - -9.0).abs()
                < f32::EPSILON,
            "selected scene tracks the mixer"
        );
    }

    #[test]
    fn delete_scene_keeps_selection_in_range() {
        let mut state = AppState::new(Vec::new());
        state.scene_editor = Some("Two".into());
        state.commit_scene_editor();
        assert_eq!(state.scenes.len(), 2);
        state.selected_scene = 1;

        state.delete_scene(1);
        assert_eq!(state.scenes.len(), 1);
        assert_eq!(state.selected_scene, 0, "selection clamps after delete");
        assert!(state.dirty);

        // Deleting the only remaining scene is refused (there is always one).
        state.delete_scene(0);
        assert_eq!(state.scenes.len(), 1);
    }

    #[test]
    fn channel_editor_keeps_a_selected_endpoint_through_the_flow() {
        let physical_out = endpoint(EndpointType::PhysicalOutput);
        let mut state = AppState::new(Vec::new());
        // Outputs default to the Virtual role only when the backend can
        // create virtual devices.
        state.backend_capabilities.virtual_devices = true;
        state.set_devices(vec![physical_out.clone()]);

        // ADD BUS from the mixer: outputs default to the Virtual role.
        state.open_channel_editor(false);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.role),
            Some(EditorRole::Virtual)
        );

        // Pick PHYSICAL, then select the device from the endpoint list.
        state.editor_choose_role(EditorRole::Physical);
        state.editor_select_endpoint(physical_out.id);
        state.editor_next_step();

        let editor = state.take_channel_editor().expect("editor completes");
        assert_eq!(
            editor.endpoint,
            Some(physical_out.id),
            "the selected endpoint survives to commit (no virtual creation)"
        );
        assert_eq!(editor.role, EditorRole::Physical);
    }

    #[test]
    fn channel_editor_virtual_role_without_selection_creates_virtual() {
        let mut state = AppState::new(Vec::new());
        state.backend_capabilities.virtual_devices = true;
        state.open_channel_editor(false);
        // Stay Virtual, pick nothing.
        state.editor_next_step();
        state.editor_next_step();
        let editor = state.take_channel_editor().expect("editor completes");
        assert_eq!(editor.endpoint, None, "no selection means: create virtual");
        assert_eq!(editor.role, EditorRole::Virtual);
    }

    #[test]
    fn virtual_role_requires_backend_capability() {
        // Backend did not report virtual-device support: the role is refused
        // and the output editor defaults to Physical instead of Virtual.
        let mut state = AppState::new(Vec::new());
        assert!(!state.backend_capabilities.virtual_devices);
        state.open_channel_editor(false);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.role),
            Some(EditorRole::Physical)
        );
        state.editor_set_role(EditorRole::Virtual);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.role),
            Some(EditorRole::Physical),
            "unsupported virtual role must not be selectable"
        );

        // With support reported, the default and selection open up.
        let mut state = AppState::new(Vec::new());
        state.backend_capabilities.virtual_devices = true;
        state.open_channel_editor(false);
        assert_eq!(
            state.channel_editor.as_ref().map(|editor| editor.role),
            Some(EditorRole::Virtual)
        );
    }

    #[test]
    fn channel_mode_cycles_and_persists_in_scenes() {
        let mut state = AppState::new(Vec::new());
        assert_eq!(state.sources[0].mode, ChannelMode::Auto);

        state.cycle_channel_mode(true, 0);
        assert_eq!(state.sources[0].mode, ChannelMode::Stereo);
        state.cycle_channel_mode(true, 0);
        assert_eq!(state.sources[0].mode, ChannelMode::Mono);
        assert!(state.dirty);

        // Full cycle wraps back to Auto.
        for _ in 0..4 {
            state.cycle_channel_mode(true, 0);
        }
        assert_eq!(state.sources[0].mode, ChannelMode::Auto);

        // Scenes capture and restore the mode.
        state.set_channel_mode(false, 0, ChannelMode::Swap);
        state.capture_scene(0);
        state.set_channel_mode(false, 0, ChannelMode::Auto);
        state.apply_scene_controls(0);
        assert_eq!(state.outputs[0].mode, ChannelMode::Swap);
    }

    #[test]
    fn eq_bands_clamp_and_mark_dirty() {
        let mut state = AppState::new(Vec::new());
        state.set_source_eq(0, EqBand::Low, 6.0);
        state.set_output_eq(0, EqBand::High, -3.5);
        assert!((state.sources[0].eq.low_db - 6.0).abs() < f32::EPSILON);
        assert!((state.outputs[0].eq.high_db - (-3.5)).abs() < f32::EPSILON);
        assert!(state.dirty);

        state.set_source_eq(0, EqBand::Mid, 99.0);
        assert!(
            (state.sources[0].eq.mid_db - orion_dsp::EQ_MAX_DB).abs() < f32::EPSILON,
            "clamped to +12 dB"
        );

        // Scenes capture and restore EQ.
        state.capture_scene(0);
        state.set_source_eq(0, EqBand::Low, 0.0);
        state.apply_scene_controls(0);
        assert!((state.sources[0].eq.low_db - 6.0).abs() < f32::EPSILON);
    }

    #[test]
    fn desired_routes_follow_endpoint_bindings() {
        let source = endpoint(EndpointType::PhysicalInput);
        let destination = endpoint(EndpointType::PhysicalOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![source.clone(), destination.clone()]);
        state.sources[0].endpoint_id = Some(source.id);
        state.outputs[0].endpoint_id = Some(destination.id);

        // Restore intent from persisted pairs.
        state.set_desired_routes_from_pairs(&[(source.id, destination.id)]);
        assert!(state.route_desired(0, 0));
        assert_eq!(
            state.desired_route_pairs(),
            vec![(source.id, destination.id)]
        );

        // Pairs for unbound endpoints are dropped.
        state.set_desired_routes_from_pairs(&[(EndpointId::new(), destination.id)]);
        assert!(!state.route_desired(0, 0));

        // Rebinding keeps the intent; the pair follows the new endpoint.
        state.toggle_route_intent(0, 0);
        let new_source = endpoint(EndpointType::PhysicalInput);
        state.set_devices(vec![new_source.clone(), destination.clone()]);
        state.sources[0].endpoint_id = Some(new_source.id);
        assert!(state.route_desired(0, 0), "intent survives rebinding");
        assert_eq!(
            state.desired_route_pairs(),
            vec![(new_source.id, destination.id)],
            "desired pair follows the new endpoint"
        );
    }

    #[test]
    fn source_delay_clamps_and_marks_dirty() {
        let mut state = AppState::new(Vec::new());
        assert!((state.sources[0].delay_ms - 0.0).abs() < f32::EPSILON);

        state.set_source_delay(0, 120.5);
        assert!((state.sources[0].delay_ms - 120.5).abs() < f32::EPSILON);
        assert!(state.dirty);

        state.set_source_delay(0, 99_999.0);
        assert!((state.sources[0].delay_ms - MAX_DELAY_MS).abs() < f32::EPSILON);

        state.set_source_delay(0, -10.0);
        assert!((state.sources[0].delay_ms - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scene_capture_round_trips_source_and_output_delays() {
        let source = endpoint(EndpointType::PhysicalInput);
        let destination = endpoint(EndpointType::PhysicalOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![source, destination]);
        state.set_source_delay(0, 33.0);
        state.set_output_delay(0, 75.0);
        state.capture_scene(0);

        // Scramble live values, then apply the scene back.
        state.set_source_delay(0, 0.0);
        state.set_output_delay(0, 0.0);
        state.apply_scene_controls(0);
        assert!((state.sources[0].delay_ms - 33.0).abs() < f32::EPSILON);
        assert!((state.outputs[0].delay_ms - 75.0).abs() < f32::EPSILON);
    }

    #[test]
    fn outputs_panel_width_clamps_to_usable_mixer() {
        let mut state = AppState::new(Vec::new());
        assert!((state.outputs_panel_width - DEFAULT_OUTPUTS_PANEL_WIDTH).abs() < f32::EPSILON);

        // Below the minimum: clamps to it.
        state.set_outputs_panel_width(100.0, 1_440.0);
        assert!((state.outputs_panel_width - MIN_OUTPUTS_PANEL_WIDTH).abs() < f32::EPSILON);

        // Too wide: leaves room for the sources panel.
        state.set_outputs_panel_width(5_000.0, 1_440.0);
        assert!((state.outputs_panel_width - 1_040.0).abs() < f32::EPSILON);

        // Normal drag lands as-is.
        state.set_outputs_panel_width(600.0, 1_440.0);
        assert!((state.outputs_panel_width - 600.0).abs() < f32::EPSILON);
        assert!(state.dirty);
    }

    #[test]
    fn output_delay_and_audio_settings_validate_and_override_detection() {
        let mut state = AppState::new(Vec::new());

        // Detection fills the defaults until the user overrides.
        state.apply_audio_settings(48_000, 1_024);
        assert_eq!(state.sample_rate, 48_000);
        assert_eq!(state.buffer_size, 1_024);

        state.set_sample_rate(96_000);
        state.set_buffer_size(256);
        state.set_output_delay(0, 80.0);
        assert_eq!(state.sample_rate, 96_000);
        assert_eq!(state.buffer_size, 256);
        assert!((state.outputs[0].delay_ms - 80.0).abs() < f32::EPSILON);
        assert!(state.dirty);
        assert!(state.audio_overridden);

        // Detection no longer overrides the user's choice.
        state.apply_audio_settings(44_100, 2_048);
        assert_eq!(state.sample_rate, 96_000);
        assert_eq!(state.buffer_size, 256);
        assert_eq!(state.detected_audio, Some((44_100, 2_048)));

        // Out-of-range values are rejected or clamped.
        state.dirty = false;
        state.set_sample_rate(123_456);
        state.set_output_delay(0, 9_999.0);
        assert_eq!(state.sample_rate, 96_000);
        assert!((state.outputs[0].delay_ms - MAX_DELAY_MS).abs() < f32::EPSILON);
        assert!(state.dirty, "delay clamp still records the change");

        // Reset returns to the detected clock and re-arms detection.
        state.reset_audio_settings();
        assert!(!state.audio_overridden);
        assert_eq!(state.sample_rate, 44_100);
        assert_eq!(state.buffer_size, 2_048);
        state.apply_audio_settings(48_000, 512);
        assert_eq!(
            state.sample_rate, 48_000,
            "detection applies again after reset"
        );
    }

    #[test]
    fn route_events_drive_matrix_state() {
        let source = endpoint(EndpointType::PhysicalInput);
        let destination = endpoint(EndpointType::PhysicalOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![source.clone(), destination.clone()]);
        let route_id = RouteId::new();

        // User intent: route the cell.
        state.toggle_route_intent(0, 0);
        assert!(state.route_desired(0, 0));
        assert_eq!(
            state.desired_route_pairs(),
            vec![(source.id, destination.id)]
        );
        assert!(!state.route_connected(0, 0), "intent alone is not live");

        // Engine connects it.
        state.upsert_route(AudioRoute {
            id: route_id,
            source: source.id,
            destination: destination.id,
            state: RouteState::Active,
        });
        assert!(state.route_connected(0, 0));

        // Route drops (device replug, rebuild): intent survives.
        state.remove_route(route_id);
        assert!(state.route_desired(0, 0), "intent survives disconnects");
        assert!(!state.route_connected(0, 0));
    }

    #[test]
    fn meter_frames_update_bound_source_strip() {
        let source = endpoint(EndpointType::PhysicalInput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![source.clone()]);
        let levels = HashMap::from([
            (
                source.channels[0],
                MeterLevel::new(0.25).unwrap_or_default(),
            ),
            (
                source.channels[1],
                MeterLevel::new(0.75).unwrap_or_default(),
            ),
        ]);

        state.apply_meter(MeterFrame {
            endpoint_id: source.id,
            sequence: 1,
            levels,
        });

        assert_eq!(state.sources[0].meter_l, 0.25);
        assert_eq!(state.sources[0].meter_r, 0.75);
    }

    #[test]
    fn meter_frames_update_bound_output_bus() {
        let destination = endpoint(EndpointType::PhysicalOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![destination.clone()]);
        let levels = HashMap::from([
            (
                destination.channels[0],
                MeterLevel::new(0.4).unwrap_or_default(),
            ),
            (
                destination.channels[1],
                MeterLevel::new(0.6).unwrap_or_default(),
            ),
        ]);

        state.apply_meter(MeterFrame {
            endpoint_id: destination.id,
            sequence: 1,
            levels,
        });

        assert_eq!(state.outputs[0].meter_l, 0.4);
        assert_eq!(state.outputs[0].meter_r, 0.6);
    }

    #[test]
    fn application_picker_assignment_survives_inventory_refresh() {
        let application = endpoint(EndpointType::ApplicationOutput);
        let physical = endpoint(EndpointType::PhysicalInput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![physical.clone(), application.clone()]);

        // Game is the default application channel at index 2.
        assert!(state.assign_source_endpoint(2, application.id));
        assert_eq!(state.sources[2].endpoint_id, Some(application.id));
        assert!(state.sources[2].online);
        assert!(!state.assign_source_endpoint(2, physical.id));

        state.set_devices(vec![application.clone(), physical]);
        assert_eq!(state.sources[2].endpoint_id, Some(application.id));
        assert_eq!(state.sources[2].detail, application.name);
        assert_eq!(state.sources[2].name, "Game");
    }

    #[test]
    fn assigned_endpoint_stays_selected_when_it_disconnects() {
        let mut application = endpoint(EndpointType::ApplicationOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![application.clone()]);
        assert!(state.assign_source_endpoint(2, application.id));

        application.state = EndpointState::Disconnected;
        state.set_devices(vec![application.clone()]);

        assert_eq!(state.sources[2].endpoint_id, Some(application.id));
        assert!(!state.sources[2].online);
    }

    #[test]
    fn bound_device_is_kept_and_marked_offline_when_it_disappears() {
        let application = endpoint(EndpointType::ApplicationOutput);
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![application.clone()]);
        assert!(state.assign_source_endpoint(2, application.id));
        assert_eq!(
            state.sources[2].endpoint_name.as_deref(),
            Some("test endpoint")
        );

        // The device vanishes entirely: the binding survives and shows the
        // offline legend instead of being cleared.
        state.set_devices(Vec::new());

        assert_eq!(state.sources[2].endpoint_id, Some(application.id));
        assert!(!state.sources[2].online);
        assert_eq!(state.sources[2].detail, "test endpoint — offline");

        // And it reconnects by itself when the device comes back.
        state.set_devices(vec![application.clone()]);
        assert!(state.sources[2].online);
        assert_eq!(state.sources[2].detail, "test endpoint");
    }

    #[test]
    fn default_template_is_minimal_and_customizable() {
        let state = AppState::new(Vec::new());
        assert_eq!(state.sources.len(), 3);
        assert_eq!(state.sources[0].name, "Mic 1");
        assert_eq!(state.sources[1].name, "Desktop");
        assert_eq!(state.sources[2].name, "Game");
        assert_eq!(state.sources[0].color, ChannelColor::Cyan);
        assert_eq!(state.sources[2].color, ChannelColor::Green);
        assert_eq!(state.outputs.len(), 2);
        assert_eq!(state.outputs[0].name, "Speakers");
        assert_eq!(state.outputs[1].name, "Stream");
        assert_eq!(state.outputs[1].color, ChannelColor::Purple);
    }

    #[test]
    fn auto_binds_system_defaults_without_renaming_channels() {
        let mut mic = endpoint(EndpointType::PhysicalInput);
        mic.is_default = true;
        mic.name = "UMC Mic".into();
        let mut speakers = endpoint(EndpointType::PhysicalOutput);
        speakers.is_default = true;
        speakers.name = "G560".into();
        let mut stream = endpoint(EndpointType::VirtualOutput);
        stream.name = "Orion Virtual Output 1".into();
        let mut desktop_input = endpoint(EndpointType::VirtualInput);
        desktop_input.name = "Orion Virtual Input 1".into();

        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![
            mic.clone(),
            speakers.clone(),
            stream.clone(),
            desktop_input.clone(),
        ]);

        assert_eq!(state.sources[0].endpoint_id, Some(mic.id));
        assert_eq!(state.sources[0].name, "Mic 1");
        assert_eq!(state.sources[0].detail, "UMC Mic");
        // Desktop auto-follows the first Orion virtual input.
        assert_eq!(state.sources[1].endpoint_id, Some(desktop_input.id));
        assert_eq!(state.sources[1].name, "Desktop");
        assert_eq!(state.outputs[0].endpoint_id, Some(speakers.id));
        assert_eq!(state.outputs[0].name, "Speakers");
        assert_eq!(state.outputs[1].endpoint_id, Some(stream.id));
        assert_eq!(state.outputs[1].name, "Stream");
        assert_eq!(state.sources.len(), 3);
        assert_eq!(state.outputs.len(), 2);
    }

    #[test]
    fn channel_color_cycles_and_marks_dirty() {
        let mut state = AppState::new(Vec::new());
        assert!(!state.dirty);
        state.set_source_color(0, ChannelColor::Amber);
        assert_eq!(state.sources[0].color, ChannelColor::Amber);
        assert!(state.dirty);
        assert_eq!(ChannelColor::Pink.next(), ChannelColor::Cyan);
        assert_eq!(ChannelColor::Cyan.next(), ChannelColor::Blue);
    }

    #[test]
    fn rename_modal_edits_and_commits_name() {
        let mut state = AppState::new(Vec::new());
        state.open_rename(true, 2);
        assert_eq!(state.rename_buffer, "Game");
        state.rename_backspace();
        state.rename_input(" Stream");
        state.commit_rename();
        assert_eq!(state.sources[2].name, "Gam Stream");
        assert!(state.rename_target.is_none());

        state.open_rename(false, 0);
        state.cancel_rename();
        assert_eq!(state.outputs[0].name, "Speakers");
    }

    #[test]
    fn user_binding_is_not_replaced_by_inventory_refresh() {
        let default_mic = {
            let mut endpoint = endpoint(EndpointType::PhysicalInput);
            endpoint.is_default = true;
            endpoint.name = "Default Mic".into();
            endpoint
        };
        let other_mic = {
            let mut endpoint = endpoint(EndpointType::PhysicalInput);
            endpoint.name = "Other Mic".into();
            endpoint
        };
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![default_mic.clone(), other_mic.clone()]);
        assert_eq!(state.sources[0].endpoint_id, Some(default_mic.id));

        assert!(state.assign_source_endpoint(0, other_mic.id));
        state.set_devices(vec![default_mic, other_mic.clone()]);
        assert_eq!(state.sources[0].endpoint_id, Some(other_mic.id));
        assert_eq!(state.sources[0].detail, "Other Mic");
    }
}
