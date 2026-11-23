"""Windows.Media.Ocr via the WinRT ABI, with no Python or .NET dependency."""

import ctypes as C
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from uuid import UUID

from kakaotalk_win32_api import Blocked, bind, until


P = C.c_void_p
PP = C.POINTER(P)
HR = C.c_int32
U32 = C.c_uint32
I32 = C.c_int32


class Rect(C.Structure):
    _fields_ = [(field, C.c_float) for field in ("x", "y", "width", "height")]


@dataclass(frozen=True)
class Line:
    text: str
    x: float
    y: float
    width: float
    height: float


def iid(value):
    return C.create_string_buffer(UUID(value).bytes_le)


def check(hr):
    if hr < 0:
        raise Blocked(f"windows_ocr_error_0x{hr & 0xffffffff:08x}")


def vcall(obj, slot, result, argtypes, *args):
    address = C.cast(obj, C.POINTER(C.POINTER(P)))[0][slot]
    return C.WINFUNCTYPE(result, P, *argtypes)(address)(obj, *args)


def com(obj, slot, out_type, *args):
    """Read one out parameter from a WinRT method on its default interface."""
    out = out_type()
    check(vcall(obj, slot, HR, [kind for kind, _ in args] + [C.POINTER(out_type)],
                *(value for _, value in args), C.byref(out)))
    return out


def release(obj):
    if obj:
        vcall(obj, 2, U32, ())


def keep(stack, obj):
    if obj:
        stack.callback(release, obj)
    return obj


def query(obj, guid):
    identifier = iid(guid)
    return com(obj, 0, P, (P, identifier))


class OCR:
    def __init__(self):
        self.dll = d = C.WinDLL("combase", use_last_error=True)
        bind(d, "RoInitialize", HR, U32)
        bind(d, "RoUninitialize", None)
        bind(d, "RoGetActivationFactory", HR, P, P, PP)
        bind(d, "WindowsCreateString", HR, C.c_wchar_p, U32, PP)
        bind(d, "WindowsDeleteString", HR, P)
        bind(d, "WindowsGetStringRawBuffer", P, P, C.POINTER(U32))

    @contextmanager
    def string(self, text):
        value = P()
        check(self.dll.WindowsCreateString(text, len(text.encode("utf-16-le")) // 2, C.byref(value)))
        try:
            yield value
        finally:
            self.dll.WindowsDeleteString(value)

    def take_string(self, value):
        size = U32()
        try:
            raw = self.dll.WindowsGetStringRawBuffer(value, C.byref(size))
            return C.wstring_at(raw, size.value) if size.value else ""
        finally:
            self.dll.WindowsDeleteString(value)

    def factory(self, stack, name, guid):
        with self.string(name) as name_string:
            out = P()
            check(self.dll.RoGetActivationFactory(name_string, iid(guid), C.byref(out)))
            return keep(stack, out)

    @contextmanager
    def session(self, language="ko"):
        with ExitStack() as stack:
            hr = self.dll.RoInitialize(1)  # MTA; a caller already in an STA can stay there.
            if hr != -2147417850:  # RPC_E_CHANGED_MODE
                check(hr)
                stack.callback(self.dll.RoUninitialize)
            ocr = self.factory(stack, "Windows.Media.Ocr.OcrEngine", "5bffa85a-3384-3540-9940-699120d428a8")
            lf = self.factory(stack, "Windows.Globalization.Language", "9b0252ac-0c27-44f8-b792-9793fb66c63e")
            with self.string(language) as tag:
                lang = keep(stack, com(lf, 6, P, (P, tag)))
            engine = keep(stack, com(ocr, 9, P, (P, lang)))
            if not engine:
                raise Blocked("windows_ocr_language_unavailable")
            bitmap = self.factory(stack, "Windows.Graphics.Imaging.SoftwareBitmap", "df0385db-672f-4a9d-806e-c2442f343e86")
            buffer = self.factory(stack, "Windows.Storage.Streams.Buffer", "71af914d-c10f-484b-bc50-14bc623b3a27")
            maximum = com(ocr, 6, U32).value

            def recognize(frame):
                if max(frame.width, frame.height) > maximum:
                    raise Blocked("windows_ocr_image_too_large")
                with ExitStack() as call:
                    size = len(frame.data)
                    ibuffer = keep(call, com(buffer, 6, P, (U32, size)))
                    access = keep(call, query(ibuffer, "905a0fef-bc53-11df-8c49-001e4fc686da"))
                    address = com(access, 3, P)
                    C.memmove(address, frame.data, size)
                    check(vcall(ibuffer, 8, HR, (U32,), size))  # IBuffer::put_Length
                    # ISoftwareBitmapStatics::CreateCopyWithAlphaFromBuffer (Bgra8, Ignore).
                    image = keep(call, com(bitmap, 10, P, (P, ibuffer), (I32, 87),
                                           (I32, frame.width), (I32, frame.height), (I32, 2)))
                    operation = keep(call, com(engine, 6, P, (P, image)))
                    info = keep(call, query(operation, "00000036-0000-0000-c000-000000000046"))
                    if not until(lambda: com(info, 7, I32).value, seconds=5, interval=0.01):
                        vcall(info, 9, HR, ())  # Cancel
                        raise Blocked("windows_ocr_timeout")
                    result = keep(call, com(operation, 8, P))  # GetResults
                    lines = keep(call, com(result, 6, P))
                    return self.lines(lines)

            yield recognize

    def lines(self, lines):
        output = []
        for index in range(com(lines, 7, U32).value):
            with ExitStack() as stack:
                line = keep(stack, com(lines, 6, P, (U32, index)))
                words = keep(stack, com(line, 6, P))
                bounds = []
                for j in range(com(words, 7, U32).value):
                    word = keep(stack, com(words, 6, P, (U32, j)))
                    bounds.append(com(word, 6, Rect))
                if bounds:
                    x = min(r.x for r in bounds)
                    y = min(r.y for r in bounds)
                    right = max(r.x + r.width for r in bounds)
                    bottom = max(r.y + r.height for r in bounds)
                    output.append(Line(self.take_string(com(line, 7, P)), x, y, right - x, bottom - y))
        return output
