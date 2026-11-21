//! RetroZone Home's window and tray icon. The work is the engine's (engine.rs); this is how a
//! player sees and steers it: signing in through their browser, choosing folders with the
//! system's own picker, seeing what was played from here, and leaving it running in the tray.
//!
//! The page (ui/) only calls the commands below and listens for the engine's `state`. Choosing a
//! folder, opening the browser and starting with the computer are done here, not by the page.
//!
//! `--hidden` starts it in the tray without the window: how it starts with the computer.
//!
//! In the tray there is no window at all - closing it closes it for real, and its browser engine
//! (WebView2, a few processes and some 180 MB) goes with it - so what runs all the time is the
//! engine alone. The window is made again when it is opened.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use retrozone_home::config;
use retrozone_home::engine::{Engine, SignIn, State as Shown};
use retrozone_home::library::Library;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

const TRAY: &str = "main";

struct App {
    engine: Arc<Engine>,
}

/// Opens the window, making it first if it is not there: it is not, while the program is in the tray
fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }

    // Made away from the window's own thread: on Windows, making one from inside an event handler there deadlocks
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(config) = app.config().app.windows.first().cloned() else { return };
        match tauri::WebviewWindowBuilder::from_config(&app, &config).and_then(|builder| builder.build()) {
            Ok(window) => {
                let _ = window.show();
                let _ = window.set_focus();
            }
            Err(error) => log::error!("the window could not be made: {error}"),
        }
    });
}

/// Opens a page in the player's browser. `RETROZONE_HOME_NO_BROWSER` only notes it, for trying
/// the program out without a browser that is signed in as somebody else
fn open(app: &AppHandle, url: &str) {
    if std::env::var_os("RETROZONE_HOME_NO_BROWSER").is_some() {
        log::info!("would open {url}");
        return;
    }
    if let Err(error) = app.opener().open_url(url, None::<&str>) {
        log::warn!("{url} could not be opened: {error}");
    }
}

fn tooltip(state: &Shown) -> String {
    let said = match state.status.as_str() {
        "ready" => "Ready",
        "reading" => "Getting your games ready",
        "offline" => "Can't reach RetroZone",
        "starting" => "Connecting",
        "no-folders" => "Choose your games folders",
        _ => "Not signed in",
    };
    format!("RetroZone Home - {said}")
}

// ---- what the page can ask ---------------------------------------------
//
// Whatever reaches the engine is async: a command that is not runs on the window's own thread,
// which the engine may be waiting on at that moment (to set the tray's tooltip).

#[tauri::command]
async fn state(app: tauri::State<'_, App>) -> Result<Shown, String> {
    Ok(app.engine.state())
}

#[tauri::command]
async fn sign_in(handle: AppHandle, app: tauri::State<'_, App>, site: String) -> Result<SignIn, String> {
    let sign_in = app.engine.sign_in(&site).await.map_err(|error| format!("{error:#}"))?;
    open(&handle, &sign_in.url);
    Ok(sign_in)
}

#[tauri::command]
async fn open_sign_in(handle: AppHandle, app: tauri::State<'_, App>) -> Result<(), String> {
    if let Some(sign_in) = app.engine.state().sign_in {
        open(&handle, &sign_in.url);
    }
    Ok(())
}

#[tauri::command]
async fn cancel_sign_in(app: tauri::State<'_, App>) -> Result<(), String> {
    app.engine.cancel_sign_in();
    Ok(())
}

#[tauri::command]
async fn sign_out(app: tauri::State<'_, App>) -> Result<(), String> {
    app.engine.sign_out().await;
    Ok(())
}

/// The system's folder picker, and the folder it gave, followed from now on
#[tauri::command]
async fn choose_folder(handle: AppHandle, app: tauri::State<'_, App>) -> Result<(), String> {
    let (picked_tx, picked) = tokio::sync::oneshot::channel();
    let mut dialog = handle.dialog().file().set_title("Choose a folder of games");
    if let Some(window) = handle.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    dialog.pick_folder(move |folder| {
        let _ = picked_tx.send(folder);
    });

    let Some(folder) = picked.await.map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let path = folder.into_path().map_err(|error| error.to_string())?;
    app.engine.add_folder(&path.display().to_string()).map_err(|error| format!("{error:#}"))
}

