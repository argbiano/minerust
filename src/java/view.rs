// Which chunks a player should have, as a square of `radius` around the chunk they
// stand in, nearest first.

use std::collections::HashSet;

pub struct ChunkView {
    radius: i32,
    center: Option<(i32, i32)>,
    loaded: HashSet<(i32, i32)>,
}

pub struct ViewChange {
    pub load: Vec<(i32, i32)>,
    pub forget: Vec<(i32, i32)>,
}

impl ChunkView {
    pub fn new(radius: i32) -> Self {
        Self { radius, center: None, loaded: HashSet::new() }
    }

    pub fn center(&self) -> Option<(i32, i32)> {
        self.center
    }

    pub fn contains(&self, chunk: (i32, i32)) -> bool {
        self.loaded.contains(&chunk)
    }

    pub fn move_to(&mut self, center: (i32, i32)) -> ViewChange {
        self.center = Some(center);
        let in_range = |(x, z): (i32, i32)| (x - center.0).abs() <= self.radius && (z - center.1).abs() <= self.radius;

        let mut forget: Vec<_> = self.loaded.iter().copied().filter(|c| !in_range(*c)).collect();
        forget.sort_unstable();
        for c in &forget {
            self.loaded.remove(c);
        }

        let mut load = Vec::new();
        for dx in -self.radius..=self.radius {
            for dz in -self.radius..=self.radius {
                let c = (center.0 + dx, center.1 + dz);
                if self.loaded.insert(c) {
                    load.push(c);
                }
            }
        }
        load.sort_by_key(|(x, z)| (x - center.0).pow(2) + (z - center.1).pow(2));
        ViewChange { load, forget }
    }
}

pub fn chunk_of(x: f64, z: f64) -> (i32, i32) {
    ((x.floor() as i32) >> 4, (z.floor() as i32) >> 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_view_loads_the_square_nearest_first() {
        let mut v = ChunkView::new(2);
        let change = v.move_to((0, 0));
        assert_eq!(change.load.len(), 25);
        assert_eq!(change.load[0], (0, 0));
        assert!(change.forget.is_empty());
    }

    #[test]
    fn moving_one_chunk_loads_and_forgets_a_row() {
        let mut v = ChunkView::new(2);
        v.move_to((0, 0));
        let change = v.move_to((1, 0));
        assert_eq!(change.load.len(), 5);
        assert!(change.load.iter().all(|(x, _)| *x == 3));
        assert_eq!(change.forget.len(), 5);
        assert!(change.forget.iter().all(|(x, _)| *x == -2));
        assert!(!v.contains((-2, 0)));
    }

    #[test]
    fn negative_coordinates_round_down() {
        assert_eq!(chunk_of(-0.5, 15.9), (-1, 0));
        assert_eq!(chunk_of(-16.0, -16.01), (-1, -2));
    }
}
