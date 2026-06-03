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

## Contributing

To contribute to uutils, please see [CONTRIBUTING](CONTRIBUTING.md).

## License

uutils is licensed under the MIT License - see the `LICENSE` file for details

GNU Grep is licensed under the GPL 3.0 or later.