/// A folder by its path: what a folder dropped on the window comes as
#[tauri::command]
async fn add_folder(app: tauri::State<'_, App>, path: String) -> Result<(), String> {
    app.engine.add_folder(&path).map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn remove_folder(app: tauri::State<'_, App>, path: String) -> Result<(), String> {
    app.engine.remove_folder(&path).map_err(|error| format!("{error:#}"))
}

#[tauri::command]
fn show_folder(handle: AppHandle, path: String) {
    if let Err(error) = handle.opener().open_path(&path, None::<&str>) {
        log::warn!("{path} could not be shown: {error}");
    }
}

#[tauri::command]
fn autostart(handle: AppHandle) -> bool {
    handle.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(handle: AppHandle, on: bool) -> Result<bool, String> {
    let launcher = handle.autolaunch();
    let done = if on { launcher.enable() } else { launcher.disable() };
    done.map_err(|error| error.to_string())?;
    launcher.is_enabled().map_err(|error| error.to_string())
}

#[tauri::command]
async fn open_site(handle: AppHandle, app: tauri::State<'_, App>) -> Result<(), String> {
    open(&handle, &format!("{}/library", app.engine.site()));
    Ok(())
}

/// Closes the window; the program goes on in the tray. Asked for once the player has read that it does
#[tauri::command]
async fn hide(window: tauri::WebviewWindow, app: tauri::State<'_, App>) -> Result<(), String> {
    app.engine.set_told_tray();
    let _ = window.destroy();
    Ok(())
}

#[tauri::command]
fn quit(handle: AppHandle) {
    handle.exit(0);
}

// ---- starting ---------------------------------------------------------------

/// The log goes to a file beside the settings: there is no console to see it in
fn start_log() {
    let path = config::home().join("home.log");
    let _ = std::fs::create_dir_all(config::home());
    // Started afresh once it grows past a couple of megabytes
    let fresh = std::fs::metadata(&path).map(|m| m.len() > 2 << 20).unwrap_or(false);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(!fresh)
        .write(true)
        .truncate(fresh)
        .open(&path);

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off,retrozone_home=info"));
    builder.format_target(false);
    if let Ok(file) = file {
        builder.target(env_logger::Target::Pipe(Box::new(file)));
    }
    builder.init();

    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
}

/// Indexes a folder and writes every SHA-1 it is known by, for setting against the site's
fn debug_index(args: &[String]) {
    let [_, folder, extensions, out] = args else { return };
    let library = Library::new(vec![PathBuf::from(folder)], std::env::temp_dir().join("retrozone-home-debug-index2.json"));
    library.set_extensions(&extensions.split(',').map(String::from).collect::<Vec<_>>());
    if library.look().is_ok() {
        let _ = std::fs::File::create(out).and_then(|mut file| file.write_all(library.debug_sha1s().join("\n").as_bytes()));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("debug-index") {
        debug_index(&args);
        return;
    }
    let hidden = args.iter().any(|arg| arg == "--hidden");
    start_log();
    log::info!("starting{}", if hidden { " in the tray" } else { "" });

    tauri::Builder::default()
        // A second start shows the window of the one already running
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("started again: showing the window");
            show(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::Builder::new().args(["--hidden"]).build())
        .invoke_handler(tauri::generate_handler![
            state,
            sign_in,
            open_sign_in,
            cancel_sign_in,
            sign_out,
            choose_folder,
            add_folder,
            remove_folder,
            show_folder,
            autostart,
            set_autostart,
            open_site,
            hide,
            quit
        ])
        .setup(move |app| {
            let engine = Engine::new(tauri::async_runtime::handle().inner().clone())?;
            let handle = app.handle().clone();
            // The tooltip is set only when what it says changes: setting it is a trip to the window's thread
            let said = std::sync::Mutex::new(String::new());
            engine.on_change(Arc::new(move |state| {
                let _ = handle.emit("state", state);
                let tip = tooltip(state);
                let changed = {
                    let mut said = said.lock().unwrap();
                    let changed = *said != tip;
                    *said = tip.clone();
                    changed
                };
                if changed {
                    if let Some(tray) = handle.tray_by_id(TRAY) {
                        let _ = tray.set_tooltip(Some(tip));
                    }
                }
            }));
            app.manage(App { engine: engine.clone() });

            let open_item = MenuItem::with_id(app, "open", "Open RetroZone Home", true, None::<&str>)?;
            let site_item = MenuItem::with_id(app, "site", "Open RetroZone", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&open_item, &site_item, &separator, &quit_item])?;
            let mut tray = TrayIconBuilder::with_id(TRAY)
                .tooltip(tooltip(&engine.state()))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show(app),
                    "site" => {
                        let site = app.state::<App>().engine.site();
                        open(app, &format!("{site}/library"));
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            engine.start();
            if !hidden {
                show(app.handle());
            }
            Ok(())
        })
        // Closing the window leaves the program running in the tray. The first time, the page says so first
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !window.app_handle().state::<App>().engine.told_tray() {
                    api.prevent_close();
                    let _ = window.emit("close-requested", ());
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("RetroZone Home could not start")
        // The last window closing is not the program ending: it goes on in the tray until it is told to quit
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        });
}
