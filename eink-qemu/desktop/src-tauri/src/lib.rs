//! eink-qemu desktop: Tauri commands over the emulator session, the SD card
//! image and the app's settings. The WebView is the browser page's interface.
//!
//! Host files are reached only through paths the user chose: native dialogs
//! run here in Rust, and files dropped on the window are recorded from the
//! window's drop event. Commands that take a host path from the WebView
//! accept only those recorded paths.

mod settings;

use eink_core::cards::{card_key, identity_key, open_card, reserve, reserve_path, validate_card, Card};
use eink_core::session::{self, AppError, Events, QemuPaths, Session, SessionInfo, StartOptions};
use eink_core::{display, firmware, heap, heapview, sd};
use serde::Serialize;
use settings::{Settings, SettingsPatch};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request, Response};
use tauri::{DragDropEvent, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

const FLASH_SIZE: usize = 0x100_0000;
const MAX_FIRMWARE: u64 = 64 * 1024 * 1024;
const MAX_SCREENSHOT: usize = 64 * 1024 * 1024;
const MAX_GRANTS: usize = 20_000;

/// The session's output, forwarded to the WebView: events, and the channels
/// of the WebView that attached last.
struct DesktopEvents {
    app: tauri::AppHandle,
    logs: Mutex<Option<Channel<Vec<String>>>>,
    frames: Mutex<Option<Channel<InvokeResponseBody>>>,
}

impl Events for DesktopEvents {
    fn session(&self, info: &SessionInfo) {
        let _ = self.app.emit("session", info);
    }
    fn sd_reset(&self) {
        let _ = self.app.emit("sd-reset", ());
    }
    fn logs(&self, lines: Vec<String>) {
        if let Some(channel) = self.logs.lock().unwrap().as_ref() {
            let _ = channel.send(lines);
        }
    }
    fn display(&self, message: &[u8]) {
        if let Some(frames) = self.frames.lock().unwrap().as_ref() {
            let _ = frames.send(InvokeResponseBody::Raw(message.to_vec()));
        }
    }
}

struct App {
    session: Arc<Session>,
    events: Arc<DesktopEvents>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    locks: PathBuf,
    /// Host paths the user chose in a dialog or dropped on the window.
    grants: Mutex<HashSet<PathBuf>>,
    /// Startup problems the WebView shows once it attaches.
    problems: Mutex<Vec<String>>,
    /// The heap view: ELF, symbols, allocation log, history.
    heap: tokio::sync::Mutex<heapview::HeapView>,
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
    *app.events.logs.lock().unwrap() = Some(logs);
    // The screen lock is held until the channel is swapped, so no update
    // falls between the snapshot and the new channel.
    app.session.with_screen(|screen| {
        if let Some(screen) = screen {
            let _ = frames.send(InvokeResponseBody::Raw(display::size_message(screen.width, screen.height)));
            let mut frame = Vec::with_capacity(24 + screen.pixels.len());
            for value in [display::MSG_FRAME, 1, 0, 0, screen.width, screen.height] {
                frame.extend_from_slice(&value.to_le_bytes());
            }
            frame.extend_from_slice(&screen.pixels);
            let _ = frames.send(InvokeResponseBody::Raw(frame));
        }
        *app.events.frames.lock().unwrap() = Some(frames);
    });
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

/// The firmware card: the chosen firmware, its device, the ELF, and what
/// was chosen before.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Setup {
    settings: Settings,
    firmware: Option<firmware::FirmwareInfo>,
    elf: Option<FileInfo>,
    /// Whether the ELF built the firmware; None when that cannot be told.
    elf_matches: Option<bool>,
    /// An ELF beside the firmware that was left out: another build's.
    elf_skipped: Option<String>,
    recent: Vec<RecentInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecentInfo {
    path: String,
    name: String,
    parent: String,
    device: String,
    elf: bool,
}

async fn read_image(path: &Path) -> Result<Vec<u8>, AppError> {
    check_firmware(path)?;
    tokio::fs::read(path).await.map_err(|e| AppError::new("ENOENT", format!("Cannot read {}: {e}", path.display())))
}

/// The ELF beside `firmware` that built it, or that cannot be told apart;
/// and the name of one left out as another build's.
async fn find_elf(firmware: &Path, image: &[u8]) -> (Option<(PathBuf, Arc<heap::Elf>)>, Option<String>) {
    let mut skipped = None;
    for path in firmware::elf_candidates(firmware) {
        let Ok(elf) = heapview::read_elf(path.clone()).await else { continue };
        match elf.matches(image) {
            Some(false) => skipped = skipped.or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned())),
            _ => return (Some((path, Arc::new(elf))), None),
        }
    }
    (None, skipped)
}

