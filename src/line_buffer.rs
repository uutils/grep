// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use memchr::{memchr, memrchr};
use std::fs::File;
use std::io::{self, Read as _};

pub struct LineBuffer {
    buffer: Vec<u8>,

    /// Start of the valid data and current (incomplete) line in `buffer`.
    beg: usize,
    /// Offset of where the next `memchr` should continue scanning from.
    /// This helps us avoid re-scanning bytes we've already confirmed don't contain a terminator.
    scan: usize,
    /// End of the current valid data in `buffer`.
    end: usize,

    /// Absolute file offset of `buffer[pos]`.
    next_line_start: u64,
    /// Set once a file read has returned EOF.
    eof: bool,
    /// The line terminator is typically NUL or LF.
    line_terminator: u8,
}

impl LineBuffer {
    pub fn new(line_terminator: u8) -> Self {
        Self {
            buffer: vec![0; 128 * 1024],

            beg: 0,
            scan: 0,
            end: 0,

            next_line_start: 0,
            eof: false,
            line_terminator,
        }
    }

    /// Reset the buffer to an empty state.
    pub fn reset(&mut self) {
        self.beg = 0;
        self.scan = 0;
        self.end = 0;
        self.next_line_start = 0;
        self.eof = false;
    }

    /// Read the next line from the given reader.
    /// Returns `Ok(None)` if the end of the reader is reached.
    /// Otherwise, returns `Ok(Some((line, line_start)))`, where `line` is the line read (without the
    /// `line_terminator`), and `line_start` is the absolute byte offset of the start of the line.
    pub fn read_line(&mut self, file: &mut File) -> io::Result<Option<(&[u8], u64)>> {
        if self.eof {
            return Ok(None);
        }

        loop {
            // Look for a line terminator and if found, yield that line.
            if let Some(off) = memchr(self.line_terminator, &self.buffer[self.scan..self.end]) {
                let line_start = self.next_line_start;
                let beg = self.beg;
                let end = self.scan + off;
                let line = &self.buffer[beg..end];
                self.beg = end + 1;
                self.scan = self.beg;
                self.next_line_start += (self.beg - beg) as u64;
                return Ok(Some((line, line_start)));
            }

            // `buffer[pos..end]` has no terminator. Remember that for the next scan.
            self.scan = self.end;

            // Move the partial line to the beginning of the buffer.
            // The idea is that read() calls will either be very slow, and this doesn't matter,
            // or they'll be very fast with big chunks and we want to maximize the amount of space per syscall.
            if self.beg > 0 {
                self.buffer.copy_within(self.beg..self.end, 0);
                self.end -= self.beg;
                self.scan -= self.beg;
                self.beg = 0;
            }
            if self.end == self.buffer.len() {
                // A single line exceeds the current buffer; grow.
                self.buffer.resize(self.buffer.len() * 2, 0);
            }

            // Read more data!
            let n = loop {
                match file.read(&mut self.buffer[self.end..]) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            };
            if n == 0 {
                // EOF: Yield the last line, if any.
                return if self.beg == self.end {
                    Ok(None)
                } else {
                    let line = &self.buffer[self.beg..self.end];
                    let line_start = self.next_line_start;
                    self.eof = true; // shortcut the next call to read_line()
                    Ok(Some((line, line_start)))
                };
            }
            self.end += n;
        }
    }

