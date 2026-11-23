"""KakaoTalk Unicode clipboard records and the visible chat-room list."""

import re

from kakaotalk_win32_ocr import Line


TIME = r"(?:(?:오전|오후|AM|PM) )?\d{1,2}:\d{2}(?: (?:AM|PM))?"
MESSAGE = re.compile(r"^\[(?P<sender>.+?)\] \[(?P<time>" + TIME + r")\](?: (?P<body>[\s\S]*))?$",
                     re.IGNORECASE)
DATE = re.compile(
    r"(?:[-─━]+ *)?(?:\d{4}년 \d{1,2}월 \d{1,2}일(?: [월화수목금토일]요일)?|"
    r"(?:(?:Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday),? )?"
    r"(?:January|February|March|April|May|June|July|August|September|October|November|December) \d{1,2}, \d{4}|"
    r"\d{4}[./-] *\d{1,2}[./-] *\d{1,2}\.?)(?: *[-─━]+)?", re.IGNORECASE)
ATTACHMENTS = {"사진": "image", "Photo": "image", "Image": "image",
               "동영상": "video", "Video": "video", "이모티콘": "emoticon", "Emoticon": "emoticon",
               "음성메시지": "audio", "Voice Message": "audio", "파일": "file", "File": "file"}
SYSTEM = re.compile(r"(?:님이 (?:들어왔습니다|나갔습니다|초대했습니다)\.|"
                    r"삭제된 메시지입니다\.|This message was deleted\.)$")


def transcript_rows(text):
    # KakaoTalk uses CRLF between records. A line break inside a body is LF,
    # even when the next line looks exactly like a separate message or date.
    separator = "\r\n" if "\r\n" in text else "\n"
    records = text.split(separator)
    if records[-1] == "":
        records.pop()
    date = None
    rows = []
    for line in records:
        match = MESSAGE.fullmatch(line)
        is_date = DATE.fullmatch(line) is not None
        is_system = bool(SYSTEM.search(line))
        if not match and not is_date and not is_system and separator == "\n" and rows and rows[-1]["sender"]:
            rows[-1]["text"] = (rows[-1]["text"] or "") + "\n" + line
            continue
        if is_date:
            date = line
        body = match.group("body") or "" if match else line
        rows.append({"rowIndex": len(rows), "kind": "text" if match else "date" if is_date else "unclassified",
                     "sender": match.group("sender") if match else None,
                     "timeText": match.group("time") if match else None, "dateText": date, "text": body,
                     "attachments": [], "links": [], "axParts": []})
    for row in rows:
        body = row["text"]
        attachment = ATTACHMENTS.get(body)
        if attachment is None and (body.startswith(("파일: ", "File: ")) or re.fullmatch(r"사진 \d+장", body)):
            attachment = "file" if ":" in body else "image"
        if attachment and row["kind"] == "text":
            row["kind"] = "attachment"
            row["attachments"] = [{"type": attachment, "description": body, "title": None,
                                   "textSource": "clipboard_marker"}]
    return rows


def badge_present(frame, top, bottom, dpi):
    # Badge pixels are warm red and isolated from the left avatar/content.
    scale = dpi / 96
    left = max(0, frame.width - round(57 * scale))
    right = max(left, frame.width - round(13 * scale))
    occupied = 0
    for y in range(top, bottom):
        for x in range(left, right):
            b, g, r = frame.pixel(x, y)
            occupied += r >= 205 and r >= g + 65 and r >= b + 55
    return occupied >= round(14 * scale * scale)


def self_badge_present(frame, top, bottom, dpi):
    """Detect KakaoTalk's slate-grey circular '나' badge before the room title."""
    scale = dpi / 96
    left, right = round(79 * scale), min(frame.width, round(96 * scale))
    upper, lower = max(top, top + round(18 * scale)), min(bottom, top + round(35 * scale))
    if right <= left or lower <= upper:
        return False
    slate = 0
    for y in range(upper, lower):
        for x in range(left, right):
            b, g, r = frame.pixel(x, y)
            slate += (55 <= r <= 125 and 60 <= g <= 130 and 65 <= b <= 140
                      and b >= r and max(r, g, b) - min(r, g, b) <= 28)
    if slate < round(80 * scale * scale):
        return False
    core_left, core_right = round(83 * scale), min(frame.width, round(92 * scale))
    core_top, core_bottom = max(top, top + round(22 * scale)), min(bottom, top + round(30 * scale))
    glyph = sum(all(channel >= 230 for channel in frame.pixel(x, y))
                for y in range(core_top, core_bottom) for x in range(core_left, core_right))
    return glyph >= max(4, round(6 * scale * scale))


