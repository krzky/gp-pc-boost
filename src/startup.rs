use crate::journal::{default_journal_path, record_if_new};
use std::{
    collections::HashMap,
    fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::GetLastError,
    System::{
        Registry::*,
        SystemInformation::GetSystemTimeAsFileTime,
    },
};
use winrust_common::{
    journal::{Entry, svc_mode},
    proc::elevated,
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";
const SVC_OFF: &str = r"SOFTWARE\gp-pc-boost\StartupSvc";

struct Source {
    label: &'static str,
    hklm: bool,
    run: Option<&'static str>,
    approved: &'static str,
}

const SOURCES: [Source; 5] = [
    Source {
        label: "Registry (HKCU Run)",
        hklm: false,
        run: Some(RUN),
        approved: "Run",
    },
    Source {
        label: "Registry (HKLM Run)",
        hklm: true,
        run: Some(RUN),
        approved: "Run",
    },
    Source {
        label: "Registry (HKLM Run32)",
        hklm: true,
        run: Some(RUN32),
        approved: "Run32",
    },
    Source {
        label: "Startup Folder (User)",
        hklm: false,
        run: None,
        approved: "StartupFolder",
    },
    Source {
        label: "Startup Folder (All Users)",
        hklm: true,
        run: None,
        approved: "StartupFolder",
    },
];

pub const TASK: usize = 5;
pub const SERVICE: usize = 6;

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub command: String,
    pub source: usize,
    pub enabled: bool,
}

impl Item {
    pub fn source_label(&self) -> &'static str {
        match self.source {
            TASK => "Task Scheduler (Logon/Boot)",
            SERVICE => "Auto-start 3rd-party Service",
            i => SOURCES[i].label,
        }
    }

    pub fn needs_admin(&self) -> bool {
        self.source >= TASK || SOURCES[self.source].hklm
    }

    pub fn exe_name(&self) -> Option<String> {
        let cmd = self.command.trim();
        let exe = if let Some(rest) = cmd.strip_prefix('"') {
            rest.split('"').next()?
        } else {
            cmd.split_whitespace().next()?
        };
        Some(exe.rsplit(['\\', '/']).next()?.to_ascii_lowercase())
    }
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn root(hklm: bool) -> HKEY {
    if hklm { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER }
}

fn open(r: HKEY, path: &str, access: u32) -> Option<HKEY> {
    let mut k: HKEY = null_mut();
    (unsafe { RegOpenKeyExW(r, w(path).as_ptr(), 0, access, &mut k) } == 0).then_some(k)
}

fn from_utf16_bytes(b: &[u8]) -> String {
    let u: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u).trim_end_matches('\0').to_string()
}

fn string_values(r: HKEY, path: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let Some(k) = open(r, path, KEY_READ) else {
        return out;
    };
    for i in 0.. {
        let mut name = vec![0u16; 16384];
        let mut nlen = name.len() as u32;
        let mut data = vec![0u8; 32768];
        let mut dlen = data.len() as u32;
        let mut ty = 0u32;
        let rc = unsafe {
            RegEnumValueW(
                k,
                i,
                name.as_mut_ptr(),
                &mut nlen,
                null_mut(),
                &mut ty,
                data.as_mut_ptr(),
                &mut dlen,
            )
        };
        if rc != 0 {
            break;
        }
        if ty == REG_SZ || ty == REG_EXPAND_SZ {
            out.push((
                String::from_utf16_lossy(&name[..nlen as usize]),
                from_utf16_bytes(&data[..dlen as usize]),
            ));
        }
    }
    unsafe { RegCloseKey(k) };
    out
}

