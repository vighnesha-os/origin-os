//! Block buffering shared by SHA-2 and SHA-3, with Merkle–Damgård padding for SHA-256 and
//! SHA-512 (FIPS 180-4 §5.1, §5.2) and sponge padding for SHA-3 and SHAKE (FIPS 202 §5.1,
//! Appendix B.2).

/// Copies `min(dst.len(), src.len())` bytes. Unlike `copy_from_slice` it has no panic path:
/// the release object for the kernel target must reference no panic routine, and LLVM
/// cannot always prove that two derived lengths are equal. It still lowers to `memcpy`.
pub(crate) fn copy(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = *s;
    }
}

/// Collects input into `B`-byte blocks and passes each complete block to a compression
/// function.
#[derive(Clone)]
pub(crate) struct BlockBuffer<const B: usize> {
    buf: [u8; B],
    len: usize,
}

impl<const B: usize> BlockBuffer<B> {
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0; B],
            len: 0,
        }
    }

    /// Feeds `data`, calling `compress` once per complete block, in order.
    pub(crate) fn update(&mut self, data: &[u8], mut compress: impl FnMut(&[u8; B])) {
        let mut data = data;
        if self.len > 0 {
            let take = B.saturating_sub(self.len).min(data.len());
            let Some((head, rest)) = data.split_at_checked(take) else {
                return;
            };
            let end = self.len.saturating_add(take);
            if let Some(dst) = self.buf.get_mut(self.len..end) {
                copy(dst, head);
            }
            self.len = end;
            data = rest;
            if self.len < B {
                return;
            }
            compress(&self.buf);
            self.len = 0;
        }
        let (blocks, rest) = data.as_chunks::<B>();
        for block in blocks {
            compress(block);
        }
        if let Some(dst) = self.buf.get_mut(..rest.len()) {
            copy(dst, rest);
        }
        self.len = rest.len();
    }

    /// Pads the buffered tail with `0x80`, zeros and the big-endian message length in bits
    /// (`L` bytes: 8 for SHA-256, 16 for SHA-512), and compresses the final one or two
    /// blocks.
    pub(crate) fn finish<const L: usize>(
        mut self,
        length_be: [u8; L],
        mut compress: impl FnMut(&[u8; B]),
    ) {
        let length_at = B.saturating_sub(L);
        if let Some(marker) = self.buf.get_mut(self.len) {
            *marker = 0x80;
        }
        let after_marker = self.len.saturating_add(1);
        if let Some(rest) = self.buf.get_mut(after_marker..) {
            rest.fill(0);
        }
        if after_marker > length_at {
            compress(&self.buf);
            self.buf = [0; B];
        }
        if let Some(tail) = self.buf.get_mut(length_at..) {
            tail.copy_from_slice(&length_be);
        }
        compress(&self.buf);
    }

    /// Pads the buffered tail with the FIPS 202 sponge padding and returns the final block.
    /// `suffix` holds the domain-separation bits and the first bit of pad10*1 (§5.1), first
    /// bit least significant (Appendix B.2). The last bit of pad10*1 is the top bit of the
    /// block's last byte; when only one byte of the block is free, both land in it.
    pub(crate) fn finish_sponge(mut self, suffix: u8) -> [u8; B] {
        if let Some(rest) = self.buf.get_mut(self.len..) {
            rest.fill(0);
        }
        if let Some(first) = self.buf.get_mut(self.len) {
            *first = suffix;
        }
        if let Some(last) = self.buf.last_mut() {
            *last |= 0x80;
        }
        self.buf
    }
}
