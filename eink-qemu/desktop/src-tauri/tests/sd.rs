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

/// fat.js and fatfs agree on the whole card.
fn parity(card: &Card) {
    let rust = rust_view(card);
    if let Some(js) = fat_js(&card.image) {
        assert_eq!(js, rust, "fat.js and the Rust explorer disagree");
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
    sd::upload(&card.image, "/", &sources).unwrap();
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
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(lib.join("x.bin"), lib.join("link.bin")).unwrap();
        std::os::unix::fs::symlink(lib.join("sub"), lib.join("linkdir")).unwrap();
        std::os::unix::fs::symlink(lib.join("x.bin"), root.join("top-link.bin")).unwrap();
    }
    write(&root.join(".top"), b"explicitly chosen hidden file");
    sd::mkdir(&card.image, "/books").unwrap();

    let sources = vec![lib.clone(), root.join(".top"), root.join("top-link.bin")];
    let plan = sd::plan_upload(&card.image, "/books", &sources).unwrap();
    assert_eq!(plan.files, 4);
    assert_eq!(plan.bytes, 100_000 + 5 + 70_000 + 29);
    assert!(plan.conflicts.is_empty());
    let done = sd::upload(&card.image, "/books", &sources).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("book.epub")]).unwrap();
    let after_first = card.list("/").free;
    assert!(after_first <= blank_free - 300_000);

    let host2 = card.host("v2");
    write(&host2.join("BOOK.epub"), &data(2, 1_000));
    let plan = sd::plan_upload(&card.image, "/", &[host2.join("BOOK.epub")]).unwrap();
    assert_eq!(plan.conflicts, ["/BOOK.epub"]);
    sd::upload(&card.image, "/", &[host2.join("BOOK.epub")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("note.txt")]).unwrap();

    // A file onto a folder, and a folder onto a file (case-insensitive).
    let clash = card.host("clash");
    write(&clash.join("BOOKS"), b"file");
    write(&clash.join("Note.TXT/inner.txt"), b"in a folder");
    let error = sd::plan_upload(&card.image, "/", &[clash.join("BOOKS")]).unwrap_err();
    assert_eq!(error.code, "EISDIR");
    assert!(!error.changed);
    assert_eq!(code(sd::upload(&card.image, "/", &[clash.join("Note.TXT")])), "ENOTDIR");
    assert_eq!(code(sd::upload(&card.image, "/", &[clash.join("BOOKS")])), "EISDIR");

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
    let error = sd::plan_upload(&card.image, "/", &[big.clone()]).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(error.message.contains("Not enough space"), "{}", error.message);
    let error = sd::upload(&card.image, "/", &[big]).unwrap_err();
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
    sd::upload(&card.image, "/", &[host.join("f.txt")]).unwrap();
    assert_eq!(code(sd::list(&card.image, "/nope")), "ENOENT");
    assert_eq!(code(sd::list(&card.image, "/f.txt")), "ENOTDIR");
    assert_eq!(code(sd::list(&card.image, "relative")), "EINVAL");
    assert_eq!(code(sd::list(&card.image, "/bad:name")), "EINVAL");
    assert_eq!(code(sd::upload(&card.image, "/nope", &[host.join("f.txt")])), "ENOENT");
    assert_eq!(code(sd::upload(&card.image, "/", &[host.join("absent.txt")])), "ENOENT");
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
    sd::upload(&card.image, "/", &[host.join("lib"), host.join("b.txt")]).unwrap();

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
    sd::upload(&card.image, "/", &[tree, host.join("single.txt")]).unwrap();
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
    sd::upload(&card.image, "/", &sources).unwrap();
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

