// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::Config;
use crate::context_buffer::LineView;
use std::ffi::OsStr;
use std::io::{self, BufWriter, StdoutLock, Write};
use std::path::Path;
use uucore::error::strip_errno;

#[cfg(target_pointer_width = "64")]
const BUF_SIZE: usize = 128 * 1024;
#[cfg(target_pointer_width = "32")]
const BUF_SIZE: usize = 16 * 1024;

pub struct OutputWriter<'a> {
    config: &'a Config<'a>,
    out: BufWriter<StdoutLock<'static>>,
    line_number_width: usize,
}

impl<'a> OutputWriter<'a> {
    pub fn new(config: &'a Config<'a>) -> Self {
        Self {
            config,
            out: BufWriter::with_capacity(BUF_SIZE, io::stdout().lock()),
            line_number_width: 0,
        }
    }

    /// Returns true if line numbers will be padded with whitespace (`-T`),
    /// which means that you should call `set_line_number_width`.
    pub fn wants_padded_line_numbers(&self) -> bool {
        self.config.initial_tab && self.config.line_number
    }

    /// Set the minimum field width for line numbers (used with `-T`).
    ///
    /// It's sort of wrong for this to be a setter, because it's technically
    /// per-session state in searcher.rs, but it's convenient and it's cheap.
    pub fn set_line_number_width(&mut self, width: usize) {
        debug_assert!(self.wants_padded_line_numbers(), "don't call this function");
        self.line_number_width = width;
    }

    /// Flush stdout.
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Write a matching or context line.
    pub fn write_line(&mut self, view: &LineView<'_>, filename: &Path) -> io::Result<()> {
        if self.config.only_matching && view.is_match {
            self.write_only_matching(view, filename)
        } else {
            self.write_line_with_matches(view, filename)
        }
    }

    /// Write only the matching portions of a line (`-o` mode).
    fn write_only_matching(&mut self, view: &LineView<'_>, filename: &Path) -> io::Result<()> {
        for &(start, end) in view.match_positions {
            if start == end {
                continue; // Skip zero-length matches
            }

            self.write_prefix(
                filename,
                view.line_number,
                view.byte_offset + start as u64,
                b':',
            )?;

            self.write_colored_bytes(
                self.config.color_config.matched_selected,
                &view.line[start..end],
            )?;
            self.write_terminator()?;
        }

        self.maybe_flush()
    }

    /// Otherwise, write the whole line, with optional color highlighting of matches.
    fn write_line_with_matches(&mut self, view: &LineView<'_>, filename: &Path) -> io::Result<()> {
        self.write_prefix(
            filename,
            view.line_number,
            view.byte_offset,
            if view.is_match { b':' } else { b'-' },
        )?;

        let mut last_end = 0;

        if self.config.use_color && view.is_match && !view.match_positions.is_empty() {
            for &(start, end) in view.match_positions {
                if start > last_end {
                    self.out.write_all(&view.line[last_end..start])?;
                }
                if start < end {
                    let match_bytes = &view.line[start..end];
                    self.write_colored_bytes(
                        self.config.color_config.matched_selected,
                        match_bytes,
                    )?;
                }
                last_end = end;
            }
        }

        if last_end < view.line.len() {
            self.out.write_all(&view.line[last_end..])?;
        }

        self.write_terminator()?;
        self.maybe_flush()
    }

    /// Write a line prefix.
    fn write_prefix(
        &mut self,
        filename: &Path,
        line_number: u64,
        byte_offset: u64,
        sep_char: u8,
    ) -> io::Result<()> {
        if self.config.show_filename {
            self.write_colored_fmt(
                self.config.color_config.filename,
                format_args!("{}", filename.display()),
            )?;
            if self.config.null_separator {
                self.out.write_all(b"\0")?;
            } else {
                self.write_separator(sep_char)?;
            }
        }

        if self.config.line_number {
            let width = self.line_number_width;
            self.write_colored_fmt(
                self.config.color_config.line_number,
                format_args!("{:>width$}", line_number, width = width),
            )?;
            self.write_separator(sep_char)?;
        }

        if self.config.byte_offset {
            self.write_colored_fmt(
                self.config.color_config.byte_offset,
                format_args!("{}", byte_offset),
            )?;
            self.write_separator(sep_char)?;
        }

        if self.config.initial_tab
            && (self.config.line_number || self.config.byte_offset || self.config.show_filename)
        {
            self.out.write_all(b"\t")?;
        }

        Ok(())
    }

