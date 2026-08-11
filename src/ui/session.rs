//! Session persistence mapping: converts between the live `AppState` and the
//! persisted `SessionDocument` (and back). Pure functions, no GPUI.

use crate::state::{
    AppState, BusKind, ChannelColor, ChannelSnapshot, OutputBus, Scene, SceneSnapshot, SourceKind,
    SourceStrip,
};
use orion::persistence::{
    PersistedChannel, PersistedRoute, PersistedScene, PersistedSettings, PersistedVirtualDevice,
    SessionDocument,
};

pub(crate) fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Physical => "physical",
        SourceKind::Desktop => "desktop",
        SourceKind::Application => "application",
        SourceKind::Virtual => "virtual",
    }
}

pub(crate) fn source_kind_from_label(label: &str) -> SourceKind {
    match label {
        "physical" => SourceKind::Physical,
        "desktop" => SourceKind::Desktop,
        "application" => SourceKind::Application,
        _ => SourceKind::Virtual,
    }
}

pub(crate) fn bus_kind_label(kind: BusKind) -> &'static str {
    match kind {
        BusKind::Physical => "physical",
        BusKind::Virtual => "virtual",
    }
}

pub(crate) fn bus_kind_from_label(label: &str) -> BusKind {
    match label {
        "physical" => BusKind::Physical,
        _ => BusKind::Virtual,
    }
}

pub(crate) fn color_from_value(value: u32) -> ChannelColor {
    ChannelColor::ALL
        .into_iter()
        .find(|color| color.value() == value)
        .unwrap_or(ChannelColor::Cyan)
}

fn persisted_source(source: &SourceStrip) -> PersistedChannel {
    PersistedChannel {
        name: source.name.clone(),
        color: source.color.value(),
        kind: source_kind_label(source.kind).into(),
        gain_db: source.gain_db,
        muted: source.muted,
        delay_ms: source.delay_ms,
        mode: source.mode,
        eq_low_db: source.eq.low_db,
        eq_mid_db: source.eq.mid_db,
        eq_high_db: source.eq.high_db,
        endpoint: source.endpoint_id,
        endpoint_name: source.endpoint_name.clone(),
        code: None,
    }
}

fn persisted_output(output: &OutputBus) -> PersistedChannel {
    PersistedChannel {
        name: output.name.clone(),
        color: output.color.value(),
        kind: bus_kind_label(output.kind).into(),
        gain_db: output.gain_db,
        muted: output.muted,
        delay_ms: output.delay_ms,
        mode: output.mode,
        eq_low_db: output.eq.low_db,
        eq_mid_db: output.eq.mid_db,
        eq_high_db: output.eq.high_db,
        endpoint: output.endpoint_id,
        endpoint_name: output.endpoint_name.clone(),
        code: Some(output.code.clone()),
    }
}

