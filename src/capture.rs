//! macOS: capture the web player's audio output with a Core Audio process tap
//! (macOS 14.2+), so the visualizer can analyse DRM-protected playback after
//! the system has decoded it. Only irie's own WebKit audio process is tapped.

use anyhow::{Result, bail};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::*;
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::{CFDictionary, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const WEBKIT_GPU: &str = "com.apple.WebKit.GPU";

unsafe extern "C" {
    // libSystem; returns the pid macOS attributes a process to (XPC services
    // are attributed to the app that uses them).
    fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

fn addr(sel: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: sel,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

unsafe fn get<T: Copy>(obj: AudioObjectID, sel: u32, default: T) -> Option<T> {
    let a = addr(sel);
    let mut v = default;
    let mut size = std::mem::size_of::<T>() as u32;
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut v).cast(),
        )
    };
    (st == 0).then_some(v)
}

unsafe fn get_string(obj: AudioObjectID, sel: u32) -> Option<String> {
    let p = unsafe { get::<*const CFString>(obj, sel, std::ptr::null())? };
    (!p.is_null()).then(|| unsafe { &*p }.to_string())
}

/// Core Audio process objects.
fn processes() -> Vec<AudioObjectID> {
    let sys = kAudioObjectSystemObject as AudioObjectID;
    let a = addr(kAudioHardwarePropertyProcessObjectList);
    let mut size = 0u32;
    unsafe {
        if AudioObjectGetPropertyDataSize(
            sys,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        ) != 0
        {
            return vec![];
        }
        let mut ids = vec![0u32; size as usize / 4];
        let Some(ptr) = NonNull::new(ids.as_mut_ptr()) else {
            return vec![];
        };
        if AudioObjectGetPropertyData(
            sys,
            NonNull::from(&a),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            ptr.cast(),
        ) != 0
        {
            return vec![];
        }
        ids.truncate(size as usize / 4);
        ids
    }
}

/// Our own WebKit audio process: `(audio object, is producing output)`.
pub fn find_player_audio() -> Option<(AudioObjectID, bool)> {
    let me = std::process::id() as i32;
    processes().into_iter().find_map(|id| unsafe {
        let bundle = get_string(id, kAudioProcessPropertyBundleID)?;
        if bundle != WEBKIT_GPU {
            return None;
        }
        let pid = get::<i32>(id, kAudioProcessPropertyPID, -1)?;
        if responsibility_get_pid_responsible_for_pid(pid) != me {
            return None;
        }
        let running = get::<u32>(id, kAudioProcessPropertyIsRunningOutput, 0).unwrap_or(0) == 1;
        Some((id, running))
    })
}

/// Whether `obj` is still producing output.
pub fn is_running(obj: AudioObjectID) -> bool {
    unsafe { get::<u32>(obj, kAudioProcessPropertyIsRunningOutput, 0) == Some(1) }
}

type Sink = Box<dyn Fn(&[f32]) + Send + Sync>;

struct Shared {
    sink: Sink,
    /// Bits of the loudest absolute sample since the last check.
    peak: AtomicU64,
    got_frames: AtomicBool,
}

pub struct Capture {
    tap: AudioObjectID,
    agg: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    shared: *const Shared,
    pub rate: u32,
}

// The raw pointer is only touched by the IOProc and freed in Drop after the
// IOProc is stopped.
unsafe impl Send for Capture {}

unsafe extern "C-unwind" fn ioproc(
    _dev: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _in_time: NonNull<AudioTimeStamp>,
    _out: NonNull<AudioBufferList>,
    _out_time: NonNull<AudioTimeStamp>,
    ctx: *mut c_void,
) -> i32 {
    let shared = unsafe { &*(ctx as *const Shared) };
    let list = unsafe { input.as_ref() };
    let bufs =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) };
    let Some(b) = bufs.first().filter(|b| !b.mData.is_null()) else {
        return 0;
    };
    let samples =
        unsafe { std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4) };
    let ch = b.mNumberChannels.max(1) as usize;
    let mut peak = 0f32;
    let mono: Vec<f32> = samples
        .chunks_exact(ch)
        .map(|f| {
            let m = f.iter().sum::<f32>() / ch as f32;
            peak = peak.max(m.abs());
            m
        })
        .collect();
    shared.got_frames.store(true, Ordering::Relaxed);
    let prev = f64::from_bits(shared.peak.load(Ordering::Relaxed)) as f32;
    if peak > prev {
        shared
            .peak
            .store((peak as f64).to_bits(), Ordering::Relaxed);
    }
    (shared.sink)(&mono);
    0
}

