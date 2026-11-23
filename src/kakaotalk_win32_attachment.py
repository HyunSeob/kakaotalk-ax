"""Locate a photo using its rendered bounds and exact clipboard row evidence."""

from hashlib import sha256
import time

from kakaotalk_win32_api import Blocked, until
from kakaotalk_win32_rows import transcript_rows


def photo_boxes(frame, dpi):
    """Find compact rectangular foreground regions against the chat background."""
    scale = dpi / 96
    minimum = round(54 * scale)
    tolerance = round(7 * scale)
    gap = max(2, round(3 * scale))
    left, right = round(9 * scale), frame.width - round(22 * scale)
    if right-left < minimum:
        return []
    tracks, finished = [], []
    for y in range(1, frame.height-1):
        b, g, r = frame.pixel(3, y)
        runs = []
        start = last = None
        for x in range(left, right):
            bb, gg, rr = frame.pixel(x, y)
            if max(abs(b-bb), abs(g-gg), abs(r-rr)) >= 25:
                if last is None or x-last > gap:
                    if last is not None and last-start+1 >= minimum:
                        runs.append((start, last+1))
                    start = x
                last = x
        if last is not None and last-start+1 >= minimum:
            runs.append((start, last+1))
        for run in runs:
            same = next((track for track in tracks if y-track[3] <= tolerance and
                         abs(track[0]-run[0]) <= tolerance and abs(track[2]-run[1]) <= tolerance), None)
            if same is None:
                tracks.append([run[0], y, run[1], y+1, 1])
            else:
                same[0] = min(same[0], run[0])
                same[2] = max(same[2], run[1])
                same[3] = y+1
                same[4] += 1
        expired = [track for track in tracks if y-track[3] > tolerance]
        finished.extend(expired)
        tracks = [track for track in tracks if y-track[3] <= tolerance]
    finished.extend(tracks)
    return [tuple(track[:4]) for track in finished if
            track[3]-track[1] >= round(40 * scale) and track[4] >= (track[3]-track[1])*0.76
            and track[1] > 2 and track[3] < frame.height-2]


def candidate_rows(rows, copied, *, last=False):
    if not copied:
        return set()
    part = transcript_rows(copied)
    # A single selected message is copied without its sender and time.
    single_plain = len(part) == 1 and part[0]["sender"] is None and part[0]["kind"] != "date"

    def matches(full, selected):
        if single_plain:
            return full["text"] == copied
        return all(full[key] == selected[key] for key in ("text", "sender", "timeText"))

    size = len(part)
    return {rows[start+size-1 if last else start]["rowIndex"] for start in range(len(rows)-size+1)
            if size and all(matches(full, selected) for full, selected in zip(rows[start:start+size], part))}


def viewport_key(frame):
    if frame.width < 24 or frame.height < 8:
        raise Blocked("capture_geometry_invalid")
    return sha256(frame.crop(1, 1, frame.width-15, frame.height-1).data).digest()


def selected_ranges(clean, selected):
    """Read the selection bands KakaoTalk paints across each selected row."""
    if (clean.width, clean.height) != (selected.width, selected.height):
        return []
    bands = []
    for y in range(1, clean.height-1):
        darkened = 0
        for x in range(8, clean.width-17, 4):
            before, after = clean.pixel(x, y), selected.pixel(x, y)
            darkened += sum(before) - sum(after) > 50
        if darkened < 7:
            continue
        if bands and y-bands[-1][1] <= 2:
            bands[-1] = (bands[-1][0], y+1)
        else:
            bands.append((y, y+1))
    return [band for band in bands if band[1]-band[0] >= 12]


def selection_overlay(frame):
    """Recognize the app's dimmed chat background inside selected rows."""
    bands = []
    for y in range(frame.height):
        background = frame.pixel(3, y)
        if max(background) < 28:
            continue
        expected = tuple(round(channel * 0.682) for channel in background)
        matches = sum(all(abs(actual-target) <= 2 for actual, target in zip(frame.pixel(x, y), expected))
                      for x in range(12, frame.width-20, 3))
        if matches < 6:
            continue
        if bands and y-bands[-1][1] <= 2:
            bands[-1] = (bands[-1][0], y+1)
        else:
            bands.append((y, y+1))
    return [band for band in bands if band[1]-band[0] >= 12]


def scrollbar_edge(frame, dpi):
    """Identify a viewport anchored to the top or bottom of Kakao's scrollbar."""
    scale = dpi / 96
    margin = max(3, round(4 * scale))
    column = frame.width-round(8 * scale)
    if column <= 4:
        return None
    thumb = []
    for y in range(frame.height):
        background = frame.pixel(3, y)
        shade = frame.pixel(column, y)
        if all(12 <= base-value <= 100 for base, value in zip(background, shade)):
            thumb.append(y)
    if len(thumb) < round(20 * scale) or thumb[-1]-thumb[0]+1 != len(thumb):
        return None
    if thumb[0] <= margin and thumb[-1] < frame.height-margin-1:
        return "top"
    if thumb[-1] >= frame.height-margin-1 and thumb[0] > margin:
        return "bottom"
    return None


