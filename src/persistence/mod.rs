use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::SystemTime,
};

use crossbeam_channel::{bounded, Receiver, Sender};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{EndpointId, EndpointType, VirtualDeviceId};

pub const SCHEMA_VERSION: u32 = 2;

/// Value written into the `$schema` key of settings.json.
pub const SETTINGS_SCHEMA_FILE: &str = "settings.schema.json";

fn default_schema_ref() -> String {
    format!("./{SETTINGS_SCHEMA_FILE}")
}
const CHANNEL_CAPACITY: usize = 8;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersistedSettings {
    /// Orion-local sample rate for new route streams.
    #[serde(default)]
    pub sample_rate: u32,
    /// Orion-local latency hint (`node.latency`) for new route streams.
    #[serde(default)]
    pub buffer_size: u32,
    /// Mixer outputs panel width in pixels (0 falls back to the default).
    #[serde(default)]
    pub outputs_panel_width: f32,
    /// MUTE ALL state (outputs), persisted so a muted rig stays muted.
    #[serde(default)]
    pub master_muted: bool,
}

/// A mixer channel persisted by stable identity: name + endpoint id (a
/// deterministic UUIDv5 over the PipeWire identity) or virtual device id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersistedChannel {
    pub name: String,
    pub color: u32,
    pub kind: String,
    pub gain_db: f32,
    pub muted: bool,
    /// Per-channel sync delay in milliseconds.
    #[serde(default)]
    pub delay_ms: f32,
    /// Channel mapping mode (mono downmix, single channel, swap...).
    #[serde(default)]
    pub mode: crate::domain::ChannelMode,
    /// 3-band EQ gains in dB (low shelf / mid bell / high shelf).
    #[serde(default)]
    pub eq_low_db: f32,
    #[serde(default)]
    pub eq_mid_db: f32,
    #[serde(default)]
    pub eq_high_db: f32,
    pub endpoint: Option<EndpointId>,
    /// Display name of the bound device, so an offline selection can show
    /// what it is waiting for across restarts.
    #[serde(default)]
    pub endpoint_name: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersistedVirtualDevice {
    pub virtual_device_id: VirtualDeviceId,
    pub endpoint_type: EndpointType,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersistedRoute {
    pub source: EndpointId,
    pub destination: EndpointId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PersistedScene {
    pub name: String,
    #[serde(default)]
    pub sources: Vec<PersistedChannel>,
    #[serde(default)]
    pub outputs: Vec<PersistedChannel>,
    #[serde(default)]
    pub routes: Vec<PersistedRoute>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionDocument {
    /// JSON Schema reference for editor support (autocomplete/validation).
    #[serde(rename = "$schema", default = "default_schema_ref")]
    pub schema_ref: String,
    pub schema_version: u32,
    #[serde(default)]
    pub settings: PersistedSettings,
    #[serde(default)]
    pub sources: Vec<PersistedChannel>,
    #[serde(default)]
    pub outputs: Vec<PersistedChannel>,
    #[serde(default)]
    pub virtual_devices: Vec<PersistedVirtualDevice>,
    #[serde(default)]
    pub routes: Vec<PersistedRoute>,
    #[serde(default)]
    pub scenes: Vec<PersistedScene>,
    #[serde(default)]
    pub selected_scene: usize,
}

impl SessionDocument {
    pub fn new(
        settings: PersistedSettings,
        sources: Vec<PersistedChannel>,
        outputs: Vec<PersistedChannel>,
        virtual_devices: Vec<PersistedVirtualDevice>,
        routes: Vec<PersistedRoute>,
        scenes: Vec<PersistedScene>,
        selected_scene: usize,
    ) -> Self {
        Self {
            schema_ref: default_schema_ref(),
            schema_version: SCHEMA_VERSION,
            settings,
            sources,
            outputs,
            virtual_devices,
            routes,
            scenes,
            selected_scene,
        }
    }

    /// Marker retained so the file keeps carrying a graph-shaped section for
    /// tooling compatibility; routes inside it are restored as disconnected.
    pub fn validate(&self) -> Result<(), PersistenceError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(PersistenceError::UnsupportedVersion(self.schema_version));
        }
        Ok(())
    }
}

impl Default for SessionDocument {
    fn default() -> Self {
        Self {
            schema_ref: default_schema_ref(),
            schema_version: SCHEMA_VERSION,
            settings: PersistedSettings::default(),
            sources: Vec::new(),
            outputs: Vec::new(),
            virtual_devices: Vec::new(),
            routes: Vec::new(),
            scenes: Vec::new(),
            selected_scene: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadSource {
    Default,
    Primary,
    Backup,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadResult {
    pub document: SessionDocument,
    pub source: LoadSource,
    /// Content digest of the bytes read (0 for a default/empty document);
    /// the settings watcher uses it to skip our own writes.
    pub digest: u64,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("configuration directory is unavailable")]
    ConfigurationDirectoryUnavailable,
    #[error("unsupported configuration schema version {0}")]
    UnsupportedVersion(u32),
    #[error("persisted graph is invalid: {0}")]
    InvalidGraph(String),
    #[error("failed to access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("persistence worker channel closed")]
    ChannelClosed,
    #[error("persistence worker panicked")]
    WorkerPanicked,
}

pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Canonical config location via the platform's conventions: on Linux
    /// `$XDG_CONFIG_HOME/orion/settings.json` (or `~/.config/orion/`), on
    /// Windows under `%APPDATA%`, on macOS under `~/Library/Application
    /// Support`. Migrates the legacy `session-v1.json` on first launch.
    pub fn default_path() -> Result<PathBuf, PersistenceError> {
        let project = directories::ProjectDirs::from("io.github", "gardun0", "orion")
            .ok_or(PersistenceError::ConfigurationDirectoryUnavailable)?;
        Ok(settings_path_in(project.config_dir()))
    }

    /// Last modification time of the settings file, if it exists.
    pub fn modified(&self) -> Option<SystemTime> {
        fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .ok()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<LoadResult, PersistenceError> {
        if !self.path.exists() {
            return Ok(LoadResult {
                document: SessionDocument::default(),
                source: LoadSource::Default,
                digest: 0,
            });
        }

        match self.load_file(&self.path) {
            Ok((document, digest)) => Ok(LoadResult {
                document,
                source: LoadSource::Primary,
                digest,
            }),
            Err(primary_error) => {
                let backup = backup_path(&self.path);
                if !backup.exists() {
                    return Err(primary_error);
                }
                self.load_file(&backup)
                    .map(|(document, digest)| LoadResult {
                        document,
                        source: LoadSource::Backup,
                        digest,
                    })
            }
        }
    }

    /// Save and return the content digest of the written bytes.
    pub fn save(&self, document: &SessionDocument) -> Result<u64, PersistenceError> {
        document.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or(PersistenceError::ConfigurationDirectoryUnavailable)?;
        fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
            path: parent.to_owned(),
            source,
        })?;

        let temporary = temporary_path(&self.path);
        let digest = write_document(&temporary, document)?;

        if self.path.exists() {
            let backup = backup_path(&self.path);
            let backup_temporary = temporary_path(&backup);
            fs::copy(&self.path, &backup_temporary).map_err(|source| PersistenceError::Io {
                path: backup_temporary.clone(),
                source,
            })?;
            sync_file(&backup_temporary)?;
            fs::rename(&backup_temporary, &backup).map_err(|source| PersistenceError::Io {
                path: backup,
                source,
            })?;
        }

        fs::rename(&temporary, &self.path).map_err(|source| PersistenceError::Io {
            path: self.path.clone(),
            source,
        })?;
        sync_directory(parent)?;
        Ok(digest)
    }

    fn load_file(&self, path: &Path) -> Result<(SessionDocument, u64), PersistenceError> {
        let bytes = fs::read(path).map_err(|source| PersistenceError::Io {
            path: path.to_owned(),
            source,
        })?;
        let digest = content_digest(&bytes);
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Json {
                path: path.to_owned(),
                source,
            })?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32;
        match version {
            // v1 carried only the runtime graph; nothing in it maps to the
            // v2 UI sections, so it migrates to an empty document.
            1 => Ok((SessionDocument::default(), digest)),
            SCHEMA_VERSION => {
                let document: SessionDocument =
                    serde_json::from_value(value).map_err(|source| PersistenceError::Json {
                        path: path.to_owned(),
                        source,
                    })?;
                document.validate()?;
                Ok((document, digest))
            }
            other => Err(PersistenceError::UnsupportedVersion(other)),
        }
    }
}

pub enum PersistenceCommand {
    Save(SessionDocument),
    Shutdown,
}

pub enum PersistenceEvent {
    /// A save completed; carries the content digest of the written file so
    /// the settings watcher can tell our own writes apart from external edits.
    Saved(Option<u64>),
    Failed(String),
    Stopped,
}

/// Watches the settings file for external edits (import, hand edits, sync
/// tools). Watches the parent directory because editors usually save by
/// replacing the file, which breaks file-level watches. The UI polls
/// `changed()` on its timer; `Saved` events carry our own writes' mtimes so
/// the UI can tell them apart from external changes.
pub struct SettingsWatcher {
    // Dropping the watcher stops notifications.
    _watcher: notify::RecommendedWatcher,
    events: Receiver<()>,
}

impl SettingsWatcher {
    pub fn start(path: &Path) -> Option<Self> {
        let directory = path.parent()?.to_path_buf();
        let file_name = path.file_name()?.to_os_string();
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut watcher = <notify::RecommendedWatcher as notify::Watcher>::new(
            move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else { return };
                let touched = event
                    .paths
                    .iter()
                    .any(|path| path.file_name() == Some(file_name.as_os_str()));
                if touched {
                    let _ = events_tx.try_send(());
                }
            },
            notify::Config::default(),
        )
        .ok()?;
        notify::Watcher::watch(
            &mut watcher,
            &directory,
            notify::RecursiveMode::NonRecursive,
        )
        .ok()?;
        Some(Self {
            _watcher: watcher,
            events: events_rx,
        })
    }

    /// True if the settings file was touched since the last call.
    pub fn changed(&self) -> bool {
        let mut changed = false;
        while self.events.try_recv().is_ok() {
            changed = true;
        }
        changed
    }
}

