//! SD explorer tests: the fatfs-based module against the contract in
//! src/sd.rs, with every modified image re-read by the browser's fat.js
//! (scripts/sd-parity.mjs) when Node is available.

use eink_qemu_desktop_lib::sd::{self, SdError, SdListing};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

const DESKTOP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u32 = 0x811c9dc5;
    for &byte in bytes {
        hash = (hash ^ byte as u32).wrapping_mul(0x0100_0193);
    }
    hash as u64
}

/// Deterministic pseudo-random bytes.
fn data(seed: u32, length: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

struct Card {
    dir: tempfile::TempDir,
    image: PathBuf,
}

impl Card {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("sd.img");
        sd::create_blank(&image).unwrap();
        Card { dir, image }
    }
    /// A host folder for upload sources.
    fn host(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("host").join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn list(&self, path: &str) -> SdListing {
        sd::list(&self.image, path).unwrap_or_else(|e| panic!("list {path}: {e:?}"))
    }
    fn read(&self, path: &str) -> Vec<u8> {
        let dest = self.dir.path().join("export.bin");
        sd::export(&self.image, path, &dest).unwrap_or_else(|e| panic!("export {path}: {e:?}"));
        fs::read(dest).unwrap()
    }
    fn names(&self, path: &str) -> Vec<String> {
        self.list(path).entries.into_iter().map(|e| e.name).collect()
    }
}

/// Upload, agreeing to replace whatever the plan reports as conflicts.
fn upload_all(image: &Path, dir: &str, sources: &[PathBuf]) -> Result<sd::UploadPlan, SdError> {
    let confirmed = sd::plan_upload(image, dir, sources).map(|plan| plan.conflicts).unwrap_or_default();
    sd::upload(image, dir, sources, &confirmed)
}

/// A symbolic link, or false where the host refuses one (Windows without
/// Developer Mode or administrator rights).
fn symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).unwrap();
        true
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        } else {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn code<T: std::fmt::Debug>(result: Result<T, SdError>) -> &'static str {
    match result {
        Ok(value) => panic!("expected an error, got {value:?}"),
        Err(error) => error.code,
    }
}

