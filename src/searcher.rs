// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::context_buffer::{ContextBuffer, LineView};
use crate::line_buffer::LineBuffer;
use crate::matcher::Matcher;
use crate::output::OutputWriter;
use crate::{BinaryMode, Config, DeviceMode, DirectoryMode};
use memchr::memchr;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::ManuallyDrop;
use std::ops::ControlFlow;
use std::path::Path;
use uucore::error::{ExitCode, FromIo, UResult};
use walkdir::WalkDir;

pub struct Searcher<'a> {
    config: &'a Config<'a>,
    writer: OutputWriter<'a>,
    matcher: Matcher<'a>,
    any_match: bool,
    had_error: bool,
    binary_notice_enabled: bool,

    // Per-session state
    session_context_buf: ContextBuffer,
    session_match_count: u64,
    session_after_remaining: usize,
    session_last_printed_line: u64, // 0 = nothing yet
    session_binary_detected: bool,
}

impl<'a> Searcher<'a> {
    pub fn new(config: &'a Config<'a>, matcher: Matcher<'a>, writer: OutputWriter<'a>) -> Self {
        Self {
            config,
            writer,
            matcher,
            any_match: false,
            had_error: false,
            binary_notice_enabled: config.binary_mode == BinaryMode::Binary
                && !config.quiet
                && !config.count
                && !config.files_with_matches
                && !config.files_without_match,

            session_context_buf: ContextBuffer::new(config.before_context),
            session_match_count: 0,
            session_after_remaining: 0,
            session_last_printed_line: 0,
            session_binary_detected: false,
        }
    }

    /// Search the given path.
    pub fn process_path(&mut self, lb: &mut LineBuffer, path: &Path) -> ControlFlow<()> {
        if path.is_dir() {
            match self.config.directory_mode {
                DirectoryMode::Skip => ControlFlow::Continue(()),
                // Yield the directory path itself; `process_file` will
                // report the OS error from trying to open it as a file.
                DirectoryMode::Read => self.process_file(lb, path),
                DirectoryMode::Recurse => self.process_dir(lb, path, false),
            }
        } else {
            // Top-level FIFOs/sockets/devices: read by default, drop only on `-D skip`.
            if self.config.device_mode == DeviceMode::Skip && Self::is_special_file(path) {
                return ControlFlow::Continue(());
            }
            self.process_file(lb, path)
        }
    }

    /// Recursive search of the current directory. This is used when `-r`
    /// is used without a path. In that case, GNU grep emits bare paths
    /// for which we need special handling.
    pub fn process_implicit_cwd(&mut self, lb: &mut LineBuffer) -> ControlFlow<()> {
        self.process_dir(lb, Path::new("."), true)
    }

    /// Search on standard input.
    pub fn process_stdin(&mut self, lb: &mut LineBuffer) -> ControlFlow<()> {
        // Turn the Stdin struct into a File to avoid monomorphization just for files VS stdin.
        // The cast is "unsafe" because we package a borrowed handle into an owned File.
        // SAFETY: To make that safe, we simply use ManuallyDrop.
        #[cfg(windows)]
        let mut stdin = {
            use std::os::windows::io::{AsRawHandle, FromRawHandle};
            let handle = io::stdin().as_raw_handle();
            ManuallyDrop::new(unsafe { File::from_raw_handle(handle) })
        };
        #[cfg(not(windows))]
        let mut stdin = {
            use std::os::fd::FromRawFd;
            ManuallyDrop::new(unsafe { File::from_raw_fd(0) })
        };

        let path = Path::new(&self.config.label);

        // Stdin is not seekable; use fixed width.
        if self.writer.wants_padded_line_numbers() {
            self.writer.set_line_number_width(19);
        }

        let result = self.session_run(lb, path, &mut stdin);
        self.record_result(OsStr::new(&self.config.label), result)
    }

    /// Flush output and produce the overall result.
    /// Returns true if any input matched.
    pub fn finish(mut self) -> UResult<()> {
        self.writer
            .flush()
            .map_err_context(|| "(standard output)".to_string())?;

        if self.had_error {
            Err(ExitCode::new(2))
        } else if self.any_match {
            Ok(())
        } else {
            Err(ExitCode::new(1)) // aka: no match
        }
    }

