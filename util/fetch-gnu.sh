#!/bin/bash -e
# This file is part of the uutils grep package.
#
# For the full copyright and license information, please view the LICENSE
# file that was distributed with this source code.
#
# Download and extract the upstream GNU grep release tarball into the current
# directory. Run it from an (empty) directory that will hold the GNU grep tree,
# e.g.:
#
#   mkdir -p ../gnu.grep && (cd ../gnu.grep && bash ../grep/util/fetch-gnu.sh)
#
# The extracted tree ships a ready-to-use gnulib test framework under tests/
# (init.sh + init.cfg + the extensionless test scripts), which
# util/run-gnu-testsuite.sh drives against the Rust grep binary.
ver="3.12"
sha256="2649b27c0e90e632eadcd757be06c6e9a4f48d941de51e7c0f83ff76408a07b9"
curl -fsSL "https://github.com/uutils/thirdparty-artifacts/releases/download/grep-${ver}/grep-${ver}.tar.xz" -o grep.tar.xz
echo $sha256 grep.tar.xz | sha256sum -c
tar --strip-components=1 -xJf grep.tar.xz
