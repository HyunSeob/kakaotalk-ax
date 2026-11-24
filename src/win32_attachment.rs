//! Locate a photo in a Windows KakaoTalk chat using rendered bounds and exact clipboard evidence.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{KaxError, Result};
use crate::win32_api::{until, Win32Api};
use crate::win32_clipboard::ForegroundGuard;
use crate::win32_image::Frame;
use crate::win32_rows::transcript_rows;

/// Find compact rectangular foreground regions against the chat background.
pub fn photo_boxes(frame: &Frame, dpi: u32) -> Vec<(usize, usize, usize, usize)> {
    let scale = dpi as f64 / 96.0;
    let minimum = (54.0 * scale).round() as usize;
    let tolerance = (7.0 * scale).round() as usize;
    let gap = ((3.0 * scale).round() as usize).max(2);
    let left = (9.0 * scale).round() as usize;
    let right = frame.width.saturating_sub((22.0 * scale).round() as usize);
    if right <= left || right - left < minimum {
        return Vec::new();
    }

    // Track: [x1, y1, x2, y2, hit_rows]
    let mut tracks: Vec<[usize; 5]> = Vec::new();
    let mut finished: Vec<[usize; 5]> = Vec::new();

    for y in 1..frame.height.saturating_sub(1) {
        let (b, g, r) = frame.pixel(3, y);
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut start: Option<usize> = None;
        let mut last: Option<usize> = None;

        for x in left..right {
            let (bb, gg, rr) = frame.pixel(x, y);
            let diff = (b as i32 - bb as i32)
                .abs()
                .max((g as i32 - gg as i32).abs())
                .max((r as i32 - rr as i32).abs());
            if diff >= 25 {
                if last.is_none() || x - last.unwrap() > gap {
                    if let (Some(s), Some(l)) = (start, last) {
                        if l + 1 - s >= minimum {
                            runs.push((s, l + 1));
                        }
                    }
                    start = Some(x);
                }
                last = Some(x);
            }
        }
        if let (Some(s), Some(l)) = (start, last) {
            if l + 1 - s >= minimum {
                runs.push((s, l + 1));
            }
        }

        for (r_start, r_end) in runs {
            let existing = tracks.iter_mut().find(|t| {
                y.saturating_sub(t[3]) <= tolerance
                    && t[0].abs_diff(r_start) <= tolerance
                    && t[2].abs_diff(r_end) <= tolerance
            });
            if let Some(t) = existing {
                t[0] = t[0].min(r_start);
                t[2] = t[2].max(r_end);
                t[3] = y + 1;
                t[4] += 1;
            } else {
                tracks.push([r_start, y, r_end, y + 1, 1]);
            }
        }

        let mut remaining = Vec::with_capacity(tracks.len());
        for t in tracks {
            if y.saturating_sub(t[3]) > tolerance {
                finished.push(t);
            } else {
                remaining.push(t);
            }
        }
        tracks = remaining;
    }
    finished.extend(tracks);

    let min_height = (40.0 * scale).round() as usize;
    finished
        .into_iter()
        .filter(|t| {
            let h = t[3].saturating_sub(t[1]);
            h >= min_height
                && (t[4] as f64) >= (h as f64) * 0.76
                && t[1] > 2
                && t[3] < frame.height.saturating_sub(2)
        })
        .map(|t| (t[0], t[1], t[2], t[3]))
        .collect()
}

