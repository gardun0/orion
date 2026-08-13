//! macOS application audio sources via Core Audio process taps (macOS 14.2+).
//!
//! Discovery: the HAL publishes one process object per app it knows; we keep
//! those actually producing audio and expose each as an `ApplicationOutput`
//! endpoint (the platform-neutral application-source model — the same type
//! PipeWire uses for app streams and Windows uses for loopback sessions).
//!
//! Capture: routing *from* an application endpoint creates a process tap
//! (stereo mixdown of that process), embeds it in a private aggregate
//! device, and reads it with a HAL IOProc that drives a `SourceEngine` —
//! identical to a device capture from the engine's point of view. The first
//! tap triggers the system's "audio capture" permission prompt (TCC); a
//! denial surfaces as a route error naming the fix.

// HAL property selectors keep their Apple names for greppability.
#![allow(non_upper_case_globals)]

use std::ffi::CStr;
use std::ptr::NonNull;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

/// HAL status codes are plain i32 (the crate's OSStatus alias is private).
type OSStatus = i32;

use crate::domain::{
    stable_channel_id, stable_endpoint_id, AudioEndpoint, AudioError, EndpointIdentity,
    EndpointState, EndpointType, ErrorCode, ErrorSeverity,
};
use crate::realtime::{
    ControlHub, PlanSlot, RouteMeter, SourceEngine, SourcePlan, SourcePublisher,
};

const NO_ERR: OSStatus = 0;
const kAudioObjectPropertyScopeGlobal: u32 = 0x676C6F62; // 'glob'
const kAudioObjectPropertyElementMain: u32 = 0x6D61696E; // 'main'
const kAudioHardwarePropertyProcessObjectList: u32 = 0x70727323; // 'prs#'
const kAudioProcessPropertyPID: u32 = 0x70706964; // 'ppid'
const kAudioProcessPropertyIsRunningOutput: u32 = 0x7069726f; // 'piro'
const kAudioTapPropertyUID: u32 = 0x74756964; // 'tuid'

/// Whether process taps exist on this macOS (14.2+). Older systems simply
/// report no application sources.
pub fn taps_available() -> bool {
    objc2::runtime::AnyClass::get(c"CATapDescription").is_some()
}

fn property_address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Read a plain-data HAL property.
fn read_pod<T: Default>(object: AudioObjectID, selector: u32) -> Option<T> {
    let mut address = property_address(selector);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == NO_ERR && size as usize == std::mem::size_of::<T>()).then_some(value)
}

/// Read a variable-length HAL property into a Vec.
fn read_vec<T: Copy + Default>(object: AudioObjectID, selector: u32) -> Vec<T> {
    let mut address = property_address(selector);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&mut address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != NO_ERR || size == 0 {
        return Vec::new();
    }
    let count = size as usize / std::mem::size_of::<T>();
    let mut buffer = vec![T::default(); count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(buffer.as_mut_ptr())
                .expect("sized buffer has a data pointer")
                .cast(),
        )
    };
    if status == NO_ERR {
        buffer
    } else {
        Vec::new()
    }
}

/// Read a CFString-valued HAL property (caller-owned copy per HAL rules).
fn read_cfstring(object: AudioObjectID, selector: u32) -> Option<Retained<CFString>> {
    let mut address = property_address(selector);
    let mut value: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    if status != NO_ERR || value.is_null() {
        return None;
    }
    let retained: Retained<CFString> =
        unsafe { CFRetained::from_raw(NonNull::new_unchecked(value as *mut CFString)).into() };
    Some(retained)
}

