//! Synthetic tree for the hidden `--demo` flag.
//!
//! The scanner streams these entries into the tree with a short pause between
//! directories, so the UI looks like it is walking a real disk without a
//! single file being read. Sizes are just numbers and the "files" do not
//! exist; this exists so screenshots and GIFs need no prepared tree.

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;

/// Extra shard directories that make the simulated scan take a few seconds.
const SHARDS: usize = 420;

/// `(path relative to the scan root, size)` entries the demo walk visits.
pub fn atlas() -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = BASE
        .iter()
        .map(|(path, size)| ((*path).to_string(), *size))
        .collect();
    for i in 0..SHARDS {
        let dir = format!("datasets/shards/part_{i:04}");
        for f in 0..4 {
            // Unique sizes keep the fabricated duplicate groups meaningful.
            let size = 1_200 + (i * 4 + f) as u64 * 3;
            out.push((format!("{dir}/chunk_{f}.bin"), size));
        }
    }
    out
}

const BASE: &[(&str, u64)] = &[
    (
        "video/documentaries/deep-ocean-trenches-2160p.mkv",
        4_800 * MB,
    ),
    (
        "video/documentaries/the-cartographers-1080p.mkv",
        2_900 * MB,
    ),
    ("video/documentaries/salt-flats-uncut.mkv", 2_100 * MB),
    ("video/raw-footage/clip.001.mp4", 868 * MB),
    ("video/raw-footage/clip.002.mp4", 514 * MB),
    ("video/raw-footage/clip.003.mp4", 312 * MB),
    ("video/raw-footage/clip.004.mp4", 417 * MB),
    ("video/raw-footage/clip.006.mp4", 642 * MB),
    ("video/raw-footage/clip.007.mp4", 515 * MB),
    ("video/raw-footage/clip.008.mp4", 881 * MB),
    ("video/raw-footage/clip.009.m4", 670 * MB),
    ("video/raw-footage/clip.010.mp4", 449 * MB),
    ("video/raw-footage/clip.011.m4", 722 * MB),
    ("video/raw-footage/clip.014.m4", 639 * MB),
    ("video/exports/cut_v2.mp4", 765 * MB),
    ("video/exports/cut_v3.mp4", 647 * MB),
    ("video/exports/cut_v4.m4", 656 * MB),
    ("video/exports/cut_v5.mp4", 627 * MB),
    ("video/exports/cut_v6.mp4", 1_030 * MB),
    ("vm-images/debian-13-build.qcow2", 6_200 * MB),
    ("vm-images/windows-lab.vhdx", 5_400 * MB),
    ("vm-images/ubuntu-builder.qcow2", 2_800 * MB),
    ("backups/weekly/snapshot-2026-w30.tar.zst", 982 * MB),
    ("backups/weekly/snapshot-2026-w31.tar.zst", 1_030 * MB),
    ("backups/weekly/snapshot-2026-w32.tar.zst", 1_120 * MB),
    ("backups/weekly/snapshot-2026-w33.tar.zst", 1_270 * MB),
    ("backups/weekly/snapshot-2026-w34.tar.zst", 1_430 * MB),
    ("backups/weekly/snapshot-2026-w35.tar.zst", 784 * MB),
    ("backups/weekly/snapshot-2026-w36.tar.zst", 902 * MB),
    ("backups/weekly/snapshot-2026-w37.tar.zst", 1_130 * MB),
    ("backups/weekly/snapshot-2026-w38.tar.zst", 1_280 * MB),
    ("backups/database/pg_dump_prod_2026-09-06.sql.gz", 703 * MB),
    ("backups/database/pg_dump_prod_2026-09-01.sql.gz", 792 * MB),
    ("backups/database/pg_dump_prod_2026-08-25.sql.gz", 721 * MB),
    (
        "backups/database/pg_dump_analytics_2026-08-25.sql.gz",
        623 * MB,
    ),
    (
        "backups/database/pg_dump_analytics_2026-08-18.sql.gz",
        627 * MB,
    ),
    ("datasets/elevation/tile_n02_e33.tif", 454 * MB),
    ("datasets/elevation/tile_n02_e12.tif", 242 * MB),
    ("datasets/elevation/tile_n03_e41.tif", 899 * MB),
    ("datasets/elevation/tile_n03_e42.tif", 487 * MB),
    ("datasets/elevation/tile_n04_w12.tif", 471 * MB),
    ("datasets/elevation/tile_n08_e41.tif", 436 * MB),
    (
        "datasets/audio-projects/field-recordings/rec_001_dawn.wav",
        396 * MB,
    ),
    (
        "datasets/audio-projects/field-recordings/rec_005_night.wav",
        289 * MB,
    ),
    (
        "datasets/audio-projects/field-recordings/rec_007_storm.wav",
        310 * MB,
    ),
    (
        "datasets/audio-projects/field-recordings/rec_011_wind.wav",
        291 * MB,
    ),
    (
        "datasets/audio-projects/field-recordings/rec_012_harbor.wav",
        344 * MB,
    ),
    (
        "datasets/audio-projects/field-recordings/rec_014_hebron.wav",
        374 * MB,
    ),
    ("containers/layers/layer_3f9a1c.bin", 336 * MB),
    ("containers/layers/layer_5b21d0.bin", 294 * MB),
    ("containers/layers/layer_77c0aa.bin", 402 * MB),
    ("containers/layers/layer_9d13fe.bin", 287 * MB),
    ("containers/layers/layer_b4e882.bin", 311 * MB),
    ("downloads/debian-13.2.0-amd64-netinst.iso", 1_100 * MB),
    ("downloads/ubuntu-24.04.3-live-server.iso", 895 * MB),
    ("photos/raw/dsc_0841.arw", 336 * MB),
    ("photos/raw/dsc_0855.arw", 351 * MB),
    ("photos/raw/dsc_0862.arw", 342 * MB),
    ("photos/mockups/atlas-legends.png", 713 * MB),
    ("photos/mockups/atlas-map.png", 452 * MB),
    ("photos/mockups/atlas-splash.png", 1_100 * MB),
    ("photos/mockups/atlas-wireframe.fig", 337 * MB),
    ("photos/mockups/atlas-nav.png", 1_400 * MB),
    ("mail-archive/2024/inbox-2024-11.mbox", 716 * MB),
    ("mail-archive/2024/inbox-2024-09.mbox", 512 * MB),
    ("mail-archive/2025/inbox-2025-02.mbox", 689 * MB),
    ("music/2022/north-line.flac", 336 * MB),
    ("music/2022/bell-tower.flac", 367 * MB),
    ("music/2023/atlas-theme.flac", 1_200 * MB),
    ("design/assets/logo.png", 2_400 * KB),
    ("design/exports/logo.png", 2_400 * KB),
    ("design/assets/hero.jpg", 8_200 * KB),
    ("downloads/hero.jpg", 8_200 * KB),
    ("design/assets/atlas-en.woff2", 1_200 * KB),
    ("design/assets/icons.svg", 480 * KB),
    ("src/target/debug/duw.pdb", 480 * MB),
];