pub fn candidate_rows(rows: &[Value], copied: &str, last: bool) -> HashSet<usize> {
    if copied.is_empty() {
        return HashSet::new();
    }
    let part = transcript_rows(copied);
    let size = part.len();
    if size == 0 || size > rows.len() {
        return HashSet::new();
    }
    let single_plain = size == 1
        && part[0].get("sender").and_then(|v| v.as_str()).is_none()
        && part[0].get("kind").and_then(|v| v.as_str()) != Some("date");

    let mut result = HashSet::new();
    for start in 0..=(rows.len() - size) {
        let all_match = rows[start..start + size]
            .iter()
            .zip(part.iter())
            .all(|(full, selected)| {
                if single_plain {
                    full.get("text").and_then(|v| v.as_str()) == Some(copied)
                } else {
                    full.get("text") == selected.get("text")
                        && full.get("sender") == selected.get("sender")
                        && full.get("timeText") == selected.get("timeText")
                }
            });
        if all_match {
            let idx = if last { start + size - 1 } else { start };
            if let Some(ri) = rows[idx].get("rowIndex").and_then(|v| v.as_u64()) {
                result.insert(ri as usize);
            }
        }
    }
    result
}

pub fn viewport_key(frame: &Frame) -> Result<[u8; 32]> {
    if frame.width < 24 || frame.height < 8 {
        return Err(KaxError::blocked("capture_geometry_invalid"));
    }
    let cropped = frame.crop(1, 1, frame.width - 15, frame.height - 1)?;
    let mut hasher = Sha256::new();
    hasher.update(&cropped.data);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

/// Viewport hash excluding the top floating date-pill band (y < 40 * scale) and scrollbar,
/// so fading date pills after search/scroll do not falsely report `scrollRestored: false`.
pub fn stable_viewport_key(frame: &Frame, dpi: u32) -> Result<[u8; 32]> {
    if frame.width < 24 || frame.height < 8 {
        return Err(KaxError::blocked("capture_geometry_invalid"));
    }
    let scale = (if dpi == 0 { 96 } else { dpi }) as f64 / 96.0;
    let top_cut = (40.0 * scale).round() as usize;
    let y_start = if frame.height > top_cut + 24 { top_cut } else { 1 };
    let cropped = frame.crop(1, y_start, frame.width - 15, frame.height - 1)?;
    let mut hasher = Sha256::new();
    hasher.update(&cropped.data);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

pub fn selected_ranges(clean: &Frame, selected: &Frame) -> Vec<(usize, usize)> {
    if (clean.width, clean.height) != (selected.width, selected.height) {
        return Vec::new();
    }
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for y in 1..clean.height.saturating_sub(1) {
        let mut darkened = 0usize;
        let mut x = 8usize;
        while x < clean.width.saturating_sub(17) {
            let (b1, g1, r1) = clean.pixel(x, y);
            let (b2, g2, r2) = selected.pixel(x, y);
            let sum1 = b1 as i32 + g1 as i32 + r1 as i32;
            let sum2 = b2 as i32 + g2 as i32 + r2 as i32;
            if sum1 - sum2 > 50 {
                darkened += 1;
            }
            x += 4;
        }
        if darkened < 7 {
            continue;
        }
        if let Some(last) = bands.last_mut() {
            if y.saturating_sub(last.1) <= 2 {
                last.1 = y + 1;
                continue;
            }
        }
        bands.push((y, y + 1));
    }
    bands.into_iter().filter(|(a, b)| b - a >= 12).collect()
}

pub fn selection_overlay(frame: &Frame) -> Vec<(usize, usize)> {
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for y in 0..frame.height {
        let bg = frame.pixel(3, y);
        if bg.0.max(bg.1).max(bg.2) < 28 {
            continue;
        }
        let exp = (
            (bg.0 as f64 * 0.682).round() as i32,
            (bg.1 as f64 * 0.682).round() as i32,
            (bg.2 as f64 * 0.682).round() as i32,
        );
        let mut matches = 0usize;
        let mut x = 12usize;
        while x < frame.width.saturating_sub(20) {
            let p = frame.pixel(x, y);
            if (p.0 as i32 - exp.0).abs() <= 2
                && (p.1 as i32 - exp.1).abs() <= 2
                && (p.2 as i32 - exp.2).abs() <= 2
            {
                matches += 1;
            }
            x += 3;
        }
        if matches < 6 {
            continue;
        }
        if let Some(last) = bands.last_mut() {
            if y.saturating_sub(last.1) <= 2 {
                last.1 = y + 1;
                continue;
            }
        }
        bands.push((y, y + 1));
    }
    bands.into_iter().filter(|(a, b)| b - a >= 12).collect()
}

pub fn scrollbar_edge(frame: &Frame, dpi: u32) -> Option<&'static str> {
    let scale = dpi as f64 / 96.0;
    let margin = ((4.0 * scale).round() as usize).max(3);
    let column = frame.width.saturating_sub((8.0 * scale).round() as usize);
    if column <= 4 || column >= frame.width {
        return None;
    }
    let mut thumb = Vec::new();
    for y in 0..frame.height {
        let bg = frame.pixel(3, y);
        let shade = frame.pixel(column, y);
        let d0 = bg.0 as i32 - shade.0 as i32;
        let d1 = bg.1 as i32 - shade.1 as i32;
        let d2 = bg.2 as i32 - shade.2 as i32;
        if (12..=100).contains(&d0) && (12..=100).contains(&d1) && (12..=100).contains(&d2) {
            thumb.push(y);
        }
    }
    if thumb.len() < (20.0 * scale).round() as usize
        || thumb[thumb.len() - 1] - thumb[0] + 1 != thumb.len()
    {
        return None;
    }
    if thumb[0] <= margin && thumb[thumb.len() - 1] < frame.height.saturating_sub(margin + 1) {
        return Some("top");
    }
    if thumb[thumb.len() - 1] >= frame.height.saturating_sub(margin + 1) && thumb[0] > margin {
        return Some("bottom");
    }
    None
}

/// Callbacks required by `PhotoLocator` to interact with the chat history list.
pub trait PhotoLocatorHost<A: Win32Api> {
    fn api(&self) -> &A;
    fn chat_hwnd(&self) -> usize;
    fn history_hwnd(&self) -> usize;
    fn arm_history(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<()>;
    fn after_copy(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<()>;
    fn copied_selection(&self, guard: &mut ForegroundGuard<'_, A>) -> Result<String>;
}

pub struct PhotoLocator<'a, 'b, 'g, A: Win32Api, H: PhotoLocatorHost<A>> {
    host: &'a H,
    guard: &'b mut ForegroundGuard<'g, A>,
    rows: &'a [Value],
    target_row_index: usize,
    wheel_steps: i32,
}

impl<'a, 'b, 'g, A: Win32Api, H: PhotoLocatorHost<A>> PhotoLocator<'a, 'b, 'g, A, H> {
    pub fn new(
        host: &'a H,
        guard: &'b mut ForegroundGuard<'g, A>,
        rows: &'a [Value],
        target_row_index: usize,
    ) -> Self {
        Self {
            host,
            guard,
            rows,
            target_row_index,
            wheel_steps: 0,
        }
    }

    fn view(&self) -> Result<Frame> {
        self.host.api().capture(self.host.history_hwnd())
    }

    fn clean_view(&mut self) -> Result<Frame> {
        let frame = self.view()?;
        if selection_overlay(&frame).is_empty() {
            return Ok(frame);
        }
        self.host.after_copy(self.guard)?;
        let frame = self.view()?;
        if selection_overlay(&frame).is_empty() {
            return Ok(frame);
        }
        self.guard
            .click(self.host.history_hwnd(), 5, frame.height as i32 - 8)?;
        let found = until(
            || {
                let pic = self.view().ok()?;
                if selection_overlay(&pic).is_empty() {
                    Some(pic)
                } else {
                    None
                }
            },
            0.4,
            0.025,
        );
        found.ok_or_else(|| KaxError::blocked("attachment_selection_not_cleared"))
    }

    #[allow(clippy::too_many_arguments)]
    fn selected(
        &mut self,
        frame: &Frame,
        top: i32,
        bottom: i32,
        point: (i32, i32),
        bx: (usize, usize, usize, usize),
        last: bool,
        single: bool,
    ) -> Result<String> {
        let history = self.host.history_hwnd();
        self.guard.focus(history)?;
        let expected_crop = frame.crop(bx.0, bx.1, bx.2, bx.3)?;
        let dpi = self.host.api().get_dpi_for_window(self.host.chat_hwnd());
        let dpi = if dpi == 0 { 96 } else { dpi };
        let clean = until(
            || {
                let c = self.clean_view().ok()?;
                if c.width != frame.width || c.height != frame.height {
                    return None;
                }
                let actual_crop = c.crop(bx.0, bx.1, bx.2, bx.3).ok()?;
                if actual_crop == expected_crop && photo_boxes(&c, dpi).contains(&bx) {
                    Some(c)
                } else {
                    None
                }
            },
            0.35,
            0.04,
        )
        .ok_or_else(|| KaxError::blocked("attachment_view_changed"))?;
        let outcome = (|| -> Result<String> {
            self.guard.drag(
                history,
                (5, top),
                (frame.width as i32 - 20, bottom),
                &[point],
            )?;
            let bands = selected_ranges(&clean, &self.view()?);
            if bands.is_empty() || (single && bands.len() != 1) {
                return Ok(String::new());
            }
            let (marked_top, marked_bottom) = if last {
                bands[bands.len() - 1]
            } else {
                bands[0]
            };
            if !(marked_top.saturating_sub(3) <= bx.1 && bx.3 <= marked_bottom + 3) {
                return Ok(String::new());
            }
            match self.host.copied_selection(self.guard) {
                Ok(text) => Ok(text),
                Err(KaxError::Blocked(ref r)) if r == "clipboard_copy_not_observed" => {
                    Ok(String::new())
                }
                Err(e) => Err(e),
            }
        })();
        let _ = self.host.after_copy(self.guard);
        outcome
    }

    fn in_view(&mut self, frame: &Frame) -> Result<Option<Frame>> {
        let dpi = self.host.api().get_dpi_for_window(self.host.chat_hwnd());
        let dpi = if dpi == 0 { 96 } else { dpi };
        let boxes = photo_boxes(frame, dpi);
        if boxes.is_empty() {
            return Ok(None);
        }
        let index = self.target_row_index;
        let history = self.host.history_hwnd();
        self.host.arm_history(self.guard)?;
        self.guard.keys(history, &[65], true)?;
        let bands = {
            std::thread::sleep(Duration::from_millis(130));
            let res = self.view().map(|v| selected_ranges(frame, &v));
            let _ = self.host.after_copy(self.guard);
            res?
        };

        let mut offset: Option<usize> = None;
        if !bands.is_empty() && bands.len() <= self.rows.len() {
            let edge = scrollbar_edge(frame, dpi);
            if bands[0].0 > 3
                && (edge == Some("top")
                    || (bands.len() == self.rows.len()
                        && bands[bands.len() - 1].1 < frame.height.saturating_sub(3)))
            {
                offset = Some(0);
            } else if edge == Some("bottom")
                && bands[bands.len() - 1].1 < frame.height.saturating_sub(3)
            {
                offset = Some(self.rows.len() - bands.len());
            }
        }
        if let Some(off) = offset {
            if index >= off && index - off < bands.len() {
                let (top, bottom) = bands[index - off];
                let inside: Vec<_> = boxes
                    .iter()
                    .copied()
                    .filter(|b| top.saturating_sub(3) <= b.1 && b.3 <= bottom + 3)
                    .collect();
                if inside.len() == 1 {
                    let b = inside[0];
                    return Ok(Some(frame.crop(b.0, b.1, b.2, b.3)?));
                }
            }
        }

        // Refresh the clean frame after Ctrl+A + after_copy so transient hover
        // buttons or fading date pills from before Ctrl+A do not trigger a
        // spurious `attachment_view_changed`, and re-read boxes from it.
        let base = self.clean_view()?;
        let boxes = photo_boxes(&base, dpi);
        let mut evidence = Vec::new();
        for bx in boxes {
            let (x, top, right, bottom) = bx;
            let middle = ((top + bottom) / 2) as i32;
            let point = (((x + right) / 2) as i32, middle);
            let sel_top = (top as i32 - 5).max(4);
            let sel_bot = (bottom as i32 + 4).min(base.height as i32 - 5);
            let sel = self.selected(&base, sel_top, sel_bot, point, bx, true, true)?;
            let mut possibilities = candidate_rows(self.rows, &sel, true);
            if !possibilities.contains(&index) {
                continue;
            }
            if possibilities.len() > 1 && top > 10 {
                let prefix = self.selected(&base, 5, middle, point, bx, true, false)?;
                let pref_set = candidate_rows(self.rows, &prefix, true);
                if !pref_set.is_empty() {
                    possibilities = possibilities.intersection(&pref_set).copied().collect();
                }
            }
            if possibilities.len() > 1 && bottom < base.height.saturating_sub(10) {
                let suffix =
                    self.selected(&base, middle, base.height as i32 - 5, point, bx, false, false)?;
                let suf_set = candidate_rows(self.rows, &suffix, false);
                if !suf_set.is_empty() {
                    possibilities = possibilities.intersection(&suf_set).copied().collect();
                }
            }
            if possibilities.len() == 1 && possibilities.contains(&index) {
                evidence.push(bx);
            }
        }
        if evidence.len() > 1 {
            return Err(KaxError::blocked("attachment_position_ambiguous"));
        }
        if let Some(b) = evidence.first().copied() {
            Ok(Some(base.crop(b.0, b.1, b.2, b.3)?))
        } else {
            Ok(None)
        }
    }

    fn wheel(&mut self, direction: i32) -> Result<Frame> {
        self.guard.check()?;
        self.host
            .api()
            .wheel(self.host.history_hwnd(), 120 * direction)?;
        self.wheel_steps += direction;
        std::thread::sleep(Duration::from_millis(140));
        self.clean_view()
    }

    fn restore(&mut self, initial: [u8; 32], initial_stable: [u8; 32], dpi: u32) -> Result<bool> {
        if self.wheel_steps == 0 {
            return Ok(true);
        }
        let steps = self.wheel_steps.unsigned_abs();
        for _ in 0..steps {
            let step = if self.wheel_steps > 0 { -1 } else { 1 };
            let f = self.wheel(step)?;
            if viewport_key(&f)? == initial || stable_viewport_key(&f, dpi)? == initial_stable {
                self.wheel_steps = 0;
                return Ok(true);
            }
        }
        let f = self.clean_view()?;
        Ok(viewport_key(&f)? == initial || stable_viewport_key(&f, dpi)? == initial_stable)
    }

    pub fn locate(&mut self) -> Result<(Frame, bool)> {
        let dpi = self.host.api().get_dpi_for_window(self.host.chat_hwnd());
        let mut frame = self.clean_view()?;
        let initial = viewport_key(&frame)?;
        let initial_stable = stable_viewport_key(&frame, dpi)?;
        let mut image = self.in_view(&frame)?;
        if let Some(img) = image {
            return Ok((img, true));
        }

        let mut seen = HashSet::new();
        seen.insert(initial);
        let search_res: Result<Option<Frame>> = (|| {
            for direction in [1i32, -1] {
                for _ in 0..16 {
                    let old = viewport_key(&frame)?;
                    frame = self.wheel(direction)?;
                    let current = viewport_key(&frame)?;
                    if current == old {
                        self.wheel_steps -= direction;
                        break;
                    }
                    if seen.insert(current) {
                        if let Some(found) = self.in_view(&frame)? {
                            return Ok(Some(found));
                        }
                    }
                }
                if direction == 1 {
                    let _ = self.restore(initial, initial_stable, dpi)?;
                    frame = self.clean_view()?;
                }
            }
            Ok(None)
        })();

        let restored = self.restore(initial, initial_stable, dpi).unwrap_or(false);
        image = search_res?;
        let Some(img) = image else {
            return Err(KaxError::blocked("attachment_position_not_verified"));
        };
        Ok((img, restored))
    }
}
