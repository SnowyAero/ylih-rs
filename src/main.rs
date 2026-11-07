// SPDX-License-Identifier: GPL-3.0-or-later
mod config;
mod database;
mod i18n;
mod telemetry;
mod ui;

use database::Tracker;
use std::{env, time::Duration};

fn print_help() {
    println!(
        "YLIH-RS v{} — Your Life in Headphones (Linux)",
        env!("CARGO_PKG_VERSION")
    );
    println!("Monitor ultraligero de autonomía y telemetría para auriculares Bluetooth/Jack.\n");
    println!("USO:");
    println!("  ylih [OPCIONES]\n");
    println!("OPCIONES:");
    println!("  (sin argumentos)   Inicia la interfaz gráfica GTK3");
    println!("  --tui              Inicia la interfaz interactiva en terminal (TUI)");
    println!("  --daemon           Ejecuta el demonio de telemetría en segundo plano");
    println!("  -h, --help         Muestra esta ayuda detallada y sale");
    println!("  -v, --version      Muestra la versión del programa y sale\n");
    println!("RUTAS DE DATOS:");
    println!("  Configuración:     ~/.local/share/ylih/config.json");
    println!("  Dispositivos:      ~/.local/share/ylih/devices/");
    println!("  Iconos custom:     ~/.local/share/ylih/icons/");
    println!("  Reportes debug:    ~/.config/ylih-rs/applogs/");
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("ylih {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let is_daemon = args.iter().any(|a| a == "--daemon");
    let mut tracker = Tracker::new(!is_daemon);

    if is_daemon {
        loop {
            let poll = tracker.db.settings.poll_interval.max(1);
            std::thread::sleep(Duration::from_secs(poll));
            tracker.tick(false);
        }
    } else if args.iter().any(|a| a == "--tui") {
        let _ = ui::tui_app::run_tui(tracker);
    } else {
        ui::gtk_app::run_gtk(tracker);
    }
}