#[test]
fn invalid_host_names_are_rejected() {
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("what?.txt"), b"x");
    write(&host.join("folder/ok.txt"), b"x");
    write(&host.join("folder/trailing."), b"x");
    assert_eq!(code(sd::plan_upload(&card.image, "/", &[host.join("what?.txt")])), "EINVAL");
    assert_eq!(code(sd::upload(&card.image, "/", &[host.join("folder")])), "EINVAL");
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
    sd::upload(&card.image, "/", &[host.join("a.bin")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
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
        sd::upload(&card.image, "/", &[host.join("dir"), host.join("b.txt")]).unwrap();
        assert_eq!(card.names("/"), ["dir", "b.txt"]);
        assert_eq!(card.read("/dir/a.bin"), data(5, 40_000));
        assert!(card.list("/").free < blank.free);
        sd::remove(&card.image, "/dir").unwrap();
        sd::remove(&card.image, "/b.txt").unwrap();
        assert_eq!(card.list("/").free, blank.free, "{name}");
        // fat.js reads FAT32 only, so there is no parity check here.
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
    sd::upload(&card.image, "/", &[root.join(decomposed)]).unwrap();
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
    sd::upload(&image, "/", &[host.clone()]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("probe.bin")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
    let free = card.list("/").free;
    write(&host.join("b.txt"), &data(2, free as usize));
    sd::upload(&card.image, "/", &[host.join("b.txt")]).unwrap();
    assert_eq!(card.list("/").free, 0);
    let before = fs::read(&card.image).unwrap();

    // Growing a.txt first needs space that only shrinking b.txt frees.
    let second = card.host("second");
    let grown = data(3, 3 * cluster as usize);
    write(&second.join("a.txt"), &grown);
    write(&second.join("b.txt"), b"");
    let sources = [second.join("a.txt"), second.join("b.txt")];
    let plan = sd::plan_upload(&card.image, "/", &sources).unwrap();
    assert_eq!(plan.conflicts, ["/a.txt", "/b.txt"]);
    let error = sd::upload(&card.image, "/", &sources).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(!error.changed);
    identical(&card, &before);
    assert_eq!(card.read("/a.txt"), data(1, 400));

    // In the other order it fits.
    sd::upload(&card.image, "/", &[second.join("b.txt"), second.join("a.txt")]).unwrap();
    assert_eq!(card.read("/a.txt"), grown);
    assert_eq!(card.read("/b.txt"), b"");
}

#[cfg(unix)]
#[test]
fn host_read_error_rolls_back_the_upload() {
    use std::os::unix::fs::PermissionsExt;
    let card = Card::new();
    let host = card.host("files");
    write(&host.join("precious.txt"), b"original");
    sd::upload(&card.image, "/", &[host.join("precious.txt")]).unwrap();
    let before = fs::read(&card.image).unwrap();
    let next = card.host("next");
    write(&next.join("large.bin"), &data(4, 3 << 20));
    write(&next.join("precious.txt"), b"replacement");
    fs::set_permissions(next.join("precious.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(next.join("precious.txt")).is_ok() {
        eprintln!("running with permission overrides: skipping");
        return;
    }
    let error = sd::upload(&card.image, "/", &[next.join("large.bin"), next.join("precious.txt")]).unwrap_err();
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
    sd::upload(&card.image, "/", &sources).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
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
        sd::upload(&card.image, "/missing", &[host.join("a.txt")]).unwrap_err(),
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
    sd::upload(&card.image, "/", &[host.join("dir")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.bin"), host.join("b.bin")]).unwrap();
    let good = fs::read(&card.image).unwrap();
    let raw = Raw::of(&good);
    let (_, a) = raw.record(&good, raw.root, b"A       BIN");
    let (_, b) = raw.record(&good, raw.root, b"B       BIN");
    let a_chain = raw.chain(&good, a);
    let b_chain = raw.chain(&good, b);
    let free_cluster = (2..).find(|&c| raw.get(&good, c) == 0).unwrap();

    let cases: Vec<(&str, Box<dyn Fn(&mut Vec<u8>)>)> = vec![
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
    sd::upload(&card.image, "/", &[host.join("tree")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("x1.txt"), host.join("x2.txt")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("a.txt")]).unwrap();
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
    sd::upload(&card.image, "/", &[host.join("big.bin")]).unwrap();
    assert!(card.read("/big.bin") == bytes);
    parity(&card);
}
