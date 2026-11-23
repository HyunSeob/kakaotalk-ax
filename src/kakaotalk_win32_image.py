"""In-memory BGRA frames for PrintWindow, local OCR and PNG export."""

from dataclasses import dataclass
import struct
import zlib


@dataclass(frozen=True)
class Frame:
    width: int
    height: int
    data: bytes

    def crop(self, x, y, right, bottom):
        if not (0 <= x < right <= self.width and 0 <= y < bottom <= self.height):
            raise ValueError("Crop is outside the captured window")
        data = b"".join(self.data[(row * self.width + x) * 4:(row * self.width + right) * 4]
                        for row in range(y, bottom))
        return Frame(right - x, bottom - y, data)

    def scale(self, factor):
        if factor == 1:
            return self
        stride = self.width * 4
        rows = []
        for y in range(self.height):
            row = self.data[y * stride:(y + 1) * stride]
            scaled = b"".join(row[i:i+4] * factor for i in range(0, stride, 4))
            rows.append(scaled * factor)
        return Frame(self.width * factor, self.height * factor, b"".join(rows))

    def pixel(self, x, y):
        i = (y * self.width + x) * 4
        return tuple(self.data[i:i+3])

    def fill(self, x, y, right, bottom, color):
        x, y = max(0, x), max(0, y)
        right, bottom = min(self.width, right), min(self.height, bottom)
        if x >= right or y >= bottom:
            return self
        data = bytearray(self.data)
        span = bytes((*color[:3], 255)) * (right - x)
        for row in range(y, bottom):
            start = (row * self.width + x) * 4
            data[start:start + len(span)] = span
        return Frame(self.width, self.height, bytes(data))

    def png(self):
        def chunk(name, payload):
            return (struct.pack(">I", len(payload)) + name + payload +
                    struct.pack(">I", zlib.crc32(name + payload)))

        compressor = zlib.compressobj()
        encoded = []
        for start in range(0, len(self.data), self.width * 4):
            row = self.data[start:start + self.width * 4]
            rgb = bytearray(self.width * 3)
            rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
            encoded.append(compressor.compress(b"\0" + rgb))
        encoded.append(compressor.flush())
        return (b"\x89PNG\r\n\x1a\n" +
                chunk(b"IHDR", struct.pack(">IIBBBBB", self.width, self.height, 8, 2, 0, 0, 0)) +
                chunk(b"IDAT", b"".join(encoded)) + chunk(b"IEND", b""))