/// An application process's display name, from the kernel's process table.
fn process_name(pid: u32) -> Option<String> {
    let mut buffer = [0i8; 256];
    let written = unsafe {
        libc::proc_name(
            pid as i32,
            buffer.as_mut_ptr().cast::<std::ffi::c_void>(),
            buffer.len() as u32,
        )
    };
    if written <= 0 {
        return None;
    }
    let name = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

/// Enumerate applications currently producing audio as routeable source
/// endpoints. Identity keys on the process name so persisted routes
/// reconnect when the same app plays again; the PID is the runtime handle.
pub fn enumerate_application_processes() -> Vec<AudioEndpoint> {
    let mut endpoints = Vec::new();
    if !taps_available() {
        return endpoints;
    }
    let objects = read_vec::<AudioObjectID>(
        1, // kAudioObjectSystemObject
        kAudioHardwarePropertyProcessObjectList,
    );
    let own_pid = std::process::id();
    for object in objects {
        let Some(pid) = read_pod::<u32>(object, kAudioProcessPropertyPID) else {
            continue;
        };
        if pid == own_pid || pid == 0 {
            continue;
        }
        // Only apps actually playing audio right now.
        let running = read_pod::<u32>(object, kAudioProcessPropertyIsRunningOutput).unwrap_or(0);
        if running == 0 {
            continue;
        }
        let display = process_name(pid);
        let name = display.clone().unwrap_or_else(|| format!("Process {pid}"));
        let mut identity = EndpointIdentity::new("coreaudio");
        identity.serial = display;
        identity.device_name = Some(name.clone());
        let endpoint_id = stable_endpoint_id(&identity, EndpointType::ApplicationOutput);
        endpoints.push(AudioEndpoint {
            id: endpoint_id,
            runtime_id: Some(pid),
            device_id: None,
            virtual_device_id: None,
            identity,
            name: name.clone(),
            description: name,
            endpoint_type: EndpointType::ApplicationOutput,
            state: EndpointState::Available,
            channel_count: 2,
            sample_rate: None,
            is_default: false,
            channels: (0..2)
                .map(|index| stable_channel_id(endpoint_id, index))
                .collect(),
            gain: crate::domain::GainDb::default(),
            muted: false,
            balance: crate::domain::NormalizedBalance::default(),
        });
    }
    endpoints
}

/// Callback context handed to the HAL IOProc; reclaimed on drop after the
/// proc is destroyed, so no engine state is freed on an audio thread.
struct TapContext {
    engine: SourceEngine,
    activity: Arc<AtomicU64>,
}

/// The HAL IOProc: forward the tapped block to the engine. Runs on a HAL
/// audio thread — no allocation, no locks, no logging here.
unsafe extern "C-unwind" fn tap_io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input_data: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output_data: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client_data: *mut std::ffi::c_void,
) -> OSStatus {
    if client_data.is_null() {
        return NO_ERR;
    }
    let context = unsafe { &mut *(client_data as *mut TapContext) };
    let list = unsafe { input_data.read() };
    if list.mNumberBuffers == 0 {
        return NO_ERR;
    }
    let buffer = list.mBuffers[0];
    if buffer.mData.is_null() || buffer.mNumberChannels != 2 {
        // The stereo mixdown tap always delivers interleaved stereo f32;
        // anything else is dropped rather than risk frame misalignment.
        return NO_ERR;
    }
    let samples = unsafe {
        std::slice::from_raw_parts(
            buffer.mData as *const f32,
            buffer.mDataByteSize as usize / 4,
        )
    };
    context.engine.process(samples);
    context.activity.fetch_add(1, Ordering::Relaxed);
    NO_ERR
}

/// A running process-tap capture. Owns the tap, the aggregate device, and
/// the IOProc registration; dropping tears them down (backend thread only).
pub struct TapCapture {
    aggregate: AudioObjectID,
    tap: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    context: *mut TapContext,
    activity: Arc<AtomicU64>,
}