fn approved_enabled(hklm: bool, kind: &str, name: &str) -> bool {
    let Some(k) = open(root(hklm), &format!(r"{APPROVED}\{kind}"), KEY_READ) else {
        return true;
    };
    let mut buf = [0u8; 12];
    let mut len = buf.len() as u32;
    let rc = unsafe {
        RegQueryValueExW(
            k,
            w(name).as_ptr(),
            null_mut(),
            null_mut(),
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    unsafe { RegCloseKey(k) };
    if rc == 0 && len != 0 {
        buf[0] & 1 == 0
    } else {
        true
    }
}

fn startup_dir(hklm: bool) -> Option<PathBuf> {
    let var = if hklm { "ProgramData" } else { "APPDATA" };
    std::env::var_os(var).map(|p| PathBuf::from(p).join(r"Microsoft\Windows\Start Menu\Programs\Startup"))
}

fn dword(k: HKEY, name: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    let mut ty = 0u32;
    let rc = unsafe {
        RegQueryValueExW(
            k,
            w(name).as_ptr(),
            null_mut(),
            &mut ty,
            &mut v as *mut u32 as *mut u8,
            &mut len,
        )
    };
    (rc == 0 && ty == REG_DWORD).then_some(v)
}

fn sz(k: HKEY, name: &str) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let mut len = buf.len() as u32;
    let mut ty = 0u32;
    let rc = unsafe { RegQueryValueExW(k, w(name).as_ptr(), null_mut(), &mut ty, buf.as_mut_ptr(), &mut len) };
    (rc == 0 && (ty == REG_SZ || ty == REG_EXPAND_SZ)).then(|| from_utf16_bytes(&buf[..len as usize]))
}

fn subkeys(r: HKEY, path: &str) -> Vec<String> {
    let Some(k) = open(r, path, KEY_READ) else {
        return vec![];
    };
    let mut out = vec![];
    for i in 0.. {
        let mut name = [0u16; 256];
        let mut n = name.len() as u32;
        if unsafe {
            RegEnumKeyExW(
                k,
                i,
                name.as_mut_ptr(),
                &mut n,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        } != 0
        {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..n as usize]));
    }
    unsafe { RegCloseKey(k) };
    out
}

fn value_names(k: HKEY) -> Vec<String> {
    let mut out = vec![];
    for i in 0.. {
        let mut name = [0u16; 512];
        let mut n = name.len() as u32;
        if unsafe {
            RegEnumValueW(
                k,
                i,
                name.as_mut_ptr(),
                &mut n,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        } != 0
        {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..n as usize]));
    }
    out
}

pub fn third_party_image(image: &str) -> bool {
    let i = image.to_ascii_lowercase();
    !(i.is_empty()
        || i.contains("\\windows\\")
        || i.starts_with("system32")
        || i.starts_with("\\systemroot")
        || i.starts_with("%systemroot%")
        || i.contains("svchost.exe"))
}

fn services() -> Vec<Item> {
    let ours = open(HKEY_LOCAL_MACHINE, SVC_OFF, KEY_READ).map_or(vec![], |k| {
        let v = value_names(k);
        unsafe { RegCloseKey(k) };
        v
    });
    let mut out = vec![];
    for name in subkeys(HKEY_LOCAL_MACHINE, SERVICES_KEY) {
        let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{SERVICES_KEY}\{name}"), KEY_READ) else {
            continue;
        };
        let (start, ty, image) = (
            dword(k, "Start"),
            dword(k, "Type").unwrap_or(0),
            sz(k, "ImagePath").unwrap_or_default(),
        );
        unsafe { RegCloseKey(k) };
        let mine = ours.iter().any(|o| o.eq_ignore_ascii_case(&name));
        if ty & 0x30 == 0 || !third_party_image(&image) || !(start == Some(2) || (mine && start == Some(3))) {
            continue;
        }
        out.push(Item {
            name,
            command: image,
            source: SERVICE,
            enabled: start == Some(2),
        });
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let rest = &xml[xml.find(&open)? + open.len()..];
    Some(&rest[..rest.find(&format!("</{tag}>"))?])
}

