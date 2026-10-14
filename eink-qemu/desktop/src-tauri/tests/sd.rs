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
    assert!(error.precheck());
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
fn not_enough_space_is_a_precheck() {
    let card = Card::new();
    let before = card.list("/").free;
    let host = card.host("big");
    let big = host.join("big.bin");
    let file = fs::File::create(&big).unwrap();
    file.set_len(100 * 1024 * 1024).unwrap();
    let error = sd::plan_upload(&card.image, "/", &[big.clone()]).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
    assert!(error.message.contains("Not enough space"), "{}", error.message);
    let error = sd::upload(&card.image, "/", &[big]).unwrap_err();
    assert_eq!(error.code, "ENOSPC");
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
    for error in [SdError::new("EEXIST", ""), SdError::new("ENOENT", ""), SdError::new("ENOTEMPTY", "")] {
        assert!(error.precheck());
    }
    for error in [SdError::new("ENOSPC", ""), SdError::new("EIO", "")] {
        assert!(!error.precheck());
    }
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