    /// Read the next run of *complete* lines as a single slice.
    ///
    /// Returns `Ok(None)` at end of input. Otherwise returns `Ok(Some((chunk,
    /// chunk_start)))`, where `chunk` spans one or more whole lines (each ending
    /// in the terminator) and `chunk_start` is the absolute byte offset of the
    /// first byte of the chunk. The only exception is a final line lacking a
    /// terminator, which is returned on its own as the last chunk.
    ///
    /// This hands back as much buffered data as ends on a line boundary, so a
    /// caller can scan many lines with one pass instead of line by line.
    pub fn read_chunk(&mut self, file: &mut File) -> io::Result<Option<(&[u8], u64)>> {
        loop {
            // Hand back everything up to and including the last terminator.
            if self.end > self.beg
                && let Some(off) = memrchr(self.line_terminator, &self.buffer[self.beg..self.end])
            {
                let beg = self.beg;
                let lim = self.beg + off + 1;
                let chunk_start = self.next_line_start;
                self.next_line_start += (lim - beg) as u64;
                self.beg = lim;
                self.scan = lim;
                return Ok(Some((&self.buffer[beg..lim], chunk_start)));
            }

            // No whole line buffered. At EOF, flush any unterminated remainder.
            if self.eof {
                if self.beg == self.end {
                    return Ok(None);
                }
                let beg = self.beg;
                let chunk_start = self.next_line_start;
                self.next_line_start += (self.end - beg) as u64;
                self.beg = self.end;
                self.scan = self.end;
                return Ok(Some((&self.buffer[beg..self.end], chunk_start)));
            }

            // Slide the partial tail to the front to maximize room for reading.
            if self.beg > 0 {
                self.buffer.copy_within(self.beg..self.end, 0);
                self.end -= self.beg;
                self.beg = 0;
                self.scan = 0;
            }
            if self.end == self.buffer.len() {
                // A single line is longer than the whole buffer; grow it.
                self.buffer.resize(self.buffer.len() * 2, 0);
            }

            let n = loop {
                match file.read(&mut self.buffer[self.end..]) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            };
            if n == 0 {
                self.eof = true;
            } else {
                self.end += n;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek as _, SeekFrom, Write as _};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A temp file pre-loaded with `content`, rewound to the start, and removed
    /// from disk when dropped.
    struct TempInput {
        file: File,
        path: std::path::PathBuf,
    }

    impl Drop for TempInput {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_input(content: &[u8]) -> TempInput {
        let mut path = std::env::temp_dir();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        path.push(format!("uu_grep_lb_{}_{n}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(content).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        TempInput { file, path }
    }

    /// Drain `read_chunk` into a list of (owned bytes, start offset) pairs.
    fn chunks(term: u8, content: &[u8]) -> Vec<(Vec<u8>, u64)> {
        let mut lb = LineBuffer::new(term);
        let mut input = temp_input(content);
        let mut out = Vec::new();
        while let Some((chunk, start)) = lb.read_chunk(&mut input.file).unwrap() {
            out.push((chunk.to_vec(), start));
        }
        out
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(chunks(b'\n', b"").is_empty());
    }

    #[test]
    fn whole_complete_lines_come_back_as_one_chunk() {
        // Small input arrives in a single read, so everything up to the final
        // terminator is one chunk starting at offset 0.
        assert_eq!(
            chunks(b'\n', b"a\nbb\nccc\n"),
            vec![(b"a\nbb\nccc\n".to_vec(), 0)]
        );
    }

    #[test]
    fn unterminated_tail_is_a_final_chunk_with_its_own_offset() {
        // "a\n" is the complete-line chunk; "bb" is flushed at EOF at offset 2.
        assert_eq!(
            chunks(b'\n', b"a\nbb"),
            vec![(b"a\n".to_vec(), 0), (b"bb".to_vec(), 2)]
        );
    }

    #[test]
    fn input_without_any_terminator_is_one_chunk() {
        assert_eq!(chunks(b'\n', b"abc"), vec![(b"abc".to_vec(), 0)]);
    }

    #[test]
    fn honors_a_custom_terminator() {
        assert_eq!(
            chunks(b'\0', b"a\0bb\0c"),
            vec![(b"a\0bb\0".to_vec(), 0), (b"c".to_vec(), 5)]
        );
    }

    #[test]
    fn reassembles_input_larger_than_the_buffer() {
        // Force many reads and at least one chunk boundary mid-file.
        let mut content = Vec::new();
        for i in 0..50_000u32 {
            content.extend_from_slice(format!("line number {i}\n").as_bytes());
        }
        assert!(content.len() > 128 * 1024);

        let got = chunks(b'\n', &content);
        assert!(got.len() > 1, "expected multiple chunks, got {}", got.len());

        // Chunks must tile the input exactly, contiguously, each ending on a
        // line boundary (the input ends with a terminator).
        let mut expected_start = 0u64;
        let mut joined = Vec::new();
        for (bytes, start) in &got {
            assert_eq!(*start, expected_start);
            assert_eq!(*bytes.last().unwrap(), b'\n');
            expected_start += bytes.len() as u64;
            joined.extend_from_slice(bytes);
        }
        assert_eq!(joined, content);
    }

    #[test]
    fn grows_to_hold_a_single_overlong_line() {
        // One line far bigger than the initial 128 KiB buffer, then a short one.
        let mut content = vec![b'x'; 300 * 1024];
        content.push(b'\n');
        content.extend_from_slice(b"tail\n");

        let got = chunks(b'\n', &content);
        let joined: Vec<u8> = got.iter().flat_map(|(b, _)| b.clone()).collect();
        assert_eq!(joined, content);
        assert_eq!(got[0].1, 0);
    }
}