def room_items(frame, recognize, dpi=96):
    """Split visible rows using the avatar column, then OCR each row enlarged."""
    scale = dpi / 96
    expected_height = round(70 * scale)
    anchor_left, anchor_right = round(17 * scale), round(63 * scale)
    text_left = round(78 * scale)
    if frame.width <= text_left + round(45 * scale):
        return []
    # The vertical extent of an avatar also works when the first row is clipped.
    sampled = []
    for y in range(frame.height):
        background = frame.pixel(round(5 * scale), y)
        count = sum(max(abs(a-b) for a, b in zip(frame.pixel(x, y), background)) > 18
                    for x in range(anchor_left, anchor_right, 2))
        sampled.append(count >= max(4, round(5 * scale)))
    runs = []
    y = 0
    while y < frame.height:
        if not sampled[y]:
            y += 1
            continue
        start = y
        while y < frame.height and sampled[y]:
            y += 1
        if runs and start - runs[-1][1] < 6 * scale:
            runs[-1] = (runs[-1][0], y)
        else:
            runs.append((start, y))
    avatars = [(a, b) for a, b in runs if b-a >= 23 * scale]
    if not avatars:
        # A uniform or empty search response has no selectable rows.
        return []
    first_mid = (avatars[0][0] + avatars[0][1]) // 2
    origin = max(0, first_mid - expected_height // 2)
    # Keep actual measured centers for hit testing. Gaps between avatars can
    # include section headings or service banners and aren't synthetic rows.
    items = []
    for index, (a, b) in enumerate(avatars):
        midpoint = (a+b) // 2
        top = max(0, origin if index == 0 else midpoint - expected_height // 2)
        bottom = min(frame.height, midpoint + expected_height // 2, top + expected_height)
        right = max(text_left + 1, frame.width - round(14 * scale))
        is_self = self_badge_present(frame, top, bottom, dpi)
        cropped = frame.crop(text_left, top, right, bottom)
        if is_self:
            bg = frame.pixel(round(5 * scale), min(frame.height - 1, top + round(8 * scale)))
            cropped = cropped.fill(0, round(17 * scale), round(19 * scale), round(36 * scale), bg)
        lines = [Line(line.text, line.x/2 + text_left, line.y/2, line.width/2, line.height/2)
                 for line in recognize(cropped.scale(2))]
        # At DPI 96 the time/right accessory column starts near x=w-75.
        text_right = max(text_left + round(50 * scale), right - round(55 * scale))
        lines = sorted((line for line in lines if line.x < text_right), key=lambda l: (l.y, l.x))
        if not lines or lines[0].y > 30 * scale:
            continue
        name = lines[0].text
        if is_self and name[:2] in ("0 ", "O ", "ㅇ "):
            name = name[2:].lstrip() or name
        preview = "\n".join(line.text for line in lines[1:] if line.y >= lines[0].y + 10 * scale)
        items.append({"rowIndex": index, "name": name, "preview": preview or None,
                      "unread": badge_present(frame, top, bottom, dpi), "isSelf": True if is_self else None,
                      "textExact": False, "source": "windows_ocr", "_point": (round(46 * scale), midpoint)})
    return items


def blank_chat(frame):
    """Recognize an empty, solid transcript viewport without OCR."""
    if frame.width < 40 or frame.height < 40:
        return False
    # Skip the scrollbar and single-pixel edges drawn by the list control.
    color = frame.pixel(4, 4)
    left, right = 4, frame.width - 18
    expected = bytes(color) * (right - left)
    return all(frame.data[(y*frame.width + left)*4:(y*frame.width + right)*4:4] == expected[0::3]
               and frame.data[(y*frame.width + left)*4+1:(y*frame.width + right)*4:4] == expected[1::3]
               and frame.data[(y*frame.width + left)*4+2:(y*frame.width + right)*4:4] == expected[2::3]
               for y in range(4, frame.height - 4))