    /// Write the count line for `-c` mode.
    pub fn write_count(&mut self, count: u64, filename: &Path) -> io::Result<()> {
        if self.config.show_filename {
            let sep = if self.config.null_separator {
                b'\0'
            } else {
                b':'
            };
            write!(self.out, "{}{}", filename.display(), sep as char)?;
        }

        writeln!(self.out, "{}", count)?;
        self.maybe_flush()
    }

    /// Write filename for `-l` / `-L` mode.
    pub fn write_filename(&mut self, path: &Path) -> io::Result<()> {
        self.write_colored_fmt(
            self.config.color_config.filename,
            format_args!("{}", path.display()),
        )?;
        self.out.write_all(if self.config.null_separator {
            b"\0"
        } else {
            b"\n"
        })?;
        self.maybe_flush()
    }

    /// Write an IO error to stderr.
    pub fn report_io_error(&self, label: &OsStr, err: &io::Error) {
        if !self.config.no_messages && !self.config.quiet {
            // Strip the trailing " (os error XX)" so the message matches GNU grep.
            eprintln!(
                "grep: {label}: {err}",
                label = label.to_string_lossy(),
                err = strip_errno(err)
            );
        }
    }

    /// Write the "binary file matches" message to stderr.
    pub fn report_binary_match(&self, path: &Path) {
        eprintln!("grep: {}: binary file matches", path.display());
    }

    /// Write the group separator between context groups.
    pub fn write_group_separator(&mut self) -> io::Result<()> {
        if let Some(sep) = self.config.group_separator {
            self.write_colored_bytes(self.config.color_config.separator, sep.as_bytes())?;
            self.out.write_all(b"\n")?;
            self.maybe_flush()
        } else {
            Ok(())
        }
    }

    /// Flush in `--line-buffered` mode.
    fn maybe_flush(&mut self) -> io::Result<()> {
        if self.config.line_buffered {
            self.out.flush()
        } else {
            Ok(())
        }
    }

    /// Write the line/record terminator (`\n`, or `\0` under `-z`).
    fn write_terminator(&mut self) -> io::Result<()> {
        self.out
            .write_all(if self.config.null_data { b"\0" } else { b"\n" })
    }

    fn write_separator(&mut self, ch: u8) -> io::Result<()> {
        self.write_colored_bytes(self.config.color_config.separator, &[ch])
    }

    fn write_colored_bytes(&mut self, color: &str, text: &[u8]) -> io::Result<()> {
        let colored = self.config.use_color && !color.is_empty();
        if colored {
            self.write_colored_prefix(color)?;
        }
        self.out.write_all(text)?;
        if colored {
            self.write_colored_suffix()?;
        }
        Ok(())
    }

    fn write_colored_fmt(&mut self, color: &str, args: std::fmt::Arguments<'_>) -> io::Result<()> {
        let colored = self.config.use_color && !color.is_empty();
        if colored {
            self.write_colored_prefix(color)?;
        }
        self.out.write_fmt(args)?;
        if colored {
            self.write_colored_suffix()?;
        }
        Ok(())
    }

    fn write_colored_prefix(&mut self, color: &str) -> io::Result<()> {
        write!(self.out, "\x1b[{}m", color)?;
        if !self.config.color_config.no_erase {
            self.out.write_all(b"\x1b[K")?;
        }
        Ok(())
    }

    fn write_colored_suffix(&mut self) -> io::Result<()> {
        if self.config.color_config.no_erase {
            self.out.write_all(b"\x1b[m")
        } else {
            self.out.write_all(b"\x1b[m\x1b[K")
        }
    }
}
