use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::assets::{
    ICON_ARROW_DOWN, ICON_BOOKMARK, ICON_BOOKMARK_FILLED, ICON_CONFIGURATION,
    ICON_DEVICE_UNAVAILABLE, ICON_MIX, ICON_ROUTE, ICON_TYPE_APP, ICON_TYPE_PHYSICAL,
    ICON_TYPE_VIRTUAL,
};
use crate::state::{
    editor_role_accepts_output, editor_role_accepts_source, format_sample_rate, output_accepts,
    source_accepts, AppState, AppView, ChannelColor, DeviceMonitorStatus, EditorRole, EditorStep,
    EndpointPickerTarget, FaderDrag, FaderTarget, HubTab, KnobDrag, KnobParam, KnobTarget,
    SettingsSelect, BUFFER_SIZE_OPTIONS, SAMPLE_RATE_OPTIONS,
};
use crate::ui::session::{apply_document, document_from_state};
use crate::ui::theme::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use orion::{
    app_engine::{AudioEngine, EngineHandle},
    backend,
    domain::{
        AudioEndpoint, ChannelId, CommandId, EndpointId, EndpointIdentity, EndpointState,
        EndpointType, EngineCommand, EngineCommandKind, EngineEvent, EngineStatus, ErrorCode,
        ErrorSeverity, GainDb, GraphDelta, NormalizedBalance, RouteId, RouteState, VirtualDeviceId,
    },
    persistence::{
        PersistedVirtualDevice, PersistenceEvent, PersistenceWorker, SessionDocument, SessionStore,
        SettingsWatcher,
    },
};

pub struct RootView {
    pub(crate) state: AppState,
    _audio_engine: Option<AudioEngine>,
    engine_handle: Option<EngineHandle>,
    rename_focus: FocusHandle,
    root_focus: FocusHandle,
    /// Connect commands awaiting completion, mapped to their matrix cell.
    pending_route_commands: HashMap<CommandId, (usize, usize)>,
    /// Pairs with a Connect in flight; the reconciler must not re-send them
    /// (a second Connect would stack a duplicate route on the same pair).
    inflight_routes: std::collections::HashSet<(EndpointId, EndpointId)>,
    /// Route ids with a Disconnect in flight; the reconciler must not re-send
    /// them (the backend treats repeats as already-gone, but skipping the
    /// duplicate keeps the event stream clean).
    inflight_disconnects: std::collections::HashSet<RouteId>,
    /// Process CPU load (percent across all cores) and the last sample.
    pub(crate) cpu_load: f32,
    cpu_last: Option<(u64, Instant)>,
    /// Persisted virtual devices to recreate once the backend is running.
    pending_virtuals: Vec<PersistedVirtualDevice>,
    persistence: Option<PersistenceWorker>,
    last_persist: Instant,
    /// Watches settings.json for external edits; None when unavailable.
    settings_watcher: Option<SettingsWatcher>,
    /// Content digest of the settings file version we wrote or applied, so
    /// the watcher can skip our own saves regardless of event ordering.
    settings_digest: Option<u64>,
}

