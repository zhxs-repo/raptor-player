//! Track-based collision avoidance layout engine.
//! Inspired by Next2's track + compaction approach for correct pre-computed layout.
//!
//! Key design: per-type track arrays storing lightweight collision records,
//! compact expired items before each placement, assign to first non-colliding track,
//! compute Y from track index.

use crate::dfm_core::model::{DanmakuItem, DanmakuType, GlobalFlags};

type DisplacedIndices = Vec<usize>;

/// Lightweight record stored in tracks for collision detection.
#[derive(Debug, Clone)]
struct TrackEntry {
    time_ms: i64,
    duration_ms: i64,
    paint_width: f32,
    step_x: f32,
    danmaku_type: DanmakuType,
    danmaku_index: usize,
}

impl TrackEntry {
    fn from_item(item: &DanmakuItem, index: usize) -> Self {
        Self {
            time_ms: item.time_ms,
            duration_ms: item.duration_ms,
            paint_width: item.paint_width,
            step_x: item.step_x,
            danmaku_type: item.danmaku_type,
            danmaku_index: index,
        }
    }

    fn end_ms(&self) -> i64 {
        self.time_ms + self.duration_ms
    }
}

#[derive(Debug, Clone)]
struct TrackData {
    tracks: Vec<Vec<TrackEntry>>,
    last_compact_ms: i64,
}

impl TrackData {
    fn new() -> Self {
        Self {
            tracks: Vec::new(),
            last_compact_ms: i64::MIN,
        }
    }

    fn ensure_track_count(&mut self, count: usize) {
        if self.tracks.len() != count {
            self.tracks.resize_with(count, Vec::new);
        }
    }

    fn compact(&mut self, current_time_ms: i64, _current_duration_ms: i64) {
        if current_time_ms == self.last_compact_ms {
            return;
        }
        self.last_compact_ms = current_time_ms;
        for track in self.tracks.iter_mut() {
            track.retain(|existing| current_time_ms < existing.end_ms());
        }
    }

    fn clear(&mut self) {
        self.tracks.clear();
        self.last_compact_ms = i64::MIN;
    }
}

/// Track-based collision avoidance engine.
#[derive(Debug, Clone)]
pub struct DanmakuRetainer {
    r2l_tracks: TrackData,
    lr_tracks: TrackData,
    top_tracks: TrackData,
    bottom_tracks: TrackData,
    margin: f32,
    track_gap_ratio: f32,
}

impl DanmakuRetainer {
    pub fn new(margin: f32, track_gap_ratio: f32) -> Self {
        Self {
            r2l_tracks: TrackData::new(),
            lr_tracks: TrackData::new(),
            top_tracks: TrackData::new(),
            bottom_tracks: TrackData::new(),
            margin,
            track_gap_ratio,
        }
    }

    pub fn clear(&mut self) {
        self.r2l_tracks.clear();
        self.lr_tracks.clear();
        self.top_tracks.clear();
        self.bottom_tracks.clear();
    }