fn node() -> Option<PathBuf> {
    for candidate in ["node", "/opt/homebrew/bin/node", "/usr/local/bin/node"] {
        if Command::new(candidate).arg("--version").output().is_ok_and(|o| o.status.success()) {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

/// What fat.js sees, or None without Node.
fn fat_js(image: &Path) -> Option<Value> {
    let Some(node) = node() else {
        assert!(std::env::var_os("SD_PARITY_OPTIONAL").is_some(), "node is required for the fat.js parity checks (set SD_PARITY_OPTIONAL=1 to skip them)");
        eprintln!("node not found: skipping fat.js parity");
        return None;
    };
    let script = Path::new(DESKTOP).join("scripts/sd-parity.mjs");
    let output = Command::new(node).arg(script).arg(image).output().unwrap();
    assert!(output.status.success(), "fat.js rejected the image: {}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

/// The same report from the Rust side.
fn rust_view(card: &Card) -> Value {
    let mut order = BTreeMap::new();
    let mut tree = BTreeMap::new();
    let mut stack = vec!["/".to_string()];
    let root = card.list("/");
    while let Some(dir) = stack.pop() {
        let listing = card.list(&dir);
        order.insert(dir.clone(), Value::from(listing.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>()));
        for entry in listing.entries {
            let path = if dir == "/" { format!("/{}", entry.name) } else { format!("{dir}/{}", entry.name) };
            if entry.is_dir {
                tree.insert(path.clone(), serde_json::json!({ "dir": true }));
                stack.push(path);
            } else {
                let bytes = card.read(&path);
                assert_eq!(bytes.len() as u64, entry.size, "{path}");
                tree.insert(path, serde_json::json!({ "size": entry.size, "fnv1a": fnv1a(&bytes) }));
            }
        }
    }
    serde_json::json!({ "type": root.fs_type, "free": root.free, "total": root.total, "order": order, "tree": tree })
}

/// fat.js and fatfs agree on the whole card, and every allocated cluster
/// belongs to exactly one file or folder (nothing lost or cross-linked).
fn parity(card: &Card) {
    sound(card);
    let rust = rust_view(card);
    if let Some(js) = fat_js(&card.image) {
        assert_eq!(js, rust, "fat.js and the Rust explorer disagree");
    }
}

/// No lost clusters and no cross-links (any FAT type), and the platform's
/// fsck finds nothing to repair.
fn sound(card: &Card) {
    assert_eq!(sd::lost_clusters(&card.image).expect("the card audits cleanly"), 0, "lost clusters");
    fsck(&card.image);
}

/// The FAT volume of an image: the first FAT partition of an MBR card, or
/// the whole image when it has no partition table.
fn volume(image: &Path) -> Vec<u8> {
    let bytes = fs::read(image).unwrap();
    let entry = &bytes[0x1be..0x1ce];
    let lba = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
    let count = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
    if bytes[510..512] == [0x55, 0xaa] && [0x01, 0x04, 0x06, 0x0b, 0x0c, 0x0e].contains(&entry[4]) && lba > 0 {
        bytes[lba * 512..(lba + count) * 512].to_vec()
    } else {
        bytes
    }
}

/// Run fsck_msdos (macOS) and fsck.fat (dosfstools) read-only on the
/// volume, when installed; either reporting a problem fails the test.
fn fsck(image: &Path) {
    let tools: [(&str, &[&str]); 4] = [
        ("/sbin/fsck_msdos", &["-n"]),
        ("fsck.fat", &["-n"]),
        ("/opt/homebrew/sbin/fsck.fat", &["-n"]),
        ("/usr/sbin/fsck.fat", &["-n"]),
    ];
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join("volume.img");
    let mut written = false;
    let mut ran_fat = false;
    for (tool, args) in tools {
        if tool.ends_with("fsck.fat") && ran_fat {
            continue;
        }
        let Ok(output) = Command::new(tool).arg("-h").output() else { continue };
        let _ = output;
        if !written {
            fs::write(&part, volume(image)).unwrap();
            written = true;
        }
        let output = Command::new(tool).args(args).arg(&part).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{tool} reports problems:\n{text}");
        if tool.ends_with("fsck.fat") {
            ran_fat = true;
        }
    }
}

#[test]
fn names_follow_fat_js() {
    for name in ["a.txt", "Moby Dick.epub", "한국어 책.epub", "CONSOLE", "COM0", "COM10", "con1.txt", ".hidden", "a..b", &"a".repeat(255)] {
        assert!(sd::valid_name(name), "{name} should be valid");
    }
    for name in [
        "", ".", "..", "a/b", "a\\b", "a:b", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "tab\tname", "\u{1}", "x\u{ffff}",
        "trail.", "trail ", "CON", "con", "con.txt", "PRN.tar.gz", "Aux", "NUL.x", "COM1", "lpt9.doc", &"a".repeat(256),
    ] {
        assert!(!sd::valid_name(name), "{name:?} should be invalid");
    }
    // 255 UTF-16 units, not bytes or chars.
    assert!(sd::valid_name(&"가".repeat(255)));
    assert!(!sd::valid_name(&"📚".repeat(128)));
}

#[test]
fn blank_card() {
    let card = Card::new();
    let listing = card.list("/");
    assert_eq!(listing.fs_type, "FAT32");
    assert_eq!(listing.path, "/");
    assert!(listing.entries.is_empty(), "{:?}", listing.entries);
    assert!(listing.total > 60 * 1024 * 1024 && listing.total < 64 * 1024 * 1024, "{}", listing.total);
    assert!(listing.free > 0 && listing.free <= listing.total);
    parity(&card);
}

#[test]
fn folders_first_case_insensitive_order() {
    let card = Card::new();
    for dir in ["/books", "/Fonts", "/alpha"] {
        sd::mkdir(&card.image, dir).unwrap();
    }
    let host = card.host("files");
    for (name, seed) in [("b.txt", 1), ("A.txt", 2), ("Zebra.epub", 3), ("apple.bin", 4)] {
        write(&host.join(name), &data(seed, 1000 + seed as usize));
    }
    let sources: Vec<PathBuf> = ["b.txt", "A.txt", "Zebra.epub", "apple.bin"].iter().map(|n| host.join(n)).collect();
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.names("/"), ["alpha", "books", "Fonts", "A.txt", "apple.bin", "b.txt", "Zebra.epub"]);
    let entry = card.list("/").entries.into_iter().find(|e| e.name == "A.txt").unwrap();
    assert!(!entry.is_dir);
    assert_eq!(entry.size, 1002);
    let now = chrono::Utc::now().timestamp_millis();
    let mtime = entry.mtime.expect("uploaded files have a date");
    assert!((now - mtime).abs() < 10 * 60 * 1000, "mtime {mtime} vs now {now}");
    parity(&card);
}

#[test]
fn upload_folder_tree_skips_hidden_and_links() {
    let card = Card::new();
    let root = card.host("tree");
    let lib = root.join("lib");
    write(&lib.join("x.bin"), &data(7, 100_000));
    write(&lib.join("sub/y.txt"), b"hello");
    write(&lib.join("sub/deeper/z.txt"), &data(9, 70_000));
    write(&lib.join(".hidden"), b"skip me");
    write(&lib.join("sub/.DS_Store"), b"skip me too");
    fs::create_dir_all(lib.join("empty")).unwrap();
    symlink(&lib.join("x.bin"), &lib.join("link.bin"));
    symlink(&lib.join("sub"), &lib.join("linkdir"));
    let top_link = symlink(&lib.join("x.bin"), &root.join("top-link.bin"));
    write(&root.join(".top"), b"explicitly chosen hidden file");
    sd::mkdir(&card.image, "/books").unwrap();

    let mut sources = vec![lib.clone(), root.join(".top")];
    if top_link {
        sources.push(root.join("top-link.bin"));
    }
    let plan = sd::plan_upload(&card.image, "/books", &sources).unwrap();
    assert_eq!(plan.files, 4);
    assert_eq!(plan.bytes, 100_000 + 5 + 70_000 + 29);
    assert!(plan.conflicts.is_empty());
    let done = upload_all(&card.image, "/books", &sources).unwrap();
    assert_eq!(done.files, 4);

    assert_eq!(card.names("/books"), ["lib", ".top"]);
    assert_eq!(card.names("/books/lib"), ["empty", "sub", "x.bin"]);
    assert_eq!(card.names("/books/lib/sub"), ["deeper", "y.txt"]);
    assert_eq!(card.read("/books/lib/x.bin"), data(7, 100_000));
    assert_eq!(card.read("/BOOKS/LIB/SUB/DEEPER/Z.TXT"), data(9, 70_000));
    assert_eq!(card.read("/books/.top"), b"explicitly chosen hidden file");
    assert!(card.list("/books/lib/empty").entries.is_empty());
    parity(&card);
}

#[test]
fn replacing_files_reports_conflicts_and_frees_space() {
    let card = Card::new();
    let blank_free = card.list("/").free;
    let host = card.host("v1");
    write(&host.join("book.epub"), &data(1, 300_000));
    upload_all(&card.image, "/", &[host.join("book.epub")]).unwrap();
    let after_first = card.list("/").free;
    assert!(after_first <= blank_free - 300_000);

    let host2 = card.host("v2");
    write(&host2.join("BOOK.epub"), &data(2, 1_000));
    let plan = sd::plan_upload(&card.image, "/", &[host2.join("BOOK.epub")]).unwrap();
    // The conflict names the file on the card, as it is listed.
    assert_eq!(plan.conflicts, ["/book.epub"]);
    upload_all(&card.image, "/", &[host2.join("BOOK.epub")]).unwrap();
    // The existing entry keeps its name; its contents are replaced.
    assert_eq!(card.names("/"), ["book.epub"]);
    assert_eq!(card.read("/book.epub"), data(2, 1_000));
    let listing = card.list("/");
    assert_eq!(listing.entries[0].size, 1_000);
    assert!(listing.free > after_first + 290_000, "old clusters were freed");
    parity(&card);
}

#[test]
fn type_clashes() {
    let card = Card::new();
    sd::mkdir(&card.image, "/books").unwrap();
    let host = card.host("files");
    write(&host.join("note.txt"), b"x");
    upload_all(&card.image, "/", &[host.join("note.txt")]).unwrap();

    // A file onto a folder, and a folder onto a file (case-insensitive).
    let clash = card.host("clash");
    write(&clash.join("BOOKS"), b"file");
    write(&clash.join("Note.TXT/inner.txt"), b"in a folder");
    let error = sd::plan_upload(&card.image, "/", &[clash.join("BOOKS")]).unwrap_err();
    assert_eq!(error.code, "EISDIR");
    assert!(!error.changed);
    assert_eq!(code(upload_all(&card.image, "/", &[clash.join("Note.TXT")])), "ENOTDIR");
    assert_eq!(code(upload_all(&card.image, "/", &[clash.join("BOOKS")])), "EISDIR");

    // Two sources that land on the same card path.
    let a = card.host("a");
    let b = card.host("b");
    write(&a.join("same.txt"), b"1");
    write(&b.join("SAME.txt"), b"2");
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[a.join("same.txt"), b.join("SAME.txt")])), "EINVAL");
    parity(&card);
}

#[test]
fn not_enough_space_changes_nothing() {
    let card = Card::new();
    let before = card.list("/").free;
    let image = fs::read(&card.image).unwrap();
    let host = card.host("big");
    let big = host.join("big.bin");
    let file = fs::File::create(&big).unwrap();
    file.set_len(100 * 1024 * 1024).unwrap();
    let error = sd::plan_upload(&card.image, "/", std::slice::from_ref(&big)).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(error.message.contains("Not enough space"), "{}", error.message);
    let error = upload_all(&card.image, "/", &[big]).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(!error.changed);
    assert!(fs::read(&card.image).unwrap() == image, "the image is unchanged");
    assert_eq!(card.list("/").free, before);
    assert!(card.list("/").entries.is_empty());
    parity(&card);
}

#[test]
fn missing_paths_and_wrong_types() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("f.txt"), b"x");
    upload_all(&card.image, "/", &[host.join("f.txt")]).unwrap();
    assert_eq!(code(sd::list(&card.image, "/nope")), "ENOENT");
    assert_eq!(code(sd::list(&card.image, "/f.txt")), "ENOTDIR");
    assert_eq!(code(sd::list(&card.image, "relative")), "EINVAL");
    assert_eq!(code(sd::list(&card.image, "/bad:name")), "EINVAL");
    assert_eq!(code(upload_all(&card.image, "/nope", &[host.join("f.txt")])), "ENOENT");
    assert_eq!(code(upload_all(&card.image, "/", &[host.join("absent.txt")])), "ENOENT");
    assert_eq!(code(sd::remove(&card.image, "/nope")), "ENOENT");
    assert_eq!(code(sd::remove(&card.image, "/")), "EINVAL");
    assert_eq!(code(sd::rename(&card.image, "/nope", "x")), "ENOENT");
    assert_eq!(code(sd::mkdir(&card.image, "/nope/child")), "ENOENT");
    assert_eq!(code(sd::mkdir(&card.image, "/f.txt/child")), "ENOTDIR");
    assert_eq!(code(sd::mkdir(&card.image, "/F.TXT")), "EEXIST");
    assert_eq!(code(sd::mkdir(&card.image, "/")), "EINVAL");
    assert_eq!(code(sd::mkdir(&card.image, "/con")), "EINVAL");
    let dest = card.dir.path().join("out.bin");
    assert_eq!(code(sd::export(&card.image, "/nope", &dest)), "ENOENT");
    sd::mkdir(&card.image, "/dir").unwrap();
    assert_eq!(code(sd::export(&card.image, "/dir", &dest)), "EISDIR");
    assert!(!dest.exists());
    assert!(!SdError::new("EIO", "").changed);
    parity(&card);
}

