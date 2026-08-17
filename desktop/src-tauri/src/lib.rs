//! Tauri shell for the native music service.
//!
//! The service remains an independent Rust process in development and release
//! layouts. The shell owns a process it starts and shuts it down on exit; if a
//! compatible local service is already listening, it leaves that process alone.

use std::{
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::Mutex,
    time::{Duration, Instant},
};

use music_engine::mm_server::{MmServerLocation, MmServerSupervisor};

const SERVER_PORT: u16 = 8765;
const RELEASES_URL: &str = "https://github.com/timoncool/MiniMax-Music3-Studio/releases/latest";
const STUDIO_DATA_DIRECTORY: &str = "MiniMax Music3 Studio";

struct ServerProcess(Mutex<Option<Child>>);
struct EngineProcess(Mutex<Option<MmServerSupervisor>>);

/// The bundled runtime can be overridden for development or a portable
/// installation.  The supervisor itself validates that every configured path
/// exists; an unavailable engine is a normal first-run state, not a Tauri
/// startup failure.
fn primary_engine_location() -> MmServerLocation {
    let configured_executable = std::env::var_os("MINIMAX_MM_SERVER_BIN").map(PathBuf::from);
    let bundle_root = std::env::var_os("MINIMAX_MM_SERVER_ROOT")
        .map(PathBuf::from)
        .or_else(|| configured_executable.as_ref().and_then(|path| path.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| executable_directory().join("resources").join("minimaxmusic-cpp"));
    let configured_models_root = std::env::var_os("MINIMAX_MUSIC_MODELS_ROOT")
        .map(PathBuf::from)
        .or_else(|| Some(studio_data_directory().join("models").join("minimaxmusic-cpp")));

    MmServerLocation {
        bundle_root,
        configured_executable,
        configured_models_root,
        host: std::env::var("MINIMAX_MM_SERVER_HOST").ok(),
        port: std::env::var("MINIMAX_MM_SERVER_PORT").ok().and_then(|value| value.parse().ok()),
    }
}

/// Mutable studio data must not be derived from the process working directory
/// or from the installed executable directory: a Start Menu shortcut is free
/// to choose either of those. Environment overrides remain authoritative for
/// portable/development installations.
fn studio_data_directory() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")) {
            return PathBuf::from(root).join(STUDIO_DATA_DIRECTORY);
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(root).join("minimax-music3-studio");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("minimax-music3-studio");
        }
    }

    std::env::temp_dir().join("minimax-music3-studio")
}

/// Sets the same deterministic roots before the Axum bridge is spawned. Its
/// environment is inherited by the bridge and the subsequently started native
/// engine, so both always refer to one model library.
fn configure_studio_runtime_paths() {
    let data_root = studio_data_directory();
    if std::env::var_os("MINIMAX_MUSIC_MODELS_ROOT").is_none() {
        unsafe {
            std::env::set_var(
                "MINIMAX_MUSIC_MODELS_ROOT",
                data_root.join("models").join("minimaxmusic-cpp"),
            );
        }
    }
    if std::env::var_os("MINIMAX_STUDIO_SETTINGS_PATH").is_none() {
        unsafe {
            std::env::set_var("MINIMAX_STUDIO_SETTINGS_PATH", data_root.join("studio-settings.json"));
        }
    }

    let location = primary_engine_location();
    let host = location.host.unwrap_or_else(|| "127.0.0.1".into());
    let port = location.port.unwrap_or(8086);
    if std::env::var_os("MINIMAX_MUSIC_CPP_BASE_URL").is_none() {
        unsafe {
            std::env::set_var("MINIMAX_MUSIC_CPP_BASE_URL", format!("http://{host}:{port}"));
        }
    }
}