class PhotoLocator:
    def __init__(self, native, chat, guard, rows, target):
        self.native = native
        self.api = native.api
        self.chat = chat
        self.guard = guard
        self.rows = rows
        self.target = target
        self.wheel_steps = 0

    def view(self):
        return self.api.capture(self.chat.history)

    def clean_view(self):
        frame = self.view()
        if not selection_overlay(frame):
            return frame
        self.native.after_copy(self.chat, self.guard)
        frame = self.view()
        if not selection_overlay(frame):
            return frame
        self.guard.click(self.chat.history, 5, frame.height-8)
        frame = until(lambda: pic if not selection_overlay(pic := self.view()) else None, 0.4)
        if not frame:
            raise Blocked("attachment_selection_not_cleared")
        return frame

    def selected(self, frame, top, bottom, point, box, *, last, single=False):
        self.guard.focus(self.chat.history)
        clean = self.clean_view()
        if viewport_key(clean) != viewport_key(frame):
            raise Blocked("attachment_view_changed")
        try:
            self.guard.drag(self.chat.history, (5, top), (frame.width-20, bottom), via=(point,))
            bands = selected_ranges(clean, self.view())
            if not bands or (single and len(bands) != 1):
                return ""
            marked_top, marked_bottom = bands[-1 if last else 0]
            if not (marked_top-3 <= box[1] and box[3] <= marked_bottom+3):
                return ""
            return self.native.copied(self.chat, self.guard, select_all=False)
        except Blocked as error:
            if str(error) != "clipboard_copy_not_observed":
                raise
            return ""
        finally:
            self.native.after_copy(self.chat, self.guard)

    def in_view(self, frame):
        evidence = []
        dpi = self.api.user.GetDpiForWindow(self.chat.hwnd) or 96
        boxes = photo_boxes(frame, dpi)
        if not boxes:
            return None
        index = self.target["rowIndex"]
        if hasattr(self.native, "arm_history"):
            self.native.arm_history(self.chat, self.guard)
        else:
            self.guard.focus(self.chat.history)
        self.guard.keys(self.chat.history, 65, control=True)
        try:
            time.sleep(0.13)
            bands = selected_ranges(frame, self.view())
        finally:
            self.native.after_copy(self.chat, self.guard)
        # A full view or a viewport at a verified scrollbar edge can align
        # its selected bands with the clipboard's chronological order.
        offset = None
        if bands and len(bands) <= len(self.rows):
            edge = scrollbar_edge(frame, dpi)
            if bands[0][0] > 3 and (edge == "top" or
                    (len(bands) == len(self.rows) and bands[-1][1] < frame.height-3)):
                offset = 0
            elif edge == "bottom" and bands[-1][1] < frame.height-3:
                offset = len(self.rows)-len(bands)
        if offset is not None and 0 <= index-offset < len(bands):
            top, bottom = bands[index-offset]
            inside = [box for box in boxes if top-3 <= box[1] and box[3] <= bottom+3]
            if len(inside) == 1:
                return frame.crop(*inside[0])
        for box in boxes:
            x, top, right, bottom = box
            middle = (top+bottom)//2
            point = ((x+right)//2, middle)
            selected = self.selected(frame, max(4, top-5), min(frame.height-5, bottom+4), point,
                                     box, last=True, single=True)
            possibilities = candidate_rows(self.rows, selected, last=True)
            if index not in possibilities:
                continue
            # Extend into adjacent rows when the single-message clipboard
            # omits author/time, or several photos share the same marker.
            if len(possibilities) > 1 and top > 10:
                prefix = self.selected(frame, 5, middle, point, box, last=True)
                possibilities &= candidate_rows(self.rows, prefix, last=True) or possibilities
            if len(possibilities) > 1 and bottom < frame.height-10:
                suffix = self.selected(frame, middle, frame.height-5, point, box, last=False)
                possibilities &= candidate_rows(self.rows, suffix) or possibilities
            if possibilities == {index}:
                evidence.append(box)
        if len(evidence) > 1:
            raise Blocked("attachment_position_ambiguous")
        return frame.crop(*evidence[0]) if evidence else None

    def wheel(self, direction):
        self.guard.check()
        self.api.wheel(self.chat.history, 120*direction)
        self.wheel_steps += direction
        time.sleep(0.14)
        return self.clean_view()

    def locate(self):
        frame = self.clean_view()
        initial = viewport_key(frame)
        image = None
        restored = True
        try:
            image = self.in_view(frame)
            if image:
                return image, restored
            seen = {initial}
            # Walk both sides of the original viewport. Matching is constrained
            # to the exact snapshot the caller supplied.
            for direction in (1, -1):
                for _ in range(16):
                    old = viewport_key(frame)
                    frame = self.wheel(direction)
                    current = viewport_key(frame)
                    if current == old:
                        self.wheel_steps -= direction
                        break
                    if current not in seen:
                        seen.add(current)
                        image = self.in_view(frame)
                        if image:
                            break
                if image:
                    break
                if direction == 1:
                    self.restore(initial)
                    frame = self.clean_view()
        finally:
            restored = self.restore(initial)
        if not image:
            raise Blocked("attachment_position_not_verified")
        return image, restored

    def restore(self, initial):
        if not self.wheel_steps:
            return True
        for _ in range(abs(self.wheel_steps)):
            step = -1 if self.wheel_steps > 0 else 1
            if viewport_key(self.wheel(step)) == initial:
                self.wheel_steps = 0
                return True
        return viewport_key(self.clean_view()) == initial
