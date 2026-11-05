pub struct BitBuffer<'a> {
    pub buffer: &'a mut [u8],
    pub position: usize,
}

impl<'a> BitBuffer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> BitBuffer<'a> {
        BitBuffer { buffer, position: 0 }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn write_lte25(&mut self, bit_values: u32, num_bits: usize) {
        debug_assert!(num_bits > 0 && num_bits <= 25);
        debug_assert!(self.position + num_bits <= self.buffer.len() * 8);

        let at = self.position >> 3;
        let shift = 32 - (self.position & 7) - num_bits;

        let curr = self.load(at);

        let mask = ((!0u32) >> (32 - num_bits)) << shift;
        let main = ((bit_values << shift) & mask) | (curr & !mask);

        self.store(at, main);

        self.position += num_bits;
    }

    pub fn write(&mut self, bit_values: u32, num_bits: usize) {
        debug_assert!(num_bits > 0 && num_bits <= 32);
        debug_assert!(self.position + num_bits <= self.buffer.len() * 8);

        let at = self.position >> 3;
        let shift = (32 - ((self.position as i32) & 7) - (num_bits as i32)) as i32;

        let curr = self.load(at);

        if shift < 0 {
            let mask = (!0u32) >> -shift;
            let main = (bit_values >> -shift) | (curr & !mask);
            let tail = ((bit_values << (8 + shift)) & 0xff) as u8;

            self.store(at, main);
            self.buffer[at + 4] = tail;
        } else {
            let mask = ((!0u32) >> (32 - num_bits)) << shift;
            let main = ((bit_values << shift) & mask) | (curr & !mask);

            self.store(at, main);
        }

        self.position += num_bits;
    }

    /// Big-endian word at byte `at`. Near the end of the buffer it reaches past the last byte, where it reads zeros
    /// and writes nothing (those bits are never the ones being written): no reads or writes out of bounds.
    fn load(&self, at: usize) -> u32 {
        let mut word = [0u8; 4];
        for (i, byte) in word.iter_mut().enumerate() {
            *byte = self.buffer.get(at + i).copied().unwrap_or(0);
        }
        u32::from_be_bytes(word)
    }

    fn store(&mut self, at: usize, word: u32) {
        for (i, byte) in word.to_be_bytes().into_iter().enumerate() {
            if let Some(target) = self.buffer.get_mut(at + i) {
                *target = byte;
            }
        }
    }

    /// Align bit buffer to next byte boundary, writing zeros if requested
    pub fn byte_align(&mut self) {
        let bit = self.position & 7;

        if bit == 0 { return; }

        self.write_lte25(0, 8 - bit);
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn set_position(&mut self, position: usize) {
        self.position = position;
    }
}
