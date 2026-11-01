//! Lo que rollo sabe de Hyprland (hyprctl -j) y lo que le pide (Hypr.sh, que
//! habla igual con la config vieja que con la de Lua).

use serde_json::Value;
use std::process::Command;

pub struct Mon {
    pub id: i64,
    pub name: String,
    /// en pixeles lógicos, en el plano de todos los monitores
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub scale: f32,
    pub focused: bool,
    pub active_ws: i32,
}

pub struct Win {
    /// la dirección de `hyprctl clients` (también es el handle para capturarla)
    pub addr: u64,
    pub ws: i32,
    pub mon: i64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub class: String,
    pub floating: bool,
    /// 0 la enfocada, 1 la de antes…
    pub focus: i64,
}

fn json(what: &str) -> Value {
    Command::new("hyprctl")
        .args([what, "-j"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(Value::Null)
}

fn f(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

pub fn monitors() -> Vec<Mon> {
    json("monitors")
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| !m["disabled"].as_bool().unwrap_or(false))
        .map(|m| {
            let scale = f(&m["scale"]).max(0.1);
            let (mut w, mut h) = (f(&m["width"]) / scale, f(&m["height"]) / scale);
            if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
                (w, h) = (h, w);
            }
            Mon {
                id: m["id"].as_i64().unwrap_or(0),
                name: m["name"].as_str().unwrap_or_default().to_string(),
                x: f(&m["x"]),
                y: f(&m["y"]),
                w,
                h,
                scale,
                focused: m["focused"].as_bool().unwrap_or(false),
                active_ws: m["activeWorkspace"]["id"].as_i64().unwrap_or(1) as i32,
            }
        })
        .collect()
}

/// Las ventanas de los workspaces normales (los especiales no salen).
pub fn windows() -> Vec<Win> {
    json("clients")
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["mapped"].as_bool().unwrap_or(true) && !c["hidden"].as_bool().unwrap_or(false))
        .filter_map(|c| {
            let ws = c["workspace"]["id"].as_i64()? as i32;
            let addr = u64::from_str_radix(c["address"].as_str()?.trim_start_matches("0x"), 16).ok()?;
            (ws >= 1).then(|| Win {
                addr,
                ws,
                mon: c["monitor"].as_i64().unwrap_or(0),
                x: f(&c["at"][0]),
                y: f(&c["at"][1]),
                w: f(&c["size"][0]),
                h: f(&c["size"][1]),
                class: c["class"].as_str().unwrap_or_default().to_string(),
                floating: c["floating"].as_bool().unwrap_or(false),
                focus: c["focusHistoryID"].as_i64().unwrap_or(99),
            })
        })
        .collect()
}

/// El wallpaper de un monitor según awww (": DP-1: 1920x1080, … image: /ruta").
pub fn wallpaper(mon: &str) -> Option<String> {
    let out = Command::new("awww").arg("query").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let rest = l.strip_prefix(": ")?.strip_prefix(mon)?.strip_prefix(':')?;
        Some(rest.split("image: ").nth(1)?.trim().to_string())
    })
}

fn hypr_sh(args: &[&str]) {
    let home = std::env::var("HOME").unwrap_or_default();
    let _ = Command::new(format!("{home}/.config/hypr/scripts/Hypr.sh")).args(args).status();
}

/// Ir al workspace o a la ventana sin la animación de Hyprland: la de rollo
/// (el zoom a la miniatura) ya la hizo, y encima se vería doble.
pub fn jump(ws: i32, window: Option<u64>) {
    let home = std::env::var("HOME").unwrap_or_default();
    let had = Command::new(format!("{home}/.config/hypr/scripts/Hypr.sh"))
        .args(["get", "animations:enabled"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(true);
    if had {
        hypr_sh(&["set", "animations:enabled", "false"]);
    }
    match window {
        Some(a) => hypr_sh(&["focus", &format!("address:0x{a:x}")]),
        None => hypr_sh(&["workspace", &ws.to_string()]),
    }
    if had {
        // un respiro para que el cambio ya se haya hecho sin animar
        std::thread::sleep(std::time::Duration::from_millis(60));
        hypr_sh(&["set", "animations:enabled", "true"]);
    }
}

/// Mandar una ventana a otro workspace sin seguirla.
pub fn move_to(addr: u64, ws: i32) {
    hypr_sh(&["movetows", &ws.to_string(), &format!("address:0x{addr:x}"), "silent"]);
}