#[test]
fn rename_files_and_folders() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("lib/a.txt"), b"alpha");
    write(&host.join("b.txt"), b"beta");
    upload_all(&card.image, "/", &[host.join("lib"), host.join("b.txt")]).unwrap();

    sd::rename(&card.image, "/b.txt", "Beta Book.txt").unwrap();
    assert_eq!(card.names("/"), ["lib", "Beta Book.txt"]);
    // Case-only changes are allowed, as in fat.js.
    sd::rename(&card.image, "/BETA BOOK.TXT", "beta book.TXT").unwrap();
    assert_eq!(card.names("/"), ["lib", "beta book.TXT"]);
    sd::rename(&card.image, "/lib", "Library").unwrap();
    assert_eq!(card.read("/Library/a.txt"), b"alpha");
    // Unchanged name: nothing to do.
    sd::rename(&card.image, "/Library", "Library").unwrap();

    assert_eq!(code(sd::rename(&card.image, "/beta book.TXT", "LIBRARY")), "EEXIST");
    assert_eq!(code(sd::rename(&card.image, "/Library", "bad/name")), "EINVAL");
    assert_eq!(code(sd::rename(&card.image, "/Library", "trailing.")), "EINVAL");
    assert_eq!(code(sd::rename(&card.image, "/", "root")), "EINVAL");
    assert_eq!(card.read("/beta book.txt"), b"beta");
    parity(&card);
}

#[test]
fn remove_is_recursive_and_frees_everything() {
    let card = Card::new();
    let blank = card.list("/").free;
    let host = card.host("files");
    let tree = host.join("tree");
    for i in 0..40 {
        write(&tree.join(format!("d{}/e{}/f{i}.bin", i % 3, i % 5)), &data(i, 3_000 + i as usize * 517));
    }
    write(&host.join("single.txt"), b"one");
    upload_all(&card.image, "/", &[tree, host.join("single.txt")]).unwrap();
    assert!(card.list("/").free < blank);
    parity(&card);

    sd::remove(&card.image, "/single.txt").unwrap();
    sd::remove(&card.image, "/TREE").unwrap();
    let listing = card.list("/");
    assert!(listing.entries.is_empty(), "{:?}", listing.entries);
    assert_eq!(listing.free, blank);
    parity(&card);
}

#[test]
fn unicode_names_round_trip() {
    let card = Card::new();
    let host = card.host("files");
    for name in ["한국어 책.epub", "Ünïcødé (1).txt", "日本語のファイル名.txt", "a+b,c;d=e[f]^g.txt"] {
        write(&host.join(name), name.as_bytes());
    }
    let sources: Vec<PathBuf> =
        ["한국어 책.epub", "Ünïcødé (1).txt", "日本語のファイル名.txt", "a+b,c;d=e[f]^g.txt"].iter().map(|n| host.join(n)).collect();
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.read("/한국어 책.epub"), "한국어 책.epub".as_bytes());
    assert_eq!(card.read("/üNÏCØDÉ (1).TXT"), "Ünïcødé (1).txt".as_bytes());
    sd::mkdir(&card.image, "/폴더").unwrap();
    parity(&card);

    // Valid for fat.js but beyond what fatfs 0.3 can encode: refused up front.
    assert!(sd::valid_name("📚.epub"));
    assert_eq!(code(sd::mkdir(&card.image, "/📚")), "EINVAL");
    write(&host.join("📚.epub"), b"x");
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[host.join("📚.epub")])), "EINVAL");
}

// Windows cannot make these host files.
#[cfg(not(windows))]
#[test]
fn invalid_host_names_are_rejected() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("what?.txt"), b"x");
    write(&host.join("folder/ok.txt"), b"x");
    write(&host.join("folder/trailing."), b"x");
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[host.join("what?.txt")])), "EINVAL");
    assert_eq!(code(upload_all(&card.image, "/", &[host.join("folder")])), "EINVAL");
    assert!(card.list("/").entries.is_empty(), "nothing is written before validation");
}

#[test]
fn folder_limits() {
    let card = Card::new();
    let host = card.host("deep");
    let mut path = host.join("d");
    for i in 0..65 {
        path = path.join(format!("{i}"));
    }
    write(&path.join("leaf.txt"), b"x");
    let error = sd::plan_upload(&card.image, "/", &[host.join("d")]).unwrap_err();
    assert_eq!(error.code, "ENOTSUP");

    let wide = card.host("wide").join("many");
    fs::create_dir_all(&wide).unwrap();
    for i in 0..sd::MAX_UPLOAD_ENTRIES {
        fs::File::create(wide.join(format!("{i}"))).unwrap();
    }
    // The folder itself counts as an entry.
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[wide])), "ENOTSUP");
}

#[test]
fn fsinfo_matches_the_fat() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.bin"), &data(3, 123_457));
    upload_all(&card.image, "/", &[host.join("a.bin")]).unwrap();
    sd::mkdir(&card.image, "/x").unwrap();
    let image = fs::read(&card.image).unwrap();
    let u16_at = |at: usize| u16::from_le_bytes([image[at], image[at + 1]]) as u64;
    let u32_at = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) as u64;
    let base = u32_at(446 + 8) as usize * 512;
    let bps = u16_at(base + 11);
    let cluster = bps * image[base + 13] as u64;
    let info = base + (u16_at(base + 48) * bps) as usize;
    assert_eq!(u32_at(info), 0x4161_5252);
    assert_eq!(u32_at(info + 488) * cluster, card.list("/").free);
    // The volume's dirty flag is clear after unmount.
    assert_eq!(image[base + 0x41] & 1, 0);
}

#[test]
fn bare_volume_without_mbr() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.txt"), b"in the partition");
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let image = fs::read(&card.image).unwrap();
    let base = u32::from_le_bytes(image[454..458].try_into().unwrap()) as usize * 512;
    let bare = Card { image: card.dir.path().join("bare.img"), dir: tempfile::tempdir().unwrap() };
    fs::write(&bare.image, &image[base..]).unwrap();
    assert_eq!(bare.read("/a.txt"), b"in the partition");
    sd::mkdir(&bare.image, "/added").unwrap();
    assert_eq!(bare.names("/"), ["added", "a.txt"]);
    parity(&bare);
}

#[test]
fn not_a_fat_image() {
    let dir = tempfile::tempdir().unwrap();
    let zeros = dir.path().join("zeros.img");
    fs::write(&zeros, vec![0u8; 1 << 20]).unwrap();
    assert_eq!(code(sd::list(&zeros, "/")), "ENOTSUP");
    let tiny = dir.path().join("tiny.img");
    fs::write(&tiny, b"short").unwrap();
    assert_eq!(code(sd::list(&tiny, "/")), "ENOTSUP");
    assert_eq!(code(sd::list(&dir.path().join("absent.img"), "/")), "ENOENT");
    // An MBR whose only partition is not FAT.
    let mut mbr = vec![0u8; 1 << 20];
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    mbr[446 + 4] = 0x83;
    mbr[446 + 8] = 1;
    mbr[446 + 12] = 100;
    let linux = dir.path().join("linux.img");
    fs::write(&linux, mbr).unwrap();
    assert_eq!(code(sd::list(&linux, "/")), "ENOTSUP");
}

fn formatted(kind: fatfs::FatType, bytes: u64) -> Card {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("small.img");
    let mut file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&image).unwrap();
    file.set_len(bytes).unwrap();
    fatfs::format_volume(&mut file, fatfs::FormatVolumeOptions::new().fat_type(kind)).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    Card { dir, image }
}

#[test]
fn fat16_and_fat12_volumes() {
    for (kind, size, name) in [(fatfs::FatType::Fat16, 32 << 20, "FAT16"), (fatfs::FatType::Fat12, 1_474_560, "FAT12")] {
        let card = formatted(kind, size);
        let blank = card.list("/");
        assert_eq!(blank.fs_type, name);
        let host = card.host("files");
        write(&host.join("dir/a.bin"), &data(5, 40_000));
        write(&host.join("b.txt"), b"b");
        upload_all(&card.image, "/", &[host.join("dir"), host.join("b.txt")]).unwrap();
        assert_eq!(card.names("/"), ["dir", "b.txt"]);
        assert_eq!(card.read("/dir/a.bin"), data(5, 40_000));
        assert!(card.list("/").free < blank.free);
        sound(&card);
        sd::remove(&card.image, "/dir").unwrap();
        sd::remove(&card.image, "/b.txt").unwrap();
        assert_eq!(card.list("/").free, blank.free, "{name}");
        // fat.js reads FAT32 only, so no parity check; fsck and the audit still run.
        sound(&card);
    }
}

#[test]
fn create_blank_replaces_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("nested/folder/sd.img");
    sd::create_blank(&image).unwrap();
    assert_eq!(fs::metadata(&image).unwrap().len(), 64 * 1024 * 1024);
    sd::mkdir(&image, "/x").unwrap();
    sd::create_blank(&image).unwrap();
    assert!(sd::list(&image, "/").unwrap().entries.is_empty());
    let leftovers: Vec<_> = fs::read_dir(image.parent().unwrap()).unwrap().collect();
    assert_eq!(leftovers.len(), 1, "no temporary files remain");
}

