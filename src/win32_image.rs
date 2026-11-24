//! In-memory BGRA frames for PrintWindow, local OCR, and PNG export.

use std::io::Write;

use crc32fast::Hasher as Crc32;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::error::{KaxError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Frame {
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    pub fn crop(&self, x: usize, y: usize, right: usize, bottom: usize) -> Result<Self> {
        if !(x < right && right <= self.width && y < bottom && bottom <= self.height) {
            return Err(KaxError::validation("Crop is outside the captured window"));
        }
        let w = right - x;
        let h = bottom - y;
        let mut data = Vec::with_capacity(w * h * 4);
        for row in y..bottom {
            let start = (row * self.width + x) * 4;
            let end = (row * self.width + right) * 4;
            data.extend_from_slice(&self.data[start..end]);
        }
        Ok(Self {
            width: w,
            height: h,
            data,
        })
    }

    pub fn scale(&self, factor: usize) -> Self {
        if factor <= 1 {
            return self.clone();
        }
        let stride = self.width * 4;
        let mut out = Vec::with_capacity(self.width * factor * self.height * factor * 4);
        let mut scaled_row = Vec::with_capacity(stride * factor);
        for y in 0..self.height {
            scaled_row.clear();
            let row = &self.data[y * stride..(y + 1) * stride];
            for px in row.chunks_exact(4) {
                for _ in 0..factor {
                    scaled_row.extend_from_slice(px);
                }
            }
            for _ in 0..factor {
                out.extend_from_slice(&scaled_row);
            }
        }
        Self {
            width: self.width * factor,
            height: self.height * factor,
            data: out,
        }
    }

    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let i = (y * self.width + x) * 4;
        (self.data[i], self.data[i + 1], self.data[i + 2])
    }

    pub fn fill(
        &self,
        x: usize,
        y: usize,
        right: usize,
        bottom: usize,
        color: (u8, u8, u8),
    ) -> Self {
        let right = right.min(self.width);
        let bottom = bottom.min(self.height);
        if x >= right || y >= bottom {
            return self.clone();
        }
        let mut data = self.data.clone();
        let px = [color.0, color.1, color.2, 255u8];
        for row in y..bottom {
            let mut offset = (row * self.width + x) * 4;
            for _ in x..right {
                data[offset..offset + 4].copy_from_slice(&px);
                offset += 4;
            }
        }
        Self {
            width: self.width,
            height: self.height,
            data,
        }
    }

    pub fn png(&self) -> Result<Vec<u8>> {
        fn chunk(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut out = Vec::with_capacity(12 + payload.len());
            out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(payload);
            let mut hasher = Crc32::new();
            hasher.update(name);
            hasher.update(payload);
            out.extend_from_slice(&hasher.finalize().to_be_bytes());
            out
        }

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        let mut rgb = vec![0u8; self.width * 3 + 1];
        for row in self.data.chunks_exact(self.width * 4) {
            rgb[0] = 0;
            for (i, px) in row.chunks_exact(4).enumerate() {
                rgb[1 + i * 3] = px[2];
                rgb[1 + i * 3 + 1] = px[1];
                rgb[1 + i * 3 + 2] = px[0];
            }
            encoder.write_all(&rgb)?;
        }
        let compressed = encoder.finish()?;

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&(self.width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);

        let mut out = Vec::with_capacity(8 + 25 + 12 + compressed.len() + 12);
        out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        out.extend_from_slice(&chunk(b"IHDR", &ihdr));
        out.extend_from_slice(&chunk(b"IDAT", &compressed));
        out.extend_from_slice(&chunk(b"IEND", b""));
        Ok(out)
    }
}
