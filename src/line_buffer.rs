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

    /// Index of the first non-ASCII byte found in `buffer[..ascii_scanned]`, or
    /// `usize::MAX` when that range is all ASCII.
    non_ascii: usize,
    /// Bytes of `buffer` that were already checked for non-ASCII bytes.
    ascii_scanned: usize,
    /// Lines are asked about in order, so every line that starts at or after the
    /// last scan start and ends at `ascii_limit` or before is known to be ASCII.
    ascii_limit: usize,
}

/// Index of the first non-ASCII byte of `bytes`, if any.
fn first_non_ascii(bytes: &[u8]) -> Option<usize> {
    const BLOCK: usize = 64;
    let mut base = 0;
    for block in bytes.chunks(BLOCK) {
        if !block.is_ascii() {
            return block.iter().position(|b| !b.is_ascii()).map(|i| base + i);
        }
        base += block.len();
    }
    None
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

            non_ascii: usize::MAX,
            ascii_scanned: 0,
            ascii_limit: 0,
        }
    }

    /// Forget what is known about which bytes are ASCII (the data moved).
    fn forget_ascii(&mut self) {
        self.non_ascii = usize::MAX;
        self.ascii_scanned = 0;
        self.ascii_limit = 0;
    }

    /// Whether `buffer[start..end]` is pure ASCII, hence valid UTF-8. The data
    /// is checked once per byte in large blocks instead of once per line, so
    /// asking for every line of a mostly ASCII file costs next to nothing.
    #[inline]
    fn range_is_ascii(&mut self, start: usize, end: usize) -> bool {
        end <= self.ascii_limit || self.scan_for_ascii(start, end)
    }

    #[cold]
    fn scan_for_ascii(&mut self, start: usize, end: usize) -> bool {
        if self.non_ascii < start {
            // The known non-ASCII byte is behind us: look further ahead.
            self.forget_ascii();
            self.ascii_scanned = start;
        }
        if self.non_ascii == usize::MAX && self.ascii_scanned < self.end {
            match first_non_ascii(&self.buffer[self.ascii_scanned..self.end]) {
                Some(i) => {
                    self.non_ascii = self.ascii_scanned + i;
                    self.ascii_scanned = self.non_ascii;
                }
                None => self.ascii_scanned = self.end,
            }
        }
        self.ascii_limit = self.non_ascii.min(self.ascii_scanned);
        end <= self.ascii_limit
    }

    /// Reset the buffer to an empty state.
    pub fn reset(&mut self) {
        self.beg = 0;
        self.scan = 0;
        self.end = 0;
        self.next_line_start = 0;
        self.eof = false;
        self.forget_ascii();
    }

    /// Read the next line from the given reader.
    /// Returns `Ok(None)` if the end of the reader is reached.
    /// Otherwise, returns `Ok(Some((line, line_start)))`, where `line` is the line read (without the
    /// `line_terminator`), and `line_start` is the absolute byte offset of the start of the line.
    /// The last element tells whether `line` is pure ASCII.
    pub fn read_line(&mut self, file: &mut File) -> io::Result<Option<(&[u8], u64, bool)>> {
        if self.eof {
            return Ok(None);
        }

        loop {
            // Look for a line terminator and if found, yield that line.
            if let Some(off) = memchr(self.line_terminator, &self.buffer[self.scan..self.end]) {
                let line_start = self.next_line_start;
                let beg = self.beg;
                let end = self.scan + off;
                let ascii = self.range_is_ascii(beg, end);
                let line = &self.buffer[beg..end];
                self.beg = end + 1;
                self.scan = self.beg;
                self.next_line_start += (self.beg - beg) as u64;
                return Ok(Some((line, line_start, ascii)));
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
                self.forget_ascii();
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
                    let ascii = self.range_is_ascii(self.beg, self.end);
                    let line = &self.buffer[self.beg..self.end];
                    let line_start = self.next_line_start;
                    self.eof = true; // shortcut the next call to read_line()
                    Ok(Some((line, line_start, ascii)))
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
                self.forget_ascii();
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

    /// Read every line with `read_line`, returning (bytes, is_ascii) pairs.
    fn lines_with_ascii(content: &[u8]) -> Vec<(Vec<u8>, bool)> {
        let mut lb = LineBuffer::new(b'\n');
        let mut input = temp_input(content);
        let mut out = Vec::new();
        while let Some((line, _, ascii)) = lb.read_line(&mut input.file).unwrap() {
            out.push((line.to_vec(), ascii));
        }
        out
    }

    #[test]
    fn read_line_reports_which_lines_are_ascii() {
        let got = lines_with_ascii(b"abc\n\xffx\nplain\ncaf\xc3\xa9\nlast");
        let flags: Vec<bool> = got.iter().map(|(_, a)| *a).collect();
        assert_eq!(flags, [true, false, true, false, true]);
    }

    #[test]
    fn read_line_ascii_flag_matches_is_ascii_across_buffer_refills() {
        // Larger than the 128 KiB buffer, with non-ASCII bytes sprinkled at
        // positions that fall on and off line starts and refill boundaries.
        let mut content = Vec::new();
        for i in 0..60_000u32 {
            content.extend_from_slice(format!("line number {i}").as_bytes());
            match i % 977 {
                0 => content.push(0xff),
                1 => content.extend_from_slice("\u{e9}".as_bytes()),
                _ => {}
            }
            content.push(b'\n');
        }
        assert!(content.len() > 2 * 128 * 1024);
        let got = lines_with_ascii(&content);
        assert_eq!(got.len(), 60_000);
        for (line, ascii) in &got {
            assert_eq!(
                *ascii,
                line.is_ascii(),
                "{:?}",
                String::from_utf8_lossy(line)
            );
        }
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