#[test]
fn uploaded_names_are_composed() {
    let card = Card::new();
    let root = card.host("nfd");
    // "한글.txt" decomposed into conjoining jamo, as macOS may report it.
    let decomposed = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.txt";
    let composed = "\u{d55c}\u{ae00}.txt";
    write(&root.join(decomposed), b"hangul");
    upload_all(&card.image, "/", &[root.join(decomposed)]).unwrap();
    assert_eq!(card.names("/"), vec![composed.to_string()]);
    assert_eq!(card.read(&format!("/{composed}")), b"hangul");
    parity(&card);
}

/// Timing for wide folders on a stock 2050 MiB card (32 KiB clusters).
/// Run with `cargo test --release --test sd bench_wide_folder -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_wide_folder() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("stock.img");
    let mksd = Path::new(DESKTOP).join("../mksd.py");
    let status = Command::new("python3").arg(mksd).arg(&image).output().unwrap();
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    let host = dir.path().join("host/wide");
    fs::create_dir_all(&host).unwrap();
    for n in 0..1000 {
        fs::write(host.join(format!("file-{n:04}.txt")), b"").unwrap();
    }
    let start = std::time::Instant::now();
    upload_all(&image, "/", std::slice::from_ref(&host)).unwrap();
    let upload = start.elapsed();
    assert_eq!(sd::list(&image, "/wide").unwrap().entries.len(), 1000);
    let start = std::time::Instant::now();
    sd::remove(&image, "/wide").unwrap();
    let remove = start.elapsed();
    eprintln!("1000 empty files: upload {upload:?}, remove {remove:?}");
}

/* Raw access to FAT32 cards, to build damaged or unusual images. */

struct Raw {
    base: usize,
    bps: usize,
    cluster: usize,
    reserved: usize,
    fats: usize,
    fat_size: usize,
    data: usize,
    root: u32,
    info: Option<usize>,
    backup_info: Option<usize>,
}

fn u16_le(image: &[u8], at: usize) -> usize {
    u16::from_le_bytes([image[at], image[at + 1]]) as usize
}
fn u32_le(image: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(image[at..at + 4].try_into().unwrap())
}

impl Raw {
    fn of(image: &[u8]) -> Raw {
        let base = if matches!(u16_le(image, 11), 512 | 1024 | 2048 | 4096) { 0 } else { u32_le(image, 454) as usize * 512 };
        let b = |at: usize| base + at;
        let bps = u16_le(image, b(11));
        let reserved = u16_le(image, b(14));
        let fats = image[b(16)] as usize;
        let fat_size = u32_le(image, b(36)) as usize * bps;
        let info_sector = u16_le(image, b(48));
        let backup = u16_le(image, b(50));
        Raw {
            base,
            bps,
            cluster: bps * image[b(13)] as usize,
            reserved,
            fats,
            fat_size,
            data: base + reserved * bps + fats * fat_size,
            root: u32_le(image, b(44)),
            info: (info_sector != 0 && info_sector != 0xffff).then(|| base + info_sector * bps),
            backup_info: (backup != 0 && backup != 0xffff).then(|| base + (backup + info_sector) * bps),
        }
    }
    fn fat_offset(&self, copy: usize, cluster: u32) -> usize {
        self.base + self.reserved * self.bps + copy * self.fat_size + cluster as usize * 4
    }
    fn get(&self, image: &[u8], cluster: u32) -> u32 {
        u32_le(image, self.fat_offset(0, cluster)) & 0x0fff_ffff
    }
    fn set(&self, image: &mut [u8], copy: usize, cluster: u32, value: u32) {
        let at = self.fat_offset(copy, cluster);
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn set_all(&self, image: &mut [u8], cluster: u32, value: u32) {
        for copy in 0..self.fats {
            self.set(image, copy, cluster, value);
        }
    }
    fn chain(&self, image: &[u8], first: u32) -> Vec<u32> {
        let mut result = vec![first];
        while self.get(image, *result.last().unwrap()) < 0x0fff_fff8 {
            result.push(self.get(image, *result.last().unwrap()));
        }
        result
    }
    /// Short-name records of a folder: (offset of the record, 11-byte name, start cluster).
    fn records(&self, image: &[u8], first: u32) -> Vec<(usize, Vec<u8>, u32)> {
        let mut result = Vec::new();
        for cluster in self.chain(image, first) {
            let start = self.data + (cluster as usize - 2) * self.cluster;
            for at in (start..start + self.cluster).step_by(32) {
                if image[at] == 0 {
                    return result;
                }
                if image[at] == 0xe5 || image[at + 11] & 0x3f == 0x0f || image[at + 11] & 0x08 != 0 {
                    continue;
                }
                let first = (u16_le(image, at + 20) << 16 | u16_le(image, at + 26)) as u32;
                result.push((at, image[at..at + 11].to_vec(), first));
            }
        }
        result
    }
    fn record(&self, image: &[u8], folder: u32, name: &[u8; 11]) -> (usize, u32) {
        let (at, _, first) = self.records(image, folder).into_iter().find(|(_, n, _)| n == name).expect("record");
        (at, first)
    }
}

fn identical(card: &Card, before: &[u8]) {
    assert!(fs::read(&card.image).unwrap() == before, "the image must be unchanged");
}

/// The cluster size, from the free space one byte takes.
fn cluster_size(card: &Card) -> u64 {
    let before = card.list("/").free;
    let host = card.host("probe");
    write(&host.join("probe.bin"), b"x");
    upload_all(&card.image, "/", &[host.join("probe.bin")]).unwrap();
    let size = before - card.list("/").free;
    sd::remove(&card.image, "/probe.bin").unwrap();
    size
}

#[test]
fn upload_failing_midway_leaves_the_image_unchanged() {
    let card = formatted(fatfs::FatType::Fat12, 1_474_560);
    let cluster = cluster_size(&card);
    let host = card.host("first");
    write(&host.join("a.txt"), &data(1, 400));
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let free = card.list("/").free;
    write(&host.join("b.txt"), &data(2, free as usize));
    upload_all(&card.image, "/", &[host.join("b.txt")]).unwrap();
    assert_eq!(card.list("/").free, 0);
    let before = fs::read(&card.image).unwrap();

    // Growing a.txt first needs space that only shrinking b.txt frees.
    let second = card.host("second");
    let grown = data(3, 3 * cluster as usize);
    write(&second.join("a.txt"), &grown);
    write(&second.join("b.txt"), b"");
    let sources = [second.join("a.txt"), second.join("b.txt")];
    // The planner follows the upload's order, so it refuses this too.
    assert_eq!(code(sd::plan_upload(&card.image, "/", &sources)), "ENOSPC");
    let error = upload_all(&card.image, "/", &sources).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(!error.changed);
    identical(&card, &before);
    assert_eq!(card.read("/a.txt"), data(1, 400));

    // In the other order it fits, and the planner says so.
    let reversed = [second.join("b.txt"), second.join("a.txt")];
    assert_eq!(sd::plan_upload(&card.image, "/", &reversed).unwrap().conflicts, ["/b.txt", "/a.txt"]);
    upload_all(&card.image, "/", &reversed).unwrap();
    assert_eq!(card.read("/a.txt"), grown);
    assert_eq!(card.read("/b.txt"), b"");
    sound(&card);
}

#[cfg(unix)]
#[test]
fn host_read_error_rolls_back_the_upload() {
    use std::os::unix::fs::PermissionsExt;
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("precious.txt"), b"original");
    upload_all(&card.image, "/", &[host.join("precious.txt")]).unwrap();
    let before = fs::read(&card.image).unwrap();
    let next = card.host("next");
    write(&next.join("large.bin"), &data(4, 3 << 20));
    write(&next.join("precious.txt"), b"replacement");
    fs::set_permissions(next.join("precious.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(next.join("precious.txt")).is_ok() {
        eprintln!("running with permission overrides: skipping");
        return;
    }
    let error = upload_all(&card.image, "/", &[next.join("large.bin"), next.join("precious.txt")]).unwrap_err();
    assert_eq!(error.code, "EIO");
    assert!(!error.changed);
    identical(&card, &before);
    assert_eq!(card.read("/precious.txt"), b"original");
    parity(&card);
}

#[test]
fn rename_into_a_full_fixed_root_leaves_the_image_unchanged() {
    let card = formatted(fatfs::FatType::Fat12, 1_474_560);
    let image = fs::read(&card.image).unwrap();
    let root_entries = u16_le(&image, 17);
    // Lower-case names take a long-name slot and a short slot each.
    let host = card.host("many");
    let count = root_entries / 2 - 1;
    let sources: Vec<PathBuf> = (0..count)
        .map(|n| {
            let path = host.join(format!("f{n:04}.txt"));
            write(&path, b"");
            path
        })
        .collect();
    upload_all(&card.image, "/", &sources).unwrap();
    let before = fs::read(&card.image).unwrap();
    let long = format!("{}.txt", "x".repeat(200));
    let error = sd::rename(&card.image, "/f0000.txt", &long).unwrap_err();
    assert_eq!(error.code, "ENOSPC", "{error:?}");
    assert!(!error.changed);
    identical(&card, &before);
    assert_eq!(card.list("/").entries.len(), count);
    // The planner sees the full root folder too.
    write(&host.join("one-more-file-with-a-long-name.txt"), b"");
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[host.join("one-more-file-with-a-long-name.txt")])), "ENOSPC");
}

