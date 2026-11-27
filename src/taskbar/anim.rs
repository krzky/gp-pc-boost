//! Mochi-grade VSync-locked spring animation engine (`DwmFlush` + 240Hz sub-stepped springs).
//!
//! Matches Coucou/Mochi's SwiftUI `.spring(response:dampingFraction:)` feel:
//! - True sub-microsecond `Instant` delta times (no 15.6ms `GetTickCount64` quantization).
//! - Velocity-preserving retargeting (interrupting an open/close bends smoothly instead of snapping).
//! - Hardware VSync frame pacing via `DwmFlush()` (60/120/144/165Hz) parked on a `Condvar`
//!   so idle CPU usage is literally 0%.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Dwm::DwmFlush,
    UI::WindowsAndMessaging::PostMessageW,
};

/// SwiftUI-equivalent spring motion presets (response seconds, dampingFraction).
#[derive(Clone, Copy, Debug)]
pub struct MotionPreset {
    pub response: f32,
    pub damping: f32,
}

/// Gliding bars peeking in: smooth, unhurried Windows 11 taskbar emergence (~420ms).
pub const MOTION_PEEK: MotionPreset = MotionPreset { response: 0.44, damping: 0.90 };
/// Search bar expanding across the middle bar / panels opening: smooth Dynamic Island spring.
pub const MOTION_EXPAND: MotionPreset = MotionPreset { response: 0.36, damping: 0.80 };
/// Search bar shrinking back / flyouts closing: crisp, firm, zero sloppy bounce.
pub const MOTION_COLLAPSE: MotionPreset = MotionPreset { response: 0.28, damping: 0.92 };
/// Auto-hide bars retracting off-screen: graceful ~600ms Windows 11 taskbar tuck-away glide.
pub const MOTION_HIDE: MotionPreset = MotionPreset { response: 0.82, damping: 1.0 };
/// Horizontal dock morphing (unified Search + Calendar + live Coucou bar resizing smoothly).
pub const MOTION_FOLLOW: MotionPreset = MotionPreset { response: 0.34, damping: 0.86 };

/// Second-order damped harmonic spring integrated in 240Hz sub-steps.
#[derive(Clone, Copy, Debug)]
pub struct SpringVal {
    pub value: f32,
    pub target: f32,
    pub velocity: f32,
    omega: f32,
    zeta: f32,
    active: bool,
}

impl SpringVal {
    pub fn new(initial: f32) -> Self {
        Self {
            value: initial,
            target: initial,
            velocity: 0.0,
            omega: std::f32::consts::TAU / MOTION_PEEK.response,
            zeta: MOTION_PEEK.damping,
            active: false,
        }
    }

    pub fn jump(&mut self, v: f32) {
        self.value = v;
        self.target = v;
        self.velocity = 0.0;
        self.active = false;
    }

    /// Retargets the spring while preserving its live velocity so reversals bend organically.
    pub fn spring_to(&mut self, target: f32, preset: MotionPreset) {
        self.omega = std::f32::consts::TAU / preset.response.max(0.05);
        self.zeta = preset.damping.clamp(0.2, 2.0);
        self.target = target;
        if (self.target - self.value).abs() > 0.001 || self.velocity.abs() > 0.005 {
            self.active = true;
        }
    }

    #[inline]
    pub fn animating(&self) -> bool {
        self.active
    }

    /// Advances the spring by `dt` seconds (clamped to <= 50ms for safety) in 240Hz sub-steps.
    /// Returns `true` if the value changed this frame.
    pub fn step(&mut self, dt: f32) -> bool {
        if !self.active {
            return false;
        }
        let dt = dt.clamp(0.0005, 0.05);
        let steps = ((dt * 240.0).ceil() as u32).max(1);
        let h = dt / steps as f32;
        let w2 = self.omega * self.omega;
        let two_zw = 2.0 * self.zeta * self.omega;
        for _ in 0..steps {
            let acc = w2 * (self.target - self.value) - two_zw * self.velocity;
            self.velocity += acc * h;
            self.value += self.velocity * h;
        }
        if (self.target - self.value).abs() < 0.002 && self.velocity.abs() < 0.015 {
            self.value = self.target;
            self.velocity = 0.0;
            self.active = false;
        }
        true
    }
}

