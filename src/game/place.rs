// Placing blocks: which state an item puts where, given the clicked face and the way the
// player looks, and the ray a bucket follows.

use crate::world::{
    blocks,
    registry::{self, Block, Item, NONE},
};

pub type Pos = [i32; 3];

/// Directions in vanilla's Direction order, as packets number them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Dir {
    pub const ALL: [Dir; 6] = [Dir::Down, Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East];

    pub fn from_id(id: i32) -> Option<Self> {
        Self::ALL.get(usize::try_from(id).ok()?).copied()
    }

    pub fn offset(self) -> Pos {
        match self {
            Dir::Down => [0, -1, 0],
            Dir::Up => [0, 1, 0],
            Dir::North => [0, 0, -1],
            Dir::South => [0, 0, 1],
            Dir::West => [-1, 0, 0],
            Dir::East => [1, 0, 0],
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }

    pub fn clockwise(self) -> Self {
        match self {
            Dir::North => Dir::East,
            Dir::East => Dir::South,
            Dir::South => Dir::West,
            Dir::West => Dir::North,
            d => d,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dir::Down => "down",
            Dir::Up => "up",
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }

    pub fn axis(self) -> &'static str {
        match self {
            Dir::Down | Dir::Up => "y",
            Dir::North | Dir::South => "z",
            Dir::West | Dir::East => "x",
        }
    }

    pub fn horizontal(self) -> bool {
        !matches!(self, Dir::Down | Dir::Up)
    }

    /// Direction.fromYRot: the horizontal direction a player with this yaw faces.
    pub fn from_yaw(yaw: f32) -> Self {
        [Dir::South, Dir::West, Dir::North, Dir::East][((yaw / 90.0 + 0.5).floor() as i32 & 3) as usize]
    }
}