#[test]
fn read_only_operations_never_write() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.txt"), b"contents");
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    // Stale FSInfo hints must not be repaired on a read.
    let mut image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    let info = raw.info.unwrap();
    image[info + 488..info + 492].copy_from_slice(&0u32.to_le_bytes());
    fs::write(&card.image, &image).unwrap();
    let before = image;

    card.list("/");
    sd::plan_upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
    card.read("/a.txt");
    for error in [
        sd::mkdir(&card.image, "/A.TXT").unwrap_err(),
        sd::rename(&card.image, "/missing", "x").unwrap_err(),
        sd::remove(&card.image, "/missing").unwrap_err(),
        upload_all(&card.image, "/missing", &[host.join("a.txt")]).unwrap_err(),
        sd::mkdir(&card.image, "/bad:name").unwrap_err(),
    ] {
        assert!(!error.changed, "{error:?}");
    }
    identical(&card, &before);

    // A change rewrites both FSInfo copies from the FAT.
    sd::mkdir(&card.image, "/new").unwrap();
    fsinfo_agrees(&card);
}

/// Both FSInfo copies hold the FAT's free count and first free cluster.
fn fsinfo_agrees(card: &Card) {
    let image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    let clusters = (card.list("/").total / raw.cluster as u64) as u32;
    let free = (2..clusters + 2).filter(|&c| raw.get(&image, c) == 0).count() as u32;
    let first = (2..clusters + 2).find(|&c| raw.get(&image, c) == 0).unwrap_or(0xffff_ffff);
    assert_eq!(free as u64 * raw.cluster as u64, card.list("/").free);
    let copies = [raw.info, raw.backup_info];
    assert!(copies.iter().all(Option::is_some), "the blank card has a backup FSInfo");
    for at in copies.into_iter().flatten() {
        assert_eq!(u32_le(&image, at), 0x4161_5252);
        assert_eq!((u32_le(&image, at + 488), u32_le(&image, at + 492)), (free, first), "FSInfo at {at:#x}");
    }
}

#[test]
fn both_fsinfo_copies_follow_changes() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("dir/a.bin"), &data(6, 70_000));
    upload_all(&card.image, "/", &[host.join("dir")]).unwrap();
    fsinfo_agrees(&card);
    sd::rename(&card.image, "/dir", "renamed").unwrap();
    fsinfo_agrees(&card);
    sd::remove(&card.image, "/renamed").unwrap();
    fsinfo_agrees(&card);
    parity(&card);
}

#[test]
fn damaged_fats_are_refused() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.bin"), &data(7, 2000));
    write(&host.join("b.bin"), &data(8, 2000));
    upload_all(&card.image, "/", &[host.join("a.bin"), host.join("b.bin")]).unwrap();
    let good = fs::read(&card.image).unwrap();
    let raw = Raw::of(&good);
    let (_, a) = raw.record(&good, raw.root, b"A       BIN");
    let (_, b) = raw.record(&good, raw.root, b"B       BIN");
    let a_chain = raw.chain(&good, a);
    let b_chain = raw.chain(&good, b);
    let free_cluster = (2..).find(|&c| raw.get(&good, c) == 0).unwrap();

    type Damage<'a> = Box<dyn Fn(&mut Vec<u8>) + 'a>;
    let cases: Vec<(&str, Damage<'_>)> = vec![
        ("copies disagree", Box::new(|image: &mut Vec<u8>| raw.set(image, 1, free_cluster, 0x0fff_ffff))),
        ("mirroring disabled", Box::new(|image: &mut Vec<u8>| image[raw.base + 40] = 0x81)),
        ("root loops", Box::new(|image: &mut Vec<u8>| raw.set_all(image, raw.root, raw.root))),
        ("cross-link", Box::new(|image: &mut Vec<u8>| raw.set_all(image, b_chain[0], a_chain[1]))),
        ("into free space", Box::new(|image: &mut Vec<u8>| raw.set_all(image, *a_chain.last().unwrap(), free_cluster))),
        ("out of range", Box::new(|image: &mut Vec<u8>| raw.set_all(image, *a_chain.last().unwrap(), 0x0fff_0000))),
    ];
    for (name, damage) in cases {
        let mut image = good.clone();
        damage(&mut image);
        fs::write(&card.image, &image).unwrap();
        let started = std::time::Instant::now();
        let error = sd::list(&card.image, "/").unwrap_err();
        assert!(started.elapsed().as_secs() < 5, "{name}: listing must terminate");
        assert_eq!(error.code, "ENOTSUP", "{name}: {error:?}");
        assert_eq!(code(sd::mkdir(&card.image, "/x")), "ENOTSUP", "{name}");
        assert_eq!(code(sd::remove(&card.image, "/a.bin")), "ENOTSUP", "{name}");
        assert!(fs::read(&card.image).unwrap() == image, "{name}: the image is unchanged");
    }
}

#[test]
fn cross_linked_trees_are_not_removed() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("tree/a.txt"), &data(9, 1500));
    write(&host.join("tree/b.txt"), &data(10, 1500));
    upload_all(&card.image, "/", &[host.join("tree")]).unwrap();
    let mut image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    let (_, tree) = raw.record(&image, raw.root, b"TREE       ");
    let (_, a) = raw.record(&image, tree, b"A       TXT");
    let (b_record, _) = raw.record(&image, tree, b"B       TXT");
    // b.txt now starts at a.txt's first cluster; its own chain is orphaned.
    image[b_record + 20..b_record + 22].copy_from_slice(&((a >> 16) as u16).to_le_bytes());
    image[b_record + 26..b_record + 28].copy_from_slice(&(a as u16).to_le_bytes());
    fs::write(&card.image, &image).unwrap();
    let error = sd::remove(&card.image, "/tree").unwrap_err();
    assert_eq!(error.code, "EINVAL");
    assert!(error.message.contains("Cross-linked"), "{}", error.message);
    identical(&card, &image);
}

#[test]
fn code_page_437_short_names() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("x1.txt"), b"A");
    write(&host.join("x2.txt"), b"B");
    upload_all(&card.image, "/", &[host.join("x1.txt"), host.join("x2.txt")]).unwrap();
    let mut image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    for (name, byte) in [(b"X1      TXT", 0x82u8), (b"X2      TXT", 0x84)] {
        let (at, _) = raw.record(&image, raw.root, name);
        image[at] = byte;
        image[at + 1] = b' ';
        image[at + 12] = 0; // no lower-case flags
        // Drop the long name so only the short name remains.
        let mut slot = at - 32;
        while image[slot + 11] & 0x3f == 0x0f {
            image[slot] = 0xe5;
            slot -= 32;
        }
    }
    fs::write(&card.image, &image).unwrap();
    assert_eq!(card.names("/"), ["\u{e4}.TXT", "\u{e9}.TXT"]);
    assert_eq!(card.read("/\u{e9}.TXT"), b"A");
    assert_eq!(card.read("/\u{e4}.TXT"), b"B");
    parity(&card);
    sd::remove(&card.image, "/\u{e4}.TXT").unwrap();
    assert_eq!(card.names("/"), ["\u{e9}.TXT"]);
    assert_eq!(card.read("/\u{e9}.TXT"), b"A");
    parity(&card);
}