fn start_primary_engine() -> (Option<MmServerSupervisor>, Option<String>) {
    let config = match primary_engine_location().resolve() {
        Ok(config) => config,
        Err(error) => return (None, Some(error.to_string())),
    };

    // The Axum bridge consumes this documented base URL.  Set it before that
    // child is started, so a user-selected loopback port remains consistent.
    unsafe {
        std::env::set_var(
            "MINIMAX_MUSIC_CPP_BASE_URL",
            format!("http://{}:{}", config.host, config.port),
        );
    }

    let mut supervisor = match MmServerSupervisor::new(config) {
        Ok(supervisor) => supervisor,
        Err(error) => return (None, Some(error.to_string())),
    };
    match supervisor.ensure_started(Duration::from_secs(30)) {
        Ok(_) => (Some(supervisor), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

#[tauri::command]
fn start_primary_engine_after_setup(engine: tauri::State<'_, EngineProcess>) -> Result<(), String> {
    let mut engine = engine
        .0
        .lock()
        .map_err(|_| "local engine state is unavailable".to_string())?;

    if let Some(supervisor) = engine.as_mut() {
        supervisor
            .ensure_started(Duration::from_secs(30))
            .map_err(|error| error.to_string())?;
        return Ok(());
    }

    let (supervisor, error) = start_primary_engine();
    let supervisor = supervisor.ok_or_else(|| {
        error.unwrap_or_else(|| "could not start the local minimaxmusic.cpp engine".into())
    })?;
    *engine = Some(supervisor);
    Ok(())
}

fn service_is_ready() -> bool {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, SERVER_PORT);
    TcpStream::connect_timeout(&address.into(), Duration::from_millis(150)).is_ok()
}

fn wait_until_ready(timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if service_is_ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    false
}

fn executable_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn is_portable() -> bool {
    executable_directory().join("portable.flag").is_file()
}

fn repository_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("MINIMAX_MUSIC3_STUDIO_ROOT") {
        return Some(PathBuf::from(root));
    }

    let mut directory = executable_directory();
    for _ in 0..8 {
        if directory.join("crates").is_dir() && directory.join("Cargo.toml").is_file() {
            return Some(directory);
        }
        if !directory.pop() {
            break;
        }
    }
    None
}

fn server_command() -> Result<Command, String> {
    if let Some(path) = std::env::var_os("MINIMAX_MUSIC_SERVER_BIN") {
        return Ok(Command::new(path));
    }

    let executable_name = if cfg!(windows) { "music-server.exe" } else { "music-server" };
    let adjacent = executable_directory().join(executable_name);
    if adjacent.is_file() {
        return Ok(Command::new(adjacent));
    }

    let bundled_resource = executable_directory().join("resources").join(executable_name);
    if bundled_resource.is_file() {
        return Ok(Command::new(bundled_resource));
    }

    let root = repository_root().ok_or_else(|| {
        "could not locate MiniMax Music3 Studio workspace; set MINIMAX_MUSIC_SERVER_BIN".to_string()
    })?;
    let debug_binary = root.join("target").join("debug").join(executable_name);
    if debug_binary.is_file() {
        return Ok(Command::new(debug_binary));
    }

    let mut command = Command::new("cargo");
    command.current_dir(root).args(["run", "-p", "music-server"]);
    Ok(command)
}

fn start_service() -> Result<Option<Child>, String> {
    if service_is_ready() {
        return Ok(None);
    }

    let child = server_command()
        .and_then(|mut command| command.spawn().map_err(|error| error.to_string()))?;

    if wait_until_ready(Duration::from_secs(30)) {
        Ok(Some(child))
    } else {
        let mut child = child;
        let _ = child.kill();
        Err("music-server did not become ready on 127.0.0.1:8765".into())
    }
}

fn spawn_update_check(app: tauri::AppHandle, portable: bool) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    use tauri_plugin_updater::UpdaterExt;

    tauri::async_runtime::spawn(async move {
        let updater = match app.updater() {
            Ok(updater) => updater,
            Err(_) => return,
        };
        let update = match updater.check().await {
            Ok(Some(update)) => update,
            _ => return,
        };

        let version = update.version.clone();
        if portable {
            let open_release = app
                .dialog()
                .message(format!("MiniMax Music3 Studio {version} is available. Open the download page?"))
                .title("MiniMax Music3 Studio update")
                .kind(MessageDialogKind::Info)
                .buttons(MessageDialogButtons::OkCancelCustom("Open".into(), "Later".into()))
                .blocking_show();
            if open_release {
                use tauri_plugin_opener::OpenerExt;
                let _ = app.opener().open_url(RELEASES_URL, None::<&str>);
            }
            return;
        }

        let install = app
            .dialog()
            .message(format!("MiniMax Music3 Studio {version} is available. Install it now?"))
            .title("MiniMax Music3 Studio update")
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom("Install".into(), "Later".into()))
            .blocking_show();
        if !install {
            return;
        }

        if let Err(error) = update.download_and_install(|_, _| {}, || {}).await {
            app.dialog()
                .message(format!("Could not install the update: {error}"))
                .title("MiniMax Music3 Studio update")
                .kind(MessageDialogKind::Error)
                .blocking_show();
            return;
        }
        app.restart();
    });
}

#[cfg(windows)]
fn hide_own_console_window() {
    unsafe extern "system" {
        fn GetConsoleWindow() -> isize;
        fn GetConsoleProcessList(processes: *mut u32, count: u32) -> u32;
        fn ShowWindow(window: isize, command: i32) -> i32;
    }

    unsafe {
        let mut processes = [0_u32; 4];
        if GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) == 1 {
            let window = GetConsoleWindow();
            if window != 0 {
                ShowWindow(window, 0);
            }
        }
    }
}

pub fn run() {
    #[cfg(windows)]
    hide_own_console_window();

    if is_portable() && std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        // The process is still single-threaded here, before Tauri or its
        // worker threads are created, so updating its child WebView environment
        // cannot race with an environment read.
        unsafe {
            std::env::set_var(
                "WEBVIEW2_USER_DATA_FOLDER",
                executable_directory().join("webview-data"),
            );
        }
    }

    // The native engine is started only by the setup gate after a complete
    // verified profile is present. This keeps first launch download-free and
    // avoids starting a runtime against partial weights.
    configure_studio_runtime_paths();

    let child = match start_service() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("failed to start music-server: {error}");
            None
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(ServerProcess(Mutex::new(child)))
        .manage(EngineProcess(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![start_primary_engine_after_setup])
        .setup(|app| {
            spawn_update_check(app.handle().clone(), is_portable());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running MiniMax Music3 Studio");
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Ok(child) = self.0.get_mut() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        if let Ok(engine) = self.0.get_mut() {
            // Dropping the supervisor requests its documented graceful
            // shutdown before it falls back to terminating only this owned
            // child process.
            let _ = engine.take();
        }
    }
}
