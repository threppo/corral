//! Corral: an independent desktop icon organizer for Windows.
#![windows_subsystem = "windows"]

mod app;
mod app_icon;
mod color_state;
mod screen_picker;
mod creation;
mod desktop;
mod desktop_view;
mod dialog;
mod dpi_transition;
mod dragdrop;
mod gate_host;
mod geometry;
mod glyphs;
mod group_paint;
mod icon_metrics;
mod icon_zoom;
mod icon_overlay;
mod menu_theme;
mod model;
mod motion;
mod desk_setup;
mod pages;
mod peek_keys;
mod portal;
mod portal_menu;
mod profile_store;
mod quick_hide;
mod render;
mod round_up;
mod item_facts;
mod round_up_time;
mod rollup;
mod scrollbar;
mod settings;
mod settings_ui;
mod sort_prompt;
mod snap;
mod snapshot;
mod sorting;
mod shell;
mod shell_guard;
mod shell_menu;
mod size_tip;
mod style;
mod tabs;
mod tint;
mod camouflage;
mod view_state;
mod watchdog;

use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    match args.first().map(String::as_str) {
        Some("--watchdog") => {
            let code = watchdog::Args::parse(&args[1..]).map(|a| watchdog::run(&a)).unwrap_or(2);
            std::process::exit(code);
        }
        Some("--settings") => {
            // Settings window only (no desktop hosting). `--settings-path <file>` overrides the store.
            let path = args
                .iter()
                .position(|a| a == "--settings-path")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(settings::default_path);
            // `--layout <file>` names the layout the Rules page lists Corrals from (read-only);
            // `--page <name>` opens a page directly (QA).
            let layout = args
                .iter()
                .position(|a| a == "--layout")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_layout_path);
            let page = args.iter().position(|a| a == "--page").and_then(|i| args.get(i + 1)).and_then(|n| settings_ui::Page::from_name(n));
            tint::use_for_settings_only();
            std::process::exit(settings_ui::run_settings_window(&path, &layout, page));
        }
        Some("--probe") => probe(),
        Some("--probe-view") => probe_view(),
        Some("--probe-portal") => probe_portal(args.get(1).map(String::as_str).unwrap_or(".")),
        // QA: the "Turn off sorting?" prompt alone (`--isdesktop` variant); prints the answer, changes nothing.
        Some("--qa-sort-prompt") => {
            let answer = sort_prompt::ask(args.iter().any(|a| a == "--isdesktop"));
            println!("{answer:?}");
        }
        _ => std::process::exit(app::run(parse_options(&args))),
    }
}

/// Read-only: list a folder the way a Folder Portal orders it. Changes nothing.
fn probe_portal(path: &str) {
    let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
    match shell::enumerate_folder(path) {
        Ok(mut items) => {
            items.sort_by(|a, b| portal::explorer_order(a.is_folder, &a.display_name, b.is_folder, &b.display_name));
            for item in items {
                println!("{}	{}	{}", if item.is_folder { "dir " } else { "file" }, item.display_name, item.parsing_name);
            }
        }
        Err(e) => println!("error: {e}"),
    }
}

/// Read-only diagnostics: desktop host and monitors. Changes nothing.
fn probe() {
    match desktop::find_host() {
        Some(host) => println!("host: {}", desktop::describe(&host)),
        None => println!("host: not found"),
    }
    for monitor in desktop::monitors() {
        println!("monitor: {} bounds={:?} work={:?} dpi={}", monitor.name, monitor.bounds, monitor.work, monitor.dpi);
    }
}

/// `corral [--desktop | --preview] [--layout PATH] [--seconds N]`
/// Default is `--preview` until desktop mode has passed its gating tests.
fn parse_options(args: &[String]) -> app::Options {
    let mut options = app::Options {
        mode: app::Mode::Preview,
        layout_path: default_layout_path(),
        seconds: None,
        settings_path: settings::default_path(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--desktop" => options.mode = app::Mode::Desktop,
            "--preview" => options.mode = app::Mode::Preview,
            "--layout" => {
                if let Some(path) = iter.next() {
                    options.layout_path = std::path::PathBuf::from(path);
                }
            }
            "--seconds" => options.seconds = iter.next().and_then(|s| s.parse().ok()),
            "--settings-path" => {
                if let Some(path) = iter.next() {
                    options.settings_path = std::path::PathBuf::from(path);
                }
            }
            _ => {}
        }
    }
    options
}

fn default_layout_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Corral").join("layout.json")
}

/// Read-only: Explorer's current desktop icon positions and view metrics.
fn probe_view() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    match desktop_view::read_desktop_view() {
        Ok(v) => {
            println!("icon_size={} spacing={:?} view_mode={} auto_arrange={} align_to_grid={}", v.icon_size, v.spacing, v.view_mode, v.auto_arrange, v.align_to_grid);
            for icon in v.icons {
                println!("{:?} {}", icon.position, icon.parsing_name);
            }
        }
        Err(e) => println!("desktop view unavailable: {e}"),
    }
}