/// The card's contents. With `detect`, an ELF beside the firmware is taken
/// up when none is chosen.
async fn setup(app: &App, detect: bool) -> Result<Setup, AppError> {
    let mut settings = app.settings.lock().unwrap().clone();
    let image = match settings.firmware.as_deref() {
        Some(path) => read_image(path).await.ok(),
        None => None,
    };
    let mut skipped = None;
    if let (true, None, Some(path), Some(image)) = (detect, &settings.elf, settings.firmware.clone(), image.as_deref()) {
        let (found, left) = find_elf(&path, image).await;
        skipped = left;
        if let Some((elf, read)) = found {
            settings = app.update(|settings| {
                settings.elf = Some(elf);
                settings.elf_auto = true;
                settings.remember();
                Ok(())
            })?;
            app.heap.lock().await.set_elf(Some(read));
        }
    }
    let elf_matches = match (settings.elf.as_deref(), image.as_deref()) {
        (Some(path), Some(image)) => app.heap.lock().await.elf(path).await.ok().and_then(|elf| elf.matches(image)),
        _ => None,
    };
    let recent = settings.recent.iter()
        .filter(|r| r.firmware.is_file())
        .map(|r| RecentInfo {
            path: r.firmware.to_string_lossy().into_owned(),
            name: r.firmware.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            parent: firmware::parent_name(&r.firmware),
            device: r.device.clone(),
            elf: r.elf.as_ref().is_some_and(|e| e.is_file()),
        })
        .collect();
    Ok(Setup {
        firmware: settings.firmware.as_deref().zip(image.as_deref()).map(|(path, image)| firmware::describe(path, image)),
        elf: settings.elf.as_deref().and_then(file_info),
        elf_matches,
        elf_skipped: skipped,
        recent,
        settings,
    })
}

/// Make `path` the firmware. Its device is the one it ran on before, else
/// one that takes its chip; its ELF is the one beside it that built it, else
/// the one chosen for it before.
async fn choose_firmware(app: &App, path: PathBuf) -> Result<Setup, AppError> {
    let image = read_image(&path).await?;
    let chip = firmware::chip(&path, &image);
    let (found, skipped) = find_elf(&path, &image).await;
    let found_path = found.as_ref().map(|(p, _)| p.clone());
    let settings = app.update(|settings| {
        let before = settings.recent.iter().find(|r| r.firmware == path).cloned();
        let fits = |device: &str| chip.map_or(device != "auto", |chip| chip.devices().contains(&device));
        if let Some(device) = before.as_ref().map(|r| r.device.clone()).filter(|d| fits(d)) {
            settings.device = device;
        } else if !fits(&settings.device) {
            if let Some(chip) = chip {
                // The device last used with this chip, else its default.
                settings.device = settings.recent.iter().map(|r| r.device.as_str()).find(|d| chip.devices().contains(d))
                    .unwrap_or(chip.devices()[0]).to_string();
            }
        }
        let same = settings.firmware.as_ref() == Some(&path);
        if let Some(elf) = found_path {
            settings.elf = Some(elf);
            settings.elf_auto = true;
        } else if let Some(chosen) = before.filter(|r| !r.elf_auto).and_then(|r| r.elf).filter(|e| e.is_file()) {
            settings.elf = Some(chosen);
            settings.elf_auto = false;
        } else if !same || settings.elf_auto {
            settings.elf = None;
            settings.elf_auto = false;
        }
        settings.firmware = Some(path.clone());
        settings.remember();
        Ok(())
    })?;
    {
        let mut heap = app.heap.lock().await;
        match (found, settings.elf.as_deref()) {
            (Some((_, read)), _) => heap.set_elf(Some(read)),
            (None, Some(elf)) => { let _ = heap.elf(elf).await; }
            (None, None) => heap.set_elf(None),
        }
    }
    let mut setup = setup(app, false).await?;
    setup.elf_skipped = skipped;
    Ok(setup)
}

