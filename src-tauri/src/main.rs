// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == studio_carriers::can_process::WORKER_FLAG) {
        studio_carriers::can_process::run_worker_stdio();
    }
    tuning_tools_lib::run()
}
