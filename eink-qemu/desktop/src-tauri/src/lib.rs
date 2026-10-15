//! eink-qemu desktop: Tauri commands over the emulator session, the SD card
//! image and the app's settings. The WebView is the browser page's interface.
//!
//! Host files are reached only through paths the user chose: native dialogs
//! run here in Rust, and files dropped on the window are recorded from the
//! window's drop event. Commands that take a host path from the WebView
//! accept only those recorded paths.

mod display;
mod heap;
mod host;
mod qmp;
pub mod sd;
mod session;
mod settings;

use serde::Serialize;
use session::{AppError, Card, QemuPaths, Reservation, Session, SessionInfo, StartOptions};
use settings::{Settings, SettingsPatch};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::{DragDropEvent, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;

const FLASH_SIZE: usize = 0x100_0000;
const MAX_FIRMWARE: u64 = 64 * 1024 * 1024;
const MAX_SCREENSHOT: usize = 64 * 1024 * 1024;
const MAX_GRANTS: usize = 20_000;

struct App {
    session: Arc<Session>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    locks: PathBuf,
    /// Host paths the user chose in a dialog or dropped on the window.
    grants: Mutex<HashSet<PathBuf>>,
    /// Startup problems the WebView shows once it attaches.
    problems: Mutex<Vec<String>>,
    /// The heap view's ELF as last read.
    elf: Mutex<Option<Arc<heap::Elf>>>,
}

impl App {
    /// Change the settings and save them; nothing changes when saving fails.
    fn update(&self, change: impl FnOnce(&mut Settings) -> Result<(), AppError>) -> Result<Settings, AppError> {
        let mut settings = self.settings.lock().unwrap();
        let mut next = settings.clone();
        change(&mut next)?;
        next.save(&self.settings_path)?;
        *settings = next;
        Ok(settings.clone())
    }

    fn grant(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut grants = self.grants.lock().unwrap();
        if grants.len() > MAX_GRANTS {
            grants.clear();
        }
        grants.extend(paths);
    }

    fn granted(&self, path: &Path) -> Result<(), AppError> {
        if self.grants.lock().unwrap().contains(path) {
            Ok(())
        } else {
            Err(AppError::new("EPERM", format!("{} was not chosen in a dialog or dropped on the window.", path.display())))
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileInfo {
    path: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Attached {
    session: SessionInfo,
    settings: Settings,
    card: Option<FileInfo>,
    firmware: Option<FileInfo>,
    /// The heap view's ELF.
    elf: Option<FileInfo>,
    platform: &'static str,
    /// EINK_QEMU_AUTOSTART: start the chosen firmware as soon as the WebView attaches.
    autostart: bool,
    /// EINK_QEMU_PANE: the output tab to open ("console" or "heap").
    pane: Option<String>,
    /// Startup problems to show: a saved card or firmware that is unusable.
    problems: Vec<String>,
}

fn file_info(path: &Path) -> Option<FileInfo> {
    let size = std::fs::metadata(path).ok()?.len();
    Some(FileInfo {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        size,
    })
}

/// The lock directory shared by every instance of this user, whatever data
/// directory each uses, so a card has one owner across instances.
fn locks_dir(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("eink-qemu"))
        .join("locks")
}

/// A stable name for a card path, also before the file exists.
fn card_key(path: &Path) -> PathBuf {
    if let Ok(path) = dunce::canonicalize(path) {
        return path;
    }
    match (path.parent().and_then(|p| dunce::canonicalize(p).ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// The lock file for one key (a path or a file identity).
fn lock_file(locks: &Path, key: &str) -> Result<std::fs::File, AppError> {
    std::fs::create_dir_all(locks)?;
    // FNV-1a: a fixed hash, so every build names a key's lock file alike.
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(locks.join(format!("{hash:016x}.lock")))?;
    Ok(file)
}

/// A file's identity, so hard links to one card share one owner.
fn identity_key(path: &Path) -> Option<String> {
    host::file_identity(path).map(|(device, file)| format!("id:{device}:{file}"))
}

/// Exclusive use of a host file among all eink-qemu instances of this user:
/// a lock on its path (also before it exists) and, when it exists, on its
/// identity. Cards hold one while open; writes to a host file hold one for
/// the whole write, so they can never land on an open card.
fn reserve(locks: &Path, path: &Path) -> Result<Reservation, AppError> {
    let mut reservation = reserve_path(locks, path)?;
    reservation.refresh(locks, path)?;
    Ok(reservation)
}

/// Only the path lock: for a hard link to the card this instance already
/// owns, whose identity lock it holds.
fn reserve_path(locks: &Path, path: &Path) -> Result<Reservation, AppError> {
    let mut key = format!("path:{}", card_key(path).display());
    if cfg!(windows) {
        // NTFS names ignore case, so C:\Card.img and c:\card.img are one card.
        key = key.to_lowercase();
    }
    let file = try_lock(locks, &key, path)?;
    Ok(Reservation { path: file, identity: None })
}

fn try_lock(locks: &Path, key: &str, path: &Path) -> Result<std::fs::File, AppError> {
    let file = lock_file(locks, key)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(AppError::new(
            "EBUSY",
            format!("{} is an SD card open in eink-qemu.", path.display()),
        )),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

impl Reservation {
    /// Lock the file's current identity (after it was created or replaced),
    /// releasing the identity of a file it replaced.
    fn refresh(&mut self, locks: &Path, path: &Path) -> Result<(), AppError> {
        let Some(id) = identity_key(path) else { return Ok(()) };
        if self.identity.as_ref().is_some_and(|(held, _)| *held == id) {
            return Ok(());
        }
        let file = try_lock(locks, &id, path)?;
        self.identity = Some((id, file));
        Ok(())
    }
}

/// Check that `path` looks like an SD image and return its canonical form.
fn validate_card(path: &Path) -> Result<PathBuf, AppError> {
    let path = dunce::canonicalize(path)
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

fn open_card(locks: &Path, path: &Path) -> Result<Card, AppError> {
    let path = validate_card(path)?;
    let lock = reserve(locks, &path)?;
    Ok(Card { path, lock: lock.into() })
}

fn check_firmware(path: &Path) -> Result<FileInfo, AppError> {
    let meta = std::fs::metadata(path).map_err(|_| AppError::new("ENOENT", format!("{} does not exist.", path.display())))?;
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase());
    if !matches!(extension.as_deref(), Some("bin" | "xota")) {
        return Err(AppError::new("EINVAL", "Choose one firmware file: a .bin, or an X4 Pro .xota update."));
    }
    if !meta.is_file() || meta.len() > MAX_FIRMWARE {
        return Err(AppError::new("EINVAL", "Firmware must be a file of at most 64 MiB."));
    }
    file_info(path).ok_or_else(|| AppError::new("EINVAL", "Unreadable firmware path."))
}

/// Native dialogs, run from Rust so the WebView never names host paths.
enum Pick<'a> {
    Open { title: &'a str, filter: Option<(&'a str, &'a [&'a str])> },
    OpenMany { title: &'a str },
    Save { title: &'a str, name: &'a str, filter: Option<(&'a str, &'a [&'a str])> },
}

async fn pick(app: &tauri::AppHandle, pick: Pick<'_>) -> Vec<PathBuf> {
    let mut builder = app.dialog().file();
    if let Some(window) = app.get_webview_window("main") {
        builder = builder.set_parent(&window);
    }
    let (tx, rx) = oneshot::channel::<Vec<PathBuf>>();
    let one = |path: Option<tauri_plugin_dialog::FilePath>| path.and_then(|p| p.into_path().ok()).into_iter().collect::<Vec<_>>();
    match pick {
        Pick::Open { title, filter } => {
            builder = builder.set_title(title);
            if let Some((name, extensions)) = filter {
                builder = builder.add_filter(name, extensions);
            }
            builder.pick_file(move |path| {
                let _ = tx.send(one(path));
            });
        }
        Pick::OpenMany { title } => {
            builder.set_title(title).pick_files(move |paths| {
                let paths = paths.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).collect();
                let _ = tx.send(paths);
            });
        }
        Pick::Save { title, name, filter } => {
            builder = builder.set_title(title).set_file_name(name);
            if let Some((name, extensions)) = filter {
                builder = builder.add_filter(name, extensions);
            }
            builder.save_file(move |path| {
                let _ = tx.send(one(path));
            });
        }
    }
    rx.await.unwrap_or_default()
}

#[tauri::command]
fn attach(
    app: State<'_, App>,
    logs: Channel<Vec<String>>,
    frames: Channel<InvokeResponseBody>,
) -> Attached {
    app.session.attach(logs, frames);
    let settings = app.settings.lock().unwrap().clone();
    let firmware = settings.firmware.as_deref().and_then(|path| check_firmware(path).ok());
    let elf = settings.elf.as_deref().and_then(file_info);
    Attached {
        elf,
        session: app.session.info(),
        firmware,
        card: app.session.card_path().as_deref().and_then(file_info),
        settings,
        platform: std::env::consts::OS,
        autostart: std::env::var_os("EINK_QEMU_AUTOSTART").is_some_and(|v| v == "1"),
        pane: std::env::var("EINK_QEMU_PANE").ok(),
        problems: std::mem::take(&mut *app.problems.lock().unwrap()),
    }
}

/// The WebView's own diagnostics, for the EINK_QEMU_LOG file.
#[tauri::command]
fn note(app: State<'_, App>, line: String) {
    app.session.note(&line);
}

#[tauri::command]
fn settings_set(app: State<'_, App>, patch: SettingsPatch) -> Result<Settings, AppError> {
    app.update(|settings| settings.apply(patch).map_err(|message| AppError::new("EINVAL", message)))
}

/// Choose the firmware in a native dialog. None when cancelled.
#[tauri::command]
async fn firmware_pick(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<FileInfo>, AppError> {
    let Some(path) = pick(&handle, Pick::Open { title: "Choose firmware", filter: Some(("Firmware", &["bin", "xota"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let info = check_firmware(&path)?;
    app.update(|settings| {
        settings.firmware = Some(path);
        Ok(())
    })?;
    Ok(Some(info))
}

/// Use a firmware file that was dropped on the window.
#[tauri::command]
fn firmware_use(app: State<'_, App>, path: PathBuf) -> Result<FileInfo, AppError> {
    app.granted(&path)?;
    let info = check_firmware(&path)?;
    app.update(|settings| {
        settings.firmware = Some(path);
        Ok(())
    })?;
    Ok(info)
}

#[tauri::command]
async fn firmware_read(app: State<'_, App>) -> Result<Response, AppError> {
    let path = app.settings.lock().unwrap().firmware.clone()
        .ok_or_else(|| AppError::new("ENOENT", "Choose a firmware .bin, an X4 Pro .xota update, or a 16 MiB flash dump."))?;
    check_firmware(&path)?;
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

async fn use_card(app: &App, card: Card) -> Result<FileInfo, AppError> {
    let path = card.path.clone();
    app.session
        .select_card(card, |path| {
            app.update(|settings| {
                settings.card = Some(path.to_path_buf());
                Ok(())
            })
            .map(|_| ())
        })
        .await?;
    file_info(&path).ok_or_else(|| AppError::new("EIO", "Cannot read the SD image."))
}

/// Open another SD image (native dialog). None when cancelled.
#[tauri::command]
async fn card_pick_open(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<FileInfo>, AppError> {
    if app.session.info().state != "idle" {
        return Err(AppError::new("EBUSY", "Stop the emulator before changing the SD card."));
    }
    let Some(path) = pick(&handle, Pick::Open { title: "Open SD image", filter: Some(("SD image", &["img", "bin"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let card = open_card(&app.locks, &path)?;
    use_card(&app, card).await.map(Some)
}

/// Make a new blank 64 MiB FAT32 card (native save dialog), which becomes
/// the card in use. The destination is locked before it is replaced.
#[tauri::command]
async fn card_pick_new(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<FileInfo>, AppError> {
    if app.session.info().state != "idle" {
        return Err(AppError::new("EBUSY", "Stop the emulator before changing the SD card."));
    }
    let Some(path) = pick(&handle, Pick::Save { title: "New blank SD card", name: "sd.img", filter: Some(("SD image", &["img"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let locks = app.locks.clone();
    // The card in use is replaced in place under its own reservation; any
    // other destination is reserved before anything is written.
    let made = app
        .session
        .while_idle(|current| {
            // Only the very same path is replaced in place; a hard link to
            // the card in use is another name, which becomes its own card.
            let ours = current.is_some_and(|card| card_key(&card.path) == card_key(&path));
            if let Some(card) = current.filter(|_| ours) {
                // Replace the card itself, not a symbolic link that names it.
                sd::create_blank(&card.path).map_err(AppError::from)?;
                // The blank card is a new file: its identity replaces the old one.
                card.lock.lock().unwrap().refresh(&locks, &card.path)?;
                Ok(None)
            } else {
                let alias = current.is_some_and(|card| identity_key(&card.path).is_some() && identity_key(&card.path) == identity_key(&path));
                let mut reservation = if alias { reserve_path(&locks, &path)? } else { reserve(&locks, &path)? };
                sd::create_blank(&path).map_err(AppError::from)?;
                reservation.refresh(&locks, &path)?;
                Ok(Some(reservation))
            }
        })
        .await?;
    match made {
        Some(lock) => use_card(&app, Card { path: validate_card(&path)?, lock: lock.into() }).await.map(Some),
        None => Ok(file_info(&path)),
    }
}

/// Save a copy of the card (native save dialog; the guest is paused while
/// it is copied). Returns where it went, or None when cancelled.
#[tauri::command]
async fn card_pick_copy(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<String>, AppError> {
    let name = app.session.card_path().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "sd.img".into());
    let Some(dest) = pick(&handle, Pick::Save { title: "Save a copy of the SD card", name: &name, filter: Some(("SD image", &["img"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    // Held until the copy is written, so no instance opens it meanwhile.
    let _reservation = reserve(&app.locks, &dest)?;
    let target = dest.clone();
    app.session
        .with_card(false, move |card| {
            let same_file = identity_key(&target).is_some() && identity_key(&target) == identity_key(card);
            if card_key(&target) == card_key(card) || same_file {
                return Err(sd::SdError::new("EINVAL", "Choose a different file for the copy."));
            }
            // Through a temporary file beside the destination, then renamed,
            // so a failure never leaves half a card there.
            let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let temp = tempfile::Builder::new().prefix(".eink-qemu-copy-").tempfile_in(dir).map_err(|e| sd::SdError::new("EIO", e.to_string()))?;
            std::fs::copy(card, temp.path()).map_err(|e| sd::SdError::new("EIO", e.to_string()))?;
            temp.persist(&target).map(|_| ()).map_err(|e| sd::SdError::new("EIO", e.error.to_string()))
        })
        .await?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

#[tauri::command]
async fn sd_list(app: State<'_, App>, path: String) -> Result<sd::SdListing, AppError> {
    app.session.with_card(false, move |card| sd::list(card, &path)).await
}

/// Choose host files to copy onto the card (native dialog). They become
/// valid sources for sd_plan_upload and sd_upload.
#[tauri::command]
async fn sd_pick_sources(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Vec<String>, AppError> {
    let paths = pick(&handle, Pick::OpenMany { title: "Copy to the SD card" }).await;
    app.grant(paths.iter().cloned());
    Ok(paths.iter().map(|p| p.to_string_lossy().into_owned()).collect())
}

fn granted_sources(app: &App, sources: &[PathBuf]) -> Result<(), AppError> {
    sources.iter().try_for_each(|source| app.granted(source))
}

#[tauri::command]
async fn sd_plan_upload(app: State<'_, App>, dir: String, sources: Vec<PathBuf>) -> Result<sd::UploadPlan, AppError> {
    granted_sources(&app, &sources)?;
    app.session.with_card(false, move |card| sd::plan_upload(card, &dir, &sources)).await
}

#[tauri::command]
/// `confirmed`: the plan's conflicts the user agreed to replace; the upload
/// is refused if the card changed since.
async fn sd_upload(app: State<'_, App>, dir: String, sources: Vec<PathBuf>, confirmed: Vec<String>) -> Result<sd::UploadPlan, AppError> {
    granted_sources(&app, &sources)?;
    app.session.with_card(true, move |card| sd::upload(card, &dir, &sources, &confirmed)).await
}

/// Save a file from the card to the computer (native save dialog).
#[tauri::command]
async fn sd_pick_export(handle: tauri::AppHandle, app: State<'_, App>, path: String) -> Result<Option<String>, AppError> {
    let name = path.rsplit('/').next().unwrap_or("file").to_string();
    let Some(dest) = pick(&handle, Pick::Save { title: "Save from the SD card", name: &name, filter: None })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let _reservation = reserve(&app.locks, &dest)?;
    let target = dest.clone();
    app.session.with_card(false, move |card| sd::export(card, &path, &target)).await?;
    Ok(Some(dest.to_string_lossy().into_owned()))
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

/// Body: PNG bytes. Header `x-name`: the suggested file name. Opens a native
/// save dialog; returns where it was saved, or None when cancelled.
#[tauri::command]
async fn screenshot_save(handle: tauri::AppHandle, app: State<'_, App>, request: Request<'_>) -> Result<Option<String>, AppError> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err(AppError::new("EINVAL", "Expected PNG bytes."));
    };
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.len() > MAX_SCREENSHOT {
        return Err(AppError::new("EINVAL", "Expected PNG bytes."));
    }
    let name = request
        .headers()
        .get("x-name")
        .and_then(|value| value.to_str().ok())
        .filter(|name| name.len() < 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .unwrap_or("screen.png")
        .to_string();
    let bytes = bytes.clone();
    let Some(dest) = pick(&handle, Pick::Save { title: "Save screenshot", name: &name, filter: Some(("PNG image", &["png"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let _reservation = reserve(&app.locks, &dest)?;
    tokio::fs::write(&dest, bytes).await?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

/// Read a firmware ELF for the heap view off the async runtime.
async fn read_elf(path: PathBuf) -> Result<heap::Elf, AppError> {
    tokio::task::spawn_blocking(move || heap::Elf::load(&path))
        .await
        .unwrap_or_else(|_| Err("Reading the ELF panicked.".into()))
        .map_err(|message| AppError::new("EINVAL", message))
}

/// Make `path` the heap view's ELF once it reads as one.
async fn use_elf(app: &App, path: PathBuf) -> Result<FileInfo, AppError> {
    let elf = read_elf(path.clone()).await?;
    let info = file_info(&path).ok_or_else(|| AppError::new("EINVAL", "Unreadable ELF path."))?;
    app.update(|settings| {
        settings.elf = Some(path);
        Ok(())
    })?;
    *app.elf.lock().unwrap() = Some(Arc::new(elf));
    Ok(info)
}

/// Choose the firmware's ELF in a native dialog. None when cancelled.
#[tauri::command]
async fn heap_pick_elf(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<FileInfo>, AppError> {
    let Some(path) = pick(&handle, Pick::Open { title: "Choose the firmware's ELF", filter: Some(("ELF", &["elf"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    use_elf(&app, path).await.map(Some)
}

/// Use an ELF that was dropped on the window.
#[tauri::command]
async fn heap_use_elf(app: State<'_, App>, path: PathBuf) -> Result<FileInfo, AppError> {
    app.granted(&path)?;
    use_elf(&app, path).await
}

#[tauri::command]
fn heap_forget_elf(app: State<'_, App>) -> Result<(), AppError> {
    app.update(|settings| {
        settings.elf = None;
        Ok(())
    })?;
    *app.elf.lock().unwrap() = None;
    Ok(())
}

/// The running firmware's heaps. The ELF is read again when it was rebuilt.
#[tauri::command]
async fn heap_snapshot(app: State<'_, App>) -> Result<heap::Snapshot, AppError> {
    let path = app.settings.lock().unwrap().elf.clone()
        .ok_or_else(|| AppError::new("ENOELF", "Load the firmware's ELF to see its heaps."))?;
    let cached = app.elf.lock().unwrap().clone().filter(|elf| elf.path == path && !elf.stale());
    let elf = match cached {
        Some(elf) => elf,
        None => {
            let elf = Arc::new(read_elf(path).await?);
            *app.elf.lock().unwrap() = Some(elf.clone());
            elf
        }
    };
    app.session.heap_snapshot(&elf).await
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

/// Open the saved card, or the default card when none was chosen. Problems
/// are reported, never fatal: the app opens without a card instead.
fn startup_card(settings: &mut Settings, data: &Path, locks: &Path, problems: &mut Vec<String>) -> Option<Card> {
    let default = data.join("sd.img");
    // A deleted default card is simply made again.
    let saved = settings.card.clone().filter(|saved| !(card_key(saved) == card_key(&default) && !saved.exists()));
    if let Some(saved) = saved {
        return match open_card(locks, &saved) {
            Ok(card) => Some(card),
            Err(error) => {
                problems.push(format!(
                    "The SD card {} cannot be used: {} Open another card or make a new one from the card menu.",
                    saved.display(),
                    error.message
                ));
                None
            }
        };
    }
    // Reserve the default card before looking at it, so two first launches
    // cannot both create it.
    let opened = reserve(locks, &default).and_then(|mut reservation| {
        if !default.exists() {
            sd::create_blank(&default).map_err(|error| {
                AppError::new("EIO", format!("Could not create the default SD card {}: {error}", default.display()))
            })?;
            reservation.refresh(locks, &default)?;
        }
        Ok(Card { path: validate_card(&default)?, lock: reservation.into() })
    });
    match opened {
        Ok(card) => {
            settings.card = Some(card.path.clone());
            Some(card)
        }
        Err(error) => {
            problems.push(format!(
                "The SD card {} cannot be used: {} Open another card or make a new one from the card menu.",
                default.display(),
                error.message
            ));
            None
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // EINK_QEMU_DATA_DIR isolates an instance (settings, default card),
            // e.g. for tests running beside a normal window. Card locks stay
            // shared so two instances never edit one card.
            let (config, data) = match std::env::var_os("EINK_QEMU_DATA_DIR") {
                Some(dir) => (PathBuf::from(&dir), PathBuf::from(dir)),
                None => (app.path().app_config_dir()?, app.path().app_data_dir()?),
            };
            std::fs::create_dir_all(&data)?;
            let settings_path = config.join("settings.json");
            let mut problems = Vec::new();
            let mut settings = Settings::load(&settings_path, &mut problems);
            let locks = locks_dir(app.handle());
            let card = startup_card(&mut settings, &data, &locks, &mut problems);
            if let Some(firmware) = settings.firmware.as_deref() {
                if let Err(error) = check_firmware(firmware) {
                    problems.push(format!("The firmware {} cannot be used: {}", firmware.display(), error.message));
                }
            }
            let session = Session::new(app.handle().clone(), qemu_paths(app.handle()), card);
            let _ = settings.save(&settings_path);
            app.manage(App {
                session,
                settings: Mutex::new(settings),
                settings_path,
                locks,
                grants: Mutex::default(),
                problems: Mutex::new(problems),
                elf: Mutex::default(),
            });
            // SIGTERM and SIGINT quit like the menu does, so QEMU stops and
            // the run directory is removed.
            #[cfg(unix)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
                        return;
                    };
                    tokio::select! {
                        _ = term.recv() => {}
                        _ = int.recv() => {}
                    }
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Dropped files become valid sources for the commands that take paths.
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                if let Some(app) = window.app_handle().try_state::<App>() {
                    app.grant(paths.iter().cloned());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            attach,
            note,
            settings_set,
            firmware_pick,
            firmware_use,
            firmware_read,
            start,
            stop,
            reset,
            key,
            touch,
            card_pick_open,
            card_pick_new,
            card_pick_copy,
            sd_list,
            sd_pick_sources,
            sd_plan_upload,
            sd_upload,
            sd_pick_export,
            sd_mkdir,
            sd_rename,
            sd_remove,
            screenshot_save,
            heap_pick_elf,
            heap_use_elf,
            heap_forget_elf,
            heap_snapshot,
        ])
        .build(tauri::generate_context!())
        .expect("error while building eink-qemu");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(app) = handle.try_state::<App>() {
                let session = app.session.clone();
                tauri::async_runtime::block_on(session.shutdown());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_card_cannot_be_reserved_again() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        let first = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
        drop(first);
        assert!(reserve(&locks, &card).is_ok());
    }

    #[test]
    fn hard_links_share_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        let alias = dir.path().join("alias.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        std::fs::hard_link(&card, &alias).unwrap();
        let _owner = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &alias).err().unwrap().code, "EBUSY");
    }

    #[test]
    fn a_replaced_card_releases_its_old_identity() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("card.img");
        let old_alias = dir.path().join("old.img");
        std::fs::write(&card, vec![0u8; 1024]).unwrap();
        std::fs::hard_link(&card, &old_alias).unwrap();
        let mut owner = reserve(&locks, &card).unwrap();
        // Replace the card with a new file, as a blank card does.
        let fresh = dir.path().join("fresh.tmp");
        std::fs::write(&fresh, vec![1u8; 1024]).unwrap();
        std::fs::rename(&fresh, &card).unwrap();
        owner.refresh(&locks, &card).unwrap();
        assert!(reserve(&locks, &old_alias).is_ok(), "the replaced file is no longer the card");
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
    }

    #[test]
    fn a_path_is_reserved_before_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let card = dir.path().join("new.img");
        let _creator = reserve(&locks, &card).unwrap();
        assert_eq!(reserve(&locks, &card).err().unwrap().code, "EBUSY");
    }
}
