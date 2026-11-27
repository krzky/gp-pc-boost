use crate::journal::state_dir;
use std::{
    fs,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::{ERROR_SUCCESS, LocalFree},
        System::Power::*,
    },
    core::GUID,
};

#[derive(Clone)]
pub struct PlanInfo {
    pub index: usize,
    pub guid: GUID,
    pub name: String,
    pub active: bool,
}

fn same_guid(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

pub fn format_guid(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

pub fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.trim().trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let data1 = u32::from_str_radix(&hex[0..8], 16).ok()?;
    let data2 = u16::from_str_radix(&hex[8..12], 16).ok()?;
    let data3 = u16::from_str_radix(&hex[12..16], 16).ok()?;
    let mut data4 = [0u8; 8];
    for (idx, byte) in data4.iter_mut().enumerate() {
        let start = 16 + idx * 2;
        *byte = u8::from_str_radix(&hex[start..start + 2], 16).ok()?;
    }
    Some(GUID {
        data1,
        data2,
        data3,
        data4,
    })
}

fn orig_file() -> PathBuf {
    state_dir().join("power_orig.txt")
}

pub fn active_guid() -> Option<GUID> {
    let mut p: *mut GUID = null_mut();
    if unsafe { PowerGetActiveScheme(null_mut(), &mut p) } != ERROR_SUCCESS || p.is_null() {
        return None;
    }
    let g = unsafe { *p };
    unsafe { LocalFree(p as _) };
    Some(g)
}

fn friendly_name(g: &GUID) -> String {
    let mut n = 0u32;
    unsafe { PowerReadFriendlyName(null_mut(), g, null(), null(), null_mut(), &mut n) };
    if n < 2 {
        return format_guid(g);
    }
    let mut buf = vec![0u8; n as usize];
    if unsafe { PowerReadFriendlyName(null_mut(), g, null(), null(), buf.as_mut_ptr(), &mut n) } != ERROR_SUCCESS {
        return format_guid(g);
    }
    let w: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&w)
}

pub fn list_plans() -> Vec<PlanInfo> {
    let cur = active_guid();
    let mut out = vec![];
    for i in 0.. {
        let mut g: GUID = unsafe { std::mem::zeroed() };
        let mut n = size_of::<GUID>() as u32;
        if unsafe {
            PowerEnumerate(
                null_mut(),
                null(),
                null(),
                ACCESS_SCHEME,
                i,
                &mut g as *mut GUID as *mut u8,
                &mut n,
            )
        } != ERROR_SUCCESS
        {
            break;
        }
        let active = cur.as_ref().is_some_and(|c| same_guid(c, &g));
        out.push(PlanInfo {
            index: i as usize,
            guid: g,
            name: friendly_name(&g),
            active,
        });
    }
    out
}

pub fn set_plan_by_target(target: &str) -> Result<PlanInfo, String> {
    let plans = list_plans();
    if plans.is_empty() {
        return Err("no Windows power plans enumerated".into());
    }
    let chosen = if let Ok(idx) = target.parse::<usize>() {
        plans.get(idx).cloned()
    } else {
        let q = target.trim().to_ascii_lowercase();
        match q.as_str() {
            "high" | "high-performance" | "perf" => plans
                .iter()
                .find(|p| p.name.to_ascii_lowercase().contains("high performance"))
                .cloned(),
            "ultimate" | "ultra" => plans
                .iter()
                .find(|p| p.name.to_ascii_lowercase().contains("ultimate"))
                .or_else(|| plans.iter().find(|p| p.name.to_ascii_lowercase().contains("high performance")))
                .cloned(),
            "balanced" => plans
                .iter()
                .find(|p| p.name.to_ascii_lowercase().contains("balanced"))
                .cloned(),
            "saver" | "power-saver" => plans
                .iter()
                .find(|p| p.name.to_ascii_lowercase().contains("power saver"))
                .cloned(),
            _ => plans
                .iter()
                .find(|p| p.name.to_ascii_lowercase().contains(&q) || format_guid(&p.guid).eq_ignore_ascii_case(&q))
                .cloned(),
        }
    };
    let chosen = chosen.ok_or_else(|| format!("power plan '{target}' not found"))?;

    let orig_path = orig_file();
    if !orig_path.exists()
        && let Some(cur) = active_guid()
    {
        if let Some(parent) = orig_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&orig_path, format_guid(&cur))
            .map_err(|e| format!("failed to save original power plan: {e}"))?;
    }

    let rc = unsafe { PowerSetActiveScheme(null_mut(), &chosen.guid) };
    if rc != ERROR_SUCCESS {
        return Err(format!("PowerSetActiveScheme failed with Win32 error {rc}"));
    }
    Ok(chosen)
}

pub fn restore_plan() -> Result<Option<String>, String> {
    let orig_path = orig_file();
    let Ok(text) = fs::read_to_string(&orig_path) else {
        return Ok(None);
    };
    let guid = parse_guid(&text).ok_or_else(|| format!("invalid GUID in {}", orig_path.display()))?;
    let rc = unsafe { PowerSetActiveScheme(null_mut(), &guid) };
    if rc != ERROR_SUCCESS {
        return Err(format!("PowerSetActiveScheme restore failed with Win32 error {rc}"));
    }
    let name = friendly_name(&guid);
    let _ = fs::remove_file(&orig_path);
    Ok(Some(name))
}

pub fn run_list() -> i32 {
    let plans = list_plans();
    println!("Windows Power Plans:");
    for p in &plans {
        println!(
            "  {} [{}] {:<24} ({})",
            if p.active { "*" } else { " " },
            p.index,
            p.name,
            format_guid(&p.guid)
        );
    }
    if let Some(active) = plans.iter().find(|p| p.active) {
        let low = active.name.to_ascii_lowercase();
        let has_faster = plans.iter().any(|p| {
            let n = p.name.to_ascii_lowercase();
            n.contains("ultimate") || n.contains("high performance")
        });
        if (low.contains("balanced") || low.contains("power saver")) && has_faster {
            println!("  Hint: currently on '{}'. Run `gp-pc-boost power set high` (or `ultimate`) for max responsiveness.", active.name);
        }
    }
    0
}

pub fn run_set(target: &str) -> i32 {
    match set_plan_by_target(target) {
        Ok(p) => {
            println!("Active power plan switched to '{}' ({}).", p.name, format_guid(&p.guid));
            println!("Run `gp-pc-boost power restore` to switch back.");
            0
        }
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

pub fn run_restore() -> i32 {
    match restore_plan() {
        Ok(Some(name)) => {
            println!("Restored original power plan: '{name}'.");
            0
        }
        Ok(None) => {
            println!("No saved power plan to restore.");
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
    fn guid_round_trip() {
        let s = "381b4222-f694-41f0-9685-ff5bb260df2e";
        let g = parse_guid(s).unwrap();
        assert_eq!(format_guid(&g), s);
        let braced = "{e9a42b02-d5df-448d-aa00-03f14749eb61}";
        let g2 = parse_guid(braced).unwrap();
        assert_eq!(format_guid(&g2), "e9a42b02-d5df-448d-aa00-03f14749eb61");
        assert!(parse_guid("not-a-guid").is_none());
    }
}