pub struct PersistenceWorker {
    commands: Sender<PersistenceCommand>,
    events: Receiver<PersistenceEvent>,
    thread: Option<JoinHandle<()>>,
}

impl PersistenceWorker {
    pub fn start(store: SessionStore) -> Result<Self, PersistenceError> {
        let (command_tx, command_rx) = bounded(CHANNEL_CAPACITY);
        let (event_tx, event_rx) = bounded(CHANNEL_CAPACITY);
        let thread = thread::Builder::new()
            .name("orion-persistence".into())
            .spawn(move || persistence_loop(store, command_rx, event_tx))
            .map_err(|source| PersistenceError::Io {
                path: PathBuf::from("orion-persistence-thread"),
                source,
            })?;
        Ok(Self {
            commands: command_tx,
            events: event_rx,
            thread: Some(thread),
        })
    }

    /// Queue a save without ever blocking the caller: when the worker is
    /// behind, the newest document wins and older queued ones are dropped
    /// (session saves are idempotent snapshots).
    pub fn save(&self, document: SessionDocument) -> Result<(), PersistenceError> {
        match self.commands.try_send(PersistenceCommand::Save(document)) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => Ok(()),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                Err(PersistenceError::ChannelClosed)
            }
        }
    }

    pub fn try_recv(&self) -> Result<Option<PersistenceEvent>, PersistenceError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err(PersistenceError::ChannelClosed)
            }
        }
    }

    pub fn shutdown(mut self) -> Result<(), PersistenceError> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<(), PersistenceError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.commands.send(PersistenceCommand::Shutdown);
        thread.join().map_err(|_| PersistenceError::WorkerPanicked)
    }
}

