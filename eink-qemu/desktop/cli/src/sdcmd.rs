//! `eink-qemu sd`: make, list and edit SD card images (FAT32, the stock X3
//! card's geometry by default), without a window or FAT tools.

use crate::{EXIT_SETUP, EXIT_USAGE};
use clap::Subcommand;
use eink_core::{cards, sd};
use serde_json::json;
use std::path::PathBuf;

const MIB: u64 = 1024 * 1024;

#[derive(Subcommand)]
pub enum SdCommand {
    /// Make a blank card (MBR + FAT32, 32 KiB clusters).
    New {
        image: PathBuf,
        /// Size in MiB: 2050 (the stock X3 card) to 32768.
        #[arg(long, default_value_t = 2050, value_parser = clap::value_parser!(u64).range(2050..=32768))]
        size: u64,
    },
    /// List a folder on the card.
    Ls {
        image: PathBuf,
        #[arg(default_value = "/")]
        path: String,
    },
    /// Copy host files or folders into a folder on the card (made if missing).
    Put {
        image: PathBuf,
        /// Host files or folders.
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        /// The card folder to copy into.
        #[arg(long, default_value = "/")]
        to: String,
    },
    /// Copy a file from the card to the host.
    Get {
        image: PathBuf,
        /// The card file, e.g. /.crosspoint/settings.bin
        path: String,
        /// Host destination file.
        dest: PathBuf,
    },
    /// Make a folder on the card.
    Mkdir { image: PathBuf, path: String },
    /// Remove a file or folder (with its contents) from the card.
    Rm { image: PathBuf, path: String },
}

pub fn run(command: SdCommand) -> u8 {
    match exec(command) {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err((code, message)) => {
            println!("{}", json!({"type": "error", "stage": "sd", "error": message}));
            code
        }
    }
}

fn locked(image: &std::path::Path) -> Result<cards::Card, (u8, String)> {
    let locks = crate::launch::shared_locks();
    cards::open_card(&locks, image).map_err(|e| (EXIT_SETUP, crate::launch::lock_error(&image.display().to_string(), &e.message, &locks)))
}

fn exec(command: SdCommand) -> Result<serde_json::Value, (u8, String)> {
    let sd_err = |e: sd::SdError| (EXIT_SETUP, e.message);
    Ok(match command {
        SdCommand::New { image, size } => {
            // Reserve the path before looking at it, as the desktop app does
            // for its default card: no other instance can make or open a card
            // there until this one is written.
            let locks = crate::launch::shared_locks();
            let _lock = cards::reserve(&locks, &image).map_err(|e| (EXIT_SETUP, crate::launch::lock_error(&image.display().to_string(), &e.message, &locks)))?;
            if image.exists() {
                return Err((EXIT_USAGE, format!("{} exists; remove it first or choose another name", image.display())));
            }
            sd::check_size(size * MIB).map_err(|e| (EXIT_USAGE, e.message))?;
            let made = sd::create_card(&image, size * MIB).map_err(sd_err)?;
            json!({"type": "sd", "made": image, "size_mib": made.size / MIB, "cluster": made.cluster, "sparse": made.sparse})
        }
        SdCommand::Ls { image, path } => {
            let card = locked(&image)?;
            let listing = sd::list(&card.path, &path).map_err(sd_err)?;
            serde_json::to_value(listing).unwrap_or_default()
        }
        SdCommand::Put { image, sources, to } => {
            let card = locked(&image)?;
            let to = if to.starts_with('/') { to } else { format!("/{to}") };
            let mut path = String::new();
            for part in to.split('/').filter(|p| !p.is_empty()) {
                path.push('/');
                path.push_str(part);
                match sd::mkdir(&card.path, &path) {
                    Ok(()) => {}
                    Err(e) if e.code == "EEXIST" => {}
                    Err(e) => return Err(sd_err(e)),
                }
            }
            let plan = sd::plan_upload(&card.path, &to, &sources).map_err(sd_err)?;
            let done = sd::upload(&card.path, &to, &sources, &plan.conflicts).map_err(sd_err)?;
            json!({"type": "sd", "to": to, "files": done.files, "bytes": done.bytes, "replaced": plan.conflicts})
        }
        SdCommand::Get { image, path, dest } => {
            let card = locked(&image)?;
            // The destination may be another card someone has open: hold its lock for the write.
            let locks = crate::launch::shared_locks();
            let _dest_lock = cards::reserve(&locks, &dest).map_err(|e| (EXIT_SETUP, crate::launch::lock_error(&dest.display().to_string(), &e.message, &locks)))?;
            sd::export(&card.path, &path, &dest).map_err(sd_err)?;
            json!({"type": "sd", "got": path, "dest": dest})
        }
        SdCommand::Mkdir { image, path } => {
            let card = locked(&image)?;
            sd::mkdir(&card.path, &path).map_err(sd_err)?;
            json!({"type": "sd", "made": path})
        }
        SdCommand::Rm { image, path } => {
            let card = locked(&image)?;
            sd::remove(&card.path, &path).map_err(sd_err)?;
            json!({"type": "sd", "removed": path})
        }
    })
}
