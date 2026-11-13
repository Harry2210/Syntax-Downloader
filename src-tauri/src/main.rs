// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args_os()
        .skip(1)
        .any(|arg| arg.to_string_lossy().starts_with("--parent-window="))
    {
        syntax_downloader_lib::run_native_messaging_host();
        return;
    }
    syntax_downloader_lib::run();
}