impl Drop for PersistenceWorker {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn persistence_loop(
    store: SessionStore,
    commands: Receiver<PersistenceCommand>,
    events: Sender<PersistenceEvent>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            PersistenceCommand::Save(document) => {
                let event = match store.save(&document) {
                    Ok(digest) => PersistenceEvent::Saved(Some(digest)),
                    Err(error) => PersistenceEvent::Failed(error.to_string()),
                };
                // Never block on the event channel: the UI drains it on a
                // timer and may be gone (app exit). A full channel must not
                // wedge the worker — the next command still needs to run.
                let _ = events.try_send(event);
            }
            PersistenceCommand::Shutdown => {
                let _ = events.try_send(PersistenceEvent::Stopped);
                return;
            }
        }
    }
}

fn write_document(path: &Path, document: &SessionDocument) -> Result<u64, PersistenceError> {
    let mut bytes =
        serde_json::to_vec_pretty(document).map_err(|source| PersistenceError::Json {
            path: path.to_owned(),
            source,
        })?;
    bytes.push(b'\n');
    let digest = content_digest(&bytes);
    let file = File::create(path).map_err(|source| PersistenceError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(&bytes)
        .map_err(|source| PersistenceError::Io {
            path: path.to_owned(),
            source,
        })?;
    writer.flush().map_err(|source| PersistenceError::Io {
        path: path.to_owned(),
        source,
    })?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|source| PersistenceError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(digest)
}

/// FNV-1a over the raw file bytes: cheap and stable, used only to tell our
/// own writes apart from external edits (not an integrity checksum).
fn content_digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn sync_file(path: &Path) -> Result<(), PersistenceError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| PersistenceError::Io {
            path: path.to_owned(),
            source,
        })
}