    fn process_dir(
        &mut self,
        lb: &mut LineBuffer,
        start: &Path,
        strip_root: bool,
    ) -> ControlFlow<()> {
        let mut walker = WalkDir::new(start)
            .follow_links(self.config.follow_symlinks)
            .into_iter();

        while let Some(entry) = walker.next() {
            let Ok(entry) = entry else {
                continue;
            };

            let file_type = entry.file_type();

            // We're only interested in files, so skip dirs.
            // If we have a --exclude-dir pattern, skip matching directories entirely.
            if file_type.is_dir() {
                if self.config.exclude_dir_globs.matches(entry.file_name()) {
                    walker.skip_current_dir();
                }
                continue;
            }

            // GNU `-r` doesn't follow symlinks.
            // With `-R` we already had walkdir resolve them.
            if file_type.is_symlink() {
                continue;
            }

            // Skip non-regular files unless `-D read` was given explicitly.
            if !file_type.is_file() && self.config.device_mode != DeviceMode::Read {
                continue;
            }

            // Handle include/exclude globs.
            let name = entry.file_name();
            if (!self.config.include_globs.is_empty() && !self.config.include_globs.matches(name))
                || self.config.exclude_globs.matches(name)
            {
                continue;
            }

            let mut path = entry.path();
            if strip_root {
                path = Self::strip_dot_prefix(path);
            }

            self.process_file(lb, path)?;
        }

        ControlFlow::Continue(())
    }

    fn process_file(&mut self, lb: &mut LineBuffer, path: &Path) -> ControlFlow<()> {
        let result = File::open(path).and_then(|mut file| {
            if self.writer.wants_padded_line_numbers() {
                let file_size = file.metadata().map_or(0, |m| m.len());
                self.writer
                    .set_line_number_width(file_size.max(1).ilog10() as usize + 1);
            }
            self.session_run(lb, path, &mut file)
        });
        self.record_result(path.as_os_str(), result)
    }

    fn record_result(&mut self, label: &OsStr, result: io::Result<bool>) -> ControlFlow<()> {
        match result {
            Ok(true) => {
                self.any_match = true;

                // In quiet mode, all we want is the exit code, which means
                // we can stop searching as soon as we see the first match.
                if self.config.quiet {
                    return ControlFlow::Break(());
                }
            }
            Ok(false) => {}
            Err(err) => {
                self.had_error = true;
                self.writer.report_io_error(label, &err);
            }
        }

        ControlFlow::Continue(())
    }

    fn session_any_match(&self) -> bool {
        self.session_match_count > 0
    }

    fn session_can_match(&self) -> bool {
        self.config
            .max_count
            .is_none_or(|max| self.session_match_count < max)
    }

    fn session_should_continue(&self) -> bool {
        self.session_can_match() || self.session_after_remaining > 0
    }

    fn session_suppress_normal_output(&self) -> bool {
        self.config.count || self.config.files_without_match || self.session_binary_detected
    }

    fn session_needs_match_positions(&self) -> bool {
        (self.config.word_regexp
            || self.config.line_regexp
            || self.config.only_matching
            || self.config.use_color)
            && !self.session_suppress_normal_output()
    }

    /// Should the trailing `Binary file ... matches` notice be emitted?
    /// Suppressed by `-c`, `-l`, `-L`, `-q` (all folded into
    /// [`Self::binary_notice_enabled`] at construction time).
    fn session_should_emit_binary_notice(&self) -> bool {
        self.binary_notice_enabled && self.session_binary_detected && self.session_any_match()
    }

    fn session_run(
        &mut self,
        lb: &mut LineBuffer,
        path: &Path,
        reader: &mut File,
    ) -> io::Result<bool> {
        // Reset all session (per-file) state.
        self.session_context_buf.clear();
        self.session_match_count = 0;
        self.session_after_remaining = 0;
        self.session_last_printed_line = 0;
        self.session_binary_detected = false;
        lb.reset();

        let mut line_number: u64 = 0;

        while let Some((line, line_start)) = lb.read_line(reader)? {
            line_number += 1;

            // Handle `-U, --binary` On Windows.
            #[cfg(windows)]
            let line =
                if self.config.strip_cr && !self.config.null_data && line.last() == Some(&b'\r') {
                    &line[..line.len() - 1]
                } else {
                    line
                };

            // Any null byte flips us into binary mode.
            if !self.session_mark_binary_if(|| memchr(0, line).is_some()) {
                return Ok(false);
            }

            if let Some(positions) = self.session_match_line(line) {
                // TODO: GNU grep respects LANG. Here, I'm always checking for valid UTF-8.
                if !self.session_mark_binary_if(|| std::str::from_utf8(line).is_err()) {
                    return Ok(false);
                }

                // Print the match and context, and update session state accordingly.
                if !self.session_handle_match(path, line_number, line_start, line, &positions)? {
                    return Ok(true);
                }

                if self.session_should_emit_binary_notice() {
                    self.writer.report_binary_match(path);
                    return Ok(true);
                }
            } else {
                self.session_handle_nonmatch(path, line_number, line_start, line)?;
            }

            if !self.session_should_continue() {
                break;
            }
        }

        self.session_finalize(path)
    }