impl RootView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut state = AppState::new(Vec::new());
        let rename_focus = cx.focus_handle();
        let root_focus = cx.focus_handle();
        let (audio_engine, engine_handle) = match AudioEngine::start(backend::default_backend()) {
            Ok((engine, handle)) => (Some(engine), Some(handle)),
            Err(error) => {
                state.set_device_monitor_error(error.user_message);
                (None, None)
            }
        };

        // Load the persisted session, start the autosave worker, and watch
        // the settings file for external edits (import / sync / hand edits).
        let mut pending_virtuals = Vec::new();
        let mut settings_watcher = None;
        let mut settings_digest = None;
        let persistence = SessionStore::default_path()
            .and_then(|path| {
                let store = SessionStore::new(path);
                let loaded = store.load()?;
                settings_digest = Some(loaded.digest);
                settings_watcher = SettingsWatcher::start(store.path());
                // Keep the JSON Schema next to the settings file current.
                let _ = orion::persistence::write_schema_file(store.path());
                let worker = PersistenceWorker::start(store)?;
                Ok((loaded.document, worker))
            })
            .map(|(document, worker)| {
                apply_document(&mut state, &document, &mut pending_virtuals);
                worker
            })
            .ok();

        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            if this
                .update(cx, |this, cx| {
                    let persistence_changed = this.apply_persistence_events();
                    let reloaded = this.maybe_reload_settings();
                    let cpu_changed = this.sample_cpu_load();
                    if this.apply_engine_events() | persistence_changed | reloaded | cpu_changed {
                        cx.notify();
                    }
                    // Debounced save: route/scene changes settle asynchronously
                    // via engine events, so persist once state has been dirty
                    // for a moment rather than at command-send time.
                    if this.state.dirty && this.last_persist.elapsed() >= Duration::from_millis(500)
                    {
                        this.persist_now();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        Self {
            state,
            _audio_engine: audio_engine,
            engine_handle,
            rename_focus,
            root_focus,
            pending_route_commands: HashMap::new(),
            inflight_routes: std::collections::HashSet::new(),
            inflight_disconnects: std::collections::HashSet::new(),
            cpu_load: 0.0,
            cpu_last: None,
            pending_virtuals,
            persistence,
            last_persist: Instant::now(),
            settings_watcher,
            settings_digest,
        }
    }

    fn apply_engine_events(&mut self) -> bool {
        let mut changed = false;
        // Drain at most this many events per tick so a busy engine (meters,
        // endpoint storms) can never starve UI input processing.
        let mut budget = 512usize;
        while budget > 0 {
            budget -= 1;
            let Some(handle) = self.engine_handle.as_ref() else {
                break;
            };
            let event = match handle.try_recv() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.state.set_device_monitor_error(error.user_message);
                    self.engine_handle = None;
                    return true;
                }
            };
            let mut is_meter = false;
            match event {
                EngineEvent::Status {
                    status: EngineStatus::Running,
                } => {
                    self.state.set_device_monitor_connected();
                    self.restore_virtuals();
                    self.reconcile_routes();
                    self.send_stream_tuning();
                }
                EngineEvent::Status {
                    status: EngineStatus::Starting,
                } => {
                    self.state.device_monitor_status = DeviceMonitorStatus::Connecting;
                    self.state.status_message = "Connecting to PipeWire".into();
                }
                EngineEvent::Status {
                    status: EngineStatus::Stopping | EngineStatus::Stopped,
                } => {
                    self.state
                        .set_device_monitor_error("Audio engine stopped".into());
                }
                EngineEvent::Capabilities { capabilities } => {
                    self.state.backend_capabilities = capabilities;
                }
                EngineEvent::Snapshot { graph } => {
                    self.state
                        .set_devices(graph.endpoints.into_values().collect());
                    for route in graph.routes.into_values() {
                        self.state.upsert_route(route);
                    }
                    self.restore_virtuals();
                    self.reconcile_routes();
                }
                EngineEvent::Delta { delta } => match *delta {
                    GraphDelta::EndpointAdded { endpoint } => {
                        self.state.upsert_device(endpoint);
                        self.restore_virtuals();
                        self.reconcile_routes();
                    }
                    GraphDelta::EndpointUpdated { endpoint } => {
                        // Control changes (volume/mute) also land here; they
                        // must not re-push channel state or they would echo
                        // forever (update -> push -> update).
                        self.state.upsert_device(endpoint);
                        self.reconcile_routes();
                    }
                    GraphDelta::EndpointRemoved { endpoint_id } => {
                        self.state.remove_device(endpoint_id);
                        self.reconcile_routes();
                    }
                    GraphDelta::RouteAdded { route } | GraphDelta::RouteUpdated { route } => {
                        self.inflight_routes
                            .remove(&(route.source, route.destination));
                        // A route that came up clears any previous failure for that cell.
                        if matches!(route.state, RouteState::Connecting | RouteState::Active) {
                            let cell = self
                                .state
                                .sources
                                .iter()
                                .position(|source| source.endpoint_id == Some(route.source))
                                .zip(self.state.outputs.iter().position(|output| {
                                    output.endpoint_id == Some(route.destination)
                                }));
                            if let Some(cell) = cell {
                                self.state.route_errors.remove(&cell);
                                self.pending_route_commands
                                    .retain(|_, pending| *pending != cell);
                            }
                        }
                        self.state.upsert_route(route);
                        self.reconcile_routes();
                    }
                    GraphDelta::RouteRemoved { route_id } => {
                        self.inflight_disconnects.remove(&route_id);
                        self.inflight_routes.retain(|pair| {
                            self.state
                                .active_routes
                                .get(pair)
                                .is_some_and(|id| *id != route_id)
                        });
                        self.state.remove_route(route_id);
                        self.reconcile_routes();
                    }
                },
                EngineEvent::Error { command_id, error } => {
                    if let Some(cell) =
                        command_id.and_then(|id| self.pending_route_commands.remove(&id))
                    {
                        if let Some(pair) = self.state.cell_pair(cell.0, cell.1) {
                            self.inflight_routes.remove(&pair);
                        }
                        self.state
                            .route_errors
                            .insert(cell, error.user_message.clone());
                    }
                    if command_id.is_none()
                        && error.code == ErrorCode::BackendUnavailable
                        && matches!(
                            error.severity,
                            ErrorSeverity::Error | ErrorSeverity::Critical
                        )
                    {
                        self.state.set_device_monitor_error(error.user_message);
                    } else {
                        self.state.status_message = error.user_message;
                    }
                }
                EngineEvent::Meter { frame } => {
                    is_meter = true;
                    if self.state.apply_meter(frame) {
                        changed = true;
                    }
                }
                EngineEvent::AudioSettings {
                    rate,
                    quantum,
                    min_quantum: _,
                    max_quantum: _,
                } => {
                    self.state.apply_audio_settings(rate, quantum);
                }
                EngineEvent::CommandCompleted { command_id } => {
                    self.pending_route_commands.remove(&command_id);
                }
            }
            // Non-meter events always repaint; meter frames only when a
            // visible level actually moved.
            changed |= !is_meter;
        }
        changed
    }

    pub(crate) fn start_fader_drag(
        &mut self,
        target: FaderTarget,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            match target {
                FaderTarget::Source(index) => self.state.sources[index].gain_db = 0.0,
                FaderTarget::Output(index) => self.state.outputs[index].gain_db = 0.0,
            }
            self.state.dirty = true;
            self.send_fader_gain(target);
        } else {
            self.state.drag = Some(FaderDrag {
                target,
                last_y: event.position.y,
            });
        }
        cx.notify();
    }

    fn drag_fader(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut drag) = self.state.drag else {
            return;
        };
        if !event.dragging() {
            self.state.drag = None;
            return;
        }

        let scale = if event.modifiers.shift { 0.08 } else { 0.32 };
        let delta = f32::from(drag.last_y - event.position.y) * scale;
        self.state.adjust_fader(drag.target, delta);
        drag.last_y = event.position.y;
        self.state.drag = Some(drag);
        cx.notify();
    }

    fn end_fader_drag(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.state.drag.take() {
            self.send_fader_gain(drag.target);
        }
        cx.notify();
    }

    /// Returns true when the move was consumed by an active divider drag.
    fn drag_divider(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mut drag) = self.state.divider_drag else {
            return false;
        };
        if !event.dragging() {
            self.state.divider_drag = None;
            return false;
        }
        // Outputs sit on the right: moving the mouse left grows the panel.
        let delta = f32::from(drag.last_x - event.position.x);
        let window_width: f32 = window.viewport_size().width.into();
        let width = self.state.outputs_panel_width + delta;
        self.state.set_outputs_panel_width(width, window_width);
        drag.last_x = event.position.x;
        self.state.divider_drag = Some(drag);
        cx.notify();
        true
    }

    /// Returns true when a divider drag ended.
    fn end_divider_drag(&mut self) -> bool {
        self.state.divider_drag.take().is_some()
    }

    /// Start a knob drag; double-click resets the parameter to zero.
    pub(crate) fn start_knob_drag(
        &mut self,
        target: KnobTarget,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.set_strip_param(target, 0.0);
            self.send_param(target);
        } else {
            self.state.knob_drag = Some(KnobDrag {
                target,
                last_y: event.position.y,
            });
        }
        cx.notify();
    }

    /// Current value of a strip parameter (0 when the index is stale).
    fn strip_param_value(&self, target: KnobTarget) -> f32 {
        match target.strip {
            FaderTarget::Source(index) => self
                .state
                .sources
                .get(index)
                .map_or(0.0, |s| param_of(s.delay_ms, &s.eq, target.param)),
            FaderTarget::Output(index) => self
                .state
                .outputs
                .get(index)
                .map_or(0.0, |o| param_of(o.delay_ms, &o.eq, target.param)),
        }
    }

    fn set_strip_param(&mut self, target: KnobTarget, value: f32) {
        match (target.strip, target.param) {
            (FaderTarget::Source(index), KnobParam::Delay) => {
                self.state.set_source_delay(index, value)
            }
            (FaderTarget::Output(index), KnobParam::Delay) => {
                self.state.set_output_delay(index, value)
            }
            (FaderTarget::Source(index), band) => {
                self.state.set_source_eq(index, band_to_eq(band), value)
            }
            (FaderTarget::Output(index), band) => {
                self.state.set_output_eq(index, band_to_eq(band), value)
            }
        }
    }

    /// Returns true when the move was consumed by an active knob drag.
    fn drag_knob(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) -> bool {
        let Some(mut drag) = self.state.knob_drag else {
            return false;
        };
        if !event.dragging() {
            self.state.knob_drag = None;
            return false;
        }
        // Coarse: the full range over ~200 px; Shift for fine adjustment.
        let (coarse, fine) = match drag.target.param {
            KnobParam::Delay => (2.5, 0.1),
            _ => (0.15, 0.02),
        };
        let scale = if event.modifiers.shift { fine } else { coarse };
        let delta = f32::from(drag.last_y - event.position.y) * scale;
        let current = self.strip_param_value(drag.target);
        self.set_strip_param(drag.target, current + delta);
        drag.last_y = event.position.y;
        self.state.knob_drag = Some(drag);
        cx.notify();
        true
    }

    /// Returns true when a knob drag ended (and the command was sent).
    fn end_knob_drag(&mut self) -> bool {
        if let Some(drag) = self.state.knob_drag.take() {
            self.send_param(drag.target);
            return true;
        }
        false
    }

    /// Push a strip parameter to the engine. EQ sends all three bands.
    /// Cycle a strip's channel mode and push it to the engine.
    pub(crate) fn cycle_strip_mode(&mut self, target: FaderTarget, cx: &mut Context<Self>) {
        let (is_source, index) = match target {
            FaderTarget::Source(index) => (true, index),
            FaderTarget::Output(index) => (false, index),
        };
        self.state.cycle_channel_mode(is_source, index);
        let (endpoint_id, mode) = if is_source {
            self.state
                .sources
                .get(index)
                .map_or((None, None), |s| (s.endpoint_id, Some(s.mode)))
        } else {
            self.state
                .outputs
                .get(index)
                .map_or((None, None), |o| (o.endpoint_id, Some(o.mode)))
        };
        if let (Some(endpoint_id), Some(mode)) = (endpoint_id, mode) {
            self.send_engine_command(EngineCommandKind::SetChannelMode { endpoint_id, mode });
        }
        cx.notify();
    }

    /// Push a strip's delay + EQ + channel mode to the engine (scene apply,
    /// rebind, restore).
    fn push_strip_processing(&mut self, strip: FaderTarget) {
        self.send_param(KnobTarget {
            strip,
            param: KnobParam::Delay,
        });
        self.send_param(KnobTarget {
            strip,
            param: KnobParam::EqLow,
        });
        let (endpoint_id, mode) = match strip {
            FaderTarget::Source(index) => self
                .state
                .sources
                .get(index)
                .map_or((None, None), |s| (s.endpoint_id, Some(s.mode))),
            FaderTarget::Output(index) => self
                .state
                .outputs
                .get(index)
                .map_or((None, None), |o| (o.endpoint_id, Some(o.mode))),
        };
        if let (Some(endpoint_id), Some(mode)) = (endpoint_id, mode) {
            self.send_engine_command(EngineCommandKind::SetChannelMode { endpoint_id, mode });
        }
    }

    fn send_param(&mut self, target: KnobTarget) {
        let strip = match target.strip {
            FaderTarget::Source(index) => self
                .state
                .sources
                .get(index)
                .map(|s| (s.endpoint_id, s.delay_ms, s.eq)),
            FaderTarget::Output(index) => self
                .state
                .outputs
                .get(index)
                .map(|o| (o.endpoint_id, o.delay_ms, o.eq)),
        };
        let Some((Some(endpoint_id), delay_ms, eq)) = strip else {
            return;
        };
        match target.param {
            KnobParam::Delay => {
                self.send_engine_command(EngineCommandKind::SetDelay {
                    endpoint_id,
                    delay_ms,
                });
            }
            _ => {
                self.send_engine_command(EngineCommandKind::SetEq {
                    endpoint_id,
                    low_db: eq.low_db,
                    mid_db: eq.mid_db,
                    high_db: eq.high_db,
                });
            }
        }
    }

    pub(crate) fn toggle_route(
        &mut self,
        source_index: usize,
        output_index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.state.cell_pair(source_index, output_index).is_none() {
            self.state.status_message = "Assign a device to both channels first".into();
            cx.notify();
            return;
        }
        // A new attempt clears any previous failure shown on the chip.
        self.state
            .route_errors
            .remove(&(source_index, output_index));
        self.state.toggle_route_intent(source_index, output_index);
        self.reconcile_routes();
        cx.notify();
    }

    pub(crate) fn open_endpoint_picker(
        &mut self,
        target: EndpointPickerTarget,
        cx: &mut Context<Self>,
    ) {
        self.state.endpoint_picker = Some(target);
        cx.notify();
    }

    pub(crate) fn open_rename_modal(
        &mut self,
        source: bool,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_rename(source, index);
        self.rename_focus.focus(window, cx);
        cx.notify();
    }

    fn on_root_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Modals own their own focus; global keys only fire when none is open.
        match event.keystroke.key.as_str() {
            "c" if self.state.scenes.len() > 1 => {
                self.state.scene_dropdown_open = !self.state.scene_dropdown_open;
            }
            "escape" => {
                self.state.scene_dropdown_open = false;
                self.state.settings_dropdown = None;
            }
            _ => return,
        }
        cx.notify();
    }

    fn on_modal_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.state.rename_target.is_some() {
            let key = event.keystroke.key.as_str();
            match key {
                "enter" => self.state.commit_rename(),
                "escape" => self.state.cancel_rename(),
                "backspace" => self.state.rename_backspace(),
                _ => {
                    let modifiers = &event.keystroke.modifiers;
                    if !modifiers.control && !modifiers.platform && !modifiers.alt {
                        if let Some(text) = event.keystroke.key_char.as_deref() {
                            self.state.rename_input(text);
                        }
                    }
                }
            }
        } else if self.state.channel_editor.is_some() {
            let on_details = self
                .state
                .channel_editor
                .as_ref()
                .is_some_and(|editor| editor.step == EditorStep::Details);
            if !on_details {
                cx.notify();
                return;
            }
            let key = event.keystroke.key.as_str();
            match key {
                "enter" => self.commit_channel_editor(cx),
                "escape" => self.state.cancel_channel_editor(),
                "backspace" => self.state.editor_name_backspace(),
                _ => {
                    let modifiers = &event.keystroke.modifiers;
                    if !modifiers.control && !modifiers.platform && !modifiers.alt {
                        if let Some(text) = event.keystroke.key_char.as_deref() {
                            self.state.editor_name_input(text);
                        }
                    }
                }
            }
        } else if self.state.scene_editor.is_some() {
            let key = event.keystroke.key.as_str();
            match key {
                "enter" => {
                    self.state.commit_scene_editor();
                    self.persist_now();
                }
                "escape" => self.state.scene_editor = None,
                "backspace" => self.state.scene_editor_backspace(),
                _ => {
                    let modifiers = &event.keystroke.modifiers;
                    if !modifiers.control && !modifiers.platform && !modifiers.alt {
                        if let Some(text) = event.keystroke.key_char.as_deref() {
                            self.state.scene_editor_input(text);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn cycle_source_color(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(source) = self.state.sources.get(index) {
            let next = source.color.next();
            self.state.set_source_color(index, next);
            cx.notify();
        }
    }

    pub(crate) fn cycle_output_color(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(output) = self.state.outputs.get(index) {
            let next = output.color.next();
            self.state.set_output_color(index, next);
            cx.notify();
        }
    }

    // ---- Channel creation (editor modal) ----

    pub(crate) fn open_channel_editor(
        &mut self,
        is_source: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.open_channel_editor(is_source);
        self.rename_focus.focus(window, cx);
        cx.notify();
    }

    fn commit_channel_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.state.take_channel_editor() else {
            cx.notify();
            return;
        };
        let name = editor.name.trim().to_owned();
        let (index, create_virtual) = if editor.is_source {
            let index = self.state.push_source_channel(
                name.clone(),
                crate::state::editor_role_source_kind(editor.role),
                editor.color,
            );
            (
                index,
                editor.role == EditorRole::Virtual && editor.endpoint.is_none(),
            )
        } else {
            let index = self.state.push_output_bus(
                name.clone(),
                crate::state::editor_role_bus_kind(editor.role),
                editor.color,
            );
            (
                index,
                editor.role == EditorRole::Virtual && editor.endpoint.is_none(),
            )
        };

        if let Some(endpoint_id) = editor.endpoint {
            if editor.is_source {
                self.state.assign_source_endpoint(index, endpoint_id);
            } else {
                self.state.assign_output_endpoint(index, endpoint_id);
            }
            self.push_channel_controls(editor.is_source, index);
        } else if create_virtual {
            // Name the new virtual device after the channel so it is
            // recognizable in system sound, and let reconcile bind it.
            let endpoint_type = if editor.is_source {
                EndpointType::VirtualInput
            } else {
                EndpointType::VirtualOutput
            };
            let virtual_device_id = VirtualDeviceId::new();
            self.send_engine_command(EngineCommandKind::CreateVirtual {
                virtual_device_id,
                endpoint: Box::new(virtual_endpoint_draft(
                    virtual_device_id,
                    endpoint_type,
                    name,
                )),
            });
        }
        self.state.status_message = "Channel added".into();
        cx.notify();
    }

    fn push_channel_controls(&mut self, source: bool, index: usize) {
        let (endpoint_id, gain_db, muted) = if source {
            self.state
                .sources
                .get(index)
                .map_or((None, 0.0, false), |channel| {
                    (channel.endpoint_id, channel.gain_db, channel.muted)
                })
        } else {
            self.state
                .outputs
                .get(index)
                .map_or((None, 0.0, false), |channel| {
                    (channel.endpoint_id, channel.gain_db, channel.muted)
                })
        };
        let Some(endpoint_id) = endpoint_id else {
            return;
        };
        if let Ok(gain) = GainDb::new(gain_db) {
            self.send_engine_command(EngineCommandKind::SetVolume { endpoint_id, gain });
        }
        let muted = if source {
            muted
        } else {
            muted || self.state.master_muted
        };
        self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
    }

    // ---- Channel deletion ----

    pub(crate) fn request_delete_channel(
        &mut self,
        source: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.state.channel_route_count(source, index) > 0 {
            self.state.confirm_delete_channel = Some((source, index));
            cx.notify();
        } else {
            self.delete_channel(source, index, cx);
        }
    }

    fn delete_channel(&mut self, source: bool, index: usize, cx: &mut Context<Self>) {
        // Disconnect backend routes attached to this channel's endpoint first.
        let endpoint = if source {
            self.state
                .sources
                .get(index)
                .and_then(|channel| channel.endpoint_id)
        } else {
            self.state
                .outputs
                .get(index)
                .and_then(|channel| channel.endpoint_id)
        };
        if let Some(endpoint) = endpoint {
            let routes = self
                .state
                .active_routes
                .iter()
                .filter_map(|((route_source, route_destination), route_id)| {
                    (*route_source == endpoint || *route_destination == endpoint)
                        .then_some(*route_id)
                })
                .collect::<Vec<_>>();
            for route_id in routes {
                self.send_engine_command(EngineCommandKind::Disconnect { route_id });
            }
        }
        if source {
            self.state.remove_source(index);
        } else {
            self.state.remove_output_bus(index);
        }
        self.state.status_message = "Channel removed".into();
        cx.notify();
    }

    fn request_delete_virtual(
        &mut self,
        virtual_device_id: VirtualDeviceId,
        cx: &mut Context<Self>,
    ) {
        let (users, routes) = self.state.virtual_usage(virtual_device_id);
        if users.is_empty() && routes == 0 {
            self.delete_virtual(virtual_device_id, cx);
        } else {
            self.state.confirm_delete_virtual = Some(virtual_device_id);
            cx.notify();
        }
    }

    fn select_endpoint(
        &mut self,
        target: EndpointPickerTarget,
        endpoint_id: EndpointId,
        cx: &mut Context<Self>,
    ) {
        let assigned = match target {
            EndpointPickerTarget::Source(index) => {
                self.state.assign_source_endpoint(index, endpoint_id)
            }
            EndpointPickerTarget::Output(index) => {
                self.state.assign_output_endpoint(index, endpoint_id)
            }
        };
        if assigned {
            match target {
                EndpointPickerTarget::Source(index) => {
                    self.send_fader_gain(FaderTarget::Source(index));
                    let muted = self.state.sources[index].muted;
                    self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
                }
                EndpointPickerTarget::Output(index) => {
                    self.send_fader_gain(FaderTarget::Output(index));
                    let muted = self.effective_output_mute(index);
                    self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
                }
            }
            self.state.status_message = "Device assigned".into();
            // Keep the strip's controls and delay on the new endpoint, and
            // migrate its routes: intent flags survive the rebind, so the
            // reconciler drops routes to the old endpoint and opens the new
            // ones.
            let fader_target = match target {
                EndpointPickerTarget::Source(index) => FaderTarget::Source(index),
                EndpointPickerTarget::Output(index) => FaderTarget::Output(index),
            };
            self.send_param(KnobTarget {
                strip: fader_target,
                param: KnobParam::Delay,
            });
            self.send_param(KnobTarget {
                strip: fader_target,
                param: KnobParam::EqLow,
            });
            self.reconcile_routes();
        }
        cx.notify();
    }

    fn render_channel_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.state.channel_editor.clone() else {
            return div().into_any_element();
        };
        let title = if editor.is_source {
            "New source channel"
        } else {
            "New output"
        };
        let (step_index, step_label) = match editor.step {
            EditorStep::Type => (1, "TYPE"),
            EditorStep::Endpoint => (2, "DEVICE"),
            EditorStep::Details => (3, "DETAILS"),
        };

        let content: AnyElement = match editor.step {
            EditorStep::Type => self.render_editor_type_step(&editor, cx),
            EditorStep::Endpoint => self.render_editor_endpoint_step(&editor, cx),
            EditorStep::Details => self.render_editor_details_step(&editor, cx),
        };

        let mut modal = div()
            .id(match editor.step {
                EditorStep::Type => "editor-step-type",
                EditorStep::Endpoint => "editor-step-endpoint",
                EditorStep::Details => "editor-step-details",
            })
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8));
        if editor.step == EditorStep::Details {
            modal = modal
                .track_focus(&self.rename_focus)
                .on_key_down(cx.listener(Self::on_modal_key));
        }
        modal
            .child(
                div()
                    .w(px(480.))
                    .max_h(px(600.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_STRONG))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(FONT_VALUES)
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(format!("{step_index}/3 · {step_label}")),
                            ),
                    )
                    .child(content),
            )
            .into_any_element()
    }

    fn render_editor_type_step(
        &self,
        editor: &crate::state::ChannelEditor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let virtual_supported = self.state.backend_capabilities.virtual_devices;
        let roles: Vec<EditorRole> = if editor.is_source {
            EditorRole::SOURCES.to_vec()
        } else {
            EditorRole::OUTPUTS.to_vec()
        }
        .into_iter()
        .filter(|role| virtual_supported || *role != EditorRole::Virtual)
        .collect();
        let physical_full = editor.is_source
            && self.state.physical_source_count() >= crate::state::MAX_PHYSICAL_INPUT_CHANNELS;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(if editor.is_source {
                        "What does this channel capture?"
                    } else {
                        "Where does this output play?"
                    }),
            )
            .child(div().flex().gap_2().children({
                // Collect eagerly so the element tree owns no borrow of the
                // local roles list (the capability filter builds it per render).
                let cards = roles
                    .iter()
                    .map(|role| {
                        // Copy out of the list: the click listener escapes into the
                        // element tree and cannot borrow the local roles Vec.
                        let role = *role;
                        let selected = editor.role == role;
                        let disabled = physical_full && role == EditorRole::Physical;
                        let icon = role_icon(role);
                        let description = role_description(role, editor.is_source);
                        let has_devices = self.state.devices.iter().any(|endpoint| {
                            if editor.is_source {
                                editor_role_accepts_source(role, endpoint.endpoint_type)
                            } else {
                                editor_role_accepts_output(role, endpoint.endpoint_type)
                            }
                        });
                        let badge: Option<&'static str> = if disabled {
                            Some("LIMIT REACHED")
                        } else if !has_devices && role != EditorRole::Virtual {
                            Some("NO DEVICES AVAILABLE")
                        } else {
                            None
                        };
                        div()
                            .id(format!("editor-role-{}", role.label()))
                            .w(px(130.))
                            .h(px(140.))
                            .p_3()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(if selected { rgb(ACCENT) } else { rgb(BORDER) })
                            .bg(if selected {
                                rgb(SURFACE_RAISED)
                            } else {
                                rgb(BASE_RAISED)
                            })
                            .when(!disabled, |card| {
                                card.cursor_pointer()
                                    .hover(|style| style.border_color(rgb(ACCENT)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.editor_choose_role(role);
                                        cx.notify();
                                    }))
                            })
                            .child(svg().path(icon).size(px(26.)).text_color(if disabled {
                                rgb(TEXT_FAINT)
                            } else {
                                rgb(ACCENT)
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if disabled { rgb(TEXT_FAINT) } else { rgb(TEXT) })
                                    .child(role.label()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(rgb(TEXT_FAINT))
                                    .text_center()
                                    .child(description),
                            )
                            .when_some(badge, |card, badge| {
                                card.child(
                                    div()
                                        .px_1()
                                        .py(px(1.))
                                        .rounded_sm()
                                        .bg(rgb(WARNING).opacity(0.18))
                                        .border_1()
                                        .border_color(rgb(WARNING))
                                        .text_size(px(8.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(WARNING))
                                        .text_center()
                                        .child(badge),
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                cards
            }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .id("editor-type-cancel")
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.cancel_channel_editor();
                                cx.notify();
                            }))
                            .child("CANCEL"),
                    )
                    .child(editor_nav_button(
                        "editor-type-next",
                        "NEXT",
                        true,
                        cx,
                        |this, _cx| {
                            this.state.editor_next_step();
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_editor_endpoint_step(
        &self,
        editor: &crate::state::ChannelEditor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let candidates: Vec<AudioEndpoint> = self
            .state
            .devices
            .iter()
            .filter(|endpoint| {
                if editor.is_source {
                    editor_role_accepts_source(editor.role, endpoint.endpoint_type)
                } else {
                    editor_role_accepts_output(editor.role, endpoint.endpoint_type)
                }
            })
            .cloned()
            .collect();
        let no_candidates = candidates.is_empty();
        let create_new = editor.role == EditorRole::Virtual && editor.endpoint.is_none();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(
                if editor.role == EditorRole::Virtual {
                    "Pick an existing virtual device, or create a new one named after this channel."
                } else {
                    "Pick a device now, or assign one later."
                },
            ))
            .child(
                div()
                    .id("editor-endpoint-scroll")
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(editor.role == EditorRole::Virtual, |list| {
                        list.child(
                            div()
                                .id("editor-endpoint-create-new")
                                .px_2()
                                .py_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(if create_new { rgb(ACCENT) } else { rgb(BORDER) })
                                .bg(if create_new {
                                    rgb(SURFACE_RAISED)
                                } else {
                                    rgb(BASE_RAISED)
                                })
                                .text_xs()
                                .text_color(if create_new { rgb(TEXT) } else { rgb(ACCENT) })
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(editor) = this.state.channel_editor.as_mut() {
                                        editor.endpoint = None;
                                    }
                                    cx.notify();
                                }))
                                .child("+ CREATE NEW VIRTUAL DEVICE"),
                        )
                    })
                    .children(candidates.into_iter().map(|endpoint| {
                        let endpoint_id = endpoint.id;
                        let selected = editor.endpoint == Some(endpoint_id);
                        div()
                            .id(format!("editor-endpoint-{endpoint_id}"))
                            .px_2()
                            .py_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(if selected { rgb(ACCENT) } else { rgb(BORDER) })
                            .bg(if selected {
                                rgb(SURFACE_RAISED)
                            } else {
                                rgb(BASE_RAISED)
                            })
                            .text_xs()
                            .text_color(if selected { rgb(TEXT) } else { rgb(TEXT_MUTED) })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.editor_select_endpoint(endpoint_id);
                                cx.notify();
                            }))
                            .child(crate::ui::channel_strip::truncate_label(&endpoint.name, 50))
                    }))
                    .when(
                        no_candidates && editor.role != EditorRole::Virtual,
                        |list| {
                            list.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child("No compatible devices right now"),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(editor_nav_button(
                        "editor-endpoint-back",
                        "BACK",
                        false,
                        cx,
                        |this, _cx| {
                            this.state.editor_prev_step();
                        },
                    ))
                    .child(editor_nav_button(
                        "editor-endpoint-next",
                        "NEXT",
                        true,
                        cx,
                        |this, _cx| {
                            this.state.editor_next_step();
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_editor_details_step(
        &self,
        editor: &crate::state::ChannelEditor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name_valid = !editor.name.trim().is_empty();
        // Make the endpoint choice explicit before committing: what the
        // channel binds to, or that a new virtual device gets created.
        let endpoint_summary = if let Some(endpoint_id) = editor.endpoint {
            self.state
                .devices
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
                .map(|endpoint| {
                    format!(
                        "{} {}",
                        if editor.is_source {
                            "Captures:"
                        } else {
                            "Plays through:"
                        },
                        endpoint.name
                    )
                })
                .unwrap_or_else(|| "The selected device is offline".into())
        } else if editor.role == EditorRole::Virtual {
            format!(
                "Creates new virtual device: {}",
                if editor.name.trim().is_empty() {
                    "(channel name)".to_string()
                } else {
                    editor.name.trim().to_string()
                }
            )
        } else {
            "No device yet — you can assign one later".into()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(BASE_RAISED))
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(endpoint_summary),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(TEXT_FAINT)).child("NAME"))
                    .child(
                        div()
                            .w_full()
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .bg(rgb(BASE))
                            .text_sm()
                            .font_family(FONT_VALUES)
                            .text_color(rgb(TEXT))
                            .child(format!("{}▏", editor.name)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(TEXT_FAINT)).child("COLOR"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .children(ChannelColor::ALL.into_iter().map(|color| {
                                let selected = editor.color == color;
                                div()
                                    .id(format!("editor-color-{}", color as u8))
                                    .size(px(20.))
                                    .rounded_full()
                                    .border_2()
                                    .border_color(if selected {
                                        rgb(TEXT)
                                    } else {
                                        rgba(0x00000000)
                                    })
                                    .bg(rgb(color.value()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.editor_set_color(color);
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(editor_nav_button(
                        "editor-details-back",
                        "BACK",
                        false,
                        cx,
                        |this, _cx| {
                            this.state.editor_prev_step();
                        },
                    ))
                    .child(
                        div()
                            .id("editor-create")
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(if name_valid { rgb(ACCENT) } else { rgb(BORDER) })
                            .text_xs()
                            .text_color(if name_valid {
                                rgb(ACCENT)
                            } else {
                                rgb(TEXT_FAINT)
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.commit_channel_editor(cx);
                            }))
                            .child("CREATE"),
                    ),
            )
            .into_any_element()
    }

    fn render_delete_channel_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((source, index)) = self.state.confirm_delete_channel else {
            return div().into_any_element();
        };
        let name = if source {
            self.state
                .sources
                .get(index)
                .map(|channel| channel.name.clone())
        } else {
            self.state
                .outputs
                .get(index)
                .map(|channel| format!("{} ({})", channel.name, channel.code))
        };
        let Some(name) = name else {
            return div().into_any_element();
        };
        let routes = self.state.channel_route_count(source, index);

        div()
            .id("delete-channel-modal")
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .child(
                div()
                    .w(px(420.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(WARNING))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(WARNING))
                            .child(format!("Delete {name}?")),
                    )
                    .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                        "It has {routes} active route(s); deleting the channel disconnects them."
                    )))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("delete-channel-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.confirm_delete_channel = None;
                                        cx.notify();
                                    }))
                                    .child("CANCEL"),
                            )
                            .child(
                                div()
                                    .id("delete-channel-confirm")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(RED))
                                    .text_xs()
                                    .text_color(rgb(RED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.confirm_delete_channel = None;
                                        this.delete_channel(source, index, cx);
                                    }))
                                    .child("DELETE"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_delete_scene_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(index) = self.state.confirm_delete_scene else {
            return div().into_any_element();
        };
        let Some(name) = self.state.scenes.get(index).map(|scene| scene.name.clone()) else {
            return div().into_any_element();
        };
        let selected = index == self.state.selected_scene;

        div()
            .id("delete-scene-modal")
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .child(
                div()
                    .w(px(420.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(WARNING))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(WARNING))
                            .child(format!("Delete scene '{name}'?")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .child(if selected {
                                "It is the active scene; the first scene becomes active."
                            } else {
                                "Its saved mixer state is removed from your settings."
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("delete-scene-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.confirm_delete_scene = None;
                                        cx.notify();
                                    }))
                                    .child("CANCEL"),
                            )
                            .child(
                                div()
                                    .id("delete-scene-confirm")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(RED))
                                    .text_xs()
                                    .text_color(rgb(RED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.confirm_delete_scene = None;
                                        this.state.delete_scene(index);
                                        this.persist_now();
                                        cx.notify();
                                    }))
                                    .child("DELETE"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_rename_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((source, index)) = self.state.rename_target else {
            return div().into_any_element();
        };
        let channel_name = if source {
            self.state
                .sources
                .get(index)
                .map(|channel| channel.name.clone())
        } else {
            self.state
                .outputs
                .get(index)
                .map(|channel| channel.name.clone())
        };
        let Some(channel_name) = channel_name else {
            return div().into_any_element();
        };

        div()
            .id("rename-modal")
            .track_focus(&self.rename_focus)
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .on_key_down(cx.listener(Self::on_modal_key))
            .child(
                div()
                    .w(px(380.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_STRONG))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Rename {channel_name}")),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .bg(rgb(BASE))
                            .text_sm()
                            .font_family(FONT_VALUES)
                            .text_color(rgb(TEXT))
                            .child(format!("{}▏", self.state.rename_buffer)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child("Enter to save · Esc to cancel"),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("rename-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.cancel_rename();
                                        cx.notify();
                                    }))
                                    .child("CANCEL"),
                            )
                            .child(
                                div()
                                    .id("rename-save")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(ACCENT))
                                    .text_xs()
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.commit_rename();
                                        cx.notify();
                                    }))
                                    .child("SAVE"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_scene_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(name) = self.state.scene_editor.clone() else {
            return div().into_any_element();
        };
        div()
            .id("scene-modal")
            .track_focus(&self.rename_focus)
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .on_key_down(cx.listener(Self::on_modal_key))
            .child(
                div()
                    .w(px(380.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_STRONG))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("New scene"),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .bg(rgb(BASE))
                            .text_sm()
                            .font_family(FONT_VALUES)
                            .text_color(rgb(TEXT))
                            .child(format!("{name}▏")),
                    )
                    .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(
                        "The current mixer configuration will be used as the base snapshot.",
                    ))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("scene-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.scene_editor = None;
                                        cx.notify();
                                    }))
                                    .child("CANCEL"),
                            )
                            .child(
                                div()
                                    .id("scene-create")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(ACCENT))
                                    .text_xs()
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.commit_scene_editor();
                                        this.persist_now();
                                        cx.notify();
                                    }))
                                    .child("CREATE"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_delete_virtual_modal(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(virtual_device_id) = self.state.confirm_delete_virtual else {
            return div().into_any_element();
        };
        let (users, routes) = self.state.virtual_usage(virtual_device_id);
        let name = self
            .state
            .devices
            .iter()
            .find(|endpoint| endpoint.virtual_device_id == Some(virtual_device_id))
            .map(|endpoint| endpoint.name.clone())
            .unwrap_or_else(|| "virtual device".into());
        let usage = if users.is_empty() {
            format!("{routes} active route(s) reference it.")
        } else {
            format!("In use by: {}", users.join(", "))
        };

        div()
            .id("delete-virtual-modal")
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .child(
                div()
                    .w(px(420.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(WARNING))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(WARNING))
                            .child(format!("Delete {name}?")),
                    )
                    .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                        "{usage} Removing it disconnects those routes and unbinds the channels."
                    )))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("delete-virtual-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.confirm_delete_virtual = None;
                                        cx.notify();
                                    }))
                                    .child("CANCEL"),
                            )
                            .child(
                                div()
                                    .id("delete-virtual-confirm")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(RED))
                                    .text_xs()
                                    .text_color(rgb(RED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.confirm_delete_virtual = None;
                                        this.delete_virtual(virtual_device_id, cx);
                                    }))
                                    .child("DELETE"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_endpoint_picker(&self, cx: &mut Context<Self>) -> Div {
        let Some(target) = self.state.endpoint_picker else {
            return div();
        };
        let (title, candidates) = match target {
            EndpointPickerTarget::Source(index) => {
                let Some(source) = self.state.sources.get(index) else {
                    return div();
                };
                (
                    format!("Select the source device for {}", source.name),
                    self.state
                        .devices
                        .iter()
                        .filter(|endpoint| source_accepts(source.kind, endpoint.endpoint_type))
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            }
            EndpointPickerTarget::Output(index) => {
                let Some(output) = self.state.outputs.get(index) else {
                    return div();
                };
                (
                    format!("Select the output device for {}", output.name),
                    self.state
                        .devices
                        .iter()
                        .filter(|endpoint| output_accepts(output.kind, endpoint.endpoint_type))
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            }
        };
        let no_candidates = candidates.is_empty();

        div()
            .block_mouse_except_scroll()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b8))
            .child(
                div()
                    .w(px(560.))
                    .max_h(px(560.))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_STRONG))
                    .bg(rgb(SURFACE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .id("close-endpoint-picker")
                                    .px_3()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.endpoint_picker = None;
                                        cx.notify();
                                    }))
                                    .child("CLOSE"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child("Devices compatible with this channel"),
                    )
                    .child(
                        div()
                            .id("endpoint-picker-scroll")
                            .max_h(px(430.))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(candidates.into_iter().map(|endpoint| {
                                let endpoint_id = endpoint.id;
                                let online = !matches!(
                                    endpoint.state,
                                    EndpointState::Disconnected | EndpointState::Error
                                );
                                div()
                                    .id(format!("endpoint-option-{endpoint_id}"))
                                    .p_3()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .bg(rgb(BASE_RAISED))
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.border_color(rgb(ACCENT)).bg(rgb(SURFACE_RAISED))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_endpoint(target, endpoint_id, cx);
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(TEXT))
                                                    .child(endpoint.name),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(TEXT_FAINT))
                                                    .child(endpoint.description),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(if online {
                                                rgb(GREEN)
                                            } else {
                                                rgb(WARNING)
                                            })
                                            .child(if online { "AVAILABLE" } else { "OFFLINE" }),
                                    )
                            })),
                    )
                    .when(no_candidates, |panel| {
                        panel.child(empty_state(
                            "No compatible devices",
                            "Start an audio application or connect a PipeWire device.",
                        ))
                    }),
            )
    }

    pub(crate) fn toggle_source_mute(&mut self, index: usize) {
        let Some(source) = self.state.sources.get_mut(index) else {
            return;
        };
        source.muted = !source.muted;
        let endpoint_id = source.endpoint_id;
        let muted = source.muted;
        self.state.dirty = true;
        if let Some(endpoint_id) = endpoint_id {
            self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
        }
    }

    pub(crate) fn toggle_output_mute(&mut self, index: usize) {
        let Some(output) = self.state.outputs.get_mut(index) else {
            return;
        };
        output.muted = !output.muted;
        let endpoint_id = output.endpoint_id;
        let muted = output.muted;
        self.state.dirty = true;
        if let Some(endpoint_id) = endpoint_id {
            self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
        }
    }

    /// Effective mute for an output bus: strip mute OR master MUTE ALL.
    fn effective_output_mute(&self, index: usize) -> bool {
        self.state
            .outputs
            .get(index)
            .is_some_and(|output| output.muted || self.state.master_muted)
    }

    pub(crate) fn toggle_master_mute(&mut self, cx: &mut Context<Self>) {
        self.state.master_muted = !self.state.master_muted;
        let master_muted = self.state.master_muted;
        let outputs = self
            .state
            .outputs
            .iter()
            .filter_map(|output| {
                output
                    .endpoint_id
                    .map(|endpoint_id| (endpoint_id, master_muted || output.muted))
            })
            .collect::<Vec<_>>();
        for (endpoint_id, muted) in outputs {
            self.send_engine_command(EngineCommandKind::SetMute { endpoint_id, muted });
        }
        self.state.status_message = if master_muted {
            "All outputs are muted".into()
        } else {
            "Master mute released".into()
        };
        cx.notify();
    }

    // ---- Persistence & scenes ----

    /// Send the configured channel processing (delay, EQ, mode) of every
    /// bound strip that deviates from defaults.
    fn push_configured_delays(&mut self) {
        let non_default = |delay_ms: f32, eq: &crate::state::EqBands, mode| {
            delay_ms > 0.0
                || eq.low_db != 0.0
                || eq.mid_db != 0.0
                || eq.high_db != 0.0
                || mode != orion::domain::ChannelMode::Auto
        };
        for index in 0..self.state.sources.len() {
            let pending = self.state.sources.get(index).is_some_and(|source| {
                source.endpoint_id.is_some()
                    && non_default(source.delay_ms, &source.eq, source.mode)
            });
            if pending {
                self.push_strip_processing(FaderTarget::Source(index));
            }
        }
        for index in 0..self.state.outputs.len() {
            let pending = self.state.outputs.get(index).is_some_and(|output| {
                output.endpoint_id.is_some()
                    && non_default(output.delay_ms, &output.eq, output.mode)
            });
            if pending {
                self.push_strip_processing(FaderTarget::Output(index));
            }
        }
    }

    /// Recreate persisted virtual devices once the backend is running and
    /// endpoints have been discovered. Routes reconnect via `reconcile_routes`
    /// whenever endpoints appear.
    fn restore_virtuals(&mut self) {
        if self.engine_handle.is_none() {
            return;
        }
        // Backends without virtual-device support (runtime capability) keep
        // the persisted entries pending: they may load again on a platform
        // that supports them.
        if !self.state.backend_capabilities.virtual_devices {
            return;
        }
        let virtuals = std::mem::take(&mut self.pending_virtuals);
        for virtual_device in virtuals {
            let exists = self.state.devices.iter().any(|endpoint| {
                endpoint.virtual_device_id == Some(virtual_device.virtual_device_id)
            });
            if !exists {
                self.send_engine_command(EngineCommandKind::CreateVirtual {
                    virtual_device_id: virtual_device.virtual_device_id,
                    endpoint: Box::new(virtual_endpoint_draft(
                        virtual_device.virtual_device_id,
                        virtual_device.endpoint_type,
                        virtual_device.name.clone(),
                    )),
                });
            }
        }
        // Push restored per-channel sync delays once endpoints are up.
        self.push_configured_delays();
        self.push_restored_mutes();
    }

    /// Enforce persisted mute state (strips + MUTE ALL) once endpoints exist.
    /// Idempotent: endpoints already reporting the target state are skipped,
    /// so running this repeatedly can never produce an event echo loop.
    fn push_restored_mutes(&mut self) {
        let already = |endpoint_id: EndpointId, muted: bool| {
            self.state
                .devices
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id)
                .is_some_and(|endpoint| endpoint.muted == muted)
        };
        let mut commands = Vec::new();
        for source in &self.state.sources {
            if let Some(endpoint_id) = source.endpoint_id {
                if source.muted && !already(endpoint_id, true) {
                    commands.push(EngineCommandKind::SetMute {
                        endpoint_id,
                        muted: true,
                    });
                }
            }
        }
        let master_muted = self.state.master_muted;
        for output in &self.state.outputs {
            if let Some(endpoint_id) = output.endpoint_id {
                let muted = output.muted || master_muted;
                if muted && !already(endpoint_id, true) {
                    commands.push(EngineCommandKind::SetMute {
                        endpoint_id,
                        muted: true,
                    });
                }
            }
        }
        for kind in commands {
            self.send_engine_command(kind);
        }
    }

    /// Drain persistence worker events; surfaces save failures in the status
    /// bar and keeps the bounded event channel from filling up.
    fn apply_persistence_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(worker) = self.persistence.as_ref() {
            match worker.try_recv() {
                Ok(Some(PersistenceEvent::Saved(digest))) => {
                    // Our own write: the watcher ignores this content digest.
                    self.settings_digest = digest.or(self.settings_digest);
                }
                Ok(Some(PersistenceEvent::Failed(error))) => {
                    self.state.status_message = format!("Could not save settings: {error}");
                    changed = true;
                }
                Ok(Some(PersistenceEvent::Stopped)) => {}
                Ok(None) => break,
                Err(_) => {
                    self.persistence = None;
                    self.state.status_message = "Settings autosave stopped working".into();
                    changed = true;
                    break;
                }
            }
        }
        changed
    }

    /// Reload settings.json when it changed on disk outside of Orion (hand
    /// edits, imports, sync tools). Our own writes are skipped by mtime.
    fn maybe_reload_settings(&mut self) -> bool {
        let Some(watcher) = self.settings_watcher.as_ref() else {
            return false;
        };
        if !watcher.changed() {
            return false;
        }
        let Some(path) = SessionStore::default_path().ok() else {
            return false;
        };
        let store = SessionStore::new(path);
        match store.load() {
            Ok(loaded) => {
                // Skip our own writes by content digest: immune to the event
                // ordering between the file watcher and the save worker.
                if Some(loaded.digest) == self.settings_digest && loaded.digest != 0 {
                    return false;
                }
                self.settings_digest = Some(loaded.digest);
                self.apply_settings_document(loaded.document);
                self.state.status_message = "Settings reloaded from disk".into();
            }
            Err(error) => {
                self.state.status_message = format!("Ignoring invalid settings.json: {error}");
            }
        }
        true
    }

    /// Apply a settings document mid-session: rebuild state, then reconcile
    /// the engine (controls, delays, virtuals, routes) with it.
    fn apply_settings_document(&mut self, document: SessionDocument) {
        apply_document(&mut self.state, &document, &mut self.pending_virtuals);

        // Controls and per-bus delays for every bound channel.
        let mut commands = Vec::new();
        for source in &self.state.sources {
            if let Some(endpoint_id) = source.endpoint_id {
                if let Ok(gain) = GainDb::new(source.gain_db) {
                    commands.push(EngineCommandKind::SetVolume { endpoint_id, gain });
                }
                commands.push(EngineCommandKind::SetMute {
                    endpoint_id,
                    muted: source.muted,
                });
                commands.push(EngineCommandKind::SetDelay {
                    endpoint_id,
                    delay_ms: source.delay_ms,
                });
                commands.push(EngineCommandKind::SetEq {
                    endpoint_id,
                    low_db: source.eq.low_db,
                    mid_db: source.eq.mid_db,
                    high_db: source.eq.high_db,
                });
                commands.push(EngineCommandKind::SetChannelMode {
                    endpoint_id,
                    mode: source.mode,
                });
            }
        }
        for output in &self.state.outputs {
            if let Some(endpoint_id) = output.endpoint_id {
                if let Ok(gain) = GainDb::new(output.gain_db) {
                    commands.push(EngineCommandKind::SetVolume { endpoint_id, gain });
                }
                commands.push(EngineCommandKind::SetMute {
                    endpoint_id,
                    muted: output.muted || self.state.master_muted,
                });
                commands.push(EngineCommandKind::SetDelay {
                    endpoint_id,
                    delay_ms: output.delay_ms,
                });
                commands.push(EngineCommandKind::SetEq {
                    endpoint_id,
                    low_db: output.eq.low_db,
                    mid_db: output.eq.mid_db,
                    high_db: output.eq.high_db,
                });
                commands.push(EngineCommandKind::SetChannelMode {
                    endpoint_id,
                    mode: output.mode,
                });
            }
        }
        for kind in commands {
            self.send_engine_command(kind);
        }
        self.send_stream_tuning();

        // The document set the desired matrix; the reconciler drops routes
        // the document does not want and connects the missing ones as their
        // endpoints appear.
        self.restore_virtuals();
        self.reconcile_routes();
    }

    /// Sample process CPU load once a second; returns whether it moved enough
    /// to repaint.
    fn sample_cpu_load(&mut self) -> bool {
        if self
            .cpu_last
            .is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(1))
        {
            return false;
        }
        let Some(ticks) = crate::process_stats::read_process_ticks() else {
            return false;
        };
        let now = Instant::now();
        let Some((prev_ticks, prev_at)) = self.cpu_last else {
            self.cpu_last = Some((ticks, now));
            return false;
        };
        let elapsed = prev_at.elapsed().as_secs_f32().max(0.001);
        let cores = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1) as f32;
        let delta = ticks.saturating_sub(prev_ticks) as f32;
        // ticks are in clock ticks (100/sec on Linux); normalize across cores.
        let load = (delta / 100.0 / elapsed / cores * 100.0).clamp(0.0, 100.0);
        self.cpu_last = Some((ticks, now));
        let moved = (load - self.cpu_load).abs() >= 0.5;
        self.cpu_load = load;
        moved
    }

    pub(crate) fn persist_now(&mut self) {
        // Live scenes: the selected scene tracks the mixer on every save.
        self.state.capture_scene_silent(self.state.selected_scene);
        if let Some(worker) = self.persistence.as_ref() {
            let _ = worker.save(document_from_state(&self.state));
        }
        self.last_persist = Instant::now();
        self.state.dirty = false;
    }

    pub(crate) fn apply_scene(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.state.apply_scene_controls(index).is_none() {
            self.state.selected_scene = index;
            self.state.dirty = true;
            self.state.status_message =
                format!("Scene '{}' selected", self.state.scenes[index].name);
            cx.notify();
            return;
        }
        // Apply scene controls to live endpoints (collect first to avoid
        // borrowing state while sending commands).
        let mut control_commands = Vec::new();
        for source in &self.state.sources {
            if let Some(endpoint_id) = source.endpoint_id {
                if let Ok(gain) = GainDb::new(source.gain_db) {
                    control_commands.push(EngineCommandKind::SetVolume { endpoint_id, gain });
                }
                control_commands.push(EngineCommandKind::SetMute {
                    endpoint_id,
                    muted: source.muted,
                });
            }
        }
        for output in &self.state.outputs {
            if let Some(endpoint_id) = output.endpoint_id {
                if let Ok(gain) = GainDb::new(output.gain_db) {
                    control_commands.push(EngineCommandKind::SetVolume { endpoint_id, gain });
                }
                control_commands.push(EngineCommandKind::SetMute {
                    endpoint_id,
                    muted: output.muted || self.state.master_muted,
                });
            }
        }
        for kind in control_commands {
            self.send_engine_command(kind);
        }
        // Per-channel delay and EQ are part of the scene snapshot too.
        for index in 0..self.state.sources.len() {
            self.push_strip_processing(FaderTarget::Source(index));
        }
        for index in 0..self.state.outputs.len() {
            self.push_strip_processing(FaderTarget::Output(index));
        }
        // The scene set the desired matrix; the reconciler diffs first, so
        // re-applying the active scene is a no-op instead of a
        // disconnect/connect flap (the graph rejects duplicate Connects).
        self.reconcile_routes();
        // No persist here: route events settle asynchronously and mark the
        // state dirty; the tick loop persists once the matrix has settled.
        self.state.status_message = format!("Scene '{}' applied", self.state.scenes[index].name);
        cx.notify();
    }

    /// Reconcile live routes with the desired matrix: disconnect active
    /// routes that are no longer wanted, connect desired cells that are not
    /// live yet. Idempotent — safe to call after any endpoint/route change.
    fn reconcile_routes(&mut self) {
        let desired: std::collections::HashSet<(EndpointId, EndpointId)> =
            self.state.desired_route_pairs().into_iter().collect();
        let stale: Vec<RouteId> = self
            .state
            .active_routes
            .iter()
            .filter(|(pair, _)| !desired.contains(*pair))
            .map(|(_, route_id)| *route_id)
            .filter(|route_id| !self.inflight_disconnects.contains(route_id))
            .collect();
        for route_id in stale {
            self.send_engine_command(EngineCommandKind::Disconnect { route_id });
            self.inflight_disconnects.insert(route_id);
        }
        for source_index in 0..self.state.sources.len() {
            for output_index in 0..self.state.outputs.len() {
                if !self.state.route_desired(source_index, output_index) {
                    continue;
                }
                let Some((source, destination)) = self.state.cell_pair(source_index, output_index)
                else {
                    continue;
                };
                if self
                    .state
                    .active_routes
                    .contains_key(&(source, destination))
                    || self.inflight_routes.contains(&(source, destination))
                {
                    continue;
                }
                // Only connect when both endpoints are actually online; the
                // intent stays set and reconciles when the device appears.
                let both_online = self
                    .state
                    .devices
                    .iter()
                    .any(|endpoint| endpoint.id == source)
                    && self
                        .state
                        .devices
                        .iter()
                        .any(|endpoint| endpoint.id == destination);
                if !both_online {
                    continue;
                }
                if let Some(command_id) = self.send_engine_command(EngineCommandKind::Connect {
                    route_id: RouteId::new(),
                    source,
                    destination,
                }) {
                    self.pending_route_commands
                        .insert(command_id, (source_index, output_index));
                    self.inflight_routes.insert((source, destination));
                }
            }
        }
    }

    /// Header scene dropdown: lists all scenes; clicking one applies it.
    fn render_scene_dropdown(&self, cx: &mut Context<Self>) -> AnyElement {
        // Full-window blocking overlay: clicks outside the list close the
        // dropdown and never reach the mixer behind it.
        div()
            .id("scene-dropdown-overlay")
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .block_mouse_except_scroll()
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.scene_dropdown_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .id("scene-dropdown")
                    .absolute()
                    .top(px(56.))
                    .left(px(152.))
                    .w(px(220.))
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BORDER_STRONG))
                    .bg(rgb(SURFACE))
                    .py_1()
                    .children(self.state.scenes.iter().enumerate().map(|(index, scene)| {
                        let selected = index == self.state.selected_scene;
                        div()
                            .id(format!("scene-option-{index}"))
                            .px_3()
                            .py_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_sm()
                            .text_color(if selected {
                                rgb(ACCENT)
                            } else {
                                rgb(TEXT_MUTED)
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE_2)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.state.scene_dropdown_open = false;
                                    this.apply_scene(index, cx);
                                }),
                            )
                            .child(scene.name.clone())
                            .when(selected, |row| row.child(div().text_xs().child("ACTIVE")))
                    })),
            )
            .into_any_element()
    }

    /// Settings panel for the live config file: path, watch note, and
    /// import/export for syncing across machines.
    fn render_config_file_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        panel()
            .child(panel_heading("CONFIG FILE", "Synced live · portable"))
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child("Open to edit by hand, or Import to replace your settings. Changes apply live, scenes included, and a JSON Schema sits next to the file for editor autocomplete. Orion rewrites this file while running, so export before editing."),
            )
            .child(
                div()
                    .mt_3()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("settings-open")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(BORDER_STRONG)).bg(rgb(SURFACE_2)))
                            .on_click(cx.listener(|_, _, _, cx| {
                                if let Ok(path) = SessionStore::default_path() {
                                    cx.open_with_system(&path);
                                }
                            }))
                            .child("OPEN"),
                    )
                    .child(
                        div()
                            .id("settings-export")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT_MUTED))
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(BORDER_STRONG)).bg(rgb(SURFACE_2)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_settings(cx);
                            }))
                            .child("EXPORT"),
                    )
                    .child(
                        div()
                            .id("settings-import")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(ACCENT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE_2)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_settings(cx);
                            }))
                            .child("IMPORT"),
                    ),
            )
    }

    /// Input-style selector control for settings; opens the dropdown at the
    /// click position.
    fn render_settings_select(
        &self,
        select: SettingsSelect,
        current: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = match select {
            SettingsSelect::SampleRate => "select-sample-rate",
            SettingsSelect::BufferSize => "select-buffer-size",
        };
        div()
            .id(id)
            .w(px(200.))
            .h(px(30.))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BASE))
            .text_xs()
            .font_family(FONT_VALUES)
            .text_color(rgb(ACCENT))
            .cursor_pointer()
            .hover(|style| style.border_color(rgb(BORDER_STRONG)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.state.settings_dropdown = Some(crate::state::SettingsDropdown {
                        select,
                        anchor: event.position,
                    });
                    cx.notify();
                }),
            )
            .child(current)
            .child(
                svg()
                    .path(ICON_ARROW_DOWN)
                    .size(px(10.))
                    .flex_shrink_0()
                    .text_color(rgb(TEXT_FAINT)),
            )
    }

    /// The settings dropdown overlay: backdrop closes, the list opens where
    /// the selector was clicked and snaps inside the window.
    fn render_settings_dropdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dropdown) = self.state.settings_dropdown else {
            return div().into_any_element();
        };
        let (options, selected_value): (&[u32], u32) = match dropdown.select {
            SettingsSelect::SampleRate => (&SAMPLE_RATE_OPTIONS, self.state.sample_rate),
            SettingsSelect::BufferSize => (&BUFFER_SIZE_OPTIONS, self.state.buffer_size),
        };
        let formatter: fn(u32) -> String = match dropdown.select {
            SettingsSelect::SampleRate => format_sample_rate,
            SettingsSelect::BufferSize => |size| format!("{size}"),
        };
        let select = dropdown.select;

        div()
            .id("settings-dropdown-backdrop")
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size_full()
            .block_mouse_except_scroll()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.state.settings_dropdown = None;
                    cx.notify();
                }),
            )
            .child(deferred(
                anchored().position(dropdown.anchor).snap_to_window().child(
                    div()
                        .w(px(200.))
                        .flex()
                        .flex_col()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER_STRONG))
                        .bg(rgb(SURFACE))
                        .py_1()
                        .children(options.iter().map(|value| {
                            let selected = *value == selected_value;
                            div()
                                .id(format!("settings-option-{}-{}", select.label(), value))
                                .px_3()
                                .py_2()
                                .text_xs()
                                .font_family(FONT_VALUES)
                                .text_color(if selected {
                                    rgb(ACCENT)
                                } else {
                                    rgb(TEXT_MUTED)
                                })
                                .cursor_pointer()
                                .hover(|style| style.bg(rgb(SURFACE_2)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.state.settings_dropdown = None;
                                        match select {
                                            SettingsSelect::SampleRate => {
                                                this.state.set_sample_rate(*value)
                                            }
                                            SettingsSelect::BufferSize => {
                                                this.state.set_buffer_size(*value)
                                            }
                                        }
                                        this.send_stream_tuning();
                                        cx.notify();
                                    }),
                                )
                                .child(formatter(*value))
                        })),
                ),
            ))
            .into_any_element()
    }

    /// Settings hint for the reset row: shows the detected clock when known.
    fn reset_hint(&self) -> String {
        match self.state.detected_audio {
            Some((rate, quantum)) => format!(
                "Reset to the detected system clock: {} · {quantum} frames",
                format_sample_rate(rate)
            ),
            None => "Reset to the detected system clock once connected".into(),
        }
    }

    /// Import a settings file: validate it, then copy it over settings.json.
    /// The file watcher applies it live from there (one code path for both
    /// imports and external edits).
    fn import_settings(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import Orion settings".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, _| this.import_settings_file(&path))
                .ok();
        })
        .detach();
    }

    fn import_settings_file(&mut self, path: &std::path::Path) {
        // Validate the picked file before touching the live one.
        if let Err(error) = SessionStore::new(path.to_path_buf()).load() {
            self.state.status_message = format!("Not a valid Orion settings file: {error}");
            return;
        }
        let Ok(target) = SessionStore::default_path() else {
            self.state.status_message = "Could not find the settings file".into();
            return;
        };
        self.state.status_message = match std::fs::copy(path, &target) {
            Ok(_) => "Settings file replaced — applying…".into(),
            Err(error) => format!("Import failed: {error}"),
        };
    }

    /// Export the current settings to a user-picked file (sync/backup).
    fn export_settings(&mut self, cx: &mut Context<Self>) {
        // Make sure the file on disk reflects the live state first.
        self.persist_now();
        let Some(directory) = SessionStore::default_path()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        else {
            return;
        };
        let receiver = cx.prompt_for_new_path(&directory, Some("orion-settings.json"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(target))) = receiver.await else {
                return;
            };
            this.update(cx, |this, _| {
                let Ok(source) = SessionStore::default_path() else {
                    return;
                };
                this.state.status_message = match std::fs::copy(&source, &target) {
                    Ok(_) => format!("Settings exported to {}", target.display()),
                    Err(error) => format!("Export failed: {error}"),
                };
            })
            .ok();
        })
        .detach();
    }

    fn send_stream_tuning(&mut self) {
        self.send_engine_command(EngineCommandKind::SetStreamTuning {
            stream_rate: self.state.sample_rate,
            buffer_frames: self.state.buffer_size,
        });
    }

    fn send_fader_gain(&mut self, target: FaderTarget) {
        let control = match target {
            FaderTarget::Source(index) => self
                .state
                .sources
                .get(index)
                .and_then(|source| source.endpoint_id.map(|id| (id, source.gain_db))),
            FaderTarget::Output(index) => self
                .state
                .outputs
                .get(index)
                .and_then(|output| output.endpoint_id.map(|id| (id, output.gain_db))),
        };
        let Some((endpoint_id, gain_db)) = control else {
            return;
        };
        if let Ok(gain) = GainDb::new(gain_db) {
            self.send_engine_command(EngineCommandKind::SetVolume { endpoint_id, gain });
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .w(px(112.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_between()
            .bg(rgb(BASE_RAISED))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(AppView::ALL.into_iter().map(|view| {
                        let active = self.state.active_view == view;
                        div()
                            .id(format!("nav-{}", view.label().to_lowercase()))
                            .h(px(72.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(if active {
                                rgb(PRIMARY)
                            } else {
                                rgba(0x00000000)
                            })
                            .bg(if active {
                                rgb(SURFACE_RAISED)
                            } else {
                                rgba(0x00000000)
                            })
                            .text_color(if active { rgb(ACCENT) } else { rgb(TEXT_MUTED) })
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE_2)).text_color(rgb(TEXT)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.active_view = view;
                                cx.notify();
                            }))
                            .child(svg().path(nav_icon(view, active)).size(px(22.)).text_color(
                                if active {
                                    rgb(PRIMARY)
                                } else {
                                    rgb(TEXT_MUTED)
                                },
                            ))
                            .child(div().text_xs().child(view.label()))
                    })),
            )
        // .child(
        //     div()
        //         .h(px(50.))
        //         .flex()
        //         .items_center()
        //         .justify_center()
        //         .text_size(px(18.))
        //         .text_color(rgb(TEXT_FAINT))
        //         .child("<<"),
        // )
    }

    fn render_mixer(&self, strip_height: f32, cx: &mut Context<Self>) -> Div {
        // Fixed sections: header 62 + knobs 174 + gain 34 + mute 48; the rest
        // is shared between meter (35% of the surplus) and fader (rest).
        let flexible = strip_height - 318.0;
        let meter_height = (140.0 + (flexible - 312.0).max(0.0) * 0.35).min(strip_height * 0.45);
        div()
            .size_full()
            .flex()
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .p_3()
                    .child(section_label(
                        "SOURCES",
                        "Capture from mics, desktop and apps",
                    ))
                    .child(
                        div()
                            .id("input-strip-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .pb_2()
                                    .children((0..self.state.sources.len()).map(|index| {
                                        self.render_source_strip(
                                            index,
                                            strip_height,
                                            meter_height,
                                            cx,
                                        )
                                    }))
                                    .child(self.render_add_source(strip_height, cx)),
                            ),
                    ),
            )
            .child(
                // Drag handle: resizes the outputs panel against the sources
                // panel. Wide invisible hit area over a 1px visual line.
                div()
                    .id("mixer-divider")
                    .w(px(7.))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.state.divider_drag = Some(crate::state::DividerDrag {
                                last_x: event.position.x,
                            });
                            cx.notify();
                        }),
                    )
                    .child(div().w(px(1.)).h_full().bg(rgb(BORDER))),
            )
            .child(
                div()
                    .w(px(self.state.outputs_panel_width))
                    .h_full()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .p_3()
                    .bg(rgb(BASE))
                    .child(section_label(
                        "OUTPUTS",
                        "Play or send the mix to devices and virtual outputs",
                    ))
                    .child(
                        div()
                            .id("output-strip-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_scroll()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .pb_2()
                                    .children((0..self.state.outputs.len()).map(|index| {
                                        self.render_output_strip(
                                            index,
                                            strip_height,
                                            meter_height,
                                            cx,
                                        )
                                    }))
                                    .child(self.render_add_output(strip_height, cx)),
                            ),
                    ),
            )
    }

    fn render_add_source(&self, strip_height: f32, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_add_strip_card("add-source", "SOURCE", strip_height, cx, true)
    }

    fn render_add_output(&self, strip_height: f32, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_add_strip_card("add-output", "BUS", strip_height, cx, false)
    }

    fn render_add_strip_card(
        &self,
        id: &'static str,
        label: &'static str,
        strip_height: f32,
        cx: &mut Context<Self>,
        source: bool,
    ) -> impl IntoElement {
        div()
            .id(id)
            .w(px(108.))
            .h(px(strip_height))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BASE))
            .text_color(rgb(TEXT_FAINT))
            .cursor_pointer()
            .hover(|style| {
                style
                    .bg(rgb(SURFACE))
                    .border_color(rgb(ACCENT))
                    .text_color(rgb(ACCENT))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_channel_editor(source, window, cx);
            }))
            .child(div().text_size(px(28.)).child("+"))
            .child(
                div()
                    .px_2()
                    .text_center()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("ADD")
                    .child(
                        div()
                            .mt_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(label),
                    ),
            )
    }

    fn render_devices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.state.hub_tab;
        div()
            .id("devices-page-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .child(page_heading(
                "CHANNELS",
                "Name and color your channels, assign devices, and manage virtual devices.",
            ))
            .child(
                div()
                    .mt_4()
                    .flex()
                    .gap_1()
                    .p_1()
                    .rounded_md()
                    .bg(rgb(BASE_RAISED))
                    .children([HubTab::Channels, HubTab::VirtualDevices].into_iter().map(
                        |item| {
                            let active = item == tab;
                            div()
                                .id(match item {
                                    HubTab::Channels => "hub-tab-channels",
                                    HubTab::VirtualDevices => "hub-tab-virtual",
                                })
                                .px_3()
                                .py_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(if active {
                                    rgb(ACCENT)
                                } else {
                                    rgba(0x00000000)
                                })
                                .bg(if active {
                                    rgb(SURFACE_RAISED)
                                } else {
                                    rgba(0x00000000)
                                })
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if active {
                                    rgb(ACCENT)
                                } else {
                                    rgb(TEXT_FAINT)
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.set_hub_tab(item);
                                    cx.notify();
                                }))
                                .child(match item {
                                    HubTab::Channels => "MIXER CHANNELS",
                                    HubTab::VirtualDevices => "VIRTUAL DEVICES",
                                })
                        },
                    )),
            )
            .when(tab == HubTab::Channels, |page| {
                page.child(
                    div()
                        .mt_4()
                        .grid()
                        .grid_cols(2)
                        .gap_4()
                        .child(self.render_mixer_channel_panel(true, cx))
                        .child(self.render_mixer_channel_panel(false, cx)),
                )
            })
            .when(tab == HubTab::VirtualDevices, |page| {
                if !self.state.backend_capabilities.virtual_devices {
                    return page.child(
                        div()
                            .mt_4()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(BASE_RAISED))
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child(
                                "The active audio backend does not support virtual devices on this platform, so none are managed here.",
                            ),
                    );
                }
                page.child(
                    div()
                        .mt_4()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(BASE_RAISED))
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(
                            "While Orion is running, Virtual Inputs appear as playback targets and Virtual Outputs appear as microphones in your system's sound settings.",
                        ),
                )
                .child(
                    div()
                        .mt_4()
                        .grid()
                        .grid_cols(2)
                        .gap_4()
                        .child(self.render_virtual_panel(EndpointType::VirtualInput, cx))
                        .child(self.render_virtual_panel(EndpointType::VirtualOutput, cx)),
                )
            })
    }

    fn render_mixer_channel_panel(&self, sources: bool, cx: &mut Context<Self>) -> Div {
        let title = if sources {
            "MIXER SOURCES"
        } else {
            "MIXER OUTPUTS"
        };
        let count = if sources {
            self.state.sources.len()
        } else {
            self.state.outputs.len()
        };

        panel()
            .child(panel_heading(title, &format!("{count} channels")))
            .child(
                div()
                    .mt_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(if sources {
                        (0..self.state.sources.len())
                            .map(|index| self.render_source_config_card(index, cx))
                            .collect::<Vec<_>>()
                    } else {
                        (0..self.state.outputs.len())
                            .map(|index| self.render_output_config_card(index, cx))
                            .collect::<Vec<_>>()
                    })
                    .child(
                        div()
                            .id(if sources {
                                "hub-add-source"
                            } else {
                                "hub-add-output"
                            })
                            .mt_1()
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .text_xs()
                            .text_color(rgb(ACCENT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE_2)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_channel_editor(sources, window, cx);
                            }))
                            .child(if sources {
                                "ADD SOURCE CHANNEL"
                            } else {
                                "ADD OUTPUT"
                            }),
                    ),
            )
    }

    fn render_source_config_card(&self, index: usize, cx: &mut Context<Self>) -> Div {
        let source = &self.state.sources[index];
        let online = source.online;
        div()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BASE_RAISED))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(8.)).rounded_full().bg(if online {
                                rgb(GREEN)
                            } else {
                                rgb(TEXT_FAINT)
                            }))
                            .child(
                                div()
                                    .id(format!("hub-source-rename-{index}"))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_rename_modal(true, index, window, cx);
                                    }))
                                    .child(source.name.clone()),
                            )
                            .child(
                                div()
                                    .id(format!("hub-source-color-{index}"))
                                    .size(px(12.))
                                    .rounded_full()
                                    .bg(rgb(source.color.value()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cycle_source_color(index, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                div()
                                    .id(format!("hub-source-bind-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_endpoint_picker(
                                            EndpointPickerTarget::Source(index),
                                            cx,
                                        );
                                    }))
                                    .child("ASSIGN"),
                            )
                            .child(
                                div()
                                    .id(format!("hub-source-clear-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(TEXT_FAINT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.clear_source_binding(index);
                                        cx.notify();
                                    }))
                                    .child("CLEAR"),
                            )
                            .child(
                                div()
                                    .id(format!("hub-source-delete-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(RED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_delete_channel(true, index, cx);
                                    }))
                                    .child("DELETE"),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(crate::ui::channel_strip::truncate_label(&source.detail, 42)),
            )
    }

    fn render_output_config_card(&self, index: usize, cx: &mut Context<Self>) -> Div {
        let output = &self.state.outputs[index];
        let online = output.online;
        div()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BASE_RAISED))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(8.)).rounded_full().bg(if online {
                                rgb(GREEN)
                            } else {
                                rgb(TEXT_FAINT)
                            }))
                            .child(
                                div()
                                    .px_1()
                                    .rounded_sm()
                                    .text_size(px(9.))
                                    .font_family(FONT_VALUES)
                                    .text_color(rgb(ACCENT))
                                    .child(output.code.clone()),
                            )
                            .child(
                                div()
                                    .id(format!("hub-output-rename-{index}"))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_rename_modal(false, index, window, cx);
                                    }))
                                    .child(output.name.clone()),
                            )
                            .child(
                                div()
                                    .id(format!("hub-output-color-{index}"))
                                    .size(px(12.))
                                    .rounded_full()
                                    .bg(rgb(output.color.value()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cycle_output_color(index, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                div()
                                    .id(format!("hub-output-bind-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_endpoint_picker(
                                            EndpointPickerTarget::Output(index),
                                            cx,
                                        );
                                    }))
                                    .child("ASSIGN"),
                            )
                            .child(
                                div()
                                    .id(format!("hub-output-clear-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(TEXT_FAINT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.state.clear_output_binding(index);
                                        cx.notify();
                                    }))
                                    .child("CLEAR"),
                            )
                            .child(
                                div()
                                    .id(format!("hub-output-delete-{index}"))
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .text_size(px(9.))
                                    .text_color(rgb(RED))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.request_delete_channel(false, index, cx);
                                    }))
                                    .child("DELETE"),
                            ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(crate::ui::channel_strip::truncate_label(&output.detail, 42)),
            )
    }

    fn render_virtual_panel(&self, endpoint_type: EndpointType, cx: &mut Context<Self>) -> Div {
        let endpoints: Vec<_> = self
            .state
            .devices
            .iter()
            .filter(|endpoint| endpoint.endpoint_type == endpoint_type)
            .collect();
        let count = endpoints.len();
        let (title, add_label) = if endpoint_type == EndpointType::VirtualInput {
            ("VIRTUAL INPUTS", "ADD VIRTUAL INPUT")
        } else {
            ("VIRTUAL OUTPUTS", "ADD VIRTUAL OUTPUT")
        };

        panel()
            .child(panel_heading(title, &format!("{count} managed")))
            .child(
                div()
                    .mt_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(endpoints.is_empty(), |container| {
                        container.child(empty_state(
                            "Creating devices…",
                            "Waiting for PipeWire to register Orion's virtual devices…",
                        ))
                    })
                    .children(endpoints.into_iter().map(|endpoint| {
                        let virtual_device_id = endpoint.virtual_device_id;
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(endpoint_card(endpoint))
                            .when_some(virtual_device_id, |container, virtual_device_id| {
                                container.child(
                                    div()
                                        .id(format!("delete-virtual-{virtual_device_id}"))
                                        .h(px(32.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if count > 1 {
                                            rgb(BORDER)
                                        } else {
                                            rgb(SURFACE_2)
                                        })
                                        .text_size(px(9.))
                                        .text_color(if count > 1 {
                                            rgb(WARNING)
                                        } else {
                                            rgb(TEXT_FAINT)
                                        })
                                        .when(count > 1, |button| {
                                            button
                                                .cursor_pointer()
                                                .hover(|style| style.bg(rgb(SURFACE_2)))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.request_delete_virtual(
                                                        virtual_device_id,
                                                        cx,
                                                    );
                                                }))
                                        })
                                        .child(if count > 1 {
                                            "REMOVE"
                                        } else {
                                            "AT LEAST ONE REQUIRED"
                                        }),
                                )
                            })
                    }))
                    .child(
                        div()
                            .id(format!("add-virtual-{endpoint_type:?}"))
                            .mt_2()
                            .h(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ACCENT))
                            .text_xs()
                            .text_color(rgb(ACCENT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(SURFACE_2)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_virtual(endpoint_type, cx);
                            }))
                            .child(add_label),
                    ),
            )
    }

    fn add_virtual(&mut self, endpoint_type: EndpointType, cx: &mut Context<Self>) {
        if !self.state.backend_capabilities.virtual_devices {
            self.state.status_message =
                "Virtual devices are not supported by this audio backend".into();
            cx.notify();
            return;
        }
        let number = self
            .state
            .devices
            .iter()
            .filter(|endpoint| endpoint.endpoint_type == endpoint_type)
            .count()
            + 1;
        let virtual_device_id = VirtualDeviceId::new();
        let label = if endpoint_type == EndpointType::VirtualInput {
            format!("Orion Virtual Input {number}")
        } else {
            format!("Orion Virtual Output {number}")
        };
        self.send_engine_command(EngineCommandKind::CreateVirtual {
            virtual_device_id,
            endpoint: Box::new(virtual_endpoint_draft(
                virtual_device_id,
                endpoint_type,
                label,
            )),
        });
        self.state.status_message = "Creating virtual device…".into();
        cx.notify();
    }

    fn delete_virtual(&mut self, virtual_device_id: VirtualDeviceId, cx: &mut Context<Self>) {
        self.send_engine_command(EngineCommandKind::DeleteVirtual { virtual_device_id });
        self.state.status_message = "Removing virtual device…".into();
        cx.notify();
    }

    /// Restart the audio engine (recovery from PipeWire weirdness).
    /// Sequential, off the UI thread: the old engine is shut down and joined
    /// first — overlapping teardown/startup makes the two engines fight over
    /// the same PipeWire nodes (same names, same stable endpoint ids), which
    /// left the new engine offline and routes bound to dying nodes.
    pub(crate) fn restart_engine(&mut self, cx: &mut Context<Self>) {
        self.state.status_message = "Restarting the audio engine…".into();
        self.state.device_monitor_status = DeviceMonitorStatus::Connecting;
        let old_engine = self._audio_engine.take();
        self.engine_handle = None;
        // The old engine's routes died with it; its RouteRemoved events went
        // to the dropped channel, so the bookkeeping is stale. Clear it or
        // the reconciler would treat the desired pairs as already connected
        // and never re-establish them on the new engine.
        self.state.active_routes.clear();
        self.state.route_errors.clear();
        self.inflight_routes.clear();
        self.inflight_disconnects.clear();
        self.pending_route_commands.clear();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    if let Some(engine) = old_engine {
                        let _ = engine.shutdown();
                    }
                })
                .await;
            let started = cx
                .background_executor()
                .spawn(async move { AudioEngine::start(backend::default_backend()) });
            let started = started.await;
            this.update(cx, |this, cx| {
                match started {
                    Ok((engine, handle)) => {
                        this._audio_engine = Some(engine);
                        this.engine_handle = Some(handle);
                    }
                    Err(error) => {
                        this.state.set_device_monitor_error(error.user_message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_engine_command(&mut self, kind: EngineCommandKind) -> Option<CommandId> {
        let command = EngineCommand::new(kind);
        let command_id = command.id;
        let result = self
            .engine_handle
            .as_ref()
            .ok_or_else(|| "Audio engine is unavailable".to_string())
            .and_then(|handle| handle.send(command).map_err(|error| error.user_message));
        if let Err(error) = result {
            self.state.set_device_monitor_error(error);
            None
        } else {
            Some(command_id)
        }
    }

    fn render_scenes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("scenes-page-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .child(page_heading(
                "SCENES",
                "Save and switch between full mixer setups.",
            ))
            .child(
                div().mt_4().flex().justify_end().child(
                    div()
                        .id("new-scene")
                        .px_4()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(ACCENT))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(ACCENT))
                        .cursor_pointer()
                        .hover(|style| style.bg(rgb(SURFACE_2)))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.state.scene_editor = Some(String::new());
                            this.rename_focus.focus(window, cx);
                            cx.notify();
                        }))
                        .child("NEW SCENE"),
                ),
            )
            .child(div().mt_6().grid().grid_cols(3).gap_4().children(
                self.state.scenes.iter().enumerate().map(|(index, scene)| {
                    let selected = index == self.state.selected_scene;
                    div()
                        .id(format!("scene-card-{index}"))
                        .h(px(170.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .rounded_lg()
                        .border_1()
                        .border_color(if selected { rgb(ACCENT) } else { rgb(BORDER) })
                        .bg(if selected {
                            rgb(SURFACE_RAISED)
                        } else {
                            rgb(SURFACE)
                        })
                        .cursor_pointer()
                        .hover(|style| style.border_color(rgb(BORDER_STRONG)).bg(rgb(SURFACE_2)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.apply_scene(index, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    svg()
                                        .path(if selected {
                                            ICON_BOOKMARK_FILLED
                                        } else {
                                            ICON_BOOKMARK
                                        })
                                        .size(px(18.))
                                        .text_color(if selected {
                                            rgb(PRIMARY)
                                        } else {
                                            rgb(TEXT_MUTED)
                                        }),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(TEXT))
                                        .child(scene.name.clone()),
                                )
                                .when(selected, |row| {
                                    row.child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .bg(rgb(SURFACE_RAISED))
                                            .text_size(px(9.))
                                            .text_color(rgb(ACCENT))
                                            .child("ACTIVE"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(if scene.snapshot.is_some() {
                                    rgb(TEXT_MUTED)
                                } else {
                                    rgb(WARNING)
                                })
                                .child(if scene.snapshot.is_some() {
                                    scene.description.clone()
                                } else {
                                    "Empty — click UPDATE to capture the current mixer".to_string()
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id(format!("scene-update-{index}"))
                                        .px_3()
                                        .py_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(rgb(BORDER))
                                        .text_size(px(9.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(TEXT_MUTED))
                                        .cursor_pointer()
                                        .hover(|style| {
                                            style.border_color(rgb(ACCENT)).text_color(rgb(ACCENT))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.state.capture_scene(index);
                                            this.persist_now();
                                            cx.notify();
                                        }))
                                        .child("UPDATE"),
                                )
                                .when(self.state.scenes.len() > 1, |row| {
                                    row.child(
                                        div()
                                            .id(format!("scene-delete-{index}"))
                                            .px_3()
                                            .py_1()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(rgb(BORDER))
                                            .text_size(px(9.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(TEXT_FAINT))
                                            .cursor_pointer()
                                            .hover(|style| {
                                                style
                                                    .border_color(rgb(DANGER))
                                                    .text_color(rgb(DANGER))
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.state.confirm_delete_scene = Some(index);
                                                cx.notify();
                                            }))
                                            .child("DELETE"),
                                    )
                                }),
                        )
                }),
            ))
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-page-scroll")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .child(page_heading(
                "SETTINGS",
                "Audio engine and application settings.",
            ))
            .child(
                div()
                    .mt_6()
                    .max_w(px(900.))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        panel()
                            .child(
                                // Heading with the reset action on the right.
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(TEXT))
                                            .child("AUDIO ENGINE"),
                                    )
                                    .child(
                                        div()
                                            .id("audio-reset")
                                            .px_3()
                                            .py_1()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(rgb(BORDER))
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(TEXT_MUTED))
                                            .cursor_pointer()
                                            .tooltip(crate::ui::channel_strip::text_tooltip(self.reset_hint()))
                                            .hover(|style| style.border_color(rgb(WARNING)).text_color(rgb(WARNING)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.state.reset_audio_settings();
                                                this.send_stream_tuning();
                                                cx.notify();
                                            }))
                                            .child("RESET DEFAULTS"),
                                    ),
                            )
                            .child(
                                setting_row(
                                    "Sample rate",
                                    "Rate requested by route streams, up to 768 kHz; PipeWire converts if a device differs",
                                )
                                .child(self.render_settings_select(
                                    SettingsSelect::SampleRate,
                                    format_sample_rate(self.state.sample_rate),
                                    cx,
                                )),
                            )
                            .child(
                                setting_row(
                                    "Buffer size",
                                    "Latency hint for route streams; detected system quantum is the default",
                                )
                                .child(self.render_settings_select(
                                    SettingsSelect::BufferSize,
                                    format!("{} frames", self.state.buffer_size),
                                    cx,
                                )),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .bg(rgb(SURFACE_RAISED))
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(
                                        "Changing the rate or buffer briefly restarts active connections. Sync delay lives on the mixer: every source and output has a DELAY knob above its fader.",
                                    ),
                            ),
                    )
                    .child(self.render_config_file_panel(cx))
                    .child(
                        panel()
                            .child(panel_heading("ABOUT", env!("CARGO_PKG_VERSION")))
                            .child(
                                div()
                                    .mt_3()
                                    .text_sm()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(
                                        "Orion is a free and open-source audio mixer and routing workspace for Linux, built on GPUI and PipeWire.",
                                    ),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .text_xs()
                                    .text_color(rgb(TEXT_FAINT))
                                    .child("GNU General Public License v3.0"),
                            ),
                    ),
            )
    }
}

