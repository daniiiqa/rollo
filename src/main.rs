//! rollo: la vista de workspaces de discada. La pantalla se aleja y quedan
//! los 10 workspaces en miniatura, cada uno con sus ventanas de verdad
//! (capturadas aunque estén en otro workspace) sobre su wallpaper.
//!
//!   clic en un workspace o una ventana   ir ahí (con zoom a la miniatura)
//!   arrastrar una ventana                mandarla a otro workspace
//!   flechas / hjkl, Enter                elegir e ir;  1…9, 0  directo
//!   Esc, clic fuera, o `rollo` otra vez  cerrar
//!
//! Los colores, la fuente y los bordes salen del look (disco).

mod app;
mod draw;
mod hypr;

use std::path::PathBuf;

/// El protocolo de Hyprland para capturar una ventana por su dirección.
pub mod proto {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables, non_upper_case_globals, non_snake_case, unused_imports, missing_docs, clippy::all)]
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-toplevel-export-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/hyprland-toplevel-export-v1.xml");
}

fn pidfile() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from).join("rollo.pid")
}

fn main() {
    // otra vez `rollo` con uno abierto: que se cierre (SUPER+TAB de ida y vuelta)
    if let Some(pid) = std::fs::read_to_string(pidfile()).ok().and_then(|p| p.trim().parse::<i32>().ok())
        && unsafe { libc::kill(pid, 0) } == 0
    {
        unsafe { libc::kill(pid, libc::SIGUSR1) };
        return;
    }
    let sigs = unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut s);
        libc::sigaddset(&mut s, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &s, std::ptr::null_mut());
        s
    };
    let _ = std::fs::write(pidfile(), std::process::id().to_string());
    let result = app::run(sigs);
    let _ = std::fs::remove_file(pidfile());
    if let Err(e) = result {
        eprintln!("rollo: {e}");
        std::process::exit(1);
    }
}