    /// Mark the file as binary when `predicate` returns true.
    #[inline(always)]
    fn session_mark_binary_if(&mut self, predicate: impl FnOnce() -> bool) -> bool {
        if self.session_binary_detected || self.config.binary_mode == BinaryMode::Text {
            return true;
        }
        if !predicate() {
            return true;
        }
        self.session_binary_detected = true;
        self.config.binary_mode != BinaryMode::WithoutMatch
    }

    fn session_match_line(&self, line: &[u8]) -> Option<Vec<(usize, usize)>> {
        if !self.session_can_match() {
            None
        } else if self.session_needs_match_positions() {
            self.matcher.match_line(line)
        } else {
            self.matcher.is_match(line)
        }
    }

    /// Returns false if this file's search is complete (e.g. `-q`, `-l`, `-L`).
    fn session_handle_match(
        &mut self,
        path: &Path,
        line_number: u64,
        byte_offset: u64,
        line: &[u8],
        positions: &[(usize, usize)],
    ) -> io::Result<bool> {
        self.session_match_count += 1;

        if self.config.quiet {
            return Ok(false);
        }
        if self.config.files_with_matches {
            self.writer.write_filename(path)?;
            return Ok(false);
        }
        if self.config.files_without_match {
            return Ok(false);
        }

        if !self.session_suppress_normal_output() {
            self.session_write_match_with_context(
                path,
                &LineView {
                    line,
                    line_number,
                    byte_offset,
                    is_match: true,
                    match_positions: positions,
                },
            )?;
        }

        self.session_after_remaining = self.config.after_context;
        Ok(true)
    }

    fn session_handle_nonmatch(
        &mut self,
        path: &Path,
        line_number: u64,
        byte_offset: u64,
        line: &[u8],
    ) -> io::Result<()> {
        if self.session_after_remaining > 0 {
            if !self.session_suppress_normal_output() {
                self.writer.write_line(
                    &LineView {
                        line,
                        line_number,
                        byte_offset,
                        is_match: false,
                        match_positions: &[],
                    },
                    path,
                )?;
                self.session_last_printed_line = line_number;
            }
            self.session_after_remaining -= 1;
        } else if self.config.before_context > 0
            && self.session_can_match()
            && !self.session_suppress_normal_output()
        {
            self.session_context_buf
                .push(line, line_number, byte_offset);
        }
        Ok(())
    }

    fn session_write_match_with_context(
        &mut self,
        path: &Path,
        view: &LineView<'_>,
    ) -> io::Result<()> {
        // Group separator between non-adjacent groups.
        // `last_printed_line == 0` means we haven't printed anything yet.
        //   = first group = skip the separator
        if self.config.has_context
            && self.session_last_printed_line > 0
            && view.line_number > self.session_last_printed_line + 1
        {
            self.writer.write_group_separator()?;
        }

        for ctx in self.session_context_buf.drain_iter() {
            if ctx.line_number > self.session_last_printed_line {
                self.writer.write_line(&ctx.view(), path)?;
                self.session_last_printed_line = ctx.line_number;
            }
        }

        self.writer.write_line(view, path)?;
        self.session_last_printed_line = view.line_number;
        Ok(())
    }

    /// End-of-file bookkeeping: count / `-L` / binary notice.
    fn session_finalize(&mut self, path: &Path) -> io::Result<bool> {
        if self.config.quiet {
            return Ok(self.session_any_match());
        }
        if self.config.count && !self.config.files_with_matches && !self.config.files_without_match
        {
            self.writer.write_count(self.session_match_count, path)?;
        }
        if self.config.files_without_match && !self.session_any_match() {
            self.writer.write_filename(path)?;
        }
        if self.session_should_emit_binary_notice() {
            self.writer.report_binary_match(path);
        }
        Ok(self.session_any_match())
    }

    fn strip_dot_prefix(path: &Path) -> &Path {
        let bytes = path.as_os_str().as_encoded_bytes();

        #[cfg(windows)]
        let bytes = bytes
            .strip_prefix(b".\\")
            .or_else(|| bytes.strip_prefix(b"./"))
            .unwrap_or(bytes);

        #[cfg(not(windows))]
        let bytes = bytes.strip_prefix(b"./").unwrap_or(bytes);

        // SAFETY: We sliced off a pure ASCII prefix off of `path`.
        Path::new(unsafe { OsStr::from_encoded_bytes_unchecked(bytes) })
    }

    fn is_special_file(path: &Path) -> bool {
        match std::fs::metadata(path) {
            Ok(m) => {
                let ft = m.file_type();
                !ft.is_file() && !ft.is_dir()
            }
            Err(_) => false,
        }
    }
}