impl Drop for RootView {
    fn drop(&mut self) {
        // Persist the session on exit so nothing is lost if the user forgot
        // to save the scene explicitly.
        self.persist_now();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the root focused while no text modal is open so global
        // shortcuts (scene selector on "c") work without clicking first.
        let modal_open = self.state.rename_target.is_some()
            || self.state.channel_editor.is_some()
            || self.state.scene_editor.is_some()
            || self.state.confirm_delete_virtual.is_some()
            || self.state.confirm_delete_channel.is_some()
            || self.state.confirm_delete_scene.is_some();
        if !modal_open
            && self
                .engine_handle
                .as_ref()
                .is_some_and(|_| !self.root_focus.is_focused(window))
        {
            self.root_focus.focus(window, cx);
        }
        // Strips fill the window: compute the height the mixer row can use
        // (viewport minus header, footer, section label, panel padding).
        let viewport_height: f32 = window.viewport_size().height.into();
        let strip_height = (viewport_height - 60.0 - 62.0 - 68.0).max(630.0);
        let content = match self.state.active_view {
            AppView::Mixer => self.render_mixer(strip_height, cx).into_any_element(),
            AppView::Routing => self.render_routing(cx).into_any_element(),
            AppView::Devices => self.render_devices(cx).into_any_element(),
            AppView::Scenes => self.render_scenes(cx).into_any_element(),
            AppView::Settings => self.render_settings(cx).into_any_element(),
        };

        div()
            .id("orion-root")
            .track_focus(&self.root_focus)
            .on_key_down(cx.listener(Self::on_root_key))
            .relative()
            .size_full()
            .min_w(px(1_100.))
            .min_h(px(680.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(rgb(BASE))
            .font_family(FONT_UI)
            .text_color(rgb(TEXT))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if !this.drag_divider(event, window, cx) && !this.drag_knob(event, cx) {
                    this.drag_fader(event, window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if !this.end_divider_drag() && !this.end_knob_drag() {
                        this.end_fader_drag(event, window, cx);
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if !this.end_divider_drag() && !this.end_knob_drag() {
                        this.end_fader_drag(event, window, cx);
                    }
                }),
            )
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .overflow_hidden()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().min_w_0().h_full().child(content)),
            )
            .child(self.render_footer(cx))
            .when(self.state.scene_dropdown_open, |root| {
                root.child(self.render_scene_dropdown(cx))
            })
            .when(self.state.settings_dropdown.is_some(), |root| {
                root.child(self.render_settings_dropdown(cx))
            })
            .when(self.state.endpoint_picker.is_some(), |root| {
                root.child(self.render_endpoint_picker(cx))
            })
            .when(self.state.rename_target.is_some(), |root| {
                root.child(self.render_rename_modal(cx))
            })
            .when(self.state.channel_editor.is_some(), |root| {
                root.child(self.render_channel_editor(cx))
            })
            .when(self.state.confirm_delete_virtual.is_some(), |root| {
                root.child(self.render_delete_virtual_modal(cx))
            })
            .when(self.state.confirm_delete_channel.is_some(), |root| {
                root.child(self.render_delete_channel_modal(cx))
            })
            .when(self.state.confirm_delete_scene.is_some(), |root| {
                root.child(self.render_delete_scene_modal(cx))
            })
            .when(self.state.scene_editor.is_some(), |root| {
                root.child(self.render_scene_modal(cx))
            })
    }
}