pub fn parse_task(xml: &str) -> Option<(String, bool)> {
    let triggers = tag(xml, "Triggers")?;
    if !triggers.contains("<LogonTrigger") && !triggers.contains("<BootTrigger") {
        return None;
    }
    let exec = tag(xml, "Exec")?;
    let cmd = xml_unescape(tag(exec, "Command")?.trim());
    let args = tag(exec, "Arguments")
        .map(|a| xml_unescape(a.trim()))
        .unwrap_or_default();
    let quoted = if cmd.contains(' ') && !cmd.starts_with('"') {
        format!("\"{cmd}\"")
    } else {
        cmd
    };
    let enabled = !tag(xml, "Settings").is_some_and(|s| s.contains("<Enabled>false</Enabled>"));
    Some((
        if args.is_empty() {
            quoted
        } else {
            format!("{quoted} {args}")
        },
        enabled,
    ))
}

fn decode_task_file(b: &[u8]) -> String {
    match b {
        [0xFF, 0xFE, rest @ ..] => from_utf16_bytes(rest),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(b).into_owned(),
    }
}

fn tasks() -> Vec<Item> {
    let Some(root) = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join(r"System32\Tasks")) else {
        return vec![];
    };
    let mut out = vec![];
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if dir != root || !e.file_name().eq_ignore_ascii_case("Microsoft") {
                    stack.push(p);
                }
            } else if let Some((command, enabled)) = fs::read(&p).ok().and_then(|b| parse_task(&decode_task_file(&b))) {
                let rel = p
                    .strip_prefix(&root)
                    .map(|r| format!("\\{}", r.display()))
                    .unwrap_or_default();
                out.push(Item {
                    name: rel,
                    command,
                    source: TASK,
                    enabled,
                });
            }
        }
    }
    out
}

pub fn list() -> Vec<Item> {
    let mut out = tasks();
    out.extend(services());
    for (i, s) in SOURCES.iter().enumerate() {
        match s.run {
            Some(path) => {
                for (name, command) in string_values(root(s.hklm), path) {
                    let enabled = approved_enabled(s.hklm, s.approved, &name);
                    out.push(Item {
                        name,
                        command,
                        source: i,
                        enabled,
                    });
                }
            }
            None => {
                let Some(d) = startup_dir(s.hklm) else {
                    continue;
                };
                for e in fs::read_dir(d).into_iter().flatten().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.eq_ignore_ascii_case("desktop.ini") {
                        continue;
                    }
                    let enabled = approved_enabled(s.hklm, s.approved, &name);
                    out.push(Item {
                        name,
                        command: e.path().display().to_string(),
                        source: i,
                        enabled,
                    });
                }
            }
        }
    }
    out
}

#[derive(Default, Debug, PartialEq)]
pub struct BootLog {
    pub boots: Vec<(String, u32, u32)>,
    pub slow: Vec<(String, String, u32)>,
}

fn data_field<'a>(ev: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("<Data Name='{name}'>");
    let rest = &ev[ev.find(&key)? + key.len()..];
    Some(&rest[..rest.find("</Data>")?])
}

pub fn parse_boot_log(xml: &str) -> BootLog {
    let mut log = BootLog::default();
    for ev in xml.split("<Event ").skip(1) {
        let id = tag(ev, "EventID").and_then(|s| s.trim().parse::<u32>().ok());
        let num = |n: &str| data_field(ev, n).and_then(|v| v.trim().parse::<u32>().ok());
        match id {
            Some(100) => {
                let date = ev
                    .split("SystemTime='")
                    .nth(1)
                    .map(|s| s.chars().take(16).collect::<String>().replace('T', " "))
                    .unwrap_or_default();
                if let Some(b) = num("BootTime") {
                    log.boots.push((date, b, num("MainPathBootTime").unwrap_or(0)));
                }
            }
            Some(101 | 103) => {
                let (Some(name), Some(deg)) = (data_field(ev, "Name"), num("DegradationTime")) else {
                    continue;
                };
                let name = xml_unescape(name).to_lowercase();
                let friendly = data_field(ev, "FriendlyName").map(xml_unescape).unwrap_or_default();
                match log.slow.iter_mut().find(|s| s.0 == name) {
                    Some(s) => s.2 = s.2.max(deg),
                    None => log.slow.push((name, friendly, deg)),
                }
            }
            _ => {}
        }
    }
    log.slow.sort_by(|a, b| b.2.cmp(&a.2));
    log
}