pub(crate) fn document_from_state(state: &AppState) -> SessionDocument {
    SessionDocument::new(
        PersistedSettings {
            sample_rate: state.sample_rate,
            buffer_size: state.buffer_size,
            outputs_panel_width: state.outputs_panel_width,
            master_muted: state.master_muted,
        },
        state.sources.iter().map(persisted_source).collect(),
        state.outputs.iter().map(persisted_output).collect(),
        state
            .devices
            .iter()
            .filter_map(|endpoint| {
                endpoint
                    .virtual_device_id
                    .map(|virtual_device_id| PersistedVirtualDevice {
                        virtual_device_id,
                        endpoint_type: endpoint.endpoint_type,
                        name: endpoint.name.clone(),
                    })
            })
            .collect(),
        state
            .desired_route_pairs()
            .iter()
            .map(|(source, destination)| PersistedRoute {
                source: *source,
                destination: *destination,
            })
            .collect(),
        state
            .scenes
            .iter()
            .map(|scene| PersistedScene {
                name: scene.name.clone(),
                sources: scene
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .sources
                            .iter()
                            .map(|channel| PersistedChannel {
                                name: channel.name.clone(),
                                color: channel.color.value(),
                                kind: String::new(),
                                gain_db: channel.gain_db,
                                muted: channel.muted,
                                delay_ms: channel.delay_ms,
                                mode: channel.mode,
                                eq_low_db: channel.eq.low_db,
                                eq_mid_db: channel.eq.mid_db,
                                eq_high_db: channel.eq.high_db,
                                endpoint: channel.endpoint_id,
                                endpoint_name: None,
                                code: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                outputs: scene
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .outputs
                            .iter()
                            .map(|channel| PersistedChannel {
                                name: channel.name.clone(),
                                color: channel.color.value(),
                                kind: String::new(),
                                gain_db: channel.gain_db,
                                muted: channel.muted,
                                delay_ms: channel.delay_ms,
                                mode: channel.mode,
                                eq_low_db: channel.eq.low_db,
                                eq_mid_db: channel.eq.mid_db,
                                eq_high_db: channel.eq.high_db,
                                endpoint: channel.endpoint_id,
                                endpoint_name: None,
                                code: None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                routes: scene
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        snapshot
                            .routes
                            .iter()
                            .map(|(source, destination)| PersistedRoute {
                                source: *source,
                                destination: *destination,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
        state.selected_scene,
    )
}

/// Rebuild a scene snapshot from its persisted form. Returns None when the
/// scene was never captured (all sections empty), matching how
/// `document_from_state` writes snapshot-less scenes.
fn persisted_scene_snapshot(scene: &PersistedScene) -> Option<SceneSnapshot> {
    if scene.sources.is_empty() && scene.outputs.is_empty() && scene.routes.is_empty() {
        return None;
    }
    let channel = |channel: &PersistedChannel| ChannelSnapshot {
        name: channel.name.clone(),
        color: color_from_value(channel.color),
        gain_db: channel.gain_db,
        muted: channel.muted,
        delay_ms: channel.delay_ms,
        eq: crate::state::EqBands {
            low_db: channel.eq_low_db,
            mid_db: channel.eq_mid_db,
            high_db: channel.eq_high_db,
        },
        mode: channel.mode,
        endpoint_id: channel.endpoint,
    };
    Some(SceneSnapshot {
        sources: scene.sources.iter().map(channel).collect(),
        outputs: scene.outputs.iter().map(channel).collect(),
        routes: scene
            .routes
            .iter()
            .map(|route| (route.source, route.destination))
            .collect(),
    })
}

pub(crate) fn apply_document(
    state: &mut AppState,
    document: &SessionDocument,
    pending_virtuals: &mut Vec<PersistedVirtualDevice>,
) {
    if document.settings.sample_rate > 0 {
        state.sample_rate = document.settings.sample_rate;
        state.audio_overridden = true;
    }
    if document.settings.buffer_size > 0 {
        state.buffer_size = document.settings.buffer_size;
        state.audio_overridden = true;
    }
    state.master_muted = document.settings.master_muted;
    if document.settings.outputs_panel_width > 0.0 {
        state.outputs_panel_width = document
            .settings
            .outputs_panel_width
            .max(crate::state::MIN_OUTPUTS_PANEL_WIDTH);
    }
    if !document.sources.is_empty() {
        state.sources = document
            .sources
            .iter()
            .map(|channel| SourceStrip {
                endpoint_id: channel.endpoint,
                endpoint_name: channel.endpoint_name.clone(),
                name: channel.name.clone(),
                detail: String::new(),
                kind: source_kind_from_label(&channel.kind),
                color: color_from_value(channel.color),
                user_bound: channel.endpoint.is_some(),
                gain_db: channel.gain_db,
                muted: channel.muted,
                delay_ms: channel.delay_ms,
                eq: crate::state::EqBands {
                    low_db: channel.eq_low_db,
                    mid_db: channel.eq_mid_db,
                    high_db: channel.eq_high_db,
                },
                mode: channel.mode,
                routes: vec![false; state.outputs.len()],
                meter_l: 0.0,
                meter_r: 0.0,
                meter_rms_l: 0.0,
                meter_rms_r: 0.0,
                clip_until: None,
                online: false,
            })
            .collect();
    }
    if !document.outputs.is_empty() {
        state.outputs = document
            .outputs
            .iter()
            .enumerate()
            .map(|(index, channel)| OutputBus {
                endpoint_id: channel.endpoint,
                endpoint_name: channel.endpoint_name.clone(),
                code: channel
                    .code
                    .clone()
                    .unwrap_or_else(|| format!("B{}", index + 1)),
                name: channel.name.clone(),
                detail: String::new(),
                kind: bus_kind_from_label(&channel.kind),
                color: color_from_value(channel.color),
                user_bound: channel.endpoint.is_some(),
                gain_db: channel.gain_db,
                muted: channel.muted,
                delay_ms: channel.delay_ms,
                eq: crate::state::EqBands {
                    low_db: channel.eq_low_db,
                    mid_db: channel.eq_mid_db,
                    high_db: channel.eq_high_db,
                },
                mode: channel.mode,
                meter_l: 0.0,
                meter_r: 0.0,
                meter_rms_l: 0.0,
                meter_rms_r: 0.0,
                clip_until: None,
                online: false,
            })
            .collect();
        for source in &mut state.sources {
            source.routes.resize(state.outputs.len(), false);
        }
    }
    if !document.scenes.is_empty() {
        state.scenes = document
            .scenes
            .iter()
            .map(|scene| {
                let snapshot = persisted_scene_snapshot(scene);
                let description = snapshot.as_ref().map_or_else(String::new, |snapshot| {
                    format!(
                        "{} sources · {} outputs · {} routes",
                        snapshot.sources.len(),
                        snapshot.outputs.len(),
                        snapshot.routes.len()
                    )
                });
                Scene {
                    name: scene.name.clone(),
                    description,
                    snapshot,
                }
            })
            .collect();
        state.selected_scene = document.selected_scene.min(state.scenes.len() - 1);
    }
    // Persisted routes become routing intent; the reconciler connects them
    // as their endpoints appear.
    state.set_desired_routes_from_pairs(
        &document
            .routes
            .iter()
            .map(|route| (route.source, route.destination))
            .collect::<Vec<_>>(),
    );
    pending_virtuals.extend(document.virtual_devices.iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use orion::domain::{
        AudioEndpoint, AudioRoute, ChannelId, EndpointId, EndpointIdentity, EndpointState,
        EndpointType, GainDb, NormalizedBalance, RouteId, RouteState, VirtualDeviceId,
    };

    fn virtual_endpoint(endpoint_type: EndpointType, name: &str) -> AudioEndpoint {
        AudioEndpoint {
            id: EndpointId::new(),
            runtime_id: None,
            device_id: None,
            virtual_device_id: Some(VirtualDeviceId::new()),
            identity: EndpointIdentity::new("orion"),
            description: name.to_string(),
            name: name.to_string(),
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

    fn scene_round_trip_state() -> (AppState, EndpointId, EndpointId) {
        let source = virtual_endpoint(EndpointType::VirtualInput, "Stream In");
        let destination = virtual_endpoint(EndpointType::VirtualOutput, "Stream Out");
        let mut state = AppState::new(Vec::new());
        state.set_devices(vec![source.clone(), destination.clone()]);
        // Bind the template strips to our endpoints (as a user binding would),
        // then mark the route intent, as the UI would produce.
        state.sources[0].endpoint_id = Some(source.id);
        state.outputs[0].endpoint_id = Some(destination.id);
        state.toggle_route_intent(0, 0);
        state.upsert_route(AudioRoute {
            id: RouteId::new(),
            source: source.id,
            destination: destination.id,
            state: RouteState::Active,
        });
        (state, source.id, destination.id)
    }

    #[test]
    fn scene_snapshots_survive_document_round_trip() {
        let (mut state, source, destination) = scene_round_trip_state();
        state.set_output_delay(0, 42.5);
        state.capture_scene(0);
        let document = document_from_state(&state);
        assert!(
            !document.scenes[0].routes.is_empty(),
            "scene routes are persisted into the document"
        );

        let mut restored = AppState::new(Vec::new());
        let mut pending_virtuals = Vec::new();
        apply_document(&mut restored, &document, &mut pending_virtuals);

        let snapshot = restored.scenes[0]
            .snapshot
            .as_ref()
            .expect("scene snapshot restored from document");
        assert!(
            snapshot.routes.contains(&(source, destination)),
            "restored scene keeps its routing matrix"
        );
        assert!(!snapshot.sources.is_empty());
        assert!(!snapshot.outputs.is_empty());
        assert!(
            (snapshot.outputs[0].delay_ms - 42.5).abs() < f32::EPSILON,
            "restored scene keeps the per-output delay"
        );
        assert!(
            restored.route_desired(0, 0),
            "top-level routes restore as routing intent"
        );
        assert_eq!(
            restored.desired_route_pairs(),
            vec![(source, destination)],
            "desired matrix matches the persisted route"
        );
    }

    #[test]
    fn output_delay_survives_document_round_trip() {
        let (mut state, _, _) = scene_round_trip_state();
        state.set_source_delay(0, 21.0);
        state.set_output_delay(0, 12.5);
        let document = document_from_state(&state);

        let mut restored = AppState::new(Vec::new());
        let mut pending_virtuals = Vec::new();
        apply_document(&mut restored, &document, &mut pending_virtuals);
        assert!(
            (restored.outputs[0].delay_ms - 12.5).abs() < f32::EPSILON,
            "top-level bus delay restores"
        );
        assert!(
            (restored.sources[0].delay_ms - 21.0).abs() < f32::EPSILON,
            "top-level source delay restores"
        );
    }

    #[test]
    fn uncaptured_scene_restores_without_snapshot() {
        let document = document_from_state(&AppState::new(Vec::new()));
        let mut restored = AppState::new(Vec::new());
        let mut pending_virtuals = Vec::new();
        apply_document(&mut restored, &document, &mut pending_virtuals);
        assert!(restored.scenes[0].snapshot.is_none());
    }
}