#[test]
fn export_refuses_the_card_image() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.txt"), b"a");
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let before = fs::read(&card.image).unwrap();
    let dotted = card.dir.path().join(".").join("sd.img");
    let mut targets = vec![card.image.clone(), dotted];
    let link = card.dir.path().join("link.img");
    if fs::hard_link(&card.image, &link).is_ok() {
        targets.push(link);
    }
    for target in targets {
        let error = sd::export(&card.image, "/a.txt", &target).unwrap_err();
        assert_eq!(error.code, "EINVAL", "{}", target.display());
    }
    identical(&card, &before);
    assert_eq!(card.read("/a.txt"), b"a");
}

#[cfg(unix)]
#[test]
fn exports_and_new_cards_respect_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("a.txt"), b"a");
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let private = card.dir.path().join("private.txt");
    fs::write(&private, b"old").unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();
    sd::export(&card.image, "/a.txt", &private).unwrap();
    assert_eq!(fs::read(&private).unwrap(), b"a");
    assert_eq!(mode(&private), 0o600);
    // New files get what the umask gives an ordinary new file.
    let reference = card.dir.path().join("reference.txt");
    fs::File::create(&reference).unwrap();
    let fresh = card.dir.path().join("fresh.txt");
    sd::export(&card.image, "/a.txt", &fresh).unwrap();
    assert_eq!(mode(&fresh), mode(&reference));
    let new_card = card.dir.path().join("new.img");
    sd::create_blank(&new_card).unwrap();
    assert_eq!(mode(&new_card), mode(&reference));
    fs::set_permissions(&new_card, fs::Permissions::from_mode(0o600)).unwrap();
    sd::create_blank(&new_card).unwrap();
    assert_eq!(mode(&new_card), 0o600);
}

#[test]
fn large_uploads_spill_and_round_trip() {
    // More than the journal keeps in memory (32 MiB).
    let card = Card::new();
    let host = card.host("big");
    let bytes = data(11, 40 << 20);
    write(&host.join("big.bin"), &bytes);
    upload_all(&card.image, "/", &[host.join("big.bin")]).unwrap();
    assert!(card.read("/big.bin") == bytes);
    parity(&card);
}

/* Short-name aliases: fatfs opens an existing entry whose short name
   matches instead of creating a new one. */

#[test]
fn sources_colliding_through_a_short_alias_are_refused() {
    let card = Card::new();
    let host = card.host("alias");
    write(&host.join("LongLongFileName.txt"), &[b'A'; 1000]);
    write(&host.join("LONGLO~1.TXT"), &[b'B'; 1000]);
    let before = fs::read(&card.image).unwrap();
    // LongLongFileName.txt gets the short name LONGLO~1.TXT.
    let sources = [host.join("LongLongFileName.txt"), host.join("LONGLO~1.TXT")];
    assert_eq!(code(sd::plan_upload(&card.image, "/", &sources)), "EINVAL");
    let error = upload_all(&card.image, "/", &sources).unwrap_err();
    assert_eq!(error.code, "EINVAL");
    assert!(!error.changed);
    identical(&card, &before);

    // The other way round, the long name gets LONGLO~2.TXT: two files.
    upload_all(&card.image, "/", &[host.join("LONGLO~1.TXT"), host.join("LongLongFileName.txt")]).unwrap();
    assert_eq!(card.names("/"), ["LongLongFileName.txt", "LONGLO~1.TXT"]);
    assert_eq!(card.read("/LONGLO~1.TXT"), [b'B'; 1000]);
    assert_eq!(card.read("/LongLongFileName.txt"), [b'A'; 1000]);
    parity(&card);
}

#[test]
fn a_short_alias_names_the_existing_file() {
    let card = Card::new();
    let host = card.host("seed");
    write(&host.join("LongLongFileName.txt"), b"original");
    upload_all(&card.image, "/", &[host.join("LongLongFileName.txt")]).unwrap();
    let before = fs::read(&card.image).unwrap();

    // Both names refer to the one file on the card.
    let next = card.host("next");
    write(&next.join("LongLongFileName.txt"), &[b'A'; 3000]);
    write(&next.join("LONGLO~1.TXT"), &[b'B'; 3000]);
    let sources = [next.join("LongLongFileName.txt"), next.join("LONGLO~1.TXT")];
    assert_eq!(code(sd::plan_upload(&card.image, "/", &sources)), "EINVAL");
    assert_eq!(code(upload_all(&card.image, "/", &sources)), "EINVAL");
    identical(&card, &before);

    // The alias alone replaces that file, as fat.js would.
    let plan = sd::plan_upload(&card.image, "/", &[next.join("LONGLO~1.TXT")]).unwrap();
    assert_eq!(plan.conflicts, ["/LongLongFileName.txt"]);
    upload_all(&card.image, "/", &[next.join("LONGLO~1.TXT")]).unwrap();
    assert_eq!(card.names("/"), ["LongLongFileName.txt"]);
    assert_eq!(card.read("/LongLongFileName.txt"), [b'B'; 3000]);
    parity(&card);
}

#[test]
fn planned_short_names_follow_fatfs() {
    let card = Card::new();
    let host = card.host("names");
    let mut sources: Vec<PathBuf> = (1..=4)
        .map(|n| {
            let path = host.join(format!("LongName{n}.txt"));
            write(&path, format!("{n}").as_bytes());
            path
        })
        .collect();
    // The third long name takes LONGNA~3.TXT, so this one is the same file.
    write(&host.join("LONGNA~3.TXT"), b"x");
    let mut clash = sources.clone();
    clash.push(host.join("LONGNA~3.TXT"));
    assert_eq!(code(sd::plan_upload(&card.image, "/", &clash)), "EINVAL");
    // A fifth long name gets a checksum alias, not LONGNA~5.TXT.
    write(&host.join("LongName5.txt"), b"5");
    write(&host.join("LONGNA~5.TXT"), b"y");
    sources.push(host.join("LongName5.txt"));
    sources.push(host.join("LONGNA~5.TXT"));
    let plan = sd::plan_upload(&card.image, "/", &sources).unwrap();
    assert!(plan.conflicts.is_empty());
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.list("/").entries.len(), 6);
    for n in 1..=5 {
        assert_eq!(card.read(&format!("/LongName{n}.txt")), format!("{n}").as_bytes());
    }
    assert_eq!(card.read("/LONGNA~5.TXT"), b"y");
    let image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    let shorts: Vec<Vec<u8>> = raw.records(&image, raw.root).into_iter().map(|(_, name, _)| name).collect();
    for n in 1..=4 {
        assert!(shorts.contains(&format!("LONGNA~{n}TXT").as_bytes().to_vec()), "LONGNA~{n}");
    }
    parity(&card);
}

/* Cross-links anywhere on the card stop edits that could free shared
   storage. */

#[test]
fn cross_links_outside_the_target_are_refused() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("tree/a.txt"), b"A");
    write(&host.join("tree/b.txt"), b"B");
    upload_all(&card.image, "/", &[host.join("tree")]).unwrap();
    let mut image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    let (_, tree) = raw.record(&image, raw.root, b"TREE       ");
    let (_, a) = raw.record(&image, tree, b"A       TXT");
    let (b_record, b) = raw.record(&image, tree, b"B       TXT");
    image[b_record + 20..b_record + 22].copy_from_slice(&((a >> 16) as u16).to_le_bytes());
    image[b_record + 26..b_record + 28].copy_from_slice(&(a as u16).to_le_bytes());
    raw.set_all(&mut image, b, 0); // b's own cluster is no longer used
    fs::write(&card.image, &image).unwrap();
    for error in [
        sd::remove(&card.image, "/tree/a.txt").unwrap_err(),
        sd::mkdir(&card.image, "/x").unwrap_err(),
        sd::rename(&card.image, "/tree/a.txt", "c.txt").unwrap_err(),
    ] {
        assert_eq!(error.code, "ENOTSUP", "{error:?}");
        assert!(error.message.contains("cross-linked"), "{}", error.message);
        assert!(!error.changed);
    }
    identical(&card, &image);
    // Reading still works.
    assert_eq!(card.read("/tree/b.txt"), b"A");
}

