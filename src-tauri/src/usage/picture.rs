//! Small luminance samples are only retained for consecutive comparisons.
use image::RgbImage;
use std::time::Instant;

pub(super) const GRID_WIDTH: usize = 160;
pub(super) const GRID_HEIGHT: usize = 90;
pub(super) const GRID_LEN: usize = GRID_WIDTH * GRID_HEIGHT;

pub(super) fn rgb_grid(image: &RgbImage) -> Vec<u8> {
    if image.width() == 0 || image.height() == 0 {
        return vec![];
    }
    let mut grid = Vec::with_capacity(GRID_LEN);
    for gy in 0..GRID_HEIGHT {
        for gx in 0..GRID_WIDTH {
            let mut sum = 0u32;
            for oy in [1, 3] {
                for ox in [1, 3] {
                    let x = ((gx * 4 + ox) as u64 * image.width() as u64 / (GRID_WIDTH * 4) as u64)
                        .min(image.width() as u64 - 1) as u32;
                    let y = ((gy * 4 + oy) as u64 * image.height() as u64
                        / (GRID_HEIGHT * 4) as u64)
                        .min(image.height() as u64 - 1) as u32;
                    let p = image.get_pixel(x, y).0;
                    sum +=
                        (77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8;
                }
            }
            grid.push((sum / 4) as u8);
        }
    }
    grid
}

fn meaningful_change(previous: &[u8], current: &[u8]) -> bool {
    if previous.len() != GRID_LEN || current.len() != GRID_LEN {
        return false;
    }
    let mut changed = 0;
    let mut total = 0u32;
    for (a, b) in previous.iter().zip(current) {
        let delta = a.abs_diff(*b);
        if delta >= 12 {
            changed += 1;
        }
        total += u32::from(delta);
    }
    changed * 100 >= GRID_LEN * 2 && total as usize * 10 >= GRID_LEN * 15
}

#[derive(Default)]
pub(super) struct PictureSignals {
    target: Option<(isize, u32)>,
    normal_grid: Vec<u8>,
    probe_grid: Vec<u8>,
    normal_at: Option<Instant>,
    changed_at: Option<Instant>,
    observed_at: Option<Instant>,
    probe_at: Option<Instant>,
}

impl PictureSignals {
    pub fn observe(&mut self, hwnd: isize, pid: u32, grid: Vec<u8>, normal: bool, at: Instant) {
        if grid.len() != GRID_LEN {
            return;
        }
        if self.target != Some((hwnd, pid)) {
            self.reset();
            self.target = Some((hwnd, pid));
        }
        let (previous, observed) = if normal {
            (&mut self.normal_grid, &mut self.normal_at)
        } else {
            (&mut self.probe_grid, &mut self.probe_at)
        };
        // Normal capture may return its cached image, or a frame queued before
        // the lightweight sampler's latest one. Neither can renew activity.
        if observed.is_some_and(|old| at <= old) {
            return;
        }
        // Normal capture resizes its image. Comparing each source against its
        // own previous frame avoids treating a scaler difference as activity.
        if self.observed_at.is_none_or(|old| at > old) && meaningful_change(previous, &grid) {
            self.changed_at = Some(at);
        }
        self.observed_at = Some(self.observed_at.map_or(at, |old| old.max(at)));
        *observed = Some(at);
        *previous = grid;
    }
    pub fn normal_is_recent(&self, hwnd: isize, pid: u32) -> bool {
        self.target == Some((hwnd, pid))
            && self.normal_at.is_some_and(|at| at.elapsed().as_secs() < 15)
    }
    pub fn activity_age_ms(&self, hwnd: isize, pid: u32) -> Option<u64> {
        (self.target == Some((hwnd, pid)))
            .then_some(self.changed_at)
            .flatten()
            .map(|at| at.elapsed().as_millis() as u64)
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_blinking_clock_is_ignored_but_broad_scene_changes_count() {
        let original = vec![40; GRID_LEN];
        let mut current = original.clone();
        current[..32].fill(240);
        assert!(!meaningful_change(&original, &current));
        current[..GRID_LEN / 5].fill(90);
        assert!(meaningful_change(&original, &current));
        assert!(!meaningful_change(&current, &current));
    }
    #[test]
    fn cached_or_older_frames_do_not_renew_activity_or_normal_capture_freshness() {
        let start = Instant::now() - std::time::Duration::from_secs(60);
        let mut signals = PictureSignals::default();
        signals.observe(1, 2, vec![40; GRID_LEN], true, start);
        signals.observe(
            1,
            2,
            vec![90; GRID_LEN],
            false,
            start + std::time::Duration::from_secs(20),
        );
        let changed = signals.changed_at;
        signals.observe(
            1,
            2,
            vec![40; GRID_LEN],
            true,
            start + std::time::Duration::from_secs(10),
        );
        assert_eq!(signals.changed_at, changed);
        assert_eq!(signals.probe_grid, vec![90; GRID_LEN]);
        assert!(!signals.normal_is_recent(1, 2));
        assert!(signals.changed_at.is_none());
    }
}