pub fn read_boot_log() -> BootLog {
    let out = Command::new("wevtutil.exe")
        .args([
            "qe",
            "Microsoft-Windows-Diagnostics-Performance/Operational",
            "/q:*[System[(EventID=100 or EventID=101 or EventID=103)]]",
            "/c:200",
            "/rd:true",
            "/f:xml",
        ])
        .creation_flags(0x0800_0000)
        .output();
    match out {
        Ok(o) if o.status.success() => parse_boot_log(&String::from_utf8_lossy(&o.stdout)),
        _ => BootLog::default(),
    }
}

fn cmd_ok(prog: &str, args: &[&str]) -> Result<(), String> {
    match Command::new(prog).args(args).creation_flags(0x0800_0000).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "{prog} exited {:?}: {}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(e.to_string()),
    }
}

pub fn set_item_enabled(it: &Item, on: bool) -> Result<(), String> {
    if it.needs_admin() && !elevated() {
        return Err(format!("'{}' requires administrator privileges to toggle", it.name));
    }
    match it.source {
        TASK => cmd_ok(
            "schtasks.exe",
            &["/change", "/tn", &it.name, if on { "/enable" } else { "/disable" }],
        ),
        SERVICE => {
            if !on {
                if let Some(k) = open(HKEY_LOCAL_MACHINE, &format!(r"{SERVICES_KEY}\{}", it.name), KEY_READ) {
                    let mode = svc_mode(dword(k, "Start"), dword(k, "DelayedAutostart"));
                    unsafe { RegCloseKey(k) };
                    if let Some(m) = mode {
                        let _ = record_if_new(
                            &default_journal_path(),
                            &Entry::Svc {
                                name: it.name.clone(),
                                mode: m.into(),
                            },
                        );
                    }
                }
            }
            cmd_ok(
                "sc.exe",
                &["config", &it.name, "start=", if on { "auto" } else { "demand" }],
            )?;
            let mut k: HKEY = null_mut();
            let rc = unsafe {
                RegCreateKeyExW(
                    HKEY_LOCAL_MACHINE,
                    w(SVC_OFF).as_ptr(),
                    0,
                    null_mut(),
                    0,
                    KEY_SET_VALUE,
                    null_mut(),
                    &mut k,
                    null_mut(),
                )
            };
            if rc == 0 {
                unsafe {
                    if on {
                        RegDeleteValueW(k, w(&it.name).as_ptr());
                    } else {
                        let one = 1u32;
                        RegSetValueExW(k, w(&it.name).as_ptr(), 0, REG_DWORD, &one as *const u32 as *const u8, 4);
                    }
                    RegCloseKey(k);
                }
            }
            Ok(())
        }
        src => {
            let s = &SOURCES[src];
            let mut ft = unsafe { std::mem::zeroed() };
            unsafe { GetSystemTimeAsFileTime(&mut ft) };
            let filetime = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
            let mut value = [0u8; 12];
            value[0] = if on { 2 } else { 3 };
            if !on {
                value[4..].copy_from_slice(&filetime.to_le_bytes());
            }
            let mut k: HKEY = null_mut();
            let path = w(&format!(r"{APPROVED}\{}", s.approved));
            let rc = unsafe {
                RegCreateKeyExW(
                    root(s.hklm),
                    path.as_ptr(),
                    0,
                    null_mut(),
                    0,
                    KEY_SET_VALUE,
                    null_mut(),
                    &mut k,
                    null_mut(),
                )
            };
            if rc != 0 {
                return Err(format!("RegCreateKeyExW failed ({rc}, GetLastError={})", unsafe {
                    GetLastError()
                }));
            }
            let rc = unsafe {
                RegSetValueExW(
                    k,
                    w(&it.name).as_ptr(),
                    0,
                    REG_BINARY,
                    value.as_ptr(),
                    value.len() as u32,
                )
            };
            unsafe { RegCloseKey(k) };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("RegSetValueExW failed ({rc})"))
            }
        }
    }
}

