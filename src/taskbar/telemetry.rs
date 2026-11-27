//! Off-thread system telemetry polling (CPU%, RAM%, online state, battery%).

use std::{
    mem::{size_of, zeroed},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering::Relaxed},
    },
    thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::FILETIME,
    NetworkManagement::IpHelper::GetNetworkConnectivityHint,
    Networking::WinSock::{
        NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityLevelHintConstrainedInternetAccess,
        NetworkConnectivityLevelHintInternetAccess,
    },
    System::{
        Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS},
        SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        Threading::GetSystemTimes,
    },
    UI::WindowsAndMessaging::PostMessageW,
};

pub struct TelemetryState {
    pub cpu_pct: AtomicI32,
    pub ram_pct: AtomicI32,
    pub online: AtomicBool,
    pub bat_pct: AtomicI32, // -1 = desktop / no battery
    pub on_ac: AtomicBool,
    pub host_hwnd: AtomicIsize,
    pub stop: AtomicBool,
}

impl Default for TelemetryState {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryState {
    pub fn new() -> Self {
        Self {
            cpu_pct: AtomicI32::new(0),
            ram_pct: AtomicI32::new(0),
            online: AtomicBool::new(true),
            bat_pct: AtomicI32::new(-1),
            on_ac: AtomicBool::new(true),
            host_hwnd: AtomicIsize::new(0),
            stop: AtomicBool::new(false),
        }
    }

    pub fn snapshot(&self) -> TelemetrySnapshot {
        TelemetrySnapshot {
            cpu_pct: self.cpu_pct.load(Relaxed),
            ram_pct: self.ram_pct.load(Relaxed),
            online: self.online.load(Relaxed),
            bat_pct: self.bat_pct.load(Relaxed),
            on_ac: self.on_ac.load(Relaxed),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TelemetrySnapshot {
    pub cpu_pct: i32,
    pub ram_pct: i32,
    pub online: bool,
    pub bat_pct: i32,
    pub on_ac: bool,
}

fn ft_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

unsafe fn sample_times() -> Option<(u64, u64)> {
    let (mut idle, mut kern, mut user): (FILETIME, FILETIME, FILETIME) = zeroed();
    if GetSystemTimes(&mut idle, &mut kern, &mut user) == 0 {
        return None;
    }
    let total = ft_u64(kern).saturating_add(ft_u64(user));
    Some((ft_u64(idle), total))
}

unsafe fn sample_ram() -> i32 {
    let mut mem: MEMORYSTATUSEX = zeroed();
    mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    if GlobalMemoryStatusEx(&mut mem) != 0 {
        mem.dwMemoryLoad.clamp(0, 100) as i32
    } else {
        0
    }
}

unsafe fn sample_online() -> bool {
    let mut hint: NL_NETWORK_CONNECTIVITY_HINT = zeroed();
    if GetNetworkConnectivityHint(&mut hint) != 0 {
        return true;
    }
    let lvl = hint.ConnectivityLevel;
    lvl == NetworkConnectivityLevelHintInternetAccess
        || lvl == NetworkConnectivityLevelHintConstrainedInternetAccess
}

unsafe fn sample_battery() -> (i32, bool) {
    let mut s: SYSTEM_POWER_STATUS = zeroed();
    if GetSystemPowerStatus(&mut s) != 0 && (s.BatteryFlag & 128) == 0 && s.BatteryLifePercent <= 100 {
        (s.BatteryLifePercent as i32, s.ACLineStatus == 1)
    } else {
        (-1, true)
    }
}

pub fn spawn_poller(state: Arc<TelemetryState>, notify_msg: u32) {
    thread::spawn(move || unsafe {
        let mut prev = sample_times();
        let mut last = state.snapshot();
        loop {
            if state.stop.load(Relaxed) {
                break;
            }
            let cur = sample_times();
            let cpu = match (prev, cur) {
                (Some((pi, pt)), Some((ci, ct))) => {
                    let dt = ct.saturating_sub(pt);
                    let di = ci.saturating_sub(pi);
                    if dt > 0 {
                        (((dt.saturating_sub(di)) * 100) / dt).min(100) as i32
                    } else {
                        last.cpu_pct
                    }
                }
                _ => 0,
            };
            prev = cur;

            let ram = sample_ram();
            let online = sample_online();
            let (bat_pct, on_ac) = sample_battery();
            let next = TelemetrySnapshot {
                cpu_pct: cpu,
                ram_pct: ram,
                online,
                bat_pct,
                on_ac,
            };

            if next != last {
                state.cpu_pct.store(next.cpu_pct, Relaxed);
                state.ram_pct.store(next.ram_pct, Relaxed);
                state.online.store(next.online, Relaxed);
                state.bat_pct.store(next.bat_pct, Relaxed);
                state.on_ac.store(next.on_ac, Relaxed);
                last = next;
                let hwnd = state.host_hwnd.load(Relaxed);
                if hwnd != 0 {
                    PostMessageW(hwnd as _, notify_msg, 0, 0);
                }
            }

            thread::sleep(Duration::from_secs(2));
        }
    });
}
