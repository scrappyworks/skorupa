//! Cursors over byte slices. Every access is checked, so running out of bytes
//! is a `None` that the caller turns into a typed error.

/// Reads fields from the front of a slice.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// Bytes not read yet.
    pub(crate) fn remaining(&self) -> usize {
        self.rest.len()
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.rest.split_at_checked(len)?;
        self.rest = tail;
        Some(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, tail) = self.rest.split_first_chunk::<N>()?;
        self.rest = tail;
        Some(*head)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.array().map(u8::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.array().map(u64::from_le_bytes)
    }
}

/// Writes fields to the front of a slice and counts them.
pub(crate) struct Writer<'a> {
    out: &'a mut [u8],
    written: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut [u8]) -> Self {
        Self { out, written: 0 }
    }

    /// Bytes written so far.
    pub(crate) fn written(&self) -> usize {
        self.written
    }

    /// Appends `bytes`, or returns `None` and writes nothing if they do not fit.
    pub(crate) fn put(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.written.checked_add(bytes.len())?;
        self.out.get_mut(self.written..end)?.copy_from_slice(bytes);
        self.written = end;
        Some(())
    }
}