fn section_label(title: &'static str, subtitle: &'static str) -> Div {
    div()
        .h(px(36.))
        .flex_shrink_0()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_MUTED))
                .child(title),
        )
        .child(
            div()
                .text_size(px(9.))
                .text_color(rgb(TEXT_FAINT))
                .child(subtitle),
        )
}

fn page_heading(title: &'static str, subtitle: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(subtitle))
}

fn panel() -> Div {
    div()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
}

fn panel_heading(title: &'static str, meta: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child(meta.to_string()),
        )
}

fn empty_state(title: &'static str, description: &'static str) -> Div {
    div()
        .h(px(150.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(BORDER))
        .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(title))
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_FAINT))
                .child(description),
        )
}

fn endpoint_card(device: &AudioEndpoint) -> Div {
    let format = match (device.channel_count, device.sample_rate) {
        (0, Some(rate)) => format!("negotiated channels  /  {rate} Hz"),
        (0, None) => "Format negotiated by PipeWire".into(),
        (channels, Some(rate)) => format!("{channels} ch  /  {rate} Hz"),
        (channels, None) => format!("{channels} ch  /  negotiated rate"),
    };
    let is_virtual = matches!(
        device.endpoint_type,
        EndpointType::VirtualInput | EndpointType::VirtualOutput
    );
    let kind: String = match (device.is_default, is_virtual) {
        (true, false) => "DEFAULT".into(),
        (true, true) => "DEFAULT / VIRTUAL".into(),
        (false, false) => "DEVICE".into(),
        (false, true) => "VIRTUAL".into(),
    };
    let node_name = device
        .identity
        .node_name
        .clone()
        .unwrap_or_else(|| "Not registered yet".into());
    let disconnected = device.state == EndpointState::Disconnected;

    div()
        .p_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(if disconnected {
            rgb(WARNING)
        } else {
            rgb(BORDER)
        })
        .bg(rgb(BASE_RAISED))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(device.name.clone()),
                )
                .child(div().text_xs().text_color(rgb(TEXT_FAINT)).child(node_name))
                .child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(format)),
        )
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(rgb(SURFACE_2))
                .text_size(px(9.))
                .text_color(rgb(ACCENT))
                .child(kind),
        )
}

