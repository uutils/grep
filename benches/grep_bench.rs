// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::ffi::OsString;
use std::path::Path;

/// Run grep end-to-end through the real `uumain` entry point. `args` are the
/// arguments after the program name (flags, pattern, paths). The exit status is
/// ignored — we only care about the work performed.
fn run(args: &[&str]) {
    let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 1);
    argv.push(OsString::from("grep"));
    argv.extend(args.iter().map(OsString::from));
    let _ = uu_grep::uumain(argv.into_iter());
}

/// Build a multi-megabyte log-like corpus plus a directory holding it alongside
/// a binary file. Every line contains `worker-<n>` and a `2024-…` timestamp; a
/// rare `RAREHIT` marker appears on a handful of lines (≈ every 10000th).
/// Returns `(dir, log_file)`.
fn build_corpus() -> (std::path::PathBuf, std::path::PathBuf) {
    let mut content = String::new();
    for i in 0..80_000u32 {
        if i % 10_000 == 0 {
            content.push_str(&format!(
                "2024-01-15 10:30:{:02} RAREHIT worker-{i} special marker seen\n",
                i % 60
            ));
        } else if i % 100 == 0 {
            content.push_str(&format!(
                "2024-01-15 10:30:{:02} ERROR worker-{i} connection reset\n",
                i % 60
            ));
        } else {
            content.push_str(&format!(
                "2024-01-15 10:30:{:02} INFO  worker-{i} request handled in {}ms\n",
                i % 60,
                i % 1000
            ));
        }
    }
    assert!(content.len() > 4 * 1024 * 1024);

    let dir = std::env::temp_dir().join(format!("uu_grep_bench_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("app.log");
    std::fs::write(&log, &content).unwrap();

    // A binary file (contains NUL) that also holds the marker, so `-I` has
    // something to skip while recursing.
    let mut binary = vec![0u8, 1, 2, 3];
    binary.extend_from_slice(b"RAREHIT in binary blob");
    binary.extend(std::iter::repeat_n(0u8, 4096));
    std::fs::write(dir.join("data.bin"), &binary).unwrap();

    (dir, log)
}

fn bench_e2e(c: &mut Criterion) {
    let (dir, log) = build_corpus();
    let file = log.to_str().unwrap();
    let dir_str = dir.to_str().unwrap();

    // Pure scanning throughput: `-q` with a pattern that never matches forces a
    // full scan and produces no output. A literal (which a buffer-at-a-time
    // searcher can accelerate) versus an extended-regex control (which cannot).
    {
        let mut group = c.benchmark_group("scan");
        group.bench_function("literal_no_match", |b| {
            b.iter(|| run(black_box(&["-q", "NONEXISTENT_TOKEN_XYZ", file])))
        });
        group.bench_function("regex_no_match", |b| {
            b.iter(|| run(black_box(&["-q", "-E", "NON[0-9]EXISTENT_TOKEN", file])))
        });
        group.finish();
    }

    // Real invocation shapes from the `grep` tldr page, each scanning the whole
    // corpus. The `RAREHIT` marker matches only a handful of lines, so output
    // stays small while the full-file scan dominates.
    {
        let mut group = c.benchmark_group("usage");

        // Search for a pattern within a file.
        group.bench_function("search_pattern", |b| {
            b.iter(|| run(black_box(&["RAREHIT", file])))
        });
        // Search for an exact string (-F).
        group.bench_function("fixed_string", |b| {
            b.iter(|| run(black_box(&["-F", "RAREHIT", file])))
        });
        // Recursive search ignoring binary files (-rI).
        group.bench_function("recursive_no_binary", |b| {
            b.iter(|| run(black_box(&["-rI", "RAREHIT", dir_str])))
        });
        // Print 3 lines of context (-C 3).
        group.bench_function("context", |b| {
            b.iter(|| run(black_box(&["-C", "3", "RAREHIT", file])))
        });
        // Filename + line number with forced color (-Hn --color=always).
        group.bench_function("filename_lineno_color", |b| {
            b.iter(|| run(black_box(&["-Hn", "--color=always", "RAREHIT", file])))
        });
        // Print only the matched text (-o).
        group.bench_function("only_matching", |b| {
            b.iter(|| run(black_box(&["-o", "RAREHIT", file])))
        });
        // Invert match (-v); `worker-` is on every line, so nothing is printed
        // and this measures the full inverted scan.
        group.bench_function("invert_match", |b| {
            b.iter(|| run(black_box(&["-v", "worker-", file])))
        });
        // Extended regex, case-insensitive (-Ei).
        group.bench_function("extended_icase", |b| {
            b.iter(|| run(black_box(&["-Ei", "rarehit", file])))
        });

        group.finish();
    }

    // Invalid UTF-8 is matched run by run. A line with many invalid bytes must
    // stay linear in its length.
    {
        let invalid = dir.join("invalid.bin");
        let mut data = b"a\xff".repeat(50_000);
        data.push(b'\n');
        std::fs::write(&invalid, data).unwrap();
        let invalid = invalid.to_str().unwrap();
        let mut group = c.benchmark_group("invalid_utf8");
        group.bench_function("only_matching_many_runs", |b| {
            b.iter(|| run(black_box(&["-a", "-o", "a", invalid])))
        });
        group.bench_function("dot_many_runs", |b| {
            b.iter(|| run(black_box(&["-a", "-c", "-E", "a.", invalid])))
        });
        group.finish();
    }

    let _ = std::fs::remove_dir_all(Path::new(dir_str));
}

criterion_group!(benches, bench_e2e);
criterion_main!(benches);
