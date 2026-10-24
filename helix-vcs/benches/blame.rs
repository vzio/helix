//! Benchmarks for computing the blame of a file. This is run on a background thread by the
//! editor, but it dominates the time until the first blame shows up.
//!
//! cargo bench -p helix-vcs --features git --bench blame

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{criterion_group, criterion_main, Criterion};
use helix_vcs::{head_commit_id, FileBlame};

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
        .canonicalize()
        .unwrap()
}

fn bench_blame(c: &mut Criterion) {
    let mut group = c.benchmark_group("blame");
    // a blame of a big file takes a good part of a second
    group.sample_size(10);

    for file in [
        "helix-term/src/commands.rs",
        "helix-vcs/src/lib.rs",
        "helix-vcs/src/commit.rs",
    ] {
        let path = repo_file(file);
        // nothing to measure in a source tarball
        if FileBlame::try_new(&path).is_err() {
            eprintln!("skipping {file}: no git history");
            continue;
        }
        group.bench_function(format!("FileBlame::try_new {file}"), |b| {
            b.iter(|| black_box(FileBlame::try_new(black_box(&path)).unwrap()))
        });
    }
    group.finish();

    let path = repo_file("helix-vcs/src/lib.rs");
    if let Ok(blame) = FileBlame::try_new(&path) {
        c.bench_function("head_commit_id", |b| {
            b.iter(|| black_box(head_commit_id(black_box(&path)).unwrap()))
        });
        c.bench_function("FileBlame::blame_for_line + parse_format", |b| {
            b.iter(|| {
                let line = black_box(10);
                black_box(
                    blame.blame_for_line(line, 0, 0).map(|blame| {
                        blame.parse_format("{author}, {time-ago} • {title} • {commit}")
                    }),
                )
            })
        });
    }
}

criterion_group!(benches, bench_blame);
criterion_main!(benches);