pub fn run_list() -> i32 {
    let items = list();
    let boot = read_boot_log();
    let slow_map: HashMap<String, u32> = boot.slow.into_iter().map(|(n, _, ms)| (n, ms)).collect();
    println!("Startup Items ({} total):", items.len());
    println!("  {:<8} {:<30} {:<32} {:<10} Command", "State", "Source", "Name", "BootSlow");
    for it in &items {
        let slow = it
            .exe_name()
            .and_then(|e| slow_map.get(&e).copied())
            .or_else(|| slow_map.get(&it.name.to_ascii_lowercase()).copied())
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<8} {:<30} {:<32} {:<10} {}",
            if it.enabled { "ENABLED" } else { "DISABLED" },
            it.source_label(),
            it.name,
            slow,
            it.command
        );
    }
    0
}

pub fn run_boot_slow() -> i32 {
    let boot = read_boot_log();
    if boot.boots.is_empty() && boot.slow.is_empty() {
        println!("Boot performance log unavailable or empty (run from an elevated terminal if access is denied).");
        return 0;
    }
    println!("Recent Windows Boots (newest first):");
    for (date, total_ms, main_ms) in boot.boots.iter().take(5) {
        println!(
            "  {}  total={:.2}s  main_path={:.2}s",
            date,
            *total_ms as f64 / 1000.0,
            *main_ms as f64 / 1000.0
        );
    }
    if !boot.slow.is_empty() {
        println!("\nTop Boot-Degrading Processes/Services:");
        for (name, friendly, ms) in boot.slow.iter().take(12) {
            let label = if friendly.is_empty() { name } else { friendly };
            println!("  {:>6} ms  {} ({})", ms, label, name);
        }
    }
    0
}

pub fn run_toggle(name: &str, enable: bool) -> i32 {
    let items = list();
    let q = name.trim().to_ascii_lowercase();
    let matches: Vec<&Item> = items
        .iter()
        .filter(|it| it.name.eq_ignore_ascii_case(&q) || it.name.to_ascii_lowercase().contains(&q))
        .collect();
    if matches.is_empty() {
        eprintln!("No startup item matching '{name}'.");
        return 1;
    }
    if matches.len() > 1 && !matches.iter().any(|it| it.name.eq_ignore_ascii_case(&q)) {
        eprintln!("Ambiguous startup item '{name}'. Matches:");
        for m in matches {
            eprintln!("  - {} ({})", m.name, m.source_label());
        }
        return 1;
    }
    let chosen = matches
        .iter()
        .find(|it| it.name.eq_ignore_ascii_case(&q))
        .copied()
        .unwrap_or(matches[0]);
    match set_item_enabled(chosen, enable) {
        Ok(()) => {
            println!(
                "Startup item '{}' ({}) is now {}.",
                chosen.name,
                chosen.source_label(),
                if enable { "ENABLED" } else { "DISABLED" }
            );
            0
        }
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_logon_task_xml() {
        let xml = r#"
<Task><Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
<Settings><Enabled>true</Enabled></Settings>
<Actions><Exec><Command>C:\Program Files\App\app.exe</Command><Arguments>--bg</Arguments></Exec></Actions></Task>"#;
        let (cmd, on) = parse_task(xml).unwrap();
        assert_eq!(cmd, r#""C:\Program Files\App\app.exe" --bg"#);
        assert!(on);
    }

    #[test]
    fn parses_boot_log_xml() {
        let xml = r#"<Events>
<Event ><System><EventID>100</EventID><TimeCreated SystemTime='2026-11-26T08:00:00.000Z'/></System><EventData><Data Name='BootTime'>14500</Data><Data Name='MainPathBootTime'>7200</Data></EventData></Event>
<Event ><System><EventID>101</EventID></System><EventData><Data Name='Name'>foo.exe</Data><Data Name='FriendlyName'>Foo App</Data><Data Name='DegradationTime'>850</Data></EventData></Event>
</Events>"#;
        let log = parse_boot_log(xml);
        assert_eq!(log.boots.len(), 1);
        assert_eq!(log.boots[0].1, 14500);
        assert_eq!(log.slow, vec![("foo.exe".into(), "Foo App".into(), 850)]);
    }
}
