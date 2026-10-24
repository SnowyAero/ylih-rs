// SPDX-License-Identifier: GPL-3.0-or-later
mod config;
mod database;
mod i18n;
mod telemetry;
mod ui;

use database::Tracker;
use std::{env, time::Duration};

fn main() {
    let args: Vec<String> = env::args().collect();
    let is_daemon = args.iter().any(|a| a == "--daemon");
    let mut tracker = Tracker::new(is_daemon == false);

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
