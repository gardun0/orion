use std::thread::{self, JoinHandle};
use crossbeam_channel::{bounded, Receiver, Sender};

use crate::engine::messages::{EngineCommand, EngineEvent};
use crate::model::MatrixSnapshot;

pub struct AudioEngine {
    cmd_tx: Sender<EngineCommand>,
    pub evt_rx: Receiver<EngineEvent>,
    _thread: JoinHandle<()>,
}

impl AudioEngine {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = bounded::<EngineCommand>(64);
        let (evt_tx, evt_rx) = bounded::<EngineEvent>(64);

        let thread = thread::Builder::new()
            .name("audio-engine".to_string())
            .spawn(move || engine_thread(cmd_rx, evt_tx))
            .expect("failed to spawn audio engine thread");

        Self {
            cmd_tx,
            evt_rx,
            _thread: thread,
        }
    }

    pub fn update_matrix(&self, snapshot: MatrixSnapshot) {
        let _ = self.cmd_tx.try_send(EngineCommand::UpdateMatrix(snapshot));
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.try_send(EngineCommand::Shutdown);
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn engine_thread(cmd_rx: Receiver<EngineCommand>, evt_tx: Sender<EngineEvent>) {
    // Phase 1: idle thread — just consume commands until Shutdown
    // Phase 2: build cpal streams, run mix loop here
    let _ = evt_tx.try_send(EngineEvent::Started {
        sample_rate: 48_000,
        buffer_size: 512,
    });

    loop {
        match cmd_rx.recv() {
            Ok(EngineCommand::Shutdown) | Err(_) => break,
            Ok(EngineCommand::UpdateMatrix(_snapshot)) => {
                // Phase 2: swap ArcSwap<MatrixSnapshot> here
            }
        }
    }
}