fn virtual_endpoint_draft(
    virtual_device_id: VirtualDeviceId,
    endpoint_type: EndpointType,
    name: String,
) -> AudioEndpoint {
    AudioEndpoint {
        id: EndpointId::new(),
        runtime_id: None,
        device_id: None,
        virtual_device_id: Some(virtual_device_id),
        identity: EndpointIdentity::new("orion"),
        description: name.clone(),
        name,
        endpoint_type,
        state: EndpointState::Available,
        channel_count: 2,
        sample_rate: None,
        is_default: false,
        channels: vec![ChannelId::new(), ChannelId::new()],
        gain: GainDb::default(),
        muted: false,
        balance: NormalizedBalance::default(),
    }
}

fn role_icon(role: EditorRole) -> &'static str {
    match role {
        EditorRole::Physical => ICON_TYPE_PHYSICAL,
        EditorRole::Application => ICON_TYPE_APP,
        EditorRole::Virtual => ICON_TYPE_VIRTUAL,
    }
}

fn role_description(role: EditorRole, is_source: bool) -> &'static str {
    match (role, is_source) {
        (EditorRole::Physical, true) => "Microphone or hardware input",
        (EditorRole::Physical, false) => "Speakers or hardware output",
        (EditorRole::Application, true) => "Audio from an application",
        (EditorRole::Application, false) => "Not available for outputs",
        (EditorRole::Virtual, true) => "An Orion virtual input",
        (EditorRole::Virtual, false) => "An Orion virtual output",
    }
}