pub fn add(a: Pos, b: Pos) -> Pos {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Where a player looks, as a unit vector.
pub fn look(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = ((yaw as f64).to_radians(), (pitch as f64).to_radians());
    [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()]
}

/// The axis direction closest to where the player looks.
pub fn nearest_looking(yaw: f32, pitch: f32) -> Dir {
    let [x, y, z] = look(yaw, pitch);
    if y.abs() >= x.abs() && y.abs() >= z.abs() {
        if y > 0.0 { Dir::Up } else { Dir::Down }
    } else if x.abs() >= z.abs() {
        if x > 0.0 { Dir::East } else { Dir::West }
    } else if z > 0.0 {
        Dir::South
    } else {
        Dir::North
    }
}

/// Walks the blocks along a ray and returns the first one `hits` accepts, with the face the
/// ray entered it by.
pub fn raytrace(
    from: [f64; 3],
    dir: [f64; 3],
    reach: f64,
    block: impl Fn(Pos) -> Option<u16>,
    hits: impl Fn(u16) -> bool,
) -> Option<(Pos, Dir, u16)> {
    let mut pos = [from[0].floor() as i32, from[1].floor() as i32, from[2].floor() as i32];
    let step = [dir[0].signum() as i32, dir[1].signum() as i32, dir[2].signum() as i32];
    let delta = dir.map(|d| if d == 0.0 { f64::INFINITY } else { 1.0 / d.abs() });
    let mut next = [0.0; 3];
    for a in 0..3 {
        next[a] = if dir[a] > 0.0 {
            (pos[a] as f64 + 1.0 - from[a]) * delta[a]
        } else if dir[a] < 0.0 {
            (from[a] - pos[a] as f64) * delta[a]
        } else {
            f64::INFINITY
        };
    }
    // Faces entered when stepping along each axis in the positive direction, then negative.
    let faces = [[Dir::West, Dir::Down, Dir::North], [Dir::East, Dir::Up, Dir::South]];
    loop {
        let a = (0..3).min_by(|&i, &j| next[i].total_cmp(&next[j]))?;
        if next[a] > reach {
            return None;
        }
        pos[a] += step[a];
        next[a] += delta[a];
        let state = block(pos)?;
        if hits(state) {
            let face = if step[a] > 0 { faces[0][a] } else { faces[1][a] };
            return Some((pos, face, state));
        }
    }
}

/// A use_item_on click, as the server needs it.
pub struct Click {
    pub pos: Pos,
    pub face: Dir,
    /// Where on the block, 0-1 on each axis.
    pub cursor: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

/// The block changes from placing `item` with `click`, or None if it can't go there.
pub fn place(item: &Item, click: &Click, block_at: impl Fn(Pos) -> Option<u16>) -> Option<Vec<(Pos, u16)>> {
    if item.block == NONE {
        return None;
    }
    let clicked = block_at(click.pos)?;
    let item_block = registry::block_of(item.block)?;

    if let Some(edit) = combine_slab(item_block, click, clicked) {
        return Some(vec![edit]);
    }
    let replace_clicked = registry::block_of(clicked).is_some_and(|b| b.replaceable && !std::ptr::eq(b, item_block));
    let pos = if replace_clicked { click.pos } else { add(click.pos, click.face.offset()) };
    let there = block_at(pos)?;
    // Adding a second slab half from the side.
    if registry::block_of(there).is_some_and(|b| std::ptr::eq(b, item_block))
        && let Some(edit) = combine_slab(item_block, &Click { pos, ..*click }, there)
    {
        return Some(vec![edit]);
    }
    if !registry::block_of(there).is_some_and(|b| b.replaceable) {
        return None;
    }
    let face = click.face;
    let horizontal = Dir::from_yaw(click.yaw);

    // Torches, signs, banners...: the wall variant on a block's side.
    let (block, mut state) = if item.wall != NONE && face.horizontal() {
        let wall = registry::block_of(item.wall)?;
        (wall, wall.with(item.wall, "facing", face.name()))
    } else if item.wall != NONE && face == Dir::Down && !item_block.name.contains("hanging") {
        return None;
    } else {
        (item_block, item.block)
    };
    let name = block.name;
    let wall_mounted = !std::ptr::eq(block, item_block);

    if block.has("facing") && !wall_mounted {
        let facing = if block.has("face") {
            // Buttons, levers, grindstones.
            let attach = match face {
                Dir::Up => "floor",
                Dir::Down => "ceiling",
                _ => "wall",
            };
            state = block.with(state, "face", attach);
            if face.horizontal() { face } else { horizontal }
        } else if block.get(state, "facing").is_some()
            && block.props.iter().any(|p| p.name == "facing" && p.values.contains(&"up"))
        {
            if ["amethyst", "end_rod", "lightning_rod", "shulker_box"].iter().any(|n| name.contains(n)) {
                face
            } else if name == "observer" {
                nearest_looking(click.yaw, click.pitch)
            } else if name == "hopper" {
                if face == Dir::Down { Dir::Down } else { face.opposite() }
            } else {
                nearest_looking(click.yaw, click.pitch).opposite()
            }
        } else if name.ends_with("trapdoor") {
            if face.horizontal() && !replace_clicked {
                let top = click.cursor[1] > 0.5;
                state = block.with(state, "half", if top { "top" } else { "bottom" });
                face
            } else {
                state = block.with(state, "half", if face == Dir::Up { "bottom" } else { "top" });
                horizontal.opposite()
            }
        } else if name == "ladder" || name.ends_with("_wall_fan") || name == "tripwire_hook" {
            if !face.horizontal() {
                return None;
            }
            face
        } else if name.ends_with("anvil") {
            horizontal.clockwise()
        } else if ["stairs", "_door", "fence_gate", "_bed"].iter().any(|n| name.ends_with(n)) {
            horizontal
        } else {
            horizontal.opposite()
        };
        state = block.with(state, "facing", facing.name());
    }
    if block.props.iter().any(|p| p.name == "axis" && p.values.len() == 3) {
        state = block.with(state, "axis", face.axis());
    }
    let upper_half = face == Dir::Down || (face != Dir::Up && click.cursor[1] > 0.5);
    if block.get(state, "half").is_some_and(|h| h == "top" || h == "bottom") && !name.ends_with("trapdoor") {
        state = block.with(state, "half", if upper_half { "top" } else { "bottom" });
    }
    if name.ends_with("_slab") {
        state = block.with(state, "type", if upper_half { "top" } else { "bottom" });
    }
    if block.has("rotation") {
        let segment = ((click.yaw + 180.0) * 16.0 / 360.0 + 0.5).floor() as i32 & 15;
        state = block.with(state, "rotation", &segment.to_string());
    }
    if block.has("hanging") {
        state = block.with(state, "hanging", if face == Dir::Down { "true" } else { "false" });
    }
    let waterlog = |block: &Block, state: u16, at: u16| {
        if at == blocks::WATER { block.with(state, "waterlogged", "true") } else { state }
    };
    state = waterlog(block, state, there);

    let mut edits = vec![(pos, state)];
    // Doors, tall flowers: the upper half goes above.
    if block.get(state, "half") == Some("lower") {
        let up = add(pos, [0, 1, 0]);
        let above = block_at(up)?;
        if !registry::block_of(above).is_some_and(|b| b.replaceable) {
            return None;
        }
        edits.push((up, waterlog(block, block.with(state, "half", "upper"), above)));
    }
    // Beds: the head goes where the player looks.
    if block.get(state, "part") == Some("foot") {
        let head = add(pos, horizontal.offset());
        let there = block_at(head)?;
        if !registry::block_of(there).is_some_and(|b| b.replaceable) {
            return None;
        }
        edits.push((head, block.with(state, "part", "head")));
    }
    Some(edits)
}

/// A slab clicked so that a second half fits in: the block becomes a double slab.
fn combine_slab(item_block: &Block, click: &Click, clicked: u16) -> Option<(Pos, u16)> {
    let b = registry::block_of(clicked)?;
    if !std::ptr::eq(b, item_block) || !b.name.ends_with("_slab") {
        return None;
    }
    let fits = match b.get(clicked, "type")? {
        "bottom" => click.face == Dir::Up || (click.face.horizontal() && click.cursor[1] > 0.5),
        "top" => click.face == Dir::Down || (click.face.horizontal() && click.cursor[1] <= 0.5),
        _ => false,
    };
    fits.then(|| (click.pos, b.with(b.with(clicked, "type", "double"), "waterlogged", "false")))
}

/// Whether placing `state` at `pos` would put a block into the box from `min` to `max`.
pub fn intersects(state: u16, [x, y, z]: Pos, min: [f64; 3], max: [f64; 3]) -> bool {
    blocks::is_solid(state)
        && min[0] < (x + 1) as f64
        && max[0] > x as f64
        && min[1] < (y + 1) as f64
        && max[1] > y as f64
        && min[2] < (z + 1) as f64
        && max[2] > z as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::registry::{item, item_by_name, parse_state};

    fn click(pos: Pos, face: Dir, cursor_y: f32, yaw: f32, pitch: f32) -> Click {
        Click { pos, face, cursor: [0.5, cursor_y, 0.5], yaw, pitch }
    }

    /// A world of stone below y=64, air above, with a few overrides.
    fn world(extra: &[(Pos, &str)]) -> impl Fn(Pos) -> Option<u16> + use<> {
        let extra: Vec<(Pos, u16)> = extra.iter().map(|&(p, s)| (p, parse_state(s).unwrap())).collect();
        move |p: Pos| {
            extra.iter().find(|(q, _)| *q == p).map(|(_, s)| *s).or(Some(if p[1] < 64 {
                blocks::STONE
            } else {
                blocks::AIR
            }))
        }
    }

    fn placed(name: &str, c: &Click, w: impl Fn(Pos) -> Option<u16>) -> Option<Vec<(Pos, u16)>> {
        place(item(item_by_name(name).unwrap()).unwrap(), c, w)
    }

    fn state(s: &str) -> u16 {
        parse_state(s).unwrap()
    }

    #[test]
    fn directions_follow_vanilla() {
        assert_eq!(Dir::from_yaw(0.0), Dir::South);
        assert_eq!(Dir::from_yaw(90.0), Dir::West);
        assert_eq!(Dir::from_yaw(-90.0), Dir::East);
        assert_eq!(Dir::from_yaw(180.0), Dir::North);
        assert_eq!(nearest_looking(0.0, 90.0), Dir::Down);
        assert_eq!(nearest_looking(-90.0, 10.0), Dir::East);
    }

    #[test]
    fn raytrace_finds_the_first_hit_face() {
        let w = world(&[]);
        // Looking straight down from y=66.6 hits the stone's top at y=63.
        let hit = raytrace([0.5, 66.6, 0.5], look(0.0, 90.0), 5.0, &w, |s| !blocks::is_air(s));
        assert_eq!(hit.map(|(p, f, _)| (p, f)), Some(([0, 63, 0], Dir::Up)));
        // Out of reach.
        assert!(raytrace([0.5, 70.0, 0.5], look(0.0, 90.0), 5.0, &w, |s| !blocks::is_air(s)).is_none());
        let wall = world(&[([3, 65, 0], "stone")]);
        let hit = raytrace([0.5, 65.5, 0.5], look(-90.0, 0.0), 5.0, &wall, |s| !blocks::is_air(s));
        assert_eq!(hit.map(|(p, f, _)| (p, f)), Some(([3, 65, 0], Dir::West)));
    }

    #[test]
    fn simple_blocks_go_against_the_clicked_face() {
        let w = world(&[]);
        let edits = placed("dirt", &click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0), &w).unwrap();
        assert_eq!(edits, vec![([0, 64, 0], state("dirt"))]);
        // Short grass is replaced rather than built against.
        let grassy = world(&[([0, 64, 0], "short_grass")]);
        let edits = placed("dirt", &click([0, 64, 0], Dir::Up, 0.2, 0.0, 45.0), &grassy).unwrap();
        assert_eq!(edits, vec![([0, 64, 0], state("dirt"))]);
        // Something already there.
        let full = world(&[([0, 64, 0], "stone")]);
        assert!(placed("dirt", &click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0), &full).is_none());
        // Items that aren't blocks.
        assert!(placed("diamond", &click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0), &w).is_none());
    }

    #[test]
    fn orientation_comes_from_face_and_look() {
        let w = world(&[]);
        let one = |name: &str, c: Click| placed(name, &c, &w).unwrap()[0].1;
        // Logs take the clicked face's axis.
        assert_eq!(one("oak_log", click([0, 64, -1], Dir::South, 0.5, 0.0, 0.0)), state("oak_log[axis=z]"));
        // A furnace faces the player; stairs face away and go upside down on a high click.
        assert_eq!(one("furnace", click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0)), state("furnace[facing=north]"));
        assert_eq!(
            one("oak_stairs", click([0, 64, -1], Dir::South, 0.8, 0.0, 0.0)),
            state("oak_stairs[facing=south,half=top]")
        );
        // Pistons point at the player, observers away.
        assert_eq!(one("piston", click([0, 63, 0], Dir::Up, 1.0, 0.0, 80.0)), state("piston[facing=up]"));
        assert_eq!(one("observer", click([0, 63, 0], Dir::Up, 1.0, 0.0, 80.0)), state("observer[facing=down]"));
        // Torches on a wall become wall torches; standing signs turn with the player.
        assert_eq!(one("torch", click([0, 64, -1], Dir::South, 0.5, 0.0, 0.0)), state("wall_torch[facing=south]"));
        assert_eq!(one("torch", click([0, 63, 0], Dir::Up, 1.0, 0.0, 0.0)), state("torch"));
        assert_eq!(one("oak_sign", click([0, 63, 0], Dir::Up, 1.0, 90.0, 0.0)), state("oak_sign[rotation=12]"));
        assert_eq!(one("lever", click([0, 64, -1], Dir::South, 0.5, 0.0, 0.0)), state("lever[face=wall,facing=south]"));
    }

    #[test]
    fn slabs_doors_beds_and_water() {
        let w = world(&[]);
        let edits = placed("oak_slab", &click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0), &w).unwrap();
        assert_eq!(edits, vec![([0, 64, 0], state("oak_slab[type=bottom]"))]);
        let slab = world(&[([0, 64, 0], "oak_slab[type=bottom]")]);
        let edits = placed("oak_slab", &click([0, 64, 0], Dir::Up, 0.5, 0.0, 45.0), &slab).unwrap();
        assert_eq!(edits, vec![([0, 64, 0], state("oak_slab[type=double]"))]);

        let edits = placed("oak_door", &click([0, 63, 0], Dir::Up, 1.0, 180.0, 30.0), &w).unwrap();
        assert_eq!(
            edits,
            vec![
                ([0, 64, 0], state("oak_door[facing=north,half=lower]")),
                ([0, 65, 0], state("oak_door[facing=north,half=upper]")),
            ]
        );
        let edits = placed("red_bed", &click([0, 63, 0], Dir::Up, 1.0, -90.0, 30.0), &w).unwrap();
        assert_eq!(
            edits,
            vec![
                ([0, 64, 0], state("red_bed[facing=east,part=foot]")),
                ([1, 64, 0], state("red_bed[facing=east,part=head]"))
            ]
        );
        let under = world(&[([0, 64, 0], "water")]);
        let edits = placed("oak_slab", &click([0, 63, 0], Dir::Up, 1.0, 0.0, 45.0), &under).unwrap();
        assert_eq!(edits, vec![([0, 64, 0], state("oak_slab[type=bottom,waterlogged=true]"))]);
    }
}
