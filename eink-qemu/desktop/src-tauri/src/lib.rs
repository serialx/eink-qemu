//! eink-qemu desktop: Tauri commands over the emulator session, the SD card
//! image and the app's settings. The WebView is the browser page's interface.

mod display;
mod qmp;
pub mod sd;
mod session;
mod settings;

use serde::Serialize;
use session::{AppError, QemuPaths, Session, SessionInfo, StartOptions};
use settings::{Settings, SettingsPatch};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::{Manager, RunEvent, State};

const FLASH_SIZE: usize = 0x100_0000;
const MAX_FIRMWARE: u64 = 64 * 1024 * 1024;

struct App {
    session: Arc<Session>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    locks: PathBuf,
    card_lock: Mutex<Option<std::fs::File>>,
}

impl App {
    fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, AppError> {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        settings.save(&self.settings_path)?;
        Ok(settings.clone())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardInfo {
    path: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FirmwareInfo {
    path: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Attached {
    session: SessionInfo,
    settings: Settings,
    card: Option<CardInfo>,
    firmware: Option<FirmwareInfo>,
    platform: &'static str,
    /// EINK_QEMU_AUTOSTART: start the chosen firmware as soon as the WebView attaches.
    autostart: bool,
}

fn card_info(path: &Path) -> Option<CardInfo> {
    let size = std::fs::metadata(path).ok()?.len();
    Some(CardInfo {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        size,
    })
}

fn firmware_info(path: &Path) -> Option<FirmwareInfo> {
    let size = std::fs::metadata(path).ok()?.len();
    Some(FirmwareInfo {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        size,
    })
}

/// A card is used by one app instance at a time: a lock file per card in
/// the app data directory (QEMU's own image locks guard other QEMUs).
fn lock_card(locks: &Path, card: &Path) -> Result<std::fs::File, AppError> {
    use std::hash::{Hash, Hasher};
    std::fs::create_dir_all(locks)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    card.hash(&mut hasher);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(locks.join(format!("{:016x}.lock", hasher.finish())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(AppError::new(
            "EBUSY",
            format!("{} is open in another eink-qemu window.", card.display()),
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Check that `path` looks like an SD image and return its canonical form.
fn validate_card(path: &Path) -> Result<PathBuf, AppError> {
    let path = std::fs::canonicalize(path)
        .map_err(|_| AppError::new("ENOENT", format!("{} does not exist.", path.display())))?;
    let meta = std::fs::metadata(&path)?;
    if !meta.is_file() {
        return Err(AppError::new("EINVAL", "Choose an SD image file."));
    }
    if meta.len() == 0 || meta.len() % 512 != 0 {
        return Err(AppError::new("EINVAL", "SD image must contain a whole number of 512-byte sectors."));
    }
    Ok(path)
}

#[tauri::command]
fn attach(
    app: State<'_, App>,
    logs: Channel<Vec<String>>,
    frames: Channel<InvokeResponseBody>,
) -> Attached {
    app.session.attach(logs, frames);
    let settings = app.settings.lock().unwrap().clone();
    Attached {
        session: app.session.info(),
        firmware: settings.firmware.as_deref().and_then(firmware_info),
        card: card_info(&app.session.card()),
        settings,
        platform: std::env::consts::OS,
        autostart: std::env::var_os("EINK_QEMU_AUTOSTART").is_some_and(|v| v == "1"),
    }
}

#[tauri::command]
fn settings_set(app: State<'_, App>, patch: SettingsPatch) -> Result<Settings, AppError> {
    let mut error = None;
    let settings = app.update(|settings| {
        if let Err(message) = settings.apply(patch) {
            error = Some(AppError::new("EINVAL", message));
        }
    })?;
    error.map_or(Ok(settings), Err)
}

#[tauri::command]
fn firmware_choose(app: State<'_, App>, path: PathBuf) -> Result<FirmwareInfo, AppError> {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    if !matches!(extension.as_deref(), Some("bin" | "xota")) {
        return Err(AppError::new("EINVAL", "Choose one firmware file: a .bin, or an X4 Pro .xota update."));
    }
    let meta = std::fs::metadata(&path).map_err(|_| AppError::new("ENOENT", format!("{} does not exist.", path.display())))?;
    if !meta.is_file() || meta.len() > MAX_FIRMWARE {
        return Err(AppError::new("EINVAL", "Firmware must be a file of at most 64 MiB."));
    }
    let info = firmware_info(&path).ok_or_else(|| AppError::new("EINVAL", "Unreadable firmware path."))?;
    app.update(|settings| settings.firmware = Some(path))?;
    Ok(info)
}

#[tauri::command]
async fn firmware_read(app: State<'_, App>) -> Result<Response, AppError> {
    let path = app.settings.lock().unwrap().firmware.clone()
        .ok_or_else(|| AppError::new("ENOENT", "Choose a firmware .bin, an X4 Pro .xota update, or a 16 MiB flash dump."))?;
    let bytes = tokio::fs::read(&path).await
        .map_err(|e| AppError::new("ENOENT", format!("Cannot read {}: {e}", path.display())))?;
    if bytes.len() as u64 > MAX_FIRMWARE {
        return Err(AppError::new("EINVAL", "Firmware must be a file of at most 64 MiB."));
    }
    Ok(Response::new(bytes))
}

/// Body: the composed 16 MiB flash image. Header `x-options`: StartOptions JSON.
#[tauri::command]
async fn start(app: State<'_, App>, request: Request<'_>) -> Result<SessionInfo, AppError> {
    let InvokeBody::Raw(flash) = request.body() else {
        return Err(AppError::new("EINVAL", "Expected the flash image as the request body."));
    };
    if flash.len() != FLASH_SIZE {
        return Err(AppError::new("EINVAL", "The composed flash image must be 16 MiB."));
    }
    let options: StartOptions = request
        .headers()
        .get("x-options")
        .and_then(|value| value.to_str().ok())
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(|| AppError::new("EINVAL", "Missing start options."))?;
    if !["x3", "x4", "x4pro", "picco"].contains(&options.machine.as_str()) {
        return Err(AppError::new("EINVAL", format!("Unknown device: {}.", options.machine)));
    }
    if !settings::PANELS.contains(&options.panel.as_str()) {
        return Err(AppError::new("EINVAL", format!("Unknown X4 Pro panel: {}.", options.panel)));
    }
    let flash = flash.clone();
    app.session.start(options, flash).await
}

#[tauri::command]
async fn stop(app: State<'_, App>) -> Result<(), AppError> {
    app.session.stop().await
}

#[tauri::command]
async fn reset(app: State<'_, App>) -> Result<(), AppError> {
    app.session.reset().await
}

// Synchronous: commands run in arrival order, so a release follows its press.
#[tauri::command]
fn key(app: State<'_, App>, key: String, down: bool) {
    app.session.key(key, down);
}

#[tauri::command]
fn touch(app: State<'_, App>, x: f64, y: f64, down: bool) {
    app.session.touch(x, y, down);
}

#[tauri::command]
async fn card_open(app: State<'_, App>, path: PathBuf) -> Result<CardInfo, AppError> {
    let path = validate_card(&path)?;
    let lock = lock_card(&app.locks, &path)?;
    app.session.set_card(path.clone()).await?;
    *app.card_lock.lock().unwrap() = Some(lock);
    app.update(|settings| settings.card = Some(path.clone()))?;
    card_info(&path).ok_or_else(|| AppError::new("EIO", "Cannot read the SD image."))
}

/// A new blank 64 MiB FAT32 card at `path`, which becomes the card in use.
#[tauri::command]
async fn card_new(app: State<'_, App>, path: PathBuf) -> Result<CardInfo, AppError> {
    if app.session.info().state != "idle" {
        return Err(AppError::new("EBUSY", "Stop the emulator before changing the SD card."));
    }
    let target = path.clone();
    tokio::task::spawn_blocking(move || sd::create_blank(&target))
        .await
        .map_err(|_| AppError::new("EIO", "Could not create the card."))??;
    card_open(app, path).await
}

/// Save a copy of the card (the guest is paused while it is copied).
#[tauri::command]
async fn card_copy(app: State<'_, App>, dest: PathBuf) -> Result<(), AppError> {
    app.session
        .with_card(false, move |card| {
            if std::fs::canonicalize(&dest).ok().as_deref() == Some(card) {
                return Err(sd::SdError::new("EINVAL", "Choose a different file for the copy."));
            }
            std::fs::copy(card, &dest).map(|_| ()).map_err(|e| sd::SdError::new("EIO", e.to_string()))
        })
        .await
}

#[tauri::command]
async fn sd_list(app: State<'_, App>, path: String) -> Result<sd::SdListing, AppError> {
    app.session.with_card(false, move |card| sd::list(card, &path)).await
}

#[tauri::command]
async fn sd_plan_upload(app: State<'_, App>, dir: String, sources: Vec<PathBuf>) -> Result<sd::UploadPlan, AppError> {
    app.session.with_card(false, move |card| sd::plan_upload(card, &dir, &sources)).await
}

#[tauri::command]
async fn sd_upload(app: State<'_, App>, dir: String, sources: Vec<PathBuf>) -> Result<sd::UploadPlan, AppError> {
    app.session.with_card(true, move |card| sd::upload(card, &dir, &sources)).await
}

#[tauri::command]
async fn sd_export(app: State<'_, App>, path: String, dest: PathBuf) -> Result<(), AppError> {
    app.session.with_card(false, move |card| sd::export(card, &path, &dest)).await
}

#[tauri::command]
async fn sd_mkdir(app: State<'_, App>, path: String) -> Result<(), AppError> {
    app.session.with_card(true, move |card| sd::mkdir(card, &path)).await
}

#[tauri::command]
async fn sd_rename(app: State<'_, App>, path: String, name: String) -> Result<(), AppError> {
    app.session.with_card(true, move |card| sd::rename(card, &path, &name)).await
}

#[tauri::command]
async fn sd_remove(app: State<'_, App>, path: String) -> Result<(), AppError> {
    app.session.with_card(true, move |card| sd::remove(card, &path)).await
}

/// Body: PNG bytes. Header `x-path`: where the save dialog said to write.
#[tauri::command]
async fn screenshot_save(request: Request<'_>) -> Result<(), AppError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(AppError::new("EINVAL", "Expected PNG bytes."));
    };
    let path = request
        .headers()
        .get("x-path")
        .and_then(|value| value.to_str().ok())
        .map(|text| PathBuf::from(percent_decode(text)))
        .ok_or_else(|| AppError::new("EINVAL", "Missing screenshot path."))?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(AppError::new("EINVAL", "Expected PNG bytes."));
    }
    tokio::fs::write(&path, bytes).await?;
    Ok(())
}

/// Headers are ASCII: the WebView percent-encodes the path.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn qemu_paths(app: &tauri::AppHandle) -> QemuPaths {
    if let Some(bin) = std::env::var_os("EINK_QEMU_BIN") {
        return QemuPaths { bin: bin.into(), share: std::env::var_os("EINK_QEMU_SHARE").map(Into::into) };
    }
    if let Ok(resources) = app.path().resource_dir() {
        let bin = resources.join("qemu").join("bin");
        if bin.is_dir() {
            return QemuPaths { bin, share: Some(resources.join("qemu").join("share")) };
        }
    }
    // Development: the repository's build directory.
    QemuPaths { bin: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build"), share: None }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // EINK_QEMU_DATA_DIR isolates an instance (settings, default card,
            // locks), e.g. for tests running beside a normal window.
            let (config, data) = match std::env::var_os("EINK_QEMU_DATA_DIR") {
                Some(dir) => (PathBuf::from(&dir), PathBuf::from(dir)),
                None => (app.path().app_config_dir()?, app.path().app_data_dir()?),
            };
            std::fs::create_dir_all(&data)?;
            let settings_path = config.join("settings.json");
            let mut settings = Settings::load(&settings_path);
            let locks = data.join("locks");
            // The last card, or the default card (created blank on first run).
            let default_card = data.join("sd.img");
            let mut chosen = settings.card.clone().and_then(|card| validate_card(&card).ok());
            let mut lock = chosen.as_ref().and_then(|card| lock_card(&locks, card).ok());
            if lock.is_none() {
                if !default_card.exists() {
                    sd::create_blank(&default_card)?;
                }
                let card = validate_card(&default_card).map_err(|e| e.message)?;
                lock = lock_card(&locks, &card).ok();
                chosen = Some(card);
                settings.card = chosen.clone();
            }
            let card = chosen.unwrap();
            let session = Session::new(app.handle().clone(), qemu_paths(app.handle()), card);
            if lock.is_none() {
                session.log("The SD card is open in another eink-qemu window; changes may conflict.");
            }
            let _ = settings.save(&settings_path);
            app.manage(App {
                session,
                settings: Mutex::new(settings),
                settings_path,
                locks,
                card_lock: Mutex::new(lock),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            attach,
            settings_set,
            firmware_choose,
            firmware_read,
            start,
            stop,
            reset,
            key,
            touch,
            card_open,
            card_new,
            card_copy,
            sd_list,
            sd_plan_upload,
            sd_upload,
            sd_export,
            sd_mkdir,
            sd_rename,
            sd_remove,
            screenshot_save,
        ])
        .build(tauri::generate_context!())
        .expect("error while building eink-qemu");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            let session = handle.state::<App>().session.clone();
            tauri::async_runtime::block_on(session.shutdown());
        }
    });
}