/// Hardware-VSync frame pacer using `DwmFlush()`.
/// Parks completely on a `Condvar` when idle (0% CPU) and posts `anim_msg` once per VSync
/// while any spring animation is active.
pub struct VsyncPacer {
    active: Mutex<bool>,
    cv: Condvar,
    stop: AtomicBool,
    host_hwnd: AtomicIsize,
    last_tick: Mutex<Instant>,
}

impl VsyncPacer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            host_hwnd: AtomicIsize::new(0),
            last_tick: Mutex::new(Instant::now()),
        })
    }

    pub fn bind_and_start(self: &Arc<Self>, host_hwnd: HWND, anim_msg: u32) {
        self.host_hwnd.store(host_hwnd as isize, Relaxed);
        let pacer = Arc::clone(self);
        thread::spawn(move || unsafe {
            loop {
                {
                    let mut guard = pacer.active.lock().unwrap();
                    while !*guard && !pacer.stop.load(Relaxed) {
                        guard = pacer.cv.wait(guard).unwrap();
                    }
                    if pacer.stop.load(Relaxed) {
                        return;
                    }
                }
                // Wait for exact DWM hardware VSync (falls back to 8ms sleep if DWM unavailable)
                let hr = DwmFlush();
                if hr != 0 {
                    thread::sleep(Duration::from_millis(8));
                }
                if pacer.stop.load(Relaxed) {
                    return;
                }
                if !*pacer.active.lock().unwrap() {
                    continue;
                }
                let h = pacer.host_hwnd.load(Relaxed) as HWND;
                if !h.is_null() {
                    PostMessageW(h, anim_msg, 0, 0);
                }
            }
        });
    }

    /// Wakes the VSync pacer thread and resets the `dt` clock so the first step is crisp.
    pub fn wake(&self) {
        let mut guard = self.active.lock().unwrap();
        if !*guard {
            *guard = true;
            *self.last_tick.lock().unwrap() = Instant::now();
            self.cv.notify_one();
        }
    }

    /// Parks the VSync pacer thread (0% CPU until next animation starts).
    pub fn sleep(&self) {
        *self.active.lock().unwrap() = false;
    }

    /// Consumes elapsed seconds since the previous VSync frame.
    pub fn take_dt(&self) -> f32 {
        let mut guard = self.last_tick.lock().unwrap();
        let now = Instant::now();
        let dt = now.duration_since(*guard).as_secs_f32().clamp(0.001, 0.05);
        *guard = now;
        dt
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Relaxed);
        *self.active.lock().unwrap() = false;
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_converges_cleanly_and_preserves_velocity_on_retarget() {
        let mut s = SpringVal::new(0.0);
        s.spring_to(1.0, MOTION_EXPAND);
        let mut peak = 0.0f32;
        for _ in 0..90 {
            s.step(1.0 / 60.0);
            if s.value > peak {
                peak = s.value;
            }
        }
        assert!(!s.animating(), "spring should settle within 1.5s");
        assert!((s.value - 1.0).abs() < 0.001);
        // Underdamped expand preset (0.80) gives subtle organic overshoot (~1-3%)
        assert!(peak > 1.005 && peak < 1.06, "unexpected peak={peak}");

        // Mid-flight reversal preserves velocity instead of snapping
        let mut r = SpringVal::new(0.0);
        r.spring_to(1.0, MOTION_PEEK);
        for _ in 0..6 {
            r.step(1.0 / 60.0);
        }
        let vel_before = r.velocity;
        assert!(vel_before > 0.5);
        r.spring_to(0.0, MOTION_HIDE);
        assert_eq!(r.velocity, vel_before, "retarget must preserve velocity");
        for _ in 0..240 {
            r.step(1.0 / 60.0);
        }
        assert!(!r.animating());
        assert!((r.value - 0.0).abs() < 0.001);
    }
}