/// The firmware card's contents. With `detect`, an ELF built beside the
/// firmware is taken up when none is chosen.
#[tauri::command]
async fn firmware_setup(app: State<'_, App>, detect: bool) -> Result<Setup, AppError> {
    setup(&app, detect).await
}

/// Choose the firmware in a native dialog. None when cancelled.
#[tauri::command]
async fn firmware_pick(handle: tauri::AppHandle, app: State<'_, App>) -> Result<Option<Setup>, AppError> {
    let Some(path) = pick(&handle, Pick::Open { title: "Choose firmware", filter: Some(("Firmware", &["bin", "xota"])) })
        .await
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    choose_firmware(&app, path).await.map(Some)
}

/// Use a firmware file that was dropped on the window.
#[tauri::command]
async fn firmware_use(app: State<'_, App>, path: PathBuf) -> Result<Setup, AppError> {
    app.granted(&path)?;
    choose_firmware(&app, path).await
}

/// Choose a firmware from the recent list again.
#[tauri::command]
async fn firmware_recent(app: State<'_, App>, path: PathBuf) -> Result<Setup, AppError> {
    if !app.settings.lock().unwrap().recent.iter().any(|r| r.firmware == path) {
        return Err(AppError::new("EPERM", format!("{} is not a recent firmware.", path.display())));
    }
    choose_firmware(&app, path).await
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
    let trace = trace_plan(&app, &flash).await;
    app.session.start(options, flash, trace).await
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

/// A card just made or resized: the file, and its size, cluster size and
/// whether it is sparse (else it is the 128 MiB fallback).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MadeCard {
    card: Option<FileInfo>,
    made: sd::MadeCard,
}

/// Make a new blank FAT32 card of `size` bytes (native save dialog), which
/// becomes the card in use. The destination is locked before it is replaced.
#[tauri::command]
async fn card_pick_new(handle: tauri::AppHandle, app: State<'_, App>, size: u64) -> Result<Option<MadeCard>, AppError> {
    sd::check_size(size)?;
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
                let made = sd::create_card(&card.path, size)?;
                // The blank card is a new file: its identity replaces the old one.
                card.lock.lock().unwrap().refresh(&locks, &card.path)?;
                Ok((None, made))
            } else {
                let alias = current.is_some_and(|card| identity_key(&card.path).is_some() && identity_key(&card.path) == identity_key(&path));
                let mut reservation = if alias { reserve_path(&locks, &path)? } else { reserve(&locks, &path)? };
                let made = sd::create_card(&path, size)?;
                reservation.refresh(&locks, &path)?;
                Ok((Some(reservation), made))
            }
        })
        .await?;
    let card = match made.0 {
        Some(lock) => Some(use_card(&app, Card { path: validate_card(&path)?, lock: lock.into() }).await?),
        None => file_info(&path),
    };
    Ok(Some(MadeCard { card, made: made.1 }))
}