fn editor_nav_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    cx: &mut Context<RootView>,
    on_click: impl Fn(&mut RootView, &mut Context<RootView>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_sm()
        .border_1()
        .border_color(if primary { rgb(ACCENT) } else { rgb(BORDER) })
        .text_xs()
        .text_color(if primary {
            rgb(ACCENT)
        } else {
            rgb(TEXT_MUTED)
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
        .child(label)
}

fn nav_icon(view: AppView, active: bool) -> &'static str {
    match view {
        AppView::Mixer => ICON_MIX,
        AppView::Routing => ICON_ROUTE,
        AppView::Devices => ICON_DEVICE_UNAVAILABLE,
        AppView::Scenes if active => ICON_BOOKMARK_FILLED,
        AppView::Scenes => ICON_BOOKMARK,
        AppView::Settings => ICON_CONFIGURATION,
    }
}

fn setting_row(title: &'static str, description: impl Into<SharedString>) -> Div {
    div()
        .mt_3()
        .py_3()
        .flex()
        .items_start()
        .justify_between()
        .gap_4()
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .w(px(250.))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().text_color(rgb(TEXT)).child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_FAINT))
                        .child(description.into()),
                ),
        )
}

fn param_of(delay_ms: f32, eq: &crate::state::EqBands, param: KnobParam) -> f32 {
    match param {
        KnobParam::Delay => delay_ms,
        KnobParam::EqLow => eq.low_db,
        KnobParam::EqMid => eq.mid_db,
        KnobParam::EqHigh => eq.high_db,
    }
}

fn band_to_eq(param: KnobParam) -> crate::state::EqBand {
    match param {
        KnobParam::EqLow => crate::state::EqBand::Low,
        KnobParam::EqMid => crate::state::EqBand::Mid,
        KnobParam::EqHigh => crate::state::EqBand::High,
        KnobParam::Delay => unreachable!("delay is not an EQ band"),
    }
}