fn obj<T: objc2::Message>(r: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an AnyObject.
    unsafe { Retained::cast_unchecked(r) }
}

impl Capture {
    /// Tap `process` (a Core Audio process object) and feed mono samples to `sink`.
    pub fn start(
        process: AudioObjectID,
        sink: impl Fn(&[f32]) + Send + Sync + 'static,
    ) -> Result<Self> {
        unsafe {
            let procs = NSArray::from_retained_slice(&[NSNumber::new_u32(process)]);
            let desc =
                CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &procs);
            desc.setPrivate(true);
            desc.setMuteBehavior(CATapMuteBehavior::Unmuted);
            desc.setName(&NSString::from_str("irie visualizer"));
            let mut tap: AudioObjectID = 0;
            let st = AudioHardwareCreateProcessTap(Some(&desc), &mut tap);
            if st != 0 {
                bail!("could not create audio tap (status {st})");
            }
            let fail = |msg: String| -> anyhow::Error {
                AudioHardwareDestroyProcessTap(tap);
                anyhow::anyhow!(msg)
            };
            let tap_uid = get_string(tap, kAudioTapPropertyUID)
                .ok_or_else(|| fail("tap has no UID".into()))?;
            let fmt = get::<AudioStreamBasicDescription>(
                tap,
                kAudioTapPropertyFormat,
                std::mem::zeroed(),
            )
            .ok_or_else(|| fail("tap has no format".into()))?;

            let sys = kAudioObjectSystemObject as AudioObjectID;
            let out_dev =
                get::<u32>(sys, kAudioHardwarePropertyDefaultSystemOutputDevice, 0).unwrap_or(0);
            let out_uid = get_string(out_dev, kAudioDevicePropertyDeviceUID).unwrap_or_default();

            let s = |v: &str| obj(NSString::from_str(v));
            let b = |v: bool| obj(NSNumber::new_bool(v));
            let k = |v: &str| NSString::from_str(v);
            let sub = NSDictionary::from_retained_objects(&[&*k("uid")], &[s(&out_uid)]);
            let tapd = NSDictionary::from_retained_objects(
                &[&*k("uid"), &*k("drift")],
                &[s(&tap_uid), b(true)],
            );
            let keys = [
                &*k("uid"),
                &*k("name"),
                &*k("master"),
                &*k("private"),
                &*k("stacked"),
                &*k("tapautostart"),
                &*k("subdevices"),
                &*k("taps"),
            ];
            let vals = [
                obj(NSUUID::new().UUIDString()),
                s("irie visualizer"),
                s(&out_uid),
                b(true),
                b(false),
                b(true),
                obj(NSArray::from_retained_slice(&[sub])),
                obj(NSArray::from_retained_slice(&[tapd])),
            ];
            let dict = NSDictionary::from_retained_objects(&keys, &vals);
            // NSDictionary is toll-free bridged to CFDictionary.
            let cf: &CFDictionary = &*(Retained::as_ptr(&dict) as *const CFDictionary);
            let mut agg: AudioObjectID = 0;
            let st = AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut agg));
            if st != 0 {
                return Err(fail(format!(
                    "could not create capture device (status {st})"
                )));
            }

            let shared = Box::into_raw(Box::new(Shared {
                sink: Box::new(sink),
                peak: AtomicU64::new(0f64.to_bits()),
                got_frames: AtomicBool::new(false),
            }));
            let mut proc_id: AudioDeviceIOProcID = None;
            let st = AudioDeviceCreateIOProcID(
                agg,
                Some(ioproc),
                shared as *mut c_void,
                NonNull::from(&mut proc_id),
            );
            if st != 0 || AudioDeviceStart(agg, proc_id) != 0 {
                AudioHardwareDestroyAggregateDevice(agg);
                drop(Box::from_raw(shared));
                return Err(fail(format!("could not start capture (status {st})")));
            }
            Ok(Self {
                tap,
                agg,
                proc_id,
                shared,
                rate: fmt.mSampleRate as u32,
            })
        }
    }

    /// Loudest sample since the last call, and whether any audio arrived.
    pub fn take_peak(&self) -> (f32, bool) {
        let s = unsafe { &*self.shared };
        let peak = f64::from_bits(s.peak.swap(0f64.to_bits(), Ordering::Relaxed)) as f32;
        (peak, s.got_frames.swap(false, Ordering::Relaxed))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.agg, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.agg);
            AudioHardwareDestroyProcessTap(self.tap);
            drop(Box::from_raw(self.shared as *mut Shared));
        }
    }
}

