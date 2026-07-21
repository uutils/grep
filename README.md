[![Crates.io](https://img.shields.io/crates/v/uu_grep.svg)](https://crates.io/crates/uu_grep)
[![Discord](https://img.shields.io/badge/discord-join-7289DA.svg?logo=discord&longCache=true&style=flat)](https://discord.gg/wQVJbvJ)
[![License](http://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/uutils/grep/blob/main/LICENSE)
[![dependency status](https://deps.rs/repo/github/uutils/grep/status.svg)](https://deps.rs/repo/github/uutils/grep)

[![CodeCov](https://codecov.io/gh/uutils/grep/branch/main/graph/badge.svg)](https://codecov.io/gh/uutils/grep)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://codspeed.io/uutils/grep?utm_source=badge)

# Grep, now in Rust

A Rust implementation of [GNU Grep](https://www.gnu.org/software/grep/).
This project is an initial release and may contain bugs.

## Install

```shell
cargo install uu_grep
```

## 🚀 Try it online

You can try `grep` directly in your browser on the [uutils playground](https://uutils.github.io/playground/).
Arguments (and a full command) can be passed through the URL via the `cmd` query parameter, for example:

```shell
printf '🚀 rocket\n🛰️ satellite\n🌙 moon\n⭐ star\n' | grep 🌙
```

[Run it in the playground](https://uutils.github.io/playground/?cmd=printf%20%27%F0%9F%9A%80%20rocket%5Cn%F0%9F%9B%B0%EF%B8%8F%20satellite%5Cn%F0%9F%8C%99%20moon%5Cn%E2%AD%90%20star%5Cn%27%20%7C%20grep%20%F0%9F%8C%99)

## Building

Download Rust at: https://rustup.rs/

```shell
# Check out this repository
git clone https://github.com/uutils/grep
cd grep

# Build a release version
cargo build --release

# Run!
./target/release/grep --help

# Run tests (if needed; after making changes)
cargo test
```

## Pre-commit hooks

This project uses [pre-commit](https://pre-commit.com); run `pre-commit install` to enable the git hooks.

## Known Issues

* Does not take `LANG`, etc., into account for handling file encodings (non-UTF8 matches are treated as binary)
* No localization support yet
* Performances need to be improved

## GNU test suite compatibility

Below is the evolution of how many GNU tests uutils passes.

![Evolution over time](https://github.com/uutils/grep-tracking/blob/main/gnu-results.svg?raw=true)

## Contributing

To contribute to uutils, please see [CONTRIBUTING](CONTRIBUTING.md).

## License

uutils is licensed under the MIT License - see the `LICENSE` file for details

GNU Grep is licensed under the GPL 3.0 or later.
