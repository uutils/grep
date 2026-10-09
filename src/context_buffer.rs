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
    slots: Vec<BufferedLine>,
    head: usize,
    len: usize,
    capacity: usize,
}

impl ContextBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::new(),
            head: 0,
            len: 0,
            capacity,
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

        if self.capacity == 0 {
            return
        }

        let full = self.len == self.capacity;

        let index = if full {
            self.head 
        } else {
            let index = self.len;

            if index == self.slots.len() {
                self.slots.push(BufferedLine { 
                    line: Vec::new(), 
                    line_number, 
                    byte_offset 
                });
            }
            index
        };

        let slot = &mut self.slots[index];

        slot.line.clear();

        if slot.line.capacity() / 2 > line.len() {
            slot.line.shrink_to(line.len());
        }
        slot.line.extend_from_slice(line);
        slot.line_number = line_number;
        slot.byte_offset = byte_offset;
        
        if full {
            self.head = self.head.wrapping_add(1);

            if self.head == self.capacity {
                self.head = 0;
            }
        } else {
            self.len = self.len.wrapping_add(1);
            self.head = if self.len == self.capacity {
                0
            } else {
                self.len
            }
        }
    }

    pub fn drain_iter(&mut self) -> impl Iterator<Item = &BufferedLine> {
        let len = self.len;
        let slots = &self.slots[..];
        
        let start = if len == self.capacity {
            self.head 
        } else {
            0
        };

        self.head = 0;
        self.len = 0;

        (0..len).map(move |i| &slots[start.wrapping_add(i) % slots.len()])
    }
}