/// Make the card in use `size` bytes: its files are copied into a new card
/// with 32 KiB clusters, which takes its place (sd::resize).
#[tauri::command]
async fn card_resize(app: State<'_, App>, size: u64) -> Result<MadeCard, AppError> {
    sd::check_size(size)?;
    let locks = app.locks.clone();
    let made = app
        .session
        .while_idle(|current| {
            let card = current.ok_or_else(|| AppError::new("ENOENT", "No SD card is open."))?;
            // Copying a full card takes a while: let other tasks run meanwhile.
            let made = tokio::task::block_in_place(|| sd::resize(&card.path, size))?;
            card.lock.lock().unwrap().refresh(&locks, &card.path)?;
            Ok(made)
        })
        .await?;
    Ok(MadeCard { card: app.session.card_path().as_deref().and_then(file_info), made })
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
            // Sparse like the card, so a 16 GiB card copies as what it holds.
            sd::sparse_copy(card, temp.as_file()).map_err(|e| sd::SdError::new("EIO", e.to_string()))?;
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

/// Show the open SD card image in Finder, Explorer or the file manager.
#[tauri::command]
fn card_reveal(handle: tauri::AppHandle, app: State<'_, App>) -> Result<(), AppError> {
    let path = app.session.card_path().ok_or_else(|| AppError::new("ENOENT", "No SD card is open."))?;
    handle.opener().reveal_item_in_dir(&path)
        .map_err(|e| AppError::new("EIO", format!("Could not show {}: {e}", path.display())))
}

/// Make `path` the heap view's ELF once it reads as one.
async fn use_elf(app: &App, path: PathBuf) -> Result<FileInfo, AppError> {
    let elf = heapview::read_elf(path.clone()).await?;
    let info = file_info(&path).ok_or_else(|| AppError::new("EINVAL", "Unreadable ELF path."))?;
    app.update(|settings| {
        settings.elf = Some(path);
        settings.elf_auto = false;
        settings.remember();
        Ok(())
    })?;
    app.heap.lock().await.set_elf(Some(Arc::new(elf)));
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
async fn heap_forget_elf(app: State<'_, App>) -> Result<(), AppError> {
    app.update(|settings| {
        settings.elf = None;
        settings.elf_auto = false;
        settings.remember();
        Ok(())
    })?;
    app.heap.lock().await.set_elf(None);
    Ok(())
}

/// The running firmware's heaps, as heapview.py's /snapshot. The ELF is
/// read again when it was rebuilt; without one the heaps are still read.
#[tauri::command]
async fn heap_snapshot(app: State<'_, App>) -> Result<serde_json::Value, AppError> {
    let path = app.settings.lock().unwrap().elf.clone();
    app.heap.lock().await.snapshot(&app.session, path.as_deref()).await
}

/// The kept snapshot nearest `time` (seconds) and the one before it.
#[tauri::command]
async fn heap_snapshot_at(app: State<'_, App>, time: f64) -> Result<serde_json::Value, AppError> {
    Ok(app.heap.lock().await.snapshot_at(&app.session, time))
}

/// The current run's free-memory history, as heapview.py's /history.
#[tauri::command]
async fn heap_history(app: State<'_, App>) -> Result<serde_json::Value, AppError> {
    Ok(app.heap.lock().await.history(&app.session))
}

/// Allocation events, as heapview.py's /events.
#[tauri::command]
async fn heap_events(app: State<'_, App>, gen: u64, start: usize, stacks: usize, sites: usize) -> Result<serde_json::Value, AppError> {
    Ok(app.heap.lock().await.events(gen, start, stacks, sites))
}

/// Allocation site names from `from` on, for the Heap tab's map.
#[tauri::command]
async fn heap_sites(app: State<'_, App>, from: usize) -> Result<serde_json::Value, AppError> {
    Ok(app.heap.lock().await.sites(from))
}

/// A recorded allocation stack, symbolized, as heapview.py's /stack.
#[tauri::command]
async fn heap_stack(app: State<'_, App>, id: usize) -> Result<serde_json::Value, AppError> {
    Ok(app.heap.lock().await.stack(id))
}

/// Open the heap window (heap.html), or bring it forward.
#[tauri::command]
async fn heap_window(handle: tauri::AppHandle) -> Result<(), AppError> {
    if let Some(window) = handle.get_webview_window("heap") {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    tauri::WebviewWindowBuilder::new(&handle, "heap", tauri::WebviewUrl::App("heap.html".into()))
        .title("Heap view")
        .inner_size(1280.0, 860.0)
        .min_inner_size(960.0, 600.0)
        .build()
        .map_err(|e| AppError::new("EIO", format!("Could not open the heap window: {e}")))?;
    Ok(())
}

/// Whether the run about to start records allocations: it needs the ELF,
/// the plugin, and the ELF to be the chosen firmware's.
async fn trace_plan(app: &App, flash: &[u8]) -> session::TracePlan {
    let Some(path) = app.settings.lock().unwrap().elf.clone() else {
        return Err("Restart the emulator to record where blocks were allocated.".into());
    };
    let Some(plugin) = app.session.qemu().heaptrace() else {
        return Err("This QEMU has no heaptrace plugin, so allocation stacks are not recorded.".into());
    };
    let elf = app.heap.lock().await.elf(&path).await.map_err(|e| format!("Allocation stacks are off: {}", e.message))?;
    match elf.trace_problem(flash) {
        Some(problem) => Err(format!("Allocation stacks are off: {problem}.")),
        None => Ok((plugin, path)),
    }
}

fn qemu_paths(app: &tauri::AppHandle) -> QemuPaths {
    // A build directory keeps its plugins in contrib/plugins; a package in qemu/plugins.
    let env_plugins = || std::env::var_os("EINK_QEMU_PLUGINS").map(PathBuf::from);
    if let Some(bin) = std::env::var_os("EINK_QEMU_BIN").map(PathBuf::from) {
        let plugins = env_plugins().unwrap_or_else(|| bin.join("contrib").join("plugins"));
        return QemuPaths { bin, share: std::env::var_os("EINK_QEMU_SHARE").map(Into::into), plugins };
    }
    if let Ok(resources) = app.path().resource_dir() {
        let qemu = resources.join("qemu");
        if qemu.join("bin").is_dir() {
            return QemuPaths { bin: qemu.join("bin"), share: Some(qemu.join("share")), plugins: env_plugins().unwrap_or(qemu.join("plugins")) };
        }
    }
    // Development: the repository's build directory.
    let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build");
    QemuPaths { plugins: env_plugins().unwrap_or_else(|| build.join("contrib").join("plugins")), bin: build, share: None }
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
            let made = sd::create_card(&default, sd::DEFAULT_CARD).map_err(|error| {
                AppError::new("EIO", format!("Could not create the default SD card {}: {}", default.display(), error.message))
            })?;
            if !made.sparse {
                problems.push(format!(
                    "{} cannot hold sparse files, so the new SD card is 128 MiB with 1 KiB clusters, not a 16 GiB card \
                     with the stock card's 32 KiB clusters. Make another from the card menu on a disk that can.",
                    data.display()
                ));
            }
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
            // At Windows' larger display scales the default logical size can
            // extend below the taskbar, hiding the device controls. Let the
            // window manager fit it to the work area in that case.
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                if let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.outer_size()) {
                    let area = monitor.work_area();
                    if size.width > area.size.width || size.height > area.size.height {
                        window.maximize()?;
                    }
                }
            }
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
            let events = Arc::new(DesktopEvents { app: app.handle().clone(), logs: Mutex::default(), frames: Mutex::default() });
            let paths = qemu_paths(app.handle());
            // The session's tasks run on Tauri's Tokio runtime.
            let session = tauri::async_runtime::block_on({
                let events = events.clone();
                async move { Session::new(events, paths, card) }
            });
            let _ = settings.save(&settings_path);
            app.manage(App {
                session,
                events,
                settings: Mutex::new(settings),
                settings_path,
                locks,
                grants: Mutex::default(),
                problems: Mutex::new(problems),
                heap: Default::default(),
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
            // The heap window belongs to the main one.
            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                if let Some(heap) = window.app_handle().get_webview_window("heap") {
                    let _ = heap.close();
                }
            }
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
            firmware_recent,
            firmware_setup,
            firmware_read,
            start,
            stop,
            reset,
            key,
            touch,
            card_pick_open,
            card_pick_new,
            card_pick_copy,
            card_resize,
            card_reveal,
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
            heap_snapshot_at,
            heap_history,
            heap_events,
            heap_stack,
            heap_sites,
            heap_window,
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