fn bad_chain_variants(card: &Card, kind: &str, chain_tail: impl Fn(&mut Vec<u8>, u32, u32), get: impl Fn(&[u8], u32) -> u32, bad: u32, reserved: u32) {
    let host = card.host("bad");
    write(&host.join("a.bin"), &data(12, 3000));
    upload_all(&card.image, "/", &[host.join("a.bin")]).unwrap();
    let good = fs::read(&card.image).unwrap();
    let first_free = (2..).find(|&c| get(&good, c) == 0).unwrap();
    let last = (2..first_free).filter(|&c| get(&good, c) > bad).last().unwrap();

    // A chain that runs into a bad (or reserved) cluster: refused, never a panic.
    for mark in [bad, reserved] {
        let mut image = good.clone();
        chain_tail(&mut image, last, first_free);
        chain_tail(&mut image, first_free, mark);
        fs::write(&card.image, &image).unwrap();
        assert_eq!(code(sd::list(&card.image, "/")), "ENOTSUP", "{kind} {mark:#x}");
        assert_eq!(code(sd::remove(&card.image, "/anything")), "ENOTSUP", "{kind} {mark:#x}");
        assert_eq!(code(sd::remove(&card.image, "/a.bin")), "ENOTSUP", "{kind} {mark:#x}");
        assert!(fs::read(&card.image).unwrap() == image);
    }
    // An isolated bad or reserved mark is healthy: the cluster is just unusable.
    let mut image = good.clone();
    chain_tail(&mut image, first_free, bad);
    chain_tail(&mut image, first_free + 1, reserved);
    fs::write(&card.image, &image).unwrap();
    let free = card.list("/").free;
    write(&host.join("b.bin"), &data(13, 5000));
    upload_all(&card.image, "/", &[host.join("b.bin")]).unwrap();
    assert_eq!(card.read("/b.bin"), data(13, 5000));
    assert!(card.list("/").free < free);
    sd::remove(&card.image, "/a.bin").unwrap();
    let after = fs::read(&card.image).unwrap();
    assert_eq!(get(&after, first_free), bad, "{kind}: the bad mark stays");
    assert_eq!(get(&after, first_free + 1), reserved, "{kind}: the reserved mark stays");
    // Not sound(): fsck_msdos counts a planted reserved value as an orphan.
    assert_eq!(sd::lost_clusters(&card.image).unwrap(), 0, "{kind}: lost clusters");
}

#[test]
fn chains_into_bad_clusters_are_refused_without_panicking() {
    let card = Card::new();
    let image = fs::read(&card.image).unwrap();
    let raw = Raw::of(&image);
    bad_chain_variants(
        &card,
        "FAT32",
        |image, cluster, value| raw.set_all(image, cluster, value),
        |image, cluster| raw.get(image, cluster),
        0x0fff_fff7,
        0x0fff_fff0,
    );

    let card = formatted(fatfs::FatType::Fat16, 32 << 20);
    let image = fs::read(&card.image).unwrap();
    let bps = u16_le(&image, 11);
    let fat = u16_le(&image, 14) * bps;
    let fat_size = u16_le(&image, 22) * bps;
    let fats = image[16] as usize;
    bad_chain_variants(
        &card,
        "FAT16",
        |image, cluster, value| {
            for copy in 0..fats {
                let at = fat + copy * fat_size + cluster as usize * 2;
                image[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
        },
        |image, cluster| u16_le(image, fat + cluster as usize * 2) as u32,
        0xfff7,
        0xfff0,
    );
}

/* The planner's space model. */

/// Fill all free clusters with one file.
fn fill(card: &Card) {
    let host = card.host("fill");
    let free = card.list("/").free;
    write(&host.join("fill.bin"), &vec![0x5a; free as usize]);
    upload_all(&card.image, "/", &[host.join("fill.bin")]).unwrap();
    assert_eq!(card.list("/").free, 0);
}

#[test]
fn a_full_card_takes_empty_files_but_not_folders() {
    let card = Card::new();
    fill(&card);
    let host = card.host("empty");
    write(&host.join("empty.txt"), b"");
    fs::create_dir_all(host.join("folder")).unwrap();
    // The root folder has free slots in its cluster: no cluster is needed.
    assert!(sd::plan_upload(&card.image, "/", &[host.join("empty.txt")]).unwrap().conflicts.is_empty());
    upload_all(&card.image, "/", &[host.join("empty.txt")]).unwrap();
    assert_eq!(card.read("/empty.txt"), b"");
    // A new folder needs a cluster of its own, even when empty.
    let before = fs::read(&card.image).unwrap();
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[host.join("folder")])), "ENOSPC");
    assert_eq!(code(upload_all(&card.image, "/", &[host.join("folder")])), "ENOSPC");
    identical(&card, &before);
    parity(&card);
}

#[test]
fn plans_that_fit_pass_and_succeed() {
    // Many long names grow a folder by several clusters; replacements give
    // their clusters back first. The plan and the upload agree.
    let card = formatted(fatfs::FatType::Fat16, 32 << 20);
    let host = card.host("tree");
    for n in 0..300 {
        write(&host.join(format!("folder/a rather long file name number {n:03}.txt")), &data(n, 700));
    }
    let free = card.list("/").free;
    let plan = sd::plan_upload(&card.image, "/", &[host.join("folder")]).unwrap();
    assert_eq!(plan.files, 300);
    upload_all(&card.image, "/", &[host.join("folder")]).unwrap();
    assert!(card.list("/").free < free);
    for n in 0..300 {
        write(&host.join(format!("folder/a rather long file name number {n:03}.txt")), &data(n + 1000, 100));
    }
    let plan = sd::plan_upload(&card.image, "/", &[host.join("folder")]).unwrap();
    assert_eq!(plan.conflicts.len(), 300);
    upload_all(&card.image, "/", &[host.join("folder")]).unwrap();
    assert_eq!(card.read("/folder/a rather long file name number 123.txt"), data(1123, 100));
    sound(&card);
}

// sd::fail_commit_after is a debug-build test hook (tests run in debug).
#[cfg(debug_assertions)]
mod commit_failures {
    use super::*;

    /* Commit failures: new data reaches the card first, then folders and the
       FATs. */

    struct Injected;
    impl Injected {
        fn after(bytes: u64) -> Injected {
            sd::fail_commit_after(Some(bytes));
            Injected
        }
    }
    impl Drop for Injected {
        fn drop(&mut self) {
            sd::fail_commit_after(None);
        }
    }

    #[test]
    fn a_failure_while_writing_new_data_changes_nothing_on_the_card() {
        let card = Card::new();
        let host = card.host("files");
        write(&host.join("keep.txt"), b"keep");
        upload_all(&card.image, "/", &[host.join("keep.txt")]).unwrap();
        let before = fs::read(&card.image).unwrap();
        let view = rust_view(&card);
        write(&host.join("big.bin"), &data(14, 1 << 20));
        let error = {
            let _guard = Injected::after(8192);
            upload_all(&card.image, "/", &[host.join("big.bin")]).unwrap_err()
        };
        assert_eq!(error.code, "EIO");
        assert!(!error.changed, "{error:?}");
        // Only clusters the card does not use were written.
        let after = fs::read(&card.image).unwrap();
        let raw = Raw::of(&after);
        assert!(after[..raw.data] == before[..raw.data], "reserved sectors and FATs are unchanged");
        assert_eq!(rust_view(&card), view);
        parity(&card);
        // The same upload then works.
        upload_all(&card.image, "/", &[host.join("big.bin")]).unwrap();
        assert!(card.read("/big.bin") == data(14, 1 << 20));
        parity(&card);
    }

    #[test]
    fn a_failure_after_new_data_reports_a_changed_card() {
        let card = Card::new();
        let host = card.host("files");
        write(&host.join("a.txt"), b"a");
        upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
        // A rename only rewrites folder slots: the first write already counts.
        let error = {
            let _guard = Injected::after(0);
            sd::rename(&card.image, "/a.txt", "b.txt").unwrap_err()
        };
        assert_eq!(error.code, "EIO");
        assert!(error.changed, "{error:?}");
    }
}

/* Folder structure (review-final findings 1 and 2), names with leading
   dots, confirmed conflicts, temporary space and a full fixed root. */