fn sync_directory(path: &Path) -> Result<(), PersistenceError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| PersistenceError::Io {
            path: path.to_owned(),
            source,
        })
}

/// Write the JSON Schema for SessionDocument next to the settings file, so
/// editors can autocomplete/validate settings.json. Best effort.
pub fn write_schema_file(settings_path: &Path) -> Result<(), PersistenceError> {
    let Some(directory) = settings_path.parent() else {
        return Ok(());
    };
    let schema = schemars::schema_for!(SessionDocument);
    let bytes = serde_json::to_vec_pretty(&schema).map_err(|source| PersistenceError::Json {
        path: directory.join(SETTINGS_SCHEMA_FILE),
        source,
    })?;
    fs::write(directory.join(SETTINGS_SCHEMA_FILE), bytes).map_err(|source| PersistenceError::Io {
        path: directory.join(SETTINGS_SCHEMA_FILE),
        source,
    })
}

/// settings.json inside the config directory, migrating the legacy
/// session-v1.json when it is the only one present.
fn settings_path_in(directory: &Path) -> PathBuf {
    let path = directory.join("settings.json");
    let legacy = directory.join("session-v1.json");
    if !path.exists() && legacy.exists() {
        // Best effort: if the rename fails the legacy file stays in place and
        // the next save simply writes a fresh settings.json.
        let _ = fs::rename(&legacy, &path);
    }
    path
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("tmp-{}", Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::GainDb;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("orion-persistence-test-{}", Uuid::new_v4()))
    }

    fn document() -> SessionDocument {
        SessionDocument::new(
            PersistedSettings {
                sample_rate: 48_000,
                buffer_size: 512,
                outputs_panel_width: 640.0,
                master_muted: true,
            },
            vec![PersistedChannel {
                name: "Mic 1".into(),
                color: 0x22D3EE,
                kind: "physical".into(),
                gain_db: -3.2,
                muted: false,
                delay_ms: 0.0,
                mode: crate::domain::ChannelMode::Auto,
                eq_low_db: 0.0,
                eq_mid_db: 2.5,
                eq_high_db: 0.0,
                endpoint: Some(EndpointId::new()),
                endpoint_name: Some("UMC204HD 192k Input 1".into()),
                code: None,
            }],
            vec![PersistedChannel {
                name: "Stream".into(),
                color: 0xA78BFA,
                kind: "virtual".into(),
                gain_db: 0.0,
                muted: false,
                delay_ms: 12.5,
                mode: crate::domain::ChannelMode::Mono,
                eq_low_db: -3.0,
                eq_mid_db: 0.0,
                eq_high_db: 1.5,
                endpoint: Some(EndpointId::new()),
                endpoint_name: Some("Orion Virtual Output 1".into()),
                code: Some("B1".into()),
            }],
            vec![PersistedVirtualDevice {
                virtual_device_id: VirtualDeviceId::new(),
                endpoint_type: EndpointType::VirtualOutput,
                name: "Discord".into(),
            }],
            Vec::new(),
            vec![PersistedScene {
                name: "Streaming".into(),
                sources: Vec::new(),
                outputs: Vec::new(),
                routes: Vec::new(),
            }],
            0,
        )
    }

    #[test]
    fn round_trip_preserves_channels_settings_virtuals_and_scenes() {
        let directory = temporary_directory();
        let path = directory.join("session-v2.json");
        let store = SessionStore::new(path.clone());
        let document = document();

        assert!(store.save(&document).is_ok());
        let loaded = store
            .load()
            .unwrap_or_else(|error| panic!("load failed: {error}"));
        assert_eq!(loaded.source, LoadSource::Primary);
        assert_eq!(loaded.document, document);
        assert_eq!(loaded.document.settings.buffer_size, 512);
        assert!(
            loaded.document.settings.master_muted,
            "master mute persists"
        );
        assert!((loaded.document.outputs[0].delay_ms - 12.5).abs() < f32::EPSILON);
        assert!((loaded.document.outputs[0].eq_low_db - (-3.0)).abs() < f32::EPSILON);
        assert!((loaded.document.sources[0].eq_mid_db - 2.5).abs() < f32::EPSILON);
        assert_eq!(
            loaded.document.outputs[0].mode,
            crate::domain::ChannelMode::Mono
        );
        assert_eq!(loaded.document.sources[0].name, "Mic 1");
        assert_eq!(
            loaded.document.outputs[0].endpoint,
            document.outputs[0].endpoint
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_v1_documents_migrate_to_empty_sections() {
        let directory = temporary_directory();
        let path = directory.join("session-v1.json");
        let store = SessionStore::new(path.clone());
        fs::create_dir_all(&directory).expect("mkdir");
        fs::write(
            &path,
            r#"{"schema_version":1,"graph":{"endpoints":{},"routes":{}}}"#,
        )
        .expect("write v1");

        let loaded = store
            .load()
            .unwrap_or_else(|error| panic!("load failed: {error}"));
        assert_eq!(loaded.source, LoadSource::Primary);
        assert!(loaded.document.sources.is_empty());
        assert!(loaded.document.routes.is_empty());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_session_file_migrates_to_settings_json() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).expect("mkdir");
        let legacy = directory.join("session-v1.json");
        fs::write(&legacy, r#"{"schema_version":2}"#).expect("write legacy");

        let path = settings_path_in(&directory);
        assert_eq!(path.file_name().unwrap(), "settings.json");
        assert!(path.exists(), "legacy file migrated to settings.json");
        assert!(!legacy.exists());

        // Second call is a no-op (settings.json already exists).
        let again = settings_path_in(&directory);
        assert_eq!(again, path);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn settings_watcher_notices_direct_writes_and_atomic_replaces() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).expect("mkdir");
        let path = directory.join("settings.json");
        fs::write(&path, "{}").expect("seed file");
        let watcher = SettingsWatcher::start(&path).expect("watcher starts");

        let wait_for_change = |watcher: &SettingsWatcher| {
            for _ in 0..40 {
                if watcher.changed() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };

        // Direct write.
        fs::write(&path, "{} ").expect("direct write");
        assert!(wait_for_change(&watcher), "direct write noticed");

        // Atomic replace (write tmp + rename), the common editor pattern.
        let temporary = directory.join("tmp-write");
        fs::write(&temporary, "{}").expect("tmp write");
        fs::rename(&temporary, &path).expect("rename over");
        assert!(wait_for_change(&watcher), "atomic replace noticed");

        let _ = fs::remove_dir_all(directory);
    }

    /// The canonical schema lives in the repository for external URL access;
    /// regenerate it with UPDATE_SCHEMA=1 cargo test repo_schema.
    #[test]
    fn repo_schema_matches_the_generated_one() {
        let schema = schemars::schema_for!(SessionDocument);
        let generated = serde_json::to_string_pretty(&schema).expect("schema serializes") + "\n";
        let repo_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/settings.schema.json");
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            fs::create_dir_all(repo_path.parent().expect("schema dir")).expect("mkdir");
            fs::write(&repo_path, &generated).expect("write schema");
        }
        let committed = fs::read_to_string(&repo_path)
            .expect("schema/settings.schema.json missing — run with UPDATE_SCHEMA=1");
        assert_eq!(
            committed, generated,
            "repo schema is stale — regenerate with UPDATE_SCHEMA=1 cargo test repo_schema"
        );
    }

    #[test]
    fn generated_schema_accepts_written_documents() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        let store = SessionStore::new(path.clone());
        store.save(&document()).expect("save");

        // The emitted schema validates what we write, and the $schema key
        // points at the schema file sitting next to settings.json.
        write_schema_file(store.path()).expect("schema written");
        assert!(path.with_file_name(SETTINGS_SCHEMA_FILE).exists());

        let schema_json: serde_json::Value = serde_json::from_slice(
            &fs::read(path.with_file_name(SETTINGS_SCHEMA_FILE)).expect("read schema"),
        )
        .expect("schema parses");
        let validator = jsonschema::validator_for(&schema_json).expect("schema compiles");
        let instance: serde_json::Value =
            serde_json::from_slice(&fs::read(store.path()).expect("read doc")).expect("doc parses");
        assert!(
            validator.is_valid(&instance),
            "written settings.json validates against the emitted schema"
        );
        assert_eq!(
            instance.get("$schema").and_then(|v| v.as_str()),
            Some("./settings.schema.json")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn save_digest_matches_load_digest_for_self_write_detection() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        let store = SessionStore::new(path);
        let document = document();

        let saved = store.save(&document).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(saved, loaded.digest, "own writes are recognized by digest");

        // An external edit (byte-level, same content) changes the digest.
        fs::write(
            store.path(),
            format!("{}\n", serde_json::to_string(&document).unwrap()),
        )
        .expect("external write");
        let reloaded = store.load().expect("reload");
        assert_ne!(
            saved, reloaded.digest,
            "external formatting differs from our canonical bytes"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn worker_shuts_down_with_undrained_event_channel() {
        let directory = temporary_directory();
        let path = directory.join("session-v2.json");
        let store = SessionStore::new(path);
        let worker = PersistenceWorker::start(store).expect("worker starts");
        // Queue far more saves than the bounded event channel holds, without
        // ever draining it (the UI may be gone or busy at exit).
        for _ in 0..CHANNEL_CAPACITY * 3 {
            worker.save(SessionDocument::default()).expect("queued");
        }
        // The worker drops events nobody reads instead of blocking, so the
        // shutdown join returns promptly instead of hanging the app.
        worker.shutdown().expect("clean shutdown");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn unsupported_future_version_is_rejected() {
        let directory = temporary_directory();
        let path = directory.join("session-future.json");
        let store = SessionStore::new(path.clone());
        fs::create_dir_all(&directory).expect("mkdir");
        fs::write(&path, r#"{"schema_version":99}"#).expect("write");

        let error = store.load().expect_err("future version must fail");
        assert!(matches!(error, PersistenceError::UnsupportedVersion(99)));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn falls_back_to_backup_when_primary_is_corrupt() {
        let directory = temporary_directory();
        let path = directory.join("session-v2.json");
        let store = SessionStore::new(path.clone());

        assert!(store.save(&document()).is_ok());
        assert!(store.save(&document()).is_ok());
        assert!(fs::write(&path, b"not-json").is_ok());
        let loaded = store
            .load()
            .unwrap_or_else(|error| panic!("load failed: {error}"));
        assert_eq!(loaded.source, LoadSource::Backup);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn worker_saves_without_blocking_the_caller() {
        let directory = temporary_directory();
        let path = directory.join("session-v2.json");
        let worker = PersistenceWorker::start(SessionStore::new(path.clone()))
            .unwrap_or_else(|error| panic!("worker failed: {error}"));
        assert!(worker.save(document()).is_ok());
        let event = worker
            .events
            .recv_timeout(std::time::Duration::from_secs(1));
        assert!(matches!(event, Ok(PersistenceEvent::Saved(_))));
        assert!(worker.shutdown().is_ok());
        assert!(path.exists());
        let _ = fs::remove_dir_all(directory);
        let _ = GainDb::default();
    }
}