/// Run forever in the player host: keep a tap on our WebKit audio process,
/// write mono f32 samples to `out`, and report status with `status`.
pub fn run(
    out: impl std::io::Write + Send + 'static,
    status: impl Fn(serde_json::Value) + Send + 'static,
) {
    let out = Arc::new(std::sync::Mutex::new(out));
    let mut capture: Option<(AudioObjectID, Capture)> = None;
    let mut silent_secs = 0u32;
    let mut reported: Option<&str> = None;
    let report = |state: &'static str, extra: serde_json::Value, reported: &mut Option<&str>| {
        if *reported != Some(state) {
            *reported = Some(state);
            let mut v = serde_json::json!({ "type": "capture", "status": state });
            if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
                m.extend(e.clone());
            }
            status(v);
        }
    };
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if capture.is_none() {
            if std::env::var_os("IRIE_PLAYER_DEBUG").is_some() {
                let me = std::process::id() as i32;
                let found: Vec<String> = processes()
                    .into_iter()
                    .filter_map(|id| unsafe {
                        let b = get_string(id, kAudioProcessPropertyBundleID).unwrap_or_default();
                        let pid = get::<i32>(id, kAudioProcessPropertyPID, -1).unwrap_or(-1);
                        let resp = responsibility_get_pid_responsible_for_pid(pid);
                        let run =
                            get::<u32>(id, kAudioProcessPropertyIsRunningOutput, 0).unwrap_or(9);
                        (b.contains("WebKit") || b.contains("irie"))
                            .then(|| format!("{b} pid={pid} resp={resp} run={run}"))
                    })
                    .collect();
                status(
                    serde_json::json!({ "type": "diag", "ev": "capture_scan", "me": me, "found": found }),
                );
            }
            let Some((proc_obj, running)) = find_player_audio() else {
                continue;
            };
            if !running {
                continue;
            }
            // The first tap triggers macOS's permission prompt and blocks until it's answered.
            report("requesting", serde_json::Value::Null, &mut reported);
            let o = out.clone();
            match Capture::start(proc_obj, move |mono| {
                let mut w = o.lock().unwrap();
                let bytes: Vec<u8> = mono.iter().flat_map(|s| s.to_le_bytes()).collect();
                let _ = w.write_all(&bytes);
                let _ = w.flush();
            }) {
                Ok(c) => {
                    report(
                        "started",
                        serde_json::json!({ "rate": c.rate }),
                        &mut reported,
                    );
                    capture = Some((proc_obj, c));
                }
                Err(e) => {
                    report(
                        "error",
                        serde_json::json!({ "message": e.to_string() }),
                        &mut reported,
                    );
                    std::thread::sleep(std::time::Duration::from_secs(10));
                }
            }
            continue;
        }
        let Some((proc_obj, c)) = &capture else {
            continue;
        };
        let (peak, got) = c.take_peak();
        if !is_running(*proc_obj) {
            // Paused: silence is expected, don't blame permissions.
            silent_secs = 0;
            continue;
        }
        if got && peak == 0.0 {
            silent_secs += 1;
            // Playing, but the tap only gets zeros: macOS is withholding audio.
            if silent_secs >= 4 {
                report("denied", serde_json::Value::Null, &mut reported);
            }
        } else if peak > 0.0 {
            silent_secs = 0;
            report("ok", serde_json::Value::Null, &mut reported);
        } else if !got {
            // The process went away (GPU process restarted): tap again.
            capture = None;
            reported = None;
        }
    }
}