/// A FAT32 volume's folder records: (short name, attribute, first cluster)
/// for every slot of `cluster`'s first cluster, until the end marker.
fn fat32_records(card: &Card, cluster: u32) -> Vec<([u8; 11], u8, u32)> {
    let volume = volume(&card.image);
    let bps = u16_le(&volume, 11);
    let spc = volume[13] as usize;
    let data = (u16_le(&volume, 14) + volume[16] as usize * u32_le(&volume, 36) as usize) * bps;
    let size = spc * bps;
    let start = data + (cluster as usize - 2) * size;
    volume[start..start + size]
        .as_chunks::<32>()
        .0
        .iter()
        .take_while(|r| r[0] != 0)
        .map(|r| {
            let cluster = u16::from_le_bytes([r[26], r[27]]) as u32 | (u16::from_le_bytes([r[20], r[21]]) as u32) << 16;
            (r[..11].try_into().unwrap(), r[11], cluster)
        })
        .collect()
}

fn short(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    out[..name.len()].copy_from_slice(name.as_bytes());
    out
}

fn child_cluster(card: &Card, folder: u32, name: &str) -> u32 {
    fat32_records(card, folder).into_iter().find(|(n, attr, _)| *n == short(name) && *attr != 0x0f).unwrap().2
}

#[test]
fn new_folders_start_with_dot_entries() {
    let card = Card::new();
    sd::mkdir(&card.image, "/books").unwrap();
    sd::mkdir(&card.image, "/books/Sub Folder").unwrap();
    let host = card.host("up");
    write(&host.join("tree/inner/book.epub"), &data(3, 2000));
    upload_all(&card.image, "/books", &[host.join("tree")]).unwrap();
    parity(&card);
    let books = child_cluster(&card, 2, "BOOKS");
    let records = fat32_records(&card, books);
    // "." then "..", short entries only; ".." of a folder in the root is 0.
    assert_eq!((records[0].0, records[0].1, records[0].2), (short("."), 0x10, books));
    assert_eq!((records[1].0, records[1].1, records[1].2), (short(".."), 0x10, 0));
    let tree = child_cluster(&card, books, "TREE");
    let inner = child_cluster(&card, tree, "INNER");
    let records = fat32_records(&card, inner);
    assert_eq!((records[0].0, records[0].2), (short("."), inner));
    assert_eq!((records[1].0, records[1].2), (short(".."), tree));
    assert_eq!(card.read("/books/tree/inner/book.epub"), data(3, 2000));
}

#[test]
fn growing_the_root_folder_reuses_freed_clusters_cleanly() {
    // A deleted file leaves its bytes in free clusters, which the root
    // folder then grows into (512-byte clusters hold 16 slots).
    let dirty = |card: &Card| {
        let host = card.host("junk");
        write(&host.join("junk.bin"), &[0x41; 8192]);
        upload_all(&card.image, "/", &[host.join("junk.bin")]).unwrap();
        sd::remove(&card.image, "/junk.bin").unwrap();
    };
    let card = Card::new();
    dirty(&card);
    let host = card.host("files");
    let sources: Vec<PathBuf> = (0..10)
        .map(|n| {
            let path = host.join(format!("a file number {n:02}.txt"));
            write(&path, b"x");
            path
        })
        .collect();
    assert_eq!(sd::plan_upload(&card.image, "/", &sources).unwrap().files, 10);
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.names("/").len(), 10);
    parity(&card);

    let card = Card::new();
    dirty(&card);
    for n in 0..10 {
        sd::mkdir(&card.image, &format!("/a folder number {n:02}")).unwrap();
    }
    assert_eq!(card.names("/").len(), 10);
    parity(&card);

    // A removed folder's cluster, reused by the root, must not leave its
    // old entries behind.
    let card = Card::new();
    let host = card.host("ghost");
    write(&host.join("old/GHOST1.TXT"), b"");
    write(&host.join("old/GHOST2.TXT"), b"");
    upload_all(&card.image, "/", &[host.join("old")]).unwrap();
    sd::remove(&card.image, "/old").unwrap();
    let sources: Vec<PathBuf> = (0..6)
        .map(|n| {
            let path = host.join(format!("a file number {n:02}.txt"));
            write(&path, b"");
            path
        })
        .collect();
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.names("/").len(), 6);
    parity(&card);
}

#[test]
fn names_with_leading_dots_get_valid_short_names() {
    let card = Card::new();
    let host = card.host("dots");
    let names = [".top", ".a.b", "..x", ". spaced", ".TOP2.txt"];
    for name in names {
        write(&host.join(name), name.as_bytes());
    }
    let sources: Vec<PathBuf> = names.iter().map(|n| host.join(n)).collect();
    assert_eq!(sd::plan_upload(&card.image, "/", &sources).unwrap().conflicts, Vec::<String>::new());
    upload_all(&card.image, "/", &sources).unwrap();
    for name in names {
        assert_eq!(card.read(&format!("/{name}")), name.as_bytes());
    }
    parity(&card);
    // Uploading them again finds each one, by its long name.
    let plan = sd::plan_upload(&card.image, "/", &sources).unwrap();
    assert_eq!(plan.conflicts.len(), names.len());
    upload_all(&card.image, "/", &sources).unwrap();
    assert_eq!(card.names("/").len(), names.len());
    parity(&card);
}

#[test]
fn upload_refuses_conflicts_that_changed_since_the_plan() {
    let card = Card::new();
    let host = card.host("c");
    write(&host.join("a.txt"), b"a");
    write(&host.join("b.txt"), b"b");
    upload_all(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let sources = [host.join("a.txt"), host.join("b.txt")];
    let plan = sd::plan_upload(&card.image, "/", &sources).unwrap();
    assert_eq!(plan.conflicts, ["/a.txt"]);
    // The card changes before the upload: b.txt now exists too.
    upload_all(&card.image, "/", &[host.join("b.txt")]).unwrap();
    let before = fs::read(&card.image).unwrap();
    let error = sd::upload(&card.image, "/", &sources, &plan.conflicts).unwrap_err();
    assert_eq!((error.code, error.changed), ("EEXIST", false));
    identical(&card, &before);
    // Agreeing to nothing while a conflict exists is refused as well.
    assert_eq!(code(sd::upload(&card.image, "/", &[host.join("a.txt")], &[])), "EEXIST");
    identical(&card, &before);
    // Case differences do not matter.
    sd::upload(&card.image, "/", &sources, &["/A.TXT".to_string(), "/b.TXT".to_string()]).unwrap();
    parity(&card);
}

#[test]
fn running_out_of_temporary_space_is_not_a_card_error() {
    // TMPDIR is process-wide, so the check runs in a child test process.
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "temporary_space_child", "--nocapture", "--test-threads=1"])
        .env("TMPDIR", dir.path().join("missing"))
        // Windows takes its temporary directory from TMP, then TEMP.
        .env("TMP", dir.path().join("missing"))
        .env("TEMP", dir.path().join("missing"))
        .env("SD_TEST_DIR", dir.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success() && text.contains("1 passed"), "{text}{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
#[ignore = "run by running_out_of_temporary_space_is_not_a_card_error"]
fn temporary_space_child() {
    let dir = PathBuf::from(std::env::var_os("SD_TEST_DIR").expect("run by the parent test"));
    let image = dir.join("sd.img");
    sd::create_blank(&image).unwrap();
    // Over the 32 MiB kept in memory, so the journal needs a spill file.
    let source = dir.join("big.bin");
    fs::write(&source, vec![3u8; 40 << 20]).unwrap();
    let before = fs::read(&image).unwrap();
    let error = sd::upload(&image, "/", &[source], &[]).unwrap_err();
    assert_eq!((error.code, error.changed), ("EIO", false), "{error:?}");
    assert!(error.message.starts_with("Could not stage the change in temporary storage"), "{}", error.message);
    assert!(fs::read(&image).unwrap() == before, "the card is unchanged");
}

#[test]
fn a_full_fixed_root_says_so() {
    let card = formatted(fatfs::FatType::Fat12, 1_474_560);
    let mut made = 0;
    let error = loop {
        match sd::mkdir(&card.image, &format!("/D{made:03}")) {
            Ok(()) => made += 1,
            Err(error) => break error,
        }
        assert!(made < 1000, "the root never filled");
    };
    assert_eq!(error.code, "ENOSPC");
    assert!(error.message.contains("root folder is full"), "{}", error.message);
    assert!(!error.changed);
    sound(&card);
}
