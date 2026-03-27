//! Audio chunker
//!
//! Buffers audio data and emits fixed-size chunks for ASR.

/// Buffers audio data and emits chunks of a fixed size
pub struct AudioChunker {
    buffer: Vec<u8>,
    chunk_size: usize,
}

impl AudioChunker {
    /// Create a new chunker with the specified chunk size in bytes
    pub fn new(chunk_size: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(chunk_size * 2),
            chunk_size,
        }
    }

    /// Create a chunker for 100ms chunks at 16kHz mono 16-bit
    pub fn for_100ms() -> Self {
        // 16000 samples/sec * 2 bytes/sample * 0.1 sec = 3200 bytes
        let bytes_per_ms = 16000 * 2 / 1000;
        let chunk_size = bytes_per_ms * 100;
        Self::new(chunk_size)
    }

    /// Append data and return any complete chunks
    pub fn append(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(data);

        let mut chunks = Vec::new();
        while self.buffer.len() >= self.chunk_size {
            let chunk: Vec<u8> = self.buffer.drain(..self.chunk_size).collect();
            chunks.push(chunk);
        }

        chunks
    }

    /// Flush any remaining data as a final chunk
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }

    /// Reset the buffer
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunker() {
        let mut chunker = AudioChunker::new(10);

        // Add 25 bytes
        let chunks = chunker.append(&[0u8; 25]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 10);
        assert_eq!(chunks[1].len(), 10);

        // Flush remaining 5 bytes
        let remaining = chunker.flush();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().len(), 5);
    }
}
