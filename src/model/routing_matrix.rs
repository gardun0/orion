use std::collections::HashMap;
use gpui::Context;

use crate::device::descriptor::DeviceDescriptor;
use crate::model::channel::ChannelId;
use crate::model::route::Route;
use crate::model::virtual_channel::VirtualIoManager;

pub type MatrixSnapshot = Vec<Route>;

pub struct RoutingMatrix {
    pub inputs: Vec<ChannelId>,
    pub outputs: Vec<ChannelId>,
    routes: HashMap<(usize, usize), Route>,
}

impl RoutingMatrix {
    pub fn from_devices(devices: &[DeviceDescriptor]) -> Self {
        let virtual_io = VirtualIoManager::with_defaults();

        let mut inputs: Vec<ChannelId> = devices
            .iter()
            .filter(|d| d.is_input)
            .map(|d| ChannelId::Physical(d.name.clone()))
            .collect();

        let mut outputs: Vec<ChannelId> = devices
            .iter()
            .filter(|d| d.is_output)
            .map(|d| ChannelId::Physical(d.name.clone()))
            .collect();

        for ch in virtual_io.inputs() {
            inputs.push(ChannelId::Virtual(ch.name.clone()));
        }
        for ch in virtual_io.outputs() {
            outputs.push(ChannelId::Virtual(ch.name.clone()));
        }

        // If no physical devices were found, add a placeholder so the grid is never empty
        if inputs.is_empty() {
            inputs.push(ChannelId::Physical("No Input Device".to_string()));
        }
        if outputs.is_empty() {
            outputs.push(ChannelId::Physical("No Output Device".to_string()));
        }

        Self {
            inputs,
            outputs,
            routes: HashMap::new(),
        }
    }

    pub fn toggle(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if row >= self.inputs.len() || col >= self.outputs.len() {
            return;
        }
        let key = (row, col);
        if self.routes.contains_key(&key) {
            let route = self.routes.get_mut(&key).unwrap();
            route.enabled = !route.enabled;
        } else {
            self.routes.insert(
                key,
                Route::new(self.inputs[row].clone(), self.outputs[col].clone()),
            );
        }
        cx.notify();
    }

    pub fn set_gain(&mut self, row: usize, col: usize, gain_linear: f32, cx: &mut Context<Self>) {
        if let Some(route) = self.routes.get_mut(&(row, col)) {
            route.gain_linear = gain_linear.clamp(0.0, 2.0);
            cx.notify();
        }
    }

    pub fn is_enabled(&self, row: usize, col: usize) -> bool {
        self.routes
            .get(&(row, col))
            .map(|r| r.enabled)
            .unwrap_or(false)
    }

    pub fn gain(&self, row: usize, col: usize) -> f32 {
        self.routes
            .get(&(row, col))
            .map(|r| r.gain_linear)
            .unwrap_or(1.0)
    }

    pub fn add_input(&mut self, id: ChannelId) {
        self.inputs.push(id);
    }

    pub fn add_output(&mut self, id: ChannelId) {
        self.outputs.push(id);
    }

    pub fn snapshot(&self) -> MatrixSnapshot {
        self.routes.values().filter(|r| r.enabled).cloned().collect()
    }
}
