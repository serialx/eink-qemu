# fatfs 0.3.6, patched for eink-qemu

Upstream: https://crates.io/crates/fatfs/0.3.6 (MIT, LICENSE.txt). Only
`src/dir.rs`, `src/file.rs`, `src/fs.rs`, `src/boot_sector.rs` and `src/lib.rs` (lints) differ, each change marked `eink-qemu:`.

- `Dir::create_dir` writes `.` and `..` as short entries only, in the new
  folder's first two slots, and `..` of a folder in the root is cluster 0 on
  FAT32 too. Upstream wrote long-name records before both and gave `..` the
  root cluster (2), which `fsck_msdos` and `fsck.fat` reject; repairing such
  a folder discards its contents.
- A cluster added to the FAT32 root folder is zeroed like any folder's.
  Upstream left the old bytes, so stale entries could appear in the root.
- Short names drop leading dots, as Windows does (`.top` becomes `TOP~1`),
  and an empty stem becomes `_`. Upstream produced a short name starting
  with a space, which `fsck.fat` renames. `src/sd.rs` simulates this
  generator for upload plans, so the two must change together.
- A FAT32 volume whose FSInfo sector is 0 or 0xFFFF has none, and a backup
  boot sector of 0xFFFF means none, as fat.js reads them. Upstream refused
  such volumes.
