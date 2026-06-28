// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.
// spell-checker:ignore uumain seedable

#![no_main]
use libfuzzer_sys::fuzz_target;
use uu_grep::uumain;

use rand::prelude::IndexedRandom;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::ffi::OsString;

use uufuzz::{CommandResult, compare_result, generate_and_run_uumain, run_gnu_cmd};

static CMD_PATH: &str = "grep";

/// Derive a 32-byte RNG seed from the libFuzzer input so that every run is a
/// pure function of `data`. This is what makes crash artifacts reproducible:
/// the same bytes always generate the same pattern/args/input.
fn seed_from_data(data: &[u8]) -> StdRng {
    let mut seed = [0u8; 32];
    for (i, b) in data.iter().enumerate() {
        seed[i % 32] ^= b;
    }
    StdRng::from_seed(seed)
}

/// Random string mixing valid UTF-8 (incl. multi-byte) and the occasional
/// invalid byte. Driven by the caller's seeded RNG so output is deterministic.
fn gen_random_string(rng: &mut StdRng, max_length: usize) -> String {
    let valid_utf8: Vec<char> =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789🔩🪛🪓⚙️🔗🧰"
            .chars()
            .collect();
    let invalid_utf8 = [0xC3u8, 0x28];
    let mut result = String::new();

    for _ in 0..rng.random_range(0..=max_length) {
        if rng.random_bool(0.9) {
            result.push(*valid_utf8.choose(rng).unwrap());
        } else if let Some(c) = char::from_u32(*invalid_utf8.choose(rng).unwrap() as u32) {
            result.push(c);
        }
    }

    result
}

/// Generate a (mostly) meaningful set of grep flags, occasionally throwing in
/// garbage to exercise error handling.
fn generate_grep_args(rng: &mut StdRng) -> Vec<OsString> {
    let arg_count = rng.random_range(0..=5);
    let mut args = Vec::new();

    for _ in 0..arg_count {
        // Small chance of an invalid argument.
        if rng.random_bool(0.1) {
            let len = rng.random_range(1..=10);
            args.push(OsString::from(gen_random_string(rng, len)));
            continue;
        }

        match rng.random_range(0..=15) {
            0 => args.push(OsString::from("-i")), // ignore case
            1 => args.push(OsString::from("-v")), // invert match
            2 => args.push(OsString::from("-c")), // count
            3 => args.push(OsString::from("-n")), // line number
            4 => args.push(OsString::from("-o")), // only matching
            5 => args.push(OsString::from("-w")), // word boundaries
            6 => args.push(OsString::from("-x")), // whole line match
            7 => args.push(OsString::from("-F")), // fixed strings
            8 => args.push(OsString::from("-E")), // extended regexp
            9 => args.push(OsString::from("-G")), // basic regexp
            10 => args.push(OsString::from("--null-data")),
            11 => args.push(OsString::from("--byte-offset")),
            12 => {
                // max-count
                args.push(OsString::from("-m"));
                args.push(OsString::from(rng.random_range(0..=5).to_string()));
            }
            13 => {
                // after-context
                args.push(OsString::from("-A"));
                args.push(OsString::from(rng.random_range(0..=3).to_string()));
            }
            14 => {
                // before-context
                args.push(OsString::from("-B"));
                args.push(OsString::from(rng.random_range(0..=3).to_string()));
            }
            15 => args.push(OsString::from("-s")), // suppress error messages
            _ => (),
        }
    }

    args
}

/// Build a pattern. Sometimes a literal token, sometimes a small regex made of
/// random characters and metacharacters.
fn generate_pattern(rng: &mut StdRng) -> String {
    match rng.random_range(0..=3) {
        0 => {
            let len = rng.random_range(1..=5);
            gen_random_string(rng, len)
        }
        1 => {
            // A small alternation / anchored regex.
            let la = rng.random_range(1..=3);
            let a = gen_random_string(rng, la);
            let lb = rng.random_range(1..=3);
            let b = gen_random_string(rng, lb);
            format!("{a}|{b}")
        }
        2 => {
            let lb = rng.random_range(1..=3);
            let base = gen_random_string(rng, lb);
            let meta = ["*", "+", "?", ".", "^", "$", ".*", "[a-z]", "\\w"];
            let m = meta[rng.random_range(0..meta.len())];
            format!("{base}{m}")
        }
        _ => {
            // Pick one of a few hand-written patterns that exercise common paths.
            let canned = ["a", "^", "$", ".", ".*", "[0-9]+", "\\b", "()"];
            canned[rng.random_range(0..canned.len())].to_string()
        }
    }
}

/// Generate input text with a mix of short and long lines.
fn generate_input(rng: &mut StdRng, count: usize) -> String {
    let mut lines = Vec::new();

    for _ in 0..count {
        if rng.random_bool(0.1) {
            let len = rng.random_range(200..=500);
            lines.push(gen_random_string(rng, len));
        } else {
            let len = rng.random_range(0..=20);
            lines.push(gen_random_string(rng, len));
        }
    }

    lines.join("\n")
}

fuzz_target!(|data: &[u8]| {
    let mut rng = seed_from_data(data);

    let pattern = generate_pattern(&mut rng);

    // Pass the pattern through `-e` so it is never mistaken for a flag, then
    // append the (possibly invalid) extra arguments.
    let mut args = vec![
        OsString::from("grep"),
        OsString::from("-e"),
        OsString::from(&pattern),
    ];
    args.extend(generate_grep_args(&mut rng));

    let input = generate_input(&mut rng, 10);

    let rust_result = generate_and_run_uumain(&args, uumain, Some(&input));
    let gnu_result = match run_gnu_cmd(CMD_PATH, &args[1..], false, Some(&input)) {
        Ok(result) => result,
        Err(error_result) => {
            eprintln!("Failed to run GNU command:");
            eprintln!("Stderr: {}", error_result.stderr);
            eprintln!("Exit Code: {}", error_result.exit_code);
            CommandResult {
                stdout: String::new(),
                stderr: error_result.stderr,
                exit_code: error_result.exit_code,
            }
        }
    };

    compare_result(
        "grep",
        &format!("{:?}", &args[1..]),
        Some(&input),
        &rust_result,
        &gnu_result,
        false, // Set to true if you want to fail on stderr diff
    );
});