impl TapCapture {
    /// Start capturing the application's audio into a fresh SourceEngine.
    /// Tap creation and the first start are where macOS may deny the
    /// "audio capture" permission — errors are mapped to a user-facing
    /// message pointing at System Settings.
    pub fn start(
        endpoint: &AudioEndpoint,
        hub: &Arc<ControlHub>,
    ) -> Result<(Self, SourcePublisher, Arc<RouteMeter>), AudioError> {
        if !taps_available() {
            return Err(tap_error(
                "Capturing an application's audio needs macOS 14.2 or later.",
                "CATapDescription is unavailable on this macOS".to_string(),
            ));
        }
        let pid = endpoint.runtime_id.ok_or_else(|| {
            tap_error(
                "That application is no longer playing audio.",
                format!("application endpoint {} has no process id", endpoint.id),
            )
        })?;
        let process_object = read_vec::<AudioObjectID>(1, kAudioHardwarePropertyProcessObjectList)
            .into_iter()
            .find(|object| read_pod::<u32>(*object, kAudioProcessPropertyPID) == Some(pid))
            .ok_or_else(|| {
                tap_error(
                    "That application is no longer playing audio.",
                    format!("no HAL process object for pid {pid}"),
                )
            })?;

        let channels = 2usize;
        let meter = Arc::new(RouteMeter::new(channels));
        let (engine, handle) = SourceEngine::new(
            channels,
            hub.stream_rate(),
            hub.endpoint_seeded(endpoint),
            hub.channel(endpoint.id),
            meter.clone(),
            Arc::new(PlanSlot::new(SourcePlan {
                generation: 0,
                feeds: Vec::new(),
            })),
        )
        .map_err(|error| {
            tap_error(
                "Orion could not start realtime processing for that connection.",
                format!("failed to initialize application capture DSP: {error}"),
            )
        })?;

        // Tap: stereo mixdown of just this process object.
        let process_ids = NSArray::from_retained_slice(&[NSNumber::new_u32(process_object)]);
        let description = unsafe {
            CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &process_ids)
        };
        let mut tap: AudioObjectID = 0;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) };
        if status != NO_ERR {
            return Err(permission_or_tap_error(
                &endpoint.name,
                status,
                "failed to create process tap",
            ));
        }

        let result = Self::start_on_tap(tap, endpoint, hub, engine, handle, meter);
        if let Err(error) = &result {
            log::warn!(
                "process tap for {} failed to start: {}",
                endpoint.name,
                error.technical_message
            );
            unsafe { AudioHardwareDestroyProcessTap(tap) };
        }
        result
    }

    fn start_on_tap(
        tap: AudioObjectID,
        endpoint: &AudioEndpoint,
        hub: &Arc<ControlHub>,
        engine: SourceEngine,
        handle: crate::realtime::SourceHandle,
        meter: Arc<RouteMeter>,
    ) -> Result<(Self, SourcePublisher, Arc<RouteMeter>), AudioError> {
        // The tap must be wrapped in a private aggregate device to read it
        // with an IOProc. Dictionary keys are content-equal strings for the
        // HAL's constants (CFDictionary compares keys by value).
        let tap_uid = read_cfstring(tap, kAudioTapPropertyUID).ok_or_else(|| {
            tap_error(
                "Orion could not read the tap's identity.",
                format!("tap {tap} has no UID property"),
            )
        })?;
        let tap_uid = NSString::from_str(&tap_uid.to_string());
        let name = NSString::from_str(&format!("Orion Tap {}", endpoint.name));
        let uid = NSString::from_str(&format!("io.github.gardun0.orion.tap.{tap}"));
        let taps: Retained<NSArray<NSString>> = NSArray::from_retained_slice(&[tap_uid]);
        let yes = NSNumber::new_bool(true);
        let no = NSNumber::new_bool(false);
        let (key_name, key_uid, key_taps, key_auto, key_private, key_stacked) = (
            NSString::from_str("name"),
            NSString::from_str("uid"),
            NSString::from_str("taps"),
            NSString::from_str("tapautostart"),
            NSString::from_str("private"),
            NSString::from_str("stacked"),
        );
        let dictionary: Retained<NSDictionary<NSString, objc2::runtime::NSObject>> =
            NSDictionary::from_slices(
                &[
                    &*key_name,
                    &*key_uid,
                    &*key_taps,
                    &*key_auto,
                    &*key_private,
                    &*key_stacked,
                ],
                &[
                    name.as_ref(),
                    uid.as_ref(),
                    taps.as_ref(),
                    yes.as_ref(),
                    yes.as_ref(),
                    no.as_ref(),
                ],
            );
        // Toll-free bridge NSDictionary -> CFDictionary.
        let cf_dictionary: &CFDictionary =
            unsafe { &*(Retained::as_ptr(&dictionary).cast::<CFDictionary>()) };

        let mut aggregate: AudioObjectID = 0;
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(cf_dictionary, NonNull::from(&mut aggregate))
        };
        if status != NO_ERR {
            return Err(permission_or_tap_error(
                &endpoint.name,
                status,
                "failed to create the tap aggregate device",
            ));
        }

        let activity = Arc::new(AtomicU64::new(0));
        let context = Box::into_raw(Box::new(TapContext {
            engine,
            activity: activity.clone(),
        }));
        let mut io_proc: AudioDeviceIOProcID = None;
        let status = unsafe {
            AudioDeviceCreateIOProcID(
                aggregate,
                Some(tap_io_proc),
                context.cast::<std::ffi::c_void>(),
                NonNull::from(&mut io_proc),
            )
        };
        if status != NO_ERR || io_proc.is_none() {
            unsafe {
                drop(Box::from_raw(context));
                AudioHardwareDestroyAggregateDevice(aggregate);
            }
            return Err(tap_error(
                "Orion could not attach to the application's audio.",
                format!("IOProc registration failed on aggregate {aggregate}: {status}"),
            ));
        }

        // The first start of a tap-bearing aggregate is where macOS asks the
        // user for the "audio capture" permission; a denial lands here.
        let status = unsafe { AudioDeviceStart(aggregate, io_proc) };
        if status != NO_ERR {
            unsafe {
                let _ = AudioDeviceDestroyIOProcID(aggregate, io_proc);
                drop(Box::from_raw(context));
                AudioHardwareDestroyAggregateDevice(aggregate);
            }
            return Err(permission_or_tap_error(
                &endpoint.name,
                status,
                "failed to start the tap aggregate device",
            ));
        }

        let _ = hub;
        Ok((
            Self {
                aggregate,
                tap,
                io_proc,
                context,
                activity,
            },
            SourcePublisher::new(handle),
            meter,
        ))
    }

    pub fn activity(&self) -> &Arc<AtomicU64> {
        &self.activity
    }
}

impl Drop for TapCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = AudioDeviceStop(self.aggregate, self.io_proc);
            let _ = AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            // The engine's context outlives the IOProc registration: reclaim
            // it only after the proc is destroyed, on the backend thread.
            drop(Box::from_raw(self.context));
            let _ = AudioHardwareDestroyAggregateDevice(self.aggregate);
            let _ = AudioHardwareDestroyProcessTap(self.tap);
        }
    }
}

fn tap_error(user: impl Into<String>, technical: impl Into<String>) -> AudioError {
    AudioError::new(
        ErrorCode::InvalidRoute,
        ErrorSeverity::Error,
        true,
        user,
        technical,
    )
}

/// Tap start failures are commonly the TCC "audio capture" prompt being
/// denied; those get a message that names the fix.
fn permission_or_tap_error(name: &str, status: OSStatus, operation: &str) -> AudioError {
    let user = format!(
        "Orion could not capture {name}. If macOS asked about audio capture, allow Orion in System Settings → Privacy & Security → Screen & System Audio, then try again."
    );
    tap_error(user, format!("{operation}: OSStatus {status}"))
}