    /// Assign a Y position to a danmaku item using track-based collision avoidance.
    /// Returns true if a position was found, false if the item should be dropped.
    /// Returns indices of any displaced danmaku that should be marked as filtered.
    pub fn fix(
        &mut self,
        item: &mut DanmakuItem,
        view_width: f32,
        view_height: f32,
        flags: &GlobalFlags,
        display_area: f32,
        is_me: bool,
    ) -> (bool, DisplacedIndices) {
        self.fix_with_options(
            item,
            view_width,
            view_height,
            flags,
            display_area,
            is_me,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fix_with_options(
        &mut self,
        item: &mut DanmakuItem,
        view_width: f32,
        view_height: f32,
        flags: &GlobalFlags,
        display_area: f32,
        is_me: bool,
        allow_stacking: bool,
        allow_scroll_overwrite: bool,
    ) -> (bool, DisplacedIndices) {
        let effective_height = view_height * display_area;
        let track_height = item.paint_height + item.paint_height * self.track_gap_ratio;
        let track_count = (effective_height / track_height).floor().max(1.0) as usize;
        let danmaku_index = item.index as usize;

        let entry = TrackEntry::from_item(item, danmaku_index);

        match item.danmaku_type {
            DanmakuType::ScrollRL => {
                self.r2l_tracks.ensure_track_count(track_count);
                match select_scroll_track(
                    &entry,
                    &mut self.r2l_tracks,
                    track_count,
                    view_width,
                    is_me,
                    allow_stacking,
                    allow_scroll_overwrite,
                ) {
                    Some((row, displaced)) => {
                        item.y = self.margin + row as f32 * track_height;
                        item.is_shown = true;
                        item.flags.visible = flags.visible_flag;
                        (true, displaced)
                    }
                    None => (false, Vec::new()),
                }
            }
            DanmakuType::ScrollLR => {
                self.lr_tracks.ensure_track_count(track_count);
                match select_scroll_track(
                    &entry,
                    &mut self.lr_tracks,
                    track_count,
                    view_width,
                    is_me,
                    allow_stacking,
                    allow_scroll_overwrite,
                ) {
                    Some((row, displaced)) => {
                        item.y = self.margin + row as f32 * track_height;
                        item.is_shown = true;
                        item.flags.visible = flags.visible_flag;
                        (true, displaced)
                    }
                    None => (false, Vec::new()),
                }
            }
            DanmakuType::FixTop => {
                self.top_tracks.ensure_track_count(track_count);
                match select_fixed_track(&entry, &mut self.top_tracks, track_count) {
                    Some(row) => {
                        item.y = self.margin + row as f32 * track_height;
                        item.is_shown = true;
                        item.flags.visible = flags.visible_flag;
                        (true, Vec::new())
                    }
                    None => (false, Vec::new()),
                }
            }
            DanmakuType::FixBottom => {
                self.bottom_tracks.ensure_track_count(track_count);
                match select_fixed_track(&entry, &mut self.bottom_tracks, track_count) {
                    Some(row) => {
                        item.y = effective_height - (row as f32 + 1.0) * track_height;
                        item.is_shown = true;
                        item.flags.visible = flags.visible_flag;
                        (true, Vec::new())
                    }
                    None => (false, Vec::new()),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Track selection
// ---------------------------------------------------------------------------

/// Select a track for a scroll danmaku.
/// Returns (track_index, displaced_indices) or None if the item should be dropped.
/// When all tracks collide, uses DFM's overwriteInsert strategy: pick the track
/// whose items have the smallest right edge (furthest left), clear it, and place
/// the new danmaku there.
fn select_scroll_track(
    new_entry: &TrackEntry,
    track_data: &mut TrackData,
    track_count: usize,
    view_width: f32,
    is_me: bool,
    allow_stacking: bool,
    allow_scroll_overwrite: bool,
) -> Option<(usize, DisplacedIndices)> {
    track_data.compact(new_entry.time_ms, new_entry.duration_ms);

    if allow_stacking && track_count > 0 {
        let row = stack_track_for(new_entry, track_count);
        track_data.tracks[row].push(new_entry.clone());
        return Some((row, Vec::new()));
    }

    let overwrite_count = ((track_count as f32 * 0.6).ceil() as usize)
        .max(1)
        .min(track_count);
    let overwrite_start = track_count - overwrite_count;

    let mut best_track = overwrite_start;
    let mut min_right_edge = f32::MAX;

    for i in 0..track_count {
        if track_data.tracks[i].is_empty() {
            track_data.tracks[i].push(new_entry.clone());
            return Some((i, Vec::new()));
        }
        let mut collides = false;
        let mut track_min_right = f32::MAX;
        for existing in &track_data.tracks[i] {
            if scroll_entries_collide(new_entry, existing, view_width) {
                collides = true;
            }
            let right_edge = entry_right_edge_at(existing, new_entry.time_ms, view_width);
            if right_edge < track_min_right {
                track_min_right = right_edge;
            }
        }
        if !collides {
            track_data.tracks[i].push(new_entry.clone());
            return Some((i, Vec::new()));
        }
        if i >= overwrite_start && track_min_right < min_right_edge {
            min_right_edge = track_min_right;
            best_track = i;
        }
    }

    if is_me && track_count > 0 {
        let displaced: DisplacedIndices = track_data.tracks[0]
            .iter()
            .map(|e| e.danmaku_index)
            .collect();
        track_data.tracks[0].clear();
        track_data.tracks[0].push(new_entry.clone());
        return Some((0, displaced));
    }

    if !allow_scroll_overwrite {
        return None;
    }

    if min_right_edge < f32::MAX {
        let displaced: DisplacedIndices = track_data.tracks[best_track]
            .iter()
            .map(|e| e.danmaku_index)
            .collect();
        track_data.tracks[best_track].clear();
        track_data.tracks[best_track].push(new_entry.clone());
        return Some((best_track, displaced));
    }

    None
}

fn stack_track_for(entry: &TrackEntry, track_count: usize) -> usize {
    let mut value = entry.danmaku_index.wrapping_mul(1_103_515_245usize);
    value ^= (entry.time_ms.max(0) as usize).rotate_left(11);
    value ^= (entry.paint_width.to_bits() as usize).rotate_left(5);
    value % track_count.max(1)
}

fn entry_right_edge_at(entry: &TrackEntry, time_ms: i64, view_width: f32) -> f32 {
    entry_x_at(entry, time_ms, view_width) + entry.paint_width
}

/// Compact expired entries from fixed tracks.
fn compact_fixed_tracks(tracks: &mut [Vec<TrackEntry>], current_time_ms: i64) {
    for track in tracks.iter_mut() {
        let mut remove_count = 0;
        for entry in track.iter() {
            if entry.end_ms() <= current_time_ms {
                remove_count += 1;
            } else {
                break;
            }
        }
        if remove_count > 0 {
            track.drain(0..remove_count);
        }
    }
}

fn select_fixed_track(
    new_entry: &TrackEntry,
    track_data: &mut TrackData,
    track_count: usize,
) -> Option<usize> {
    let new_start = new_entry.time_ms;

    if new_start != track_data.last_compact_ms {
        track_data.last_compact_ms = new_start;
        compact_fixed_tracks(&mut track_data.tracks, new_start);
    }

    let tracks = &mut track_data.tracks;
    for (i, track) in tracks.iter_mut().enumerate().take(track_count) {
        if track.is_empty() {
            track.push(new_entry.clone());
            return Some(i);
        }
        let last = track.last().unwrap();
        let last_end = last.end_ms();
        if new_start >= last_end {
            track.push(new_entry.clone());
            return Some(i);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Collision detection (ported from DanmakuFlameMaster's DanmakuUtils)
// ---------------------------------------------------------------------------

/// Check if two scroll entries will collide (1:1 port from DFM)
#[inline]
fn scroll_entries_collide(entry_a: &TrackEntry, entry_b: &TrackEntry, view_width: f32) -> bool {
    if entry_a.danmaku_type != entry_b.danmaku_type {
        return false;
    }

    let (d1, d2) = if entry_a.time_ms <= entry_b.time_ms {
        (entry_a, entry_b)
    } else {
        (entry_b, entry_a)
    };

    let d_time = d2.time_ms - d1.time_ms;

    if d_time <= 0 {
        return true;
    }

    if d_time >= d1.duration_ms {
        return false;
    }

    let d1_left_at_d2_start = entry_left_at(d1, d2.time_ms, view_width);
    let d1_right_at_d2_start = d1_left_at_d2_start + d1.paint_width;
    let d2_left_at_start = entry_left_at_start(d2, view_width);

    if check_hit_same_type(
        d1.danmaku_type,
        d1_left_at_d2_start,
        d1_right_at_d2_start,
        d2_left_at_start,
        d2_left_at_start + d2.paint_width,
    ) {
        return true;
    }

    let d1_left_at_d1_end = entry_left_at(d1, d1.end_ms(), view_width);
    let d1_right_at_d1_end = d1_left_at_d1_end + d1.paint_width;
    let d2_left_at_d1_end = entry_left_at(d2, d1.end_ms(), view_width);

    check_hit_same_type(
        d1.danmaku_type,
        d1_left_at_d1_end,
        d1_right_at_d1_end,
        d2_left_at_d1_end,
        d2_left_at_d1_end + d2.paint_width,
    )
}

#[inline]
fn check_hit_same_type(
    danmaku_type: DanmakuType,
    left1: f32,
    right1: f32,
    left2: f32,
    right2: f32,
) -> bool {
    debug_assert!(danmaku_type.is_scroll());
    match danmaku_type {
        DanmakuType::ScrollRL => left2 < right1,
        DanmakuType::ScrollLR => right2 > left1,
        _ => unreachable!("check_hit_same_type called with fixed danmaku"),
    }
}

#[inline]
fn entry_left_at_start(entry: &TrackEntry, view_width: f32) -> f32 {
    match entry.danmaku_type {
        DanmakuType::ScrollRL => view_width,
        DanmakuType::ScrollLR => -entry.paint_width,
        _ => unreachable!("entry_left_at_start called with fixed danmaku"),
    }
}

#[inline]
fn entry_left_at(entry: &TrackEntry, time_ms: i64, view_width: f32) -> f32 {
    if entry.danmaku_type == DanmakuType::ScrollLR {
        return entry_x_at(entry, time_ms, view_width);
    }

    let elapsed = (time_ms - entry.time_ms).max(0) as f32;

    if entry.step_x <= 0.0 {
        return view_width;
    }

    if elapsed >= entry.duration_ms as f32 {
        return -entry.paint_width;
    }

    let pos = view_width - elapsed * entry.step_x;
    pos.max(-entry.paint_width)
}

#[inline]
fn entry_x_at(entry: &TrackEntry, time_ms: i64, view_width: f32) -> f32 {
    let elapsed = (time_ms - entry.time_ms).max(0) as f32;
    if entry.step_x <= 0.0 {
        return match entry.danmaku_type {
            DanmakuType::ScrollRL => view_width,
            DanmakuType::ScrollLR => -entry.paint_width,
            _ => unreachable!("entry_x_at called with fixed danmaku"),
        };
    }
    match entry.danmaku_type {
        DanmakuType::ScrollRL => view_width - elapsed * entry.step_x,
        DanmakuType::ScrollLR => elapsed * entry.step_x - entry.paint_width,
        _ => unreachable!("entry_x_at called with fixed danmaku"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dfm_core::model::DanmakuItem;

    fn calc_step_x(paint_width: f32, duration_ms: i64, view_width: f32) -> f32 {
        (view_width + paint_width) / duration_ms as f32
    }

    fn make_scroll_item(
        time_ms: i64,
        text: &str,
        paint_width: f32,
        danmaku_type: DanmakuType,
        duration_ms: i64,
        view_width: f32,
    ) -> DanmakuItem {
        let mut item = DanmakuItem::new(
            time_ms,
            text.into(),
            0xFFFFFFFF,
            25.0,
            danmaku_type,
            duration_ms,
        );
        item.paint_width = paint_width;
        item.paint_height = 30.0;
        item.step_x = calc_step_x(paint_width, duration_ms, view_width);
        item
    }

    fn make_fixed_item(
        time_ms: i64,
        text: &str,
        danmaku_type: DanmakuType,
        duration_ms: i64,
    ) -> DanmakuItem {
        let mut item = DanmakuItem::new(
            time_ms,
            text.into(),
            0xFFFFFFFF,
            25.0,
            danmaku_type,
            duration_ms,
        );
        item.paint_width = 100.0;
        item.paint_height = 30.0;
        item
    }

    #[test]
    fn test_first_item_placed_at_top() {
        let flags = GlobalFlags::default();
        let mut retainer = DanmakuRetainer::new(2.0, 0.5);
        let mut item = make_scroll_item(0, "test", 100.0, DanmakuType::ScrollRL, 5000, 1920.0);

        let (placed, _) = retainer.fix(&mut item, 1920.0, 1080.0, &flags, 1.0, false);
        assert!(placed);
        assert!(item.is_shown);
        assert!(
            (item.y - 2.0).abs() < 1.0,
            "first item y={} should be ~2.0",
            item.y
        );
    }

    #[test]
    fn test_same_time_items_different_tracks() {
        let flags = GlobalFlags::default();
        let mut retainer = DanmakuRetainer::new(2.0, 0.5);
        let mut items = [
            make_scroll_item(0, "first", 100.0, DanmakuType::ScrollRL, 5000, 1920.0),
            make_scroll_item(0, "second", 100.0, DanmakuType::ScrollRL, 5000, 1920.0),
        ];

        let (placed1, _) = retainer.fix(&mut items[0], 1920.0, 1080.0, &flags, 1.0, false);
        assert!(placed1);
        let first_y = items[0].y;

        let (placed2, _) = retainer.fix(&mut items[1], 1920.0, 1080.0, &flags, 1.0, false);
        assert!(placed2);
        assert!(
            items[1].y > first_y,
            "same-time items should be on different tracks: first_y={}, second_y={}",
            first_y,
            items[1].y
        );
    }

    #[test]
    fn test_non_overlapping_items_same_track() {
        let flags = GlobalFlags::default();
        let mut retainer = DanmakuRetainer::new(2.0, 0.5);
        let mut items = [
            make_scroll_item(0, "early", 100.0, DanmakuType::ScrollRL, 3000, 1920.0),
            make_scroll_item(10000, "late", 100.0, DanmakuType::ScrollRL, 3000, 1920.0),
        ];

        retainer.fix(&mut items[0], 1920.0, 1080.0, &flags, 1.0, false);
        let first_y = items[0].y;

        retainer.fix(&mut items[1], 1920.0, 1080.0, &flags, 1.0, false);
        assert!(
            (items[1].y - first_y).abs() < 1.0,
            "non-overlapping items should share track: first_y={}, second_y={}",
            first_y,
            items[1].y
        );
    }

    #[test]
    fn test_fixed_items_separate_tracks() {
        let flags = GlobalFlags::default();
        let mut retainer = DanmakuRetainer::new(2.0, 0.5);
        let mut items = [
            make_fixed_item(0, "top1", DanmakuType::FixTop, 3800),
            make_fixed_item(0, "top2", DanmakuType::FixTop, 3800),
        ];

        retainer.fix(&mut items[0], 1920.0, 1080.0, &flags, 1.0, false);
        let first_y = items[0].y;

        retainer.fix(&mut items[1], 1920.0, 1080.0, &flags, 1.0, false);
        assert!(
            items[1].y > first_y,
            "same-time fixed items should be on different tracks"
        );
    }

    #[test]
    fn test_scroll_collision_same_time() {
        let d1 = TrackEntry {
            time_ms: 0,
            duration_ms: 5000,
            paint_width: 100.0,
            step_x: calc_step_x(100.0, 5000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 0,
        };
        let d2 = TrackEntry {
            time_ms: 0,
            duration_ms: 5000,
            paint_width: 100.0,
            step_x: calc_step_x(100.0, 5000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 1,
        };
        assert!(scroll_entries_collide(&d1, &d2, 1920.0));
    }

    #[test]
    fn test_scroll_no_collision_far_apart() {
        let d1 = TrackEntry {
            time_ms: 0,
            duration_ms: 3000,
            paint_width: 100.0,
            step_x: calc_step_x(100.0, 3000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 0,
        };
        let d2 = TrackEntry {
            time_ms: 10000,
            duration_ms: 3000,
            paint_width: 100.0,
            step_x: calc_step_x(100.0, 3000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 1,
        };
        assert!(!scroll_entries_collide(&d1, &d2, 1920.0));
    }

    #[test]
    fn test_overflow_queues_item() {
        let flags = GlobalFlags::default();
        let mut retainer = DanmakuRetainer::new(2.0, 0.5);
        let mut items = [
            make_fixed_item(0, "a", DanmakuType::FixTop, 3800),
            make_fixed_item(0, "b", DanmakuType::FixTop, 3800),
        ];

        let (placed0, _) = retainer.fix(&mut items[0], 1920.0, 60.0, &flags, 1.0, false);
        assert!(placed0, "first item should be placed in the only track");

        let (placed1, _) = retainer.fix(&mut items[1], 1920.0, 60.0, &flags, 1.0, false);
        assert!(
            !placed1,
            "second item should be dropped when all tracks are full"
        );
    }

    #[test]
    fn test_long_danmaku_catches_short() {
        let short = TrackEntry {
            time_ms: 0,
            duration_ms: 8000,
            paint_width: 50.0,
            step_x: calc_step_x(50.0, 8000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 0,
        };
        let long = TrackEntry {
            time_ms: 1000,
            duration_ms: 8000,
            paint_width: 400.0,
            step_x: calc_step_x(400.0, 8000, 1920.0),
            danmaku_type: DanmakuType::ScrollRL,
            danmaku_index: 1,
        };
        assert!(
            scroll_entries_collide(&short, &long, 1920.0),
            "long danmaku starting later should catch up to short danmaku on same track"
        );
    }
}
