// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

pub struct LineView<'a> {
    /// Line content (without the terminator).
    pub line: &'a [u8],
    /// 1-based line number.
    pub line_number: u64,
    /// Byte offset of this line in the input stream.
    pub byte_offset: u64,
    /// Whether this line matched (vs. being a context line).
    pub is_match: bool,
    /// Match positions within the line (start, end).
    /// Empty for context lines or for matching lines we don't need to highlight.
    pub match_positions: &'a [(usize, usize)],
}

#[derive(Clone)]
pub struct BufferedLine {
    pub line: Vec<u8>,
    pub line_number: u64,
    pub byte_offset: u64,
}

impl BufferedLine {
    pub fn view(&self) -> LineView<'_> {
        LineView {
            line: &self.line,
            line_number: self.line_number,
            byte_offset: self.byte_offset,
            is_match: false,
            match_positions: &[],
        }
    }
}

/// A fixed-capacity ring buffer of context lines.
///
/// TODO: Ideally, this would be integrated into `LineBuffer`, which can then
/// provide a more optimized `ContextBuffer` when `mmap()` is available.
pub struct ContextBuffer {
    /// One slot per context line; indices wrap with `%`.
    slots: Vec<BufferedLine>,
    head: usize,
    len: usize,
}

impl ContextBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: vec![
                BufferedLine {
                    line: Vec::new(),
                    line_number: 0,
                    byte_offset: 0,
                };
                capacity
            ],
            head: 0,
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    pub fn push(&mut self, line: &[u8], line_number: u64, byte_offset: u64) {
        debug_assert!(
            !self.slots.is_empty(),
            "push on zero-capacity ContextBuffer"
        );
        if self.slots.is_empty() {
            return;
        }

        let slot = &mut self.slots[self.head];

        slot.line.clear();
        if slot.line.capacity() / 2 > line.len() {
            slot.line.shrink_to(line.len());
        }
        slot.line.extend_from_slice(line);
        slot.line_number = line_number;
        slot.byte_offset = byte_offset;

        self.head = (self.head + 1) % self.slots.len();
        self.len = (self.len + 1).min(self.slots.len());
    }

    pub fn drain_iter(&mut self) -> impl Iterator<Item = &BufferedLine> {
        let len = self.len;
        let slots = &self.slots[..];
        // `len` is 0 when there are no slots, so the closure is never called;
        // `max(1)` only keeps the modulo well-defined.
        let n = slots.len().max(1);
        let start = (self.head + n - len) % n;

        self.head = 0;
        self.len = 0;

        (0..len).map(move |i| &slots[(start + i) % n])
    }
}
