//! The Voronoi map: the folder shown as a region split into cells whose
//! areas match their items' sizes, folders split again inside, a few levels
//! deep. Each level is a power diagram: every item has a site and a weight,
//! and a point belongs to the item whose site is nearest once the weights
//! are taken off. Sites move to their cells' centers (Nocaj and Brandes,
//! 2012) and the weights are worked out so the cell areas match the sizes.
//!
//! Layouts are worked out on a background thread as sites and weights,
//! relative to the region they split. Each frame turns them into polygons,
//! which is cheap; that lets changes glide: the sites and weights move, not
//! the polygons.

use super::*;
use panels::SliceColoring;

/// A convex polygon, its corners in order.
pub(crate) type Poly = Vec<Pos2>;

/// The polygon's area (positive whichever way its corners go).
pub(crate) fn area(poly: &[Pos2]) -> f32 {
    signed_area(poly).abs()
}

fn signed_area(poly: &[Pos2]) -> f32 {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum::<f32>()
        / 2.0
}

/// The polygon's center of mass (its first corner if it has no area).
pub(crate) fn centroid(poly: &[Pos2]) -> Pos2 {
    let a = signed_area(poly);
    if a.abs() < 1e-6 {
        return poly.first().copied().unwrap_or(Pos2::ZERO);
    }
    let n = poly.len();
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..n {
        let (p, q) = (poly[i], poly[(i + 1) % n]);
        let cross = p.x * q.y - q.x * p.y;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    Pos2::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// True if `p` is inside the convex polygon.
pub(crate) fn contains(poly: &[Pos2], p: Pos2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    // Inside: on the same side of every edge as the polygon's turn.
    let sign = signed_area(poly).signum();
    (0..n).all(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        ((b - a).x * (p - a).y - (b - a).y * (p - a).x) * sign >= -1e-3
    })
}

/// The part of `poly` where `normal · x <= limit`.
fn clip(poly: &[Pos2], normal: Vec2, limit: f32) -> Poly {
    let mut out = Vec::with_capacity(poly.len() + 1);
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (da, db) = (
            normal.dot(a.to_vec2()) - limit,
            normal.dot(b.to_vec2()) - limit,
        );
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a + (b - a) * (da / (da - db)));
        }
    }
    out
}

/// The power diagram of `sites` with `weights` inside `bounds`: each site's
/// cell, empty if it has none.
pub(crate) fn power_cells(bounds: &[Pos2], sites: &[Pos2], weights: &[f32]) -> Vec<Poly> {
    sided_cells(bounds, sites, weights)
        .into_iter()
        .map(|c| c.poly)
        .collect()
}

/// Across a side on the region's border: no other cell.
const BORDER: u32 = u32::MAX;

/// A cell and, for each corner, the site whose cell lies across the side
/// from that corner to the next (`BORDER` if none).
#[derive(Clone, Default)]
struct SidedCell {
    poly: Poly,
    across: Vec<u32>,
}

/// `power_cells`, each cell knowing its neighbors.
fn sided_cells(bounds: &[Pos2], sites: &[Pos2], weights: &[f32]) -> Vec<SidedCell> {
    let by_x = sorted_by_x(sites);
    let heaviest = weights.iter().copied().fold(f32::MIN, f32::max);
    (0..by_x.len())
        .map(|k| {
            let i = by_x[k];
            let si = sites[i];
            let mut cell = SidedCell {
                poly: bounds.to_vec(),
                across: vec![BORDER; bounds.len()],
            };
            let mut reach = farthest(&cell.poly, si);
            // The other sites, nearest in x first: once a site is so far in
            // x that even the heaviest weight couldn't bring its edge into
            // the cell, every site beyond it on that side is too.
            let (mut left, mut right) = (k, k + 1);
            let (mut left_done, mut right_done) = (k == 0, right >= by_x.len());
            while !(left_done && right_done) {
                let left_dx = (!left_done).then(|| si.x - sites[by_x[left - 1]].x);
                let right_dx = (!right_done).then(|| sites[by_x[right]].x - si.x);
                let go_left = match (left_dx, right_dx) {
                    (Some(l), Some(r)) => l <= r,
                    (l, _) => l.is_some(),
                };
                let (j, dx) = if go_left {
                    (by_x[left - 1], left_dx.unwrap())
                } else {
                    (by_x[right], right_dx.unwrap())
                };
                if dx > 0.0 && (dx * dx + weights[i] - heaviest) / (2.0 * dx) >= reach {
                    if go_left {
                        left_done = true;
                    } else {
                        right_done = true;
                    }
                    continue;
                }
                if go_left {
                    left -= 1;
                    left_done = left == 0;
                } else {
                    right += 1;
                    right_done = right >= by_x.len();
                }
                if cut(&mut cell, sites, weights, i, j) {
                    if cell.poly.is_empty() {
                        break;
                    }
                    reach = farthest(&cell.poly, si);
                }
            }
            (i, cell)
        })
        .fold(
            vec![SidedCell::default(); sites.len()],
            |mut out, (i, cell)| {
                out[i] = cell;
                out
            },
        )
}

/// Site indices in order of x.
fn sorted_by_x(sites: &[Pos2]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sites.len()).collect();
    order.sort_by(|&a, &b| sites[a].x.total_cmp(&sites[b].x));
    order
}

/// The distance from `p` to the polygon's farthest corner.
fn farthest(poly: &[Pos2], p: Pos2) -> f32 {
    poly.iter()
        .map(|&v| (v - p).length_sq())
        .fold(0.0, f32::max)
        .sqrt()
}

/// Cuts from site i's `cell` the part nearer site j, weights taken off;
/// true if the cell changed.
fn cut(cell: &mut SidedCell, sites: &[Pos2], weights: &[f32], i: usize, j: usize) -> bool {
    // Points nearer i than j: 2 (sj - si) · x <= |sj|² - |si|² - wj + wi
    let (si, sj) = (sites[i].to_vec2(), sites[j].to_vec2());
    let normal = (sj - si) * 2.0;
    if normal.length_sq() < 1e-12 {
        // Same place: the heavier one (or the first) keeps it.
        if weights[j] > weights[i] || (weights[j] == weights[i] && j < i) {
            *cell = SidedCell::default();
            return true;
        }
        return false;
    }
    let limit = sj.length_sq() - si.length_sq() - weights[j] + weights[i];
    let beyond = |v: Pos2| normal.dot(v.to_vec2()) - limit;
    if cell.poly.iter().all(|&v| beyond(v) <= 0.0) {
        return false;
    }
    // As `clip`, keeping track of the sides: the new side faces j.
    let n = cell.poly.len();
    let mut out = SidedCell::default();
    for k in 0..n {
        let (a, b) = (cell.poly[k], cell.poly[(k + 1) % n]);
        let side = cell.across[k];
        let (da, db) = (beyond(a), beyond(b));
        if da <= 0.0 {
            out.poly.push(a);
            out.across.push(if da == 0.0 && db > 0.0 {
                j as u32
            } else {
                side
            });
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.poly.push(a + (b - a) * (da / (da - db)));
            out.across.push(if da < 0.0 { j as u32 } else { side });
        }
    }
    if out.poly.len() < 3 {
        out = SidedCell::default();
    }
    *cell = out;
    true
}

/// The polygon moved inward by `d` on every side; None if nothing is left.
pub(crate) fn inset(poly: &[Pos2], d: f32) -> Option<Poly> {
    let n = poly.len();
    if n < 3 || d <= 0.0 {
        return (n >= 3).then(|| poly.to_vec());
    }
    let inward = if signed_area(poly) > 0.0 { 1.0 } else { -1.0 };
    let mut out = poly.to_vec();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let edge = b - a;
        if edge.length_sq() < 1e-9 {
            continue;
        }
        // The inner side of edge a→b, pushed in by d.
        let normal = Vec2::new(edge.y, -edge.x).normalized() * inward;
        let limit = normal.dot(a.to_vec2()) - d;
        out = clip(&out, normal, limit);
        if out.len() < 3 {
            return None;
        }
    }
    (area(&out) > 1.0).then_some(out)
}

/// A point inside `poly`, from `seed` (spread over the polygon's bounding
/// box, tried until one lands inside; the centroid if none does).
fn point_inside(poly: &[Pos2], seed: u64) -> Pos2 {
    let rect = egui::Rect::from_points(poly);
    let mut state = seed | 1;
    for _ in 0..32 {
        // xorshift: the same seed always gives the same points.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let fx = (state & 0xffff) as f32 / 65535.0;
        let fy = ((state >> 16) & 0xffff) as f32 / 65535.0;
        let p = Pos2::new(
            rect.min.x + fx * rect.width(),
            rect.min.y + fy * rect.height(),
        );
        if contains(poly, p) {
            return p;
        }
    }
    centroid(poly)
}

/// A seed from an item's name, so it starts in the same place every time.
pub(crate) fn name_seed(name: &str) -> u64 {
    // FNV-1a.
    name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Where an item sits in the region it shares with its siblings: its site
/// relative to the region's bounding box (0 to 1 on each side) and its
/// weight relative to the region's area. Relative, so a layout still fits
/// when the region changes shape.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Place {
    pub(crate) site: Pos2,
    pub(crate) weight: f32,
}

/// `place` in `bounds` (bounding box `rect`, area `area`).
fn absolute(place: Place, rect: egui::Rect, area: f32) -> (Pos2, f32) {
    (
        Pos2::new(
            rect.min.x + place.site.x * rect.width(),
            rect.min.y + place.site.y * rect.height(),
        ),
        place.weight * area,
    )
}

fn relative(site: Pos2, weight: f32, rect: egui::Rect, area: f32) -> Place {
    Place {
        site: Pos2::new(
            (site.x - rect.min.x) / rect.width().max(1e-6),
            (site.y - rect.min.y) / rect.height().max(1e-6),
        ),
        weight: weight / area.max(1e-6),
    }
}

/// The cells of items placed at `places` inside `bounds`.
pub(crate) fn cells_of(bounds: &[Pos2], places: &[Place]) -> Vec<Poly> {
    let rect = egui::Rect::from_points(bounds);
    let total = area(bounds);
    let (sites, weights): (Vec<Pos2>, Vec<f32>) =
        places.iter().map(|p| absolute(*p, rect, total)).unzip();
    power_cells(bounds, &sites, &weights)
}

/// Most rounds of moving a level's sites to their cells' centers.
const MAX_ROUNDS: usize = 60;
/// Sites have settled when none moves this far (points) in a round.
const SETTLED: f32 = 0.5;
/// While sites still move, weights need only get areas this near.
const ROUGH: f32 = 0.1;
/// Most Newton steps when working out weights.
const NEWTON_STEPS: usize = 40;
/// A level is done when every cell's area is within this fraction of its
/// item's share.
const AREA_TOLERANCE: f32 = 0.015;

/// Places for items of sizes `sizes` inside `bounds`, so each cell's area
/// matches the item's share: from `start` places where given (an earlier
/// layout), else from points picked by `seeds`.
pub(crate) fn place_items(
    bounds: &[Pos2],
    sizes: &[f64],
    seeds: &[u64],
    start: &[Option<Place>],
) -> Vec<Place> {
    let n = sizes.len();
    let rect = egui::Rect::from_points(bounds);
    let total_area = area(bounds);
    if n == 0 || total_area < 1e-3 {
        return Vec::new();
    }
    let total: f64 = sizes.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    let targets: Vec<f32> = sizes
        .iter()
        .map(|s| (s / total) as f32 * total_area)
        .collect();
    let (mut sites, mut weights): (Vec<Pos2>, Vec<f32>) = (0..n)
        .map(|i| match start.get(i).copied().flatten() {
            Some(p) => absolute(p, rect, total_area),
            None => (point_inside(bounds, seeds[i]), 0.0),
        })
        .unzip();
    if n == 1 {
        return vec![relative(centroid(bounds), 0.0, rect, total_area)];
    }
    // Two sites in one place would share one cell: move the later one.
    for i in 1..n {
        let mut tries = 1u64;
        while tries < 64 && (0..i).any(|j| (sites[i] - sites[j]).length_sq() < 0.25) {
            sites[i] = point_inside(bounds, seeds[i] ^ tries.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            tries += 1;
        }
    }
    // Weights that give every cell its share, then sites to the centers of
    // their cells (which keeps cells round rather than long and thin), and
    // again, until the sites settle.
    for round in 0..MAX_ROUNDS {
        // Roughly while the sites move; exactly at the end.
        solve_weights(bounds, &sites, &mut weights, &targets, ROUGH);
        if round + 1 == MAX_ROUNDS {
            break;
        }
        let cells = power_cells(bounds, &sites, &weights);
        let mut moved = 0.0f32;
        for i in 0..n {
            if area(&cells[i]) > 1e-6 {
                let to = centroid(&cells[i]);
                moved = moved.max((to - sites[i]).length());
                sites[i] = to;
            }
        }
        if moved < SETTLED {
            break;
        }
    }
    solve_weights(bounds, &sites, &mut weights, &targets, AREA_TOLERANCE);
    (0..n)
        .map(|i| relative(sites[i], weights[i], rect, total_area))
        .collect()
}

/// Sets `weights` so that each site's cell has its `targets` area, the
/// sites staying put: Newton steps, shortened while one would leave a cell
/// too small (Kitagawa, Mérigot and Thibert, 2019). A weight changes its
/// cell's area along the sides it shares with other cells, which tells how
/// far to move all the weights at once.
fn solve_weights(
    bounds: &[Pos2],
    sites: &[Pos2],
    weights: &mut [f32],
    targets: &[f32],
    tolerance: f32,
) {
    let n = sites.len();
    let mut cells = sided_cells(bounds, sites, weights);
    if cells.iter().any(|c| c.poly.is_empty()) {
        // Without weights, every site has a cell.
        weights.iter_mut().for_each(|w| *w = 0.0);
        cells = sided_cells(bounds, sites, weights);
    }
    let shortfall = |cells: &[SidedCell]| -> Vec<f64> {
        (0..n)
            .map(|i| f64::from(targets[i] - area(&cells[i].poly)))
            .collect()
    };
    let size = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let smallest_target = targets.iter().copied().fold(f32::MAX, f32::min);
    for _ in 0..NEWTON_STEPS {
        let missing = shortfall(&cells);
        let worst = (0..n)
            .map(|i| missing[i].abs() as f32 / targets[i].max(1e-6))
            .fold(0.0f32, f32::max);
        if worst < tolerance {
            return;
        }
        // How much each cell's area changes with each weight.
        let mut links: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        for i in 0..n {
            let poly = &cells[i].poly;
            for (k, &j) in cells[i].across.iter().enumerate() {
                if j == BORDER {
                    continue;
                }
                let j = j as usize;
                let side = (poly[(k + 1) % poly.len()] - poly[k]).length();
                let apart = (sites[j] - sites[i]).length().max(1e-6);
                links[i].push((j, f64::from(side / (2.0 * apart))));
            }
        }
        let step = solve_links(&links, &missing);
        // Shortened until no cell gets too small and the areas are nearer.
        let smallest_now = cells.iter().map(|c| area(&c.poly)).fold(f32::MAX, f32::min);
        let floor = smallest_target.min(smallest_now) / 2.0;
        let before = size(&missing);
        let mut length = 1.0f64;
        let start = weights.to_vec();
        loop {
            for i in 0..n {
                weights[i] = start[i] + (step[i] * length) as f32;
            }
            let tried = sided_cells(bounds, sites, weights);
            let small = tried.iter().map(|c| area(&c.poly)).fold(f32::MAX, f32::min);
            if (small >= floor && size(&shortfall(&tried)) <= (1.0 - length / 2.0) * before)
                || length < 1e-3
            {
                cells = tried;
                break;
            }
            length /= 2.0;
        }
    }
}

/// Solves `links` · x = `rhs` (conjugate gradients): `links[i]` lists each
/// j with its weight, the matrix having their sum on the diagonal and minus
/// each off it.
fn solve_links(links: &[Vec<(usize, f64)>], rhs: &[f64]) -> Vec<f64> {
    let n = rhs.len();
    let times = |x: &[f64]| -> Vec<f64> {
        (0..n)
            .map(|i| {
                links[i]
                    .iter()
                    .map(|&(j, w)| w * (x[i] - x[j]))
                    .sum::<f64>()
                    + 1e-9 * x[i]
            })
            .collect()
    };
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    // The areas add up either way: only differences between weights count.
    let mean = rhs.iter().sum::<f64>() / n as f64;
    let mut residual: Vec<f64> = rhs.iter().map(|r| r - mean).collect();
    let mut x = vec![0.0; n];
    let mut dir = residual.clone();
    let mut rr = dot(&residual, &residual);
    let goal = rr * 1e-12;
    for _ in 0..4 * n.max(25) {
        if rr <= goal {
            break;
        }
        let along = times(&dir);
        let alpha = rr / dot(&dir, &along).max(f64::MIN_POSITIVE);
        for i in 0..n {
            x[i] += alpha * dir[i];
            residual[i] -= alpha * along[i];
        }
        let next = dot(&residual, &residual);
        for i in 0..n {
            dir[i] = residual[i] + next / rr * dir[i];
        }
        rr = next;
    }
    x
}

/// A rectangle as a polygon, corners clockwise on screen.
pub(crate) fn rect_poly(rect: egui::Rect) -> Poly {
    vec![
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
}

// ---- What the map shows ----

/// Most cells a folder is split into; the rest share one "other" cell.
const MAX_CELLS: usize = 400;
/// Smallest area (square pixels) of an item's cell: 16 × 16, easy to see
/// and click.
const MIN_CELL_PIXELS: f32 = 256.0;
/// The area (square pixels) of "other" when there are more items than
/// cells: 16 × 16, a button to the next items.
const OTHER_CELL_PIXELS: f32 = 256.0;
/// Smallest area of a folder's cell for the items inside to be shown.
const MIN_SPLIT_AREA: f32 = 5000.0;
/// The gap between a folder's cell and the cells inside it.
const NEST_GAP: f32 = 3.0;

/// An item of the map: its slice data (as for the sunburst), its weight
/// and, for a folder shown split, its items.
pub(crate) struct MapItem {
    pub(crate) seg: Segment,
    weight: f64,
    /// For "other" when there are more items than cells: its area (square
    /// points), cut off a corner of the region before the others share it.
    corner: Option<f32>,
    /// Its name within its folder ("other" has a name no file can have).
    key: Box<str>,
    children: Vec<MapItem>,
}

/// The folder shown, as the map lays it out.
pub(crate) struct MapTree {
    items: Vec<MapItem>,
}

/// The name "other" goes by in a layout: no file name holds a NUL.
const OTHER_KEY: &str = "\0other";

/// The map of `view` (only the items at `only`, if given), whose cells
/// will share about `area` square points, on a screen of `pixels_per_point`.
pub(crate) fn map_tree(
    view: &Node,
    only: Option<&[usize]>,
    opts: LayoutOpts,
    area: f32,
    pixels_per_point: f32,
) -> MapTree {
    let pixel = 1.0 / (pixels_per_point * pixels_per_point);
    let sizes = CellSizes {
        smallest: MIN_CELL_PIXELS * pixel,
        other: OTHER_CELL_PIXELS * pixel,
    };
    MapTree {
        items: map_items(view, only, &[], 0, area, sizes, opts),
    }
}

/// Cell areas in square points.
#[derive(Clone, Copy)]
struct CellSizes {
    smallest: f32,
    other: f32,
}

/// The items of `node` (at `idx_path` from the view), largest first, that
/// get a cell of their own in `area`, then "other" for the rest.
fn map_items(
    node: &Node,
    only: Option<&[usize]>,
    idx_path: &[usize],
    depth: usize,
    area: f32,
    sizes: CellSizes,
    opts: LayoutOpts,
) -> Vec<MapItem> {
    let weight = |n: &Node| opts.measure.of(n);
    let mut visible: Vec<(usize, &Node)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            weight(c) > 0 && !c.is_in(opts.hidden) && only.is_none_or(|o| o.contains(i))
        })
        .collect();
    visible.sort_by_key(|(_, c)| std::cmp::Reverse(weight(c)));
    let total: u64 = visible.iter().map(|(_, c)| weight(c)).sum();
    if total == 0 {
        return Vec::new();
    }
    // Every item gets a cell while they all fit at the smallest size; if
    // they don't, "other" takes a small fixed corner and as many items as
    // fit in the rest get cells. The cells share the area by size, none
    // smaller than the smallest size.
    let fits = |room: f32| (room / sizes.smallest) as usize;
    let (shown, room) = if visible.len() <= MAX_CELLS.min(fits(area)) {
        (visible.len(), area)
    } else {
        let room = (area - sizes.other).max(0.0);
        (fits(room).clamp(1, MAX_CELLS - 1).min(visible.len()), room)
    };
    let weights: Vec<u64> = visible[..shown].iter().map(|(_, c)| weight(c)).collect();
    let areas = shares(&weights, room, sizes.smallest);
    let mut items: Vec<MapItem> = visible[..shown]
        .iter()
        .zip(&areas)
        .map(|(&(i, c), &child_area)| {
            let mut path = idx_path.to_vec();
            path.push(i);
            let children =
                if c.is_dir && depth + 1 < opts.settings.map_levels && child_area >= MIN_SPLIT_AREA
                {
                    map_items(c, None, &path, depth + 1, child_area, sizes, opts)
                } else {
                    Vec::new()
                };
            MapItem {
                seg: Segment {
                    idx_path: path,
                    start_angle: 0.0,
                    end_angle: 0.0,
                    ring: depth,
                    name: c.name.to_string(),
                    size: c.size,
                    file_count: c.file_count,
                    is_dir: c.is_dir,
                    is_other: false,
                    is_free: false,
                    mode: Some(c.mode),
                    rest: Vec::new(),
                },
                weight: f64::from(child_area),
                corner: None,
                key: c.name.clone(),
                children,
            }
        })
        .collect();
    let rest = &visible[shown..];
    if !rest.is_empty() {
        let mut path = idx_path.to_vec();
        path.push(OTHER_MARKER);
        items.push(MapItem {
            seg: Segment {
                idx_path: path,
                start_angle: 0.0,
                end_angle: 0.0,
                ring: depth,
                name: trn(
                    "SEG_OTHER_ITEMS",
                    rest.len() as u64,
                    &[&rest.len().to_string()],
                ),
                size: rest.iter().map(|(_, c)| c.size).sum(),
                file_count: rest.iter().map(|(_, c)| c.file_count).sum(),
                is_dir: false,
                is_other: true,
                is_free: false,
                mode: None,
                rest: rest.iter().map(|(i, _)| *i).collect(),
            },
            weight: rest.iter().map(|(_, c)| weight(c) as f64).sum(),
            corner: Some(sizes.other),
            key: OTHER_KEY.into(),
            children: Vec::new(),
        });
    }
    items
}

/// `room` shared by `weights` (largest first), each share at least
/// `smallest`: the items whose share by weight would be smaller get just
/// that, and the others share what's left by weight.
fn shares(weights: &[u64], room: f32, smallest: f32) -> Vec<f32> {
    let n = weights.len();
    let mut raised = 0;
    loop {
        let kept = n - raised;
        let left = (room - smallest * raised as f32).max(0.0);
        let sum: u64 = weights[..kept].iter().sum();
        let share = |w: u64| left * (w as f64 / sum.max(1) as f64) as f32;
        if kept > 0 && share(weights[kept - 1]) < smallest {
            raised += 1;
            continue;
        }
        return (0..n)
            .map(|i| {
                if i < kept {
                    share(weights[i])
                } else {
                    smallest
                }
            })
            .collect();
    }
}

/// Where an item is drawn: from its folder's key and its name.
type PlaceOf<'a> = &'a dyn Fn(&[Box<str>], &str) -> Option<Place>;

/// The items laid out as a power diagram, the region they share and, when
/// the last item is a corner "other", its corner of `bounds`.
fn split_corner<'a>(items: &'a [MapItem], bounds: &[Pos2]) -> (&'a [MapItem], Poly, Option<Poly>) {
    match items.split_last() {
        Some((last, laid)) if last.corner.is_some() && !laid.is_empty() => {
            let wanted = last.corner.unwrap().min(area(bounds) / 4.0);
            let (region, corner) = cut_corner(bounds, wanted);
            (laid, region, Some(corner))
        }
        _ => (items, bounds.to_vec(), None),
    }
}

/// `poly` split by a straight cut near its bottom right corner: the rest
/// and the corner, of area `wanted`.
fn cut_corner(poly: &[Pos2], wanted: f32) -> (Poly, Poly) {
    let center = centroid(poly);
    let tip = poly
        .iter()
        .copied()
        .max_by(|a, b| (a.x + a.y).total_cmp(&(b.x + b.y)))
        .unwrap_or(center);
    let toward = (tip - center).normalized();
    // The cut's position along `toward`, found by halving: from the
    // center (too much area) to the tip (none).
    let (mut near, mut far) = (toward.dot(center.to_vec2()), toward.dot(tip.to_vec2()));
    for _ in 0..40 {
        let mid = (near + far) / 2.0;
        if area(&clip(poly, -toward, -mid)) > wanted {
            near = mid;
        } else {
            far = mid;
        }
    }
    let at = (near + far) / 2.0;
    (clip(poly, toward, at), clip(poly, -toward, -at))
}

/// Laid-out places: for each folder split (by its items' names from the
/// map's top), each of its items' places, by name.
pub(crate) type Places = HashMap<Vec<Box<str>>, HashMap<Box<str>, Place>>;

/// Lays out `tree` inside `bounds`, starting from `earlier` places where
/// there are some.
pub(crate) fn lay_out(tree: &MapTree, bounds: &[Pos2], earlier: &Places) -> Places {
    let mut out = Places::new();
    lay_out_level(&tree.items, bounds, Vec::new(), earlier, &mut out);
    out
}

fn lay_out_level(
    items: &[MapItem],
    bounds: &[Pos2],
    key: Vec<Box<str>>,
    earlier: &Places,
    out: &mut Places,
) {
    if items.is_empty() {
        return;
    }
    let before = earlier.get(&key);
    let (laid, region, corner) = split_corner(items, bounds);
    let sizes: Vec<f64> = laid.iter().map(|i| i.weight).collect();
    let seeds: Vec<u64> = laid.iter().map(|i| name_seed(&i.key)).collect();
    let start: Vec<Option<Place>> = laid
        .iter()
        .map(|i| before.and_then(|b| b.get(&i.key)).copied())
        .collect();
    let mut places = place_items(&region, &sizes, &seeds, &start);
    let cells = cells_of(&region, &places);
    if let Some(corner) = corner {
        let rect = egui::Rect::from_points(&region);
        places.push(relative(centroid(&corner), 0.0, rect, area(&region)));
    }
    for (item, cell) in laid.iter().zip(&cells) {
        if item.children.is_empty() || area(cell) < MIN_SPLIT_AREA {
            continue;
        }
        if let Some(inner) = inset(cell, NEST_GAP) {
            let mut inner_key = key.clone();
            inner_key.push(item.key.clone());
            lay_out_level(&item.children, &inner, inner_key, earlier, out);
        }
    }
    out.insert(
        key,
        items.iter().map(|i| i.key.clone()).zip(places).collect(),
    );
}

/// A cell as drawn: the item, its polygon on screen and how deep it is.
pub(crate) struct MapCell<'a> {
    pub(crate) item: &'a MapItem,
    pub(crate) poly: Poly,
    /// Its items are drawn inside it.
    pub(crate) split: bool,
}

/// The cells of `tree` inside `bounds`, outer before inner, with the places
/// `place_of` gives (folder key, item name). An item without a place yet
/// starts at a point picked by its name.
pub(crate) fn map_cells<'a>(
    tree: &'a MapTree,
    bounds: &[Pos2],
    place_of: PlaceOf,
) -> Vec<MapCell<'a>> {
    let mut out = Vec::new();
    cells_level(&tree.items, bounds, &mut Vec::new(), place_of, &mut out);
    out
}

fn cells_level<'a>(
    items: &'a [MapItem],
    bounds: &[Pos2],
    key: &mut Vec<Box<str>>,
    place_of: PlaceOf,
    out: &mut Vec<MapCell<'a>>,
) {
    if items.is_empty() || bounds.len() < 3 {
        return;
    }
    let (laid, region, corner) = split_corner(items, bounds);
    let rect = egui::Rect::from_points(&region);
    let places: Vec<Place> = laid
        .iter()
        .map(|i| {
            place_of(key, &i.key).unwrap_or_else(|| {
                let p = point_inside(&region, name_seed(&i.key));
                relative(p, 0.0, rect, area(&region))
            })
        })
        .collect();
    let mut cells = cells_of(&region, &places);
    cells.extend(corner);
    for (item, poly) in items.iter().zip(cells) {
        if poly.len() < 3 {
            continue;
        }
        key.push(item.key.clone());
        let laid_out = !item.children.is_empty() && place_of(key, &item.children[0].key).is_some();
        let inner = laid_out.then(|| inset(&poly, NEST_GAP)).flatten();
        out.push(MapCell {
            item,
            poly,
            split: inner.is_some(),
        });
        if let Some(inner) = inner {
            cells_level(&item.children, &inner, key, place_of, out);
        }
        key.pop();
    }
}

// ---- The map on screen ----

/// How long cells take to glide to a new layout, in seconds.
const GLIDE_SECS: f32 = 0.2;
/// How long zooming into or out of a folder takes, in seconds.
const ZOOM_SECS: f32 = 0.3;
/// During a scan, the map is laid out again at most this often.
const LIVE_RELAYOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// What the map shows: a folder (child indices from the tree's top) or,
/// when "other" cells were opened, only the items the last one held.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct MapView {
    folder: Vec<usize>,
    /// The items of each "other" opened, outermost first.
    others: Vec<Vec<usize>>,
}

/// What a layout is for: the folder shown, the tree it's in, what sizes
/// are measured in, what's hidden and the size of the area.
#[derive(Clone, PartialEq)]
struct MapSource {
    view: MapView,
    tree: usize,
    measure: Measure,
    hidden: usize,
    levels: usize,
    size: (i32, i32),
}

/// A cell as last drawn, to zoom from.
#[derive(Clone)]
struct DrawnCell {
    idx_path: Vec<usize>,
    poly: Poly,
    colors: (Color32, Color32),
}

/// A zoom into a folder (`inward`) or out to its parent: the picture
/// before, and when the zoom started (once the new layout was ready).
struct Zoom {
    before: Vec<DrawnCell>,
    /// The folder zoomed into or left, relative to the outer view.
    rel: Vec<usize>,
    inward: bool,
    start: Option<Instant>,
}

/// The map's state between frames.
#[derive(Default)]
pub(crate) struct MapState {
    source: Option<MapSource>,
    /// The tree the places are for, and the tree being laid out.
    tree: Option<Arc<MapTree>>,
    next_tree: Option<Arc<MapTree>>,
    /// Places the cells glide from and to, and since when.
    from: Places,
    target: Places,
    glide_start: Option<Instant>,
    /// The layout being worked out.
    job: Option<Receiver<Places>>,
    /// When the last layout was asked for (during a scan, they're spaced
    /// out), and whether another is due.
    asked_at: Option<Instant>,
    outdated: bool,
    last_picture: Vec<DrawnCell>,
    zoom: Option<Zoom>,
    /// Folders opened and not shown yet, in order, and the last one seen.
    queue: std::collections::VecDeque<MapView>,
    seen_view: Option<MapView>,
    /// For each folder, the "other" cells opened in it (kept when leaving
    /// it for a subfolder, so coming back returns there), and the tree
    /// they're for.
    others: HashMap<Vec<usize>, Vec<Vec<usize>>>,
    others_tree: usize,
    /// The items of the outer "other" cell, to open it from the keyboard.
    top_other: Option<Vec<usize>>,
}

impl MapState {
    /// How many "other" cells are open in `folder`.
    pub(crate) fn other_depth(&self, folder: &[usize]) -> usize {
        self.others.get(folder).map_or(0, Vec::len)
    }

    /// The items shown in `folder` while an "other" is open there.
    pub(crate) fn other_items(&self, folder: &[usize]) -> Option<usize> {
        self.others.get(folder).and_then(|o| o.last()).map(Vec::len)
    }

    /// Opens the outer "other" cell, holding `items`, in `folder`.
    fn open_other(&mut self, folder: &[usize], items: Vec<usize>) {
        self.others.entry(folder.to_vec()).or_default().push(items);
    }

    /// Steps out of the innermost "other" opened in `folder`; false if
    /// there's none.
    pub(crate) fn leave_other(&mut self, folder: &[usize]) -> bool {
        self.others.get_mut(folder).and_then(Vec::pop).is_some()
    }

    /// Opens the outer "other" cell of the map shown, if it has one.
    pub(crate) fn open_top_other(&mut self, folder: &[usize]) -> bool {
        match self.top_other.clone() {
            Some(items) => {
                self.open_other(folder, items);
                true
            }
            None => false,
        }
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 0 below `a`, 1 above `b`, smooth between.
fn smoothstep(a: f32, b: f32, t: f32) -> f32 {
    let x = ((t - a) / (b - a)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A place partway (`f`, 0 to 1) from `a` to `b`.
fn lerp_place(a: Place, b: Place, f: f32) -> Place {
    Place {
        site: a.site + (b.site - a.site) * f,
        weight: a.weight + (b.weight - a.weight) * f,
    }
}

impl MapState {
    /// Where an item is drawn now: gliding from its old place to its new
    /// one. A new item grows from nothing at its new place.
    fn place_now(&self, key: &[Box<str>], name: &str, glide: f32) -> Option<Place> {
        let to = *self.target.get(key)?.get(name)?;
        let from = self
            .from
            .get(key)
            .and_then(|m| m.get(name))
            .copied()
            .unwrap_or(Place {
                weight: to.weight - 0.05,
                ..to
            });
        Some(lerp_place(from, to, glide))
    }

    fn glide(&self, animate: bool) -> f32 {
        match self.glide_start {
            Some(t) if animate => ease_out(t.elapsed().as_secs_f32() / GLIDE_SECS),
            _ => 1.0,
        }
    }

    /// Every place as drawn now.
    fn places_now(&self, glide: f32) -> Places {
        self.target
            .iter()
            .map(|(key, items)| {
                let now = items
                    .keys()
                    .filter_map(|name| Some((name.clone(), self.place_now(key, name, glide)?)))
                    .collect();
                (key.clone(), now)
            })
            .collect()
    }
}

/// Maps the picture so `focus` grows (by `t`, 0 to 1) to fill `view`.
fn zoom_transform(focus: egui::Rect, view: egui::Rect, t: f32) -> impl Fn(Pos2) -> Pos2 {
    let scale_to =
        (view.width() / focus.width().max(1.0)).min(view.height() / focus.height().max(1.0));
    let scale = scale_to.powf(t);
    let center = focus.center() + (view.center() - focus.center()) * t;
    let focus_center = focus.center();
    move |p: Pos2| center + (p - focus_center) * scale
}

/// A button of the arrow pad pressed.
enum PadPress {
    Move(NavDir),
    Open,
}

/// The narrowest and widest the strip left of the map gets.
const STRIP_MIN_WIDTH: f32 = 110.0;
const STRIP_MAX_WIDTH: f32 = 360.0;

impl DiskScanApp {
    /// The map view, `size` big: the strip on the left (the folder's
    /// details, the arrow pad, the legend), the map in the rest.
    pub(crate) fn map_area_ui(
        &mut self,
        ui: &mut egui::Ui,
        top: &Node,
        tree_id: usize,
        size: Vec2,
        live: bool,
    ) {
        ui.horizontal_top(|ui| {
            // As wide as its contents, within limits; the map takes the rest.
            let widest = (size.x * 0.35).clamp(STRIP_MIN_WIDTH, STRIP_MAX_WIDTH);
            let pad = ui.allocate_ui_with_layout(
                Vec2::new(widest, size.y),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_size(Vec2::new(STRIP_MIN_WIDTH, size.y));
                    ui.set_max_width(widest);
                    self.map_strip_ui(ui, live)
                },
            );
            let (response, painter) = ui.allocate_painter(
                Vec2::new(ui.available_width(), size.y),
                egui::Sense::click_and_drag(),
            );
            self.map_ui(ui, top, tree_id, &response, &painter, live);
            // After the map, which lists the items the arrows move between.
            match pad.inner {
                Some(PadPress::Move(dir)) => self.move_selection(dir),
                Some(PadPress::Open) => self.open_selection(),
                None => {}
            }
        });
    }

    /// The strip: the folder shown (or the highlighted item) and its
    /// details, the category picked, the arrow pad, and the legend at the
    /// bottom.
    fn map_strip_ui(&mut self, ui: &mut egui::Ui, live: bool) -> Option<PadPress> {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        if !live && let Some(root) = self.root.clone() {
            let view_node = self.current_view_node(&root);
            let selected = self
                .selected_rel()
                .and_then(|rel| try_get_node(view_node, rel));
            let n = selected.unwrap_or(view_node);
            ui.strong(&*n.name);
            if selected.is_none()
                && let Some(items) = self.map.other_items(self.current_view())
            {
                let depth = self.map.other_depth(self.current_view());
                ui.add(
                    egui::Label::new(trf(
                        "MAP_IN_OTHER",
                        &[&format_count(items as u64), &depth.to_string()],
                    ))
                    .truncate(),
                );
            }
            folder_stats_ui(ui, n);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        }
        if let Some(pick) = self.pick.clone() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(trf("CAT_ACTIVE", &[&self.cats.pick_label(&pick)]));
                if ui.small_button("×").named(&tr("CAT_CLEAR")).clicked() {
                    self.pick = None;
                    self.repick();
                }
            });
        }
        let press = if live {
            None
        } else {
            ui.add_space(10.0);
            self.arrow_pad_ui(ui)
        };
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(4.0);
            self.legend_body(ui);
        });
        press
    }

    /// Buttons for the arrow keys: previous and next item, a level in and
    /// out, and open in the middle. Returns the one pressed.
    fn arrow_pad_ui(&mut self, ui: &mut egui::Ui) -> Option<PadPress> {
        let out = self.view_stack.len() > 1
            || self.map.other_depth(self.current_view()) > 0
            || self.selection.is_some();
        let open = self.selection.is_some();
        let mut press = None;
        egui::Grid::new("arrow_pad")
            .spacing([2.0, 2.0])
            .show(ui, |ui| {
                let mut button =
                    |ui: &mut egui::Ui, glyph: &str, key: &str, enabled: bool, what: PadPress| {
                        if ui
                            .add_enabled(
                                enabled,
                                egui::Button::new(glyph).min_size(Vec2::splat(30.0)),
                            )
                            .on_hover_text(tr(key))
                            .named(&tr(key))
                            .clicked()
                        {
                            press = Some(what);
                        }
                    };
                ui.label("");
                button(ui, "⬆", "MAP_PREV", true, PadPress::Move(NavDir::Prev));
                ui.end_row();
                button(ui, "⬅", "HELP_PARENT", out, PadPress::Move(NavDir::Out));
                button(ui, "⏺", "MAP_OPEN", open, PadPress::Open);
                button(ui, "➡", "MAP_IN", true, PadPress::Move(NavDir::In));
                ui.end_row();
                ui.label("");
                button(ui, "⬇", "MAP_NEXT", true, PadPress::Move(NavDir::Next));
                ui.end_row();
            });
        press
    }

    /// The Voronoi map of `view_node` in `rect`: drawn, and (unless `live`,
    /// during a scan) with hover details, clicks, keys and the menu.
    pub(crate) fn map_ui(
        &mut self,
        ui: &mut egui::Ui,
        top: &Node,
        tree_id: usize,
        response: &egui::Response,
        painter: &egui::Painter,
        live: bool,
    ) {
        let ctx = ui.ctx().clone();
        let rect = response.rect.shrink(4.0);
        let bounds = rect_poly(rect);
        let animate = self.settings.animations;
        // "Other" cells opened in another tree mean nothing in this one.
        if self.map.others_tree != tree_id {
            self.map.others.clear();
            self.map.others_tree = tree_id;
        }
        let view = if live {
            MapView::default()
        } else {
            self.map_view_now()
        };
        let view_node = get_node(top, &view.folder);
        let source = MapSource {
            view: view.clone(),
            tree: tree_id,
            measure: self.measure,
            hidden: self.hidden.len(),
            levels: self.settings.map_levels,
            size: ((rect.width() / 8.0) as i32, (rect.height() / 8.0) as i32),
        };

        // A different folder: zoom to it once it's laid out.
        let old_view = self.map.source.as_ref().map(|s| s.view.clone());
        if self.map.source.as_ref() != Some(&source) {
            if let Some(old) = old_view.filter(|v| *v != view) {
                self.map.from.clear();
                self.map.target.clear();
                self.map.zoom = (animate && !self.map.last_picture.is_empty()).then(|| {
                    let (inward, rel) = if old.folder == view.folder {
                        // Into or out of an "other" cell.
                        (view.others.len() > old.others.len(), vec![OTHER_MARKER])
                    } else if view.folder.starts_with(&old.folder) {
                        (true, view.folder[old.folder.len()..].to_vec())
                    } else {
                        (
                            false,
                            old.folder.get(view.folder.len()..).unwrap_or(&[]).to_vec(),
                        )
                    };
                    Zoom {
                        before: std::mem::take(&mut self.map.last_picture),
                        rel,
                        inward,
                        start: None,
                    }
                });
            }
            let opts = LayoutOpts {
                hidden: &self.hidden,
                settings: &self.settings,
                order: self.chart_order,
                measure: self.measure,
            };
            let only = view.others.last().map(Vec::as_slice);
            self.map.next_tree = Some(Arc::new(map_tree(
                view_node,
                only,
                opts,
                area(&bounds),
                ctx.pixels_per_point(),
            )));
            self.map.source = Some(source);
            self.map.outdated = true;
        }
        self.ask_map_layout(&bounds, live, &ctx);
        self.take_map_layout(animate);

        let Some(tree) = self.map.tree.clone() else {
            // Nothing laid out yet: the last picture stays.
            ctx.request_repaint();
            return;
        };
        let glide = self.map.glide(animate);
        if glide < 1.0 {
            ctx.request_repaint();
        }
        let state = &self.map;
        let place_of = |key: &[Box<str>], name: &str| state.place_now(key, name, glide);
        let cells = map_cells(&tree, &bounds, &place_of);

        let dark = ui.visuals().dark_mode;
        let coloring = SliceColoring {
            free_color: free_space_color(ui.visuals(), self.settings.free_space_gamma),
            view: Some(view_node),
            live,
            now: now_secs(),
        };
        if !live {
            self.update_looks_for_map();
            if self.color_fade() < 1.0 {
                ctx.request_repaint();
            }
        }
        let picture: Vec<DrawnCell> = cells
            .iter()
            .map(|c| DrawnCell {
                idx_path: c.item.seg.idx_path.clone(),
                poly: c.poly.clone(),
                colors: self.segment_colors(&c.item.seg, &coloring, dark),
            })
            .collect();

        let painter = painter.with_clip_rect(response.rect);
        let stroke = egui::Stroke::new(
            self.settings.stroke_width,
            Color32::from_black_alpha(self.settings.stroke_alpha),
        );
        match self.map.zoom.as_ref().and_then(|z| Some((z, z.start?))) {
            Some((zoom, start)) => {
                let t = ease_in_out(start.elapsed().as_secs_f32() / ZOOM_SECS);
                // The folder's cell: in the picture before when zooming in,
                // in the new one when zooming out.
                let find = |p: &[DrawnCell]| {
                    p.iter()
                        .find(|c| c.idx_path == zoom.rel)
                        .map(|c| egui::Rect::from_points(&c.poly))
                };
                if zoom.inward {
                    let focus = find(&zoom.before).unwrap_or(rect);
                    let grow = zoom_transform(focus, rect, t);
                    for c in &zoom.before {
                        let inside = c.idx_path.starts_with(&zoom.rel);
                        let fade = if inside {
                            1.0 - smoothstep(0.4, 1.0, t)
                        } else {
                            1.0 - smoothstep(0.0, 0.5, t)
                        };
                        draw_cell(
                            &painter,
                            &c.poly.iter().map(|p| grow(*p)).collect::<Vec<_>>(),
                            c.colors,
                            stroke,
                            fade,
                        );
                    }
                    for c in &picture {
                        draw_cell(&painter, &c.poly, c.colors, stroke, smoothstep(0.4, 1.0, t));
                    }
                } else {
                    let focus = find(&picture).unwrap_or(rect);
                    let shrink_old = zoom_transform(rect, focus, t);
                    let grow_new = zoom_transform(focus, rect, 1.0 - t);
                    for c in &picture {
                        let inside = c.idx_path.starts_with(&zoom.rel);
                        let fade = if inside {
                            smoothstep(0.0, 0.6, t)
                        } else {
                            smoothstep(0.4, 1.0, t)
                        };
                        draw_cell(
                            &painter,
                            &c.poly.iter().map(|p| grow_new(*p)).collect::<Vec<_>>(),
                            c.colors,
                            stroke,
                            fade,
                        );
                    }
                    for c in &zoom.before {
                        draw_cell(
                            &painter,
                            &c.poly.iter().map(|p| shrink_old(*p)).collect::<Vec<_>>(),
                            c.colors,
                            stroke,
                            1.0 - smoothstep(0.0, 0.6, t),
                        );
                    }
                }
                if start.elapsed().as_secs_f32() >= ZOOM_SECS {
                    self.map.zoom = None;
                }
                ctx.request_repaint();
            }
            None if self.map.zoom.is_some() => {
                // Waiting for the new layout: the old picture stays.
                for c in self.map.zoom.as_ref().map_or(&[][..], |z| &z.before[..]) {
                    draw_cell(&painter, &c.poly, c.colors, stroke, 1.0);
                }
            }
            None => {
                for c in &picture {
                    draw_cell(&painter, &c.poly, c.colors, stroke, 1.0);
                }
                draw_labels(&painter, &cells, &picture, ui);
            }
        }
        self.map.last_picture = picture;
        if live {
            return;
        }
        self.map_input(ui, view_node, &view.folder, response, &cells);
    }

    /// The folder the map shows now: each folder opened is shown in turn,
    /// and only once the zoom to the one before has finished.
    fn map_view_now(&mut self) -> MapView {
        let folder = self.current_view().clone();
        let current = MapView {
            others: self.map.others.get(&folder).cloned().unwrap_or_default(),
            folder,
        };
        if self.map.seen_view.as_ref() != Some(&current) {
            self.map.queue.push_back(current.clone());
            self.map.seen_view = Some(current.clone());
        }
        let shown = self.map.source.as_ref().map(|s| s.view.clone());
        if self.map.zoom.is_some() {
            return shown.unwrap_or(current);
        }
        match self.map.queue.pop_front() {
            Some(next) => next,
            None => shown.unwrap_or(current),
        }
    }

    /// Asks the background thread for a layout when the map changed (at most
    /// every `LIVE_RELAYOUT` during a scan).
    fn ask_map_layout(&mut self, bounds: &[Pos2], live: bool, ctx: &egui::Context) {
        if !self.map.outdated {
            return;
        }
        if live
            && let Some(at) = self.map.asked_at
            && at.elapsed() < LIVE_RELAYOUT
        {
            ctx.request_repaint_after(LIVE_RELAYOUT - at.elapsed());
            return;
        }
        let Some(tree) = self.map.next_tree.clone() else {
            return;
        };
        let earlier = self
            .map
            .places_now(self.map.glide(self.settings.animations));
        let bounds = bounds.to_vec();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let places = lay_out(&tree, &bounds, &earlier);
            let _ = tx.send(places);
            ctx.request_repaint();
        });
        self.map.job = Some(rx);
        self.map.asked_at = Some(Instant::now());
        self.map.outdated = false;
    }

    /// Takes a finished layout: cells glide to it, or a waiting zoom starts.
    fn take_map_layout(&mut self, animate: bool) {
        let Some(places) = self.map.job.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.map.job = None;
        let glide = self.map.glide(animate);
        self.map.from = if animate {
            self.map.places_now(glide)
        } else {
            Places::new()
        };
        self.map.target = places;
        self.map.glide_start = Some(Instant::now());
        self.map.tree = self.map.next_tree.clone();
        if let Some(zoom) = &mut self.map.zoom
            && zoom.start.is_none()
        {
            zoom.start = Some(Instant::now());
            // A zoom shows the new layout as it is, without gliding.
            self.map.from.clear();
            self.map.glide_start = None;
        }
    }

    fn update_looks_for_map(&mut self) {
        if let Some(root) = self.root.clone() {
            self.update_looks(&root);
        }
    }

    /// Hover details, clicks, the right-click menu and the arrow keys'
    /// highlight on the map.
    fn map_input(
        &mut self,
        ui: &mut egui::Ui,
        view_node: &Node,
        view: &[usize],
        response: &egui::Response,
        cells: &[MapCell],
    ) {
        let ctx = ui.ctx().clone();
        let chart_name = trf(
            "A11Y_CHART",
            &[&show_path(&view_node.path()), &human_size(view_node.size)],
        );
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &chart_name));

        let pointer = ctx
            .input(|i| i.pointer.hover_pos())
            .filter(|_| response.contains_pointer());
        // The innermost cell under the pointer.
        let hovered = pointer.and_then(|p| cells.iter().rev().find(|c| contains(&c.poly, p)));
        self.hovered = hovered.map(|c| self.hover_info(view_node, &c.item.seg));
        let painter = ui.painter().with_clip_rect(response.rect);
        let strong = ui.visuals().strong_text_color();
        let zooming = self.map.zoom.is_some();
        if let Some(c) = hovered.filter(|_| !zooming) {
            painter.add(egui::Shape::closed_line(
                c.poly.clone(),
                egui::Stroke::new(1.5, strong.gamma_multiply(0.6)),
            ));
        }

        self.map.top_other = cells
            .iter()
            .find(|c| c.item.seg.ring == 0 && c.item.seg.is_other)
            .map(|c| c.item.seg.rest.clone());
        // What the arrow keys move between, in drawing order.
        self.chart_segs = cells
            .iter()
            .enumerate()
            .map(|(i, c)| (c.item.seg.idx_path.clone(), i as f32))
            .collect();
        if let Some(rel) = self.selected_rel().filter(|_| !zooming)
            && let Some(c) = cells.iter().find(|c| c.item.seg.idx_path == *rel)
        {
            painter.add(egui::Shape::closed_line(
                c.poly.clone(),
                egui::Stroke::new(2.0, strong.gamma_multiply(0.85)),
            ));
        }

        // A click goes one level deeper: into the outer cell under the
        // pointer (the inner ones show what's next).
        let outer = pointer.and_then(|p| {
            cells
                .iter()
                .find(|c| c.item.seg.ring == 0 && contains(&c.poly, p))
        });
        if response.clicked() {
            match outer {
                // "Other" opens like a folder holding the items it groups.
                Some(c) if c.item.seg.is_other => {
                    self.map.open_other(view, c.item.seg.rest.clone())
                }
                Some(c) if c.item.seg.is_dir => {
                    let mut vp = view.to_vec();
                    vp.extend(c.item.seg.idx_path.iter());
                    self.view_stack.push(vp);
                }
                Some(_) => {}
                // Between the cells, around the map: up a level.
                None => {
                    if self.view_stack.len() > 1 {
                        self.view_stack.pop();
                    }
                }
            }
        }
        if response.secondary_clicked() {
            self.context_target = hovered
                .filter(|c| !c.item.seg.is_other)
                .map(|c| get_node(view_node, &c.item.seg.idx_path).path());
        }
        if let Some(root) = self.root.clone() {
            response.context_menu(|ui| self.chart_menu_ui(ui, &root));
        }
        if let Some(p) = pointer.filter(|_| !zooming) {
            self.hover_details_ui(&ctx, p, response.rect.center());
        }
    }
}

/// A cell filled from its newest color at the center to its oldest at the
/// border, outlined, at `opacity`.
fn draw_cell(
    painter: &egui::Painter,
    poly: &[Pos2],
    (newest, oldest): (Color32, Color32),
    stroke: egui::Stroke,
    opacity: f32,
) {
    if poly.len() < 3 || opacity <= 0.0 {
        return;
    }
    let mut mesh = egui::epaint::Mesh::default();
    let vertex = |pos: Pos2, color: Color32| egui::epaint::Vertex {
        pos,
        uv: egui::epaint::WHITE_UV,
        color: color.gamma_multiply(opacity),
    };
    mesh.vertices.push(vertex(centroid(poly), newest));
    mesh.vertices
        .extend(poly.iter().map(|p| vertex(*p, oldest)));
    let n = poly.len() as u32;
    for i in 0..n {
        mesh.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n]);
    }
    painter.add(egui::Shape::mesh(mesh));
    let stroke = egui::Stroke::new(stroke.width, stroke.color.gamma_multiply(opacity));
    painter.add(egui::Shape::closed_line(poly.to_vec(), stroke));
}

/// Names (and sizes, where there's room) on the cells big enough for them:
/// on every cell not split, and on the outer level's split ones.
fn draw_labels(painter: &egui::Painter, cells: &[MapCell], picture: &[DrawnCell], ui: &egui::Ui) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    for (cell, drawn) in cells.iter().zip(picture) {
        let seg = &cell.item.seg;
        if cell.split && seg.ring > 0 {
            continue;
        }
        let rect = egui::Rect::from_points(&cell.poly);
        // Room inside the cell: about the middle half of its bounding box.
        let width = rect.width() * 0.7;
        if width < 36.0 || rect.height() < 18.0 {
            continue;
        }
        let galley = painter.layout(seg.name.clone(), font.clone(), Color32::PLACEHOLDER, width);
        if galley.rows.len() > 2 {
            continue;
        }
        let fits = |r: egui::Rect| {
            [
                r.left_top(),
                r.right_top(),
                r.left_bottom(),
                r.right_bottom(),
            ]
            .iter()
            .all(|p| contains(&cell.poly, *p))
        };
        let center = centroid(&cell.poly);
        // A folder's name goes near its top, clear of the names inside it;
        // anything else's in the middle.
        let text_rect = if cell.split {
            (0..8)
                .map(|k| {
                    let y = rect.top() + 6.0 + k as f32 * (center.y - rect.top()) / 8.0;
                    egui::Rect::from_min_size(
                        Pos2::new(center.x - galley.size().x / 2.0, y),
                        galley.size(),
                    )
                })
                .find(|r| fits(r.expand(3.0)))
        } else {
            Some(egui::Rect::from_center_size(center, galley.size())).filter(|r| fits(*r))
        };
        let Some(text_rect) = text_rect else {
            continue;
        };
        let fill = drawn.colors.0;
        let text = if cell.split {
            painter.rect_filled(text_rect.expand(3.0), 4.0, Color32::from_black_alpha(140));
            Color32::WHITE
        } else if is_light(fill) {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        painter.galley(text_rect.min, galley, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(side: f32) -> Poly {
        rect_poly(egui::Rect::from_min_size(Pos2::ZERO, Vec2::splat(side)))
    }

    #[test]
    fn polygon_basics() {
        let sq = square(10.0);
        assert!((area(&sq) - 100.0).abs() < 1e-3);
        assert!((centroid(&sq) - Pos2::new(5.0, 5.0)).length() < 1e-3);
        assert!(contains(&sq, Pos2::new(1.0, 9.0)) && !contains(&sq, Pos2::new(11.0, 5.0)));
        let half = clip(&sq, Vec2::new(1.0, 0.0), 4.0);
        assert!((area(&half) - 40.0).abs() < 1e-3);
        let inner = inset(&sq, 1.0).unwrap();
        assert!((area(&inner) - 64.0).abs() < 1e-2);
        assert!(inset(&sq, 6.0).is_none());
    }

    #[test]
    fn equal_weights_split_by_distance() {
        let sq = square(10.0);
        let cells = power_cells(
            &sq,
            &[Pos2::new(2.0, 5.0), Pos2::new(8.0, 5.0)],
            &[0.0, 0.0],
        );
        assert!((area(&cells[0]) - 50.0).abs() < 1e-3);
        // A heavier site pushes the border away from it.
        let cells = power_cells(
            &sq,
            &[Pos2::new(2.0, 5.0), Pos2::new(8.0, 5.0)],
            &[12.0, 0.0],
        );
        assert!((area(&cells[0]) - 60.0).abs() < 1e-3);
    }

    /// Cell areas end up matching the sizes, however uneven, and the cells
    /// fill the region without overlapping.
    #[test]
    fn areas_match_sizes() {
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(800.0, 500.0),
        ));
        let total_area = area(&bounds);
        for sizes in [
            vec![1.0, 1.0, 1.0],
            vec![50.0, 20.0, 10.0, 8.0, 5.0, 4.0, 2.0, 1.0],
            (1..=40)
                .map(|i| 1000.0 / f64::from(i))
                .collect::<Vec<f64>>(),
        ] {
            let seeds: Vec<u64> = (0..sizes.len())
                .map(|i| name_seed(&i.to_string()))
                .collect();
            let places = place_items(&bounds, &sizes, &seeds, &[]);
            let cells = cells_of(&bounds, &places);
            let sum: f64 = sizes.iter().sum();
            let covered: f32 = cells.iter().map(|c| area(c)).sum();
            assert!((covered - total_area).abs() / total_area < 1e-3);
            for (cell, size) in cells.iter().zip(&sizes) {
                let want = (size / sum) as f32 * total_area;
                let err = (area(cell) - want).abs() / want;
                assert!(
                    err < 0.05,
                    "{} items: cell {} vs {want} ({err})",
                    sizes.len(),
                    area(cell)
                );
            }
        }
    }

    /// The same names give the same layout, and a layout started from an
    /// earlier one stays close to it.
    #[test]
    fn layouts_are_stable() {
        let bounds = square(400.0);
        let sizes = [5.0, 3.0, 2.0, 1.0];
        let seeds: Vec<u64> = ["a", "b", "c", "d"].iter().map(|n| name_seed(n)).collect();
        let first = place_items(&bounds, &sizes, &seeds, &[]);
        assert_eq!(first, place_items(&bounds, &sizes, &seeds, &[]));
        let start: Vec<Option<Place>> = first.iter().copied().map(Some).collect();
        let again = place_items(&bounds, &[5.0, 3.0, 2.2, 1.0], &seeds, &start);
        for (a, b) in first.iter().zip(&again) {
            assert!((a.site - b.site).length() < 0.1, "{a:?} {b:?}");
        }
    }

    fn opts<'a>(hidden: &'a HashSet<PathBuf>, settings: &'a Settings) -> LayoutOpts<'a> {
        LayoutOpts {
            hidden,
            settings,
            order: ChartOrder::Size,
            measure: Measure::Bytes,
        }
    }

    /// Sizes thousands of times apart still get their exact shares, also
    /// when starting from a layout made for very different sizes (as when
    /// the largest folder is read last during a scan).
    #[test]
    fn very_different_sizes_get_their_shares() {
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(945.0, 592.0),
        ));
        let sizes = [
            43_944.0, 15_442.0, 11_944.0, 11_504.0, 5_706.0, 5_445.0, 2_805.0, 1_566.0, 1_026.0,
            793.0, 492.0, 216.0, 207.0, 108.0, 98.0, 50.0, 36.0, 28.0, 21.0, 20.0, 9.5,
        ];
        let seeds: Vec<u64> = (0..sizes.len() as u64).collect();
        let check = |places: &[Place]| {
            let cells = cells_of(&bounds, places);
            let total: f64 = sizes.iter().sum();
            for (cell, size) in cells.iter().zip(sizes) {
                let share = (size / total) as f32 * area(&bounds);
                let off = (area(cell) - share).abs() / share;
                assert!(
                    off < AREA_TOLERANCE * 1.5,
                    "{size}: {} vs {share}",
                    area(cell)
                );
            }
        };
        let fresh = place_items(&bounds, &sizes, &seeds, &[]);
        check(&fresh);
        let mut early = sizes;
        early[0] = 1.0;
        let start: Vec<Option<Place>> = place_items(&bounds, &early, &seeds, &[])
            .into_iter()
            .map(Some)
            .collect();
        check(&place_items(&bounds, &sizes, &seeds, &start));
    }

    /// Skipping far sites gives the same cells as cutting with every site.
    #[test]
    fn power_cells_skip_only_far_sites() {
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(900.0, 600.0),
        ));
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 10_000) as f32 / 10_000.0
        };
        for n in [2, 7, 60, 300] {
            let sites: Vec<Pos2> = (0..n)
                .map(|_| Pos2::new(next() * 900.0, next() * 600.0))
                .collect();
            let weights: Vec<f32> = (0..n).map(|_| next() * 4000.0).collect();
            let all: Vec<Poly> = (0..n)
                .map(|i| {
                    let mut cell = SidedCell {
                        poly: bounds.clone(),
                        across: vec![BORDER; 4],
                    };
                    for j in 0..n {
                        if i != j && !cell.poly.is_empty() {
                            cut(&mut cell, &sites, &weights, i, j);
                        }
                    }
                    cell.poly
                })
                .collect();
            let fast = power_cells(&bounds, &sites, &weights);
            for (a, b) in all.iter().zip(&fast) {
                assert!(
                    (area(a) - area(b)).abs() < 0.5,
                    "{} vs {}",
                    area(a),
                    area(b)
                );
            }
        }
    }

    /// Every item gets a cell by its size while the smallest is still big
    /// enough to see; past that, the smallest share "other", which knows
    /// which they are. Hidden and empty items get none; folders are split
    /// only a few levels deep and only when there's room.
    #[test]
    fn items_get_cells_by_size() {
        let small = |size| {
            let mut files: Vec<Node> = (0..60)
                .map(|i| test_node(&format!("/t/small{i}"), size, false, vec![]))
                .collect();
            files.push(test_node("/t/big", 1_000_000, false, vec![]));
            files.push(test_node("/t/hidden", 50_000, false, vec![]));
            files.push(test_node("/t/empty", 0, false, vec![]));
            let deep = test_node(
                "/t/d1",
                1_000_000,
                true,
                vec![test_node(
                    "/t/d1/d2",
                    1_000_000,
                    true,
                    vec![test_node(
                        "/t/d1/d2/d3",
                        1_000_000,
                        true,
                        vec![test_node("/t/d1/d2/d3/f", 1_000_000, false, vec![])],
                    )],
                )],
            );
            files.push(deep);
            test_node("/t", 0, true, files)
        };
        let hidden: HashSet<PathBuf> = [PathBuf::from("/t/hidden")].into();
        let settings = Settings {
            map_levels: 3,
            ..Settings::default()
        };
        // 100-byte files are big enough to see: all 62 items get a cell.
        let tree = map_tree(
            &small(100),
            None,
            opts(&hidden, &settings),
            800.0 * 600.0,
            1.0,
        );
        assert_eq!(tree.items.len(), 62);
        assert!(tree.items.iter().all(|i| !i.seg.is_other));
        // 1-byte files too, far smaller than their share of a point: each
        // gets the smallest cell, the others share the rest by size.
        let root = small(1);
        let tree = map_tree(&root, None, opts(&hidden, &settings), 800.0 * 600.0, 1.0);
        assert_eq!(tree.items.len(), 62);
        assert!(tree.items.iter().all(|i| !i.seg.is_other));
        let (big, tiny) = (&tree.items[0], &tree.items[61]);
        assert_eq!(tiny.weight, f64::from(MIN_CELL_PIXELS));
        let sum: f64 = tree.items.iter().map(|i| i.weight).sum();
        assert!((sum - 800.0 * 600.0).abs() < 1.0);
        assert!((big.weight - (800.0 * 600.0 - 60.0 * 256.0) / 2.0).abs() < 1.0);
        // More items than fit at the smallest size: "other" is a 16 × 16
        // corner and as many as fit in the rest get cells.
        let tree = map_tree(&root, None, opts(&hidden, &settings), 120.0 * 120.0, 1.0);
        assert_eq!(tree.items.len(), 55 + 1);
        let other = &tree.items[55];
        assert!(other.seg.is_other && other.seg.rest.len() == 62 - 55);
        assert_eq!(other.seg.idx_path, [OTHER_MARKER]);
        assert_eq!(other.corner, Some(OTHER_CELL_PIXELS));
        // Laid out, it's a corner of exactly that area.
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(120.0, 120.0),
        ));
        let places = lay_out(&tree, &bounds, &Places::new());
        let place_of = |key: &[Box<str>], name: &str| places.get(key)?.get(name).copied();
        let cells = map_cells(&tree, &bounds, &place_of);
        let corner = cells.iter().find(|c| c.item.seg.is_other).unwrap();
        assert!((area(&corner.poly) - OTHER_CELL_PIXELS).abs() < 0.5);
        assert!(contains(&corner.poly, Pos2::new(119.5, 119.5)));
        // On a screen of 2 pixels per point, 256 pixels are 64 square points.
        let tree = map_tree(&root, None, opts(&hidden, &settings), 3200.0, 2.0);
        assert_eq!(tree.items.last().unwrap().corner, Some(64.0));
        let tree = map_tree(&root, None, opts(&hidden, &settings), 800.0 * 600.0, 1.0);
        // d1 → d2 → d3: three levels, the third not split.
        let d2 = &tree.items[1].children[0];
        assert_eq!(d2.seg.idx_path, [63, 0]);
        assert!(d2.children.len() == 1 && d2.children[0].children.is_empty());
        // At one level (the default), folders aren't split.
        let one = Settings::default();
        let tree = map_tree(&root, None, opts(&hidden, &one), 800.0 * 600.0, 1.0);
        assert!(tree.items.iter().all(|i| i.children.is_empty()));
        // In a small area, folders aren't split at all.
        let tree = map_tree(&root, None, opts(&hidden, &settings), 3000.0, 1.0);
        assert_eq!(tree.items.len(), 10 + 1);
        assert!(tree.items.iter().all(|i| i.children.is_empty()));
    }

    /// Every item shown gets a cell; cells inside a folder lie inside its
    /// cell; top-level areas follow sizes.
    #[test]
    fn map_cells_nest_and_follow_sizes() {
        let sub: Vec<Node> = (0..5)
            .map(|i| test_node(&format!("/t/a/f{i}"), 1000 * (i + 1), false, vec![]))
            .collect();
        let root = test_node(
            "/t",
            0,
            true,
            vec![
                test_node("/t/a", 15_000, true, sub),
                test_node("/t/b", 10_000, false, vec![]),
                test_node("/t/c", 5_000, false, vec![]),
            ],
        );
        let hidden = HashSet::new();
        let settings = Settings {
            map_levels: 2,
            ..Settings::default()
        };
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(900.0, 600.0),
        ));
        let tree = map_tree(&root, None, opts(&hidden, &settings), area(&bounds), 1.0);
        let places = lay_out(&tree, &bounds, &Places::new());
        assert!(places.contains_key(&Vec::new()) && places.contains_key(&vec!["a".into()]));
        let place_of = |key: &[Box<str>], name: &str| places.get(key)?.get(name).copied();
        let cells = map_cells(&tree, &bounds, &place_of);
        assert_eq!(cells.len(), 3 + 5);
        let top: Vec<&MapCell> = cells.iter().filter(|c| c.item.seg.ring == 0).collect();
        let total = area(&bounds);
        for (cell, share) in top.iter().zip([0.5, 1.0 / 3.0, 1.0 / 6.0]) {
            assert!(
                (area(&cell.poly) / total - share).abs() < 0.03,
                "{}",
                cell.item.key
            );
        }
        assert!(top[0].split);
        for inner in cells.iter().filter(|c| c.item.seg.ring == 1) {
            assert!(contains(&top[0].poly, centroid(&inner.poly)));
        }
    }

    /// Cells glide from their old places to new ones; a new item grows in.
    #[test]
    fn places_glide() {
        let key: Vec<Box<str>> = Vec::new();
        let at = |x: f32, w: f32| Place {
            site: Pos2::new(x, 0.5),
            weight: w,
        };
        let mut state = MapState::default();
        state
            .from
            .insert(key.clone(), [("a".into(), at(0.2, 0.0))].into());
        state.target.insert(
            key.clone(),
            [("a".into(), at(0.6, 0.1)), ("b".into(), at(0.3, 0.2))].into(),
        );
        let a = state.place_now(&key, "a", 0.5).unwrap();
        assert!((a.site - Pos2::new(0.4, 0.5)).length() < 1e-6 && (a.weight - 0.05).abs() < 1e-6);
        assert_eq!(state.place_now(&key, "a", 1.0), Some(at(0.6, 0.1)));
        let b = state.place_now(&key, "b", 0.0).unwrap();
        assert!(b.weight < 0.2 && b.site == Pos2::new(0.3, 0.5));
        assert_eq!(state.place_now(&key, "gone", 0.5), None);
        assert!(ease_out(0.0) == 0.0 && ease_out(1.0) == 1.0 && ease_in_out(0.5) == 0.5);
    }

    /// On the map, clicking a folder opens it and the Parent folder button
    /// goes back up.
    #[test]
    fn map_opens_folders_and_goes_back() {
        let mut app = DiskScanApp::default();
        app.settings.map_view = true;
        app.settings.animations = false;
        app.settings.map_levels = 2;
        let files: Vec<Node> = (0..4)
            .map(|i| test_node(&format!("/t/dir/f{i}"), 1000, false, vec![]))
            .collect();
        app.full_root = Some(Arc::new(test_node(
            "/t",
            9000,
            true,
            vec![
                test_node("/t/dir", 4000, true, files),
                test_node("/t/file", 5000, false, vec![]),
            ],
        )));
        app.rebuild_view_tree();
        let ctx = egui::Context::default();
        let frame = |app: &mut DiskScanApp, events: Vec<egui::Event>| {
            app.saved_config = Some(app.current_config());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(1200.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            headless_frame(&ctx, input, |ui| app.frame_ui(ui));
        };
        let click = |app: &mut DiskScanApp, at: Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(app, vec![egui::Event::PointerMoved(at)]);
            frame(app, vec![button(true)]);
            frame(app, vec![button(false)]);
        };
        // The layout comes from another thread.
        let laid_out = |app: &mut DiskScanApp| {
            for _ in 0..200 {
                frame(app, vec![]);
                if app.map.tree.is_some() && app.map.job.is_none() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("no layout");
        };
        laid_out(&mut app);
        let dir_center = app
            .map
            .last_picture
            .iter()
            .find(|c| c.idx_path == [0])
            .map(|c| centroid(&c.poly))
            .expect("the folder's cell");
        click(&mut app, dir_center);
        assert_eq!(app.current_view(), &vec![0]);
        laid_out(&mut app);
        assert_eq!(app.map.last_picture.len(), 4);
        app.chart_parent_folder();
        laid_out(&mut app);
        assert_eq!(app.current_view(), &Vec::<usize>::new());
        assert_eq!(app.map.last_picture.len(), 2 + 4);
    }

    /// A folder opened during a zoom waits for it: each zoom plays in turn.
    #[test]
    fn opened_folders_queue_behind_a_zoom() {
        let mut app = DiskScanApp::default();
        let at = |folder: Vec<usize>| MapView {
            folder,
            others: Vec::new(),
        };
        assert_eq!(app.map_view_now(), at(vec![]));
        app.map.source = Some(MapSource {
            view: at(vec![]),
            tree: 0,
            measure: Measure::Bytes,
            hidden: 0,
            levels: 1,
            size: (0, 0),
        });
        app.map.zoom = Some(Zoom {
            before: Vec::new(),
            rel: vec![],
            inward: true,
            start: Some(Instant::now()),
        });
        app.view_stack.push(vec![1]);
        assert_eq!(app.map_view_now(), at(vec![]));
        app.view_stack.push(vec![1, 2]);
        assert_eq!(app.map_view_now(), at(vec![]));
        app.map.zoom = None;
        assert_eq!(app.map_view_now(), at(vec![1]));
        assert_eq!(app.map_view_now(), at(vec![1, 2]));
    }

    /// "Other" opens like a folder, again and again while there are more
    /// items than room, and steps back out one level at a time; the
    /// folder shown stays the same.
    #[test]
    fn other_opens_like_a_folder() {
        let mut app = DiskScanApp::default();
        app.settings.map_view = true;
        app.settings.animations = false;
        let mut files: Vec<Node> = (0..1000)
            .map(|i| test_node(&format!("/t/s{i:04}"), 10, false, vec![]))
            .collect();
        files.push(test_node("/t/big", 10_000_000, false, vec![]));
        app.full_root = Some(Arc::new(test_node("/t", 10_010_000, true, files)));
        app.rebuild_view_tree();
        let ctx = egui::Context::default();
        let frame = |app: &mut DiskScanApp, events: Vec<egui::Event>| {
            app.saved_config = Some(app.current_config());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(1200.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            headless_frame(&ctx, input, |ui| app.frame_ui(ui));
        };
        let laid_out = |app: &mut DiskScanApp| {
            for _ in 0..3000 {
                frame(app, vec![]);
                if app.map.tree.is_some() && app.map.job.is_none() && app.map.queue.is_empty() {
                    frame(app, vec![]);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("no layout");
        };
        let shown = |app: &DiskScanApp| app.map.last_picture.len();
        laid_out(&mut app);
        // 1001 items, more than the most cells: 399 and "other" (602);
        // opened: 399 and "other" (203), then the last 203.
        assert_eq!(shown(&app), MAX_CELLS);
        for (depth, cells) in [(1, MAX_CELLS), (2, 203)] {
            assert!(app.map.open_top_other(&[]));
            laid_out(&mut app);
            assert_eq!(app.map.other_depth(&[]), depth);
            assert_eq!(shown(&app), cells, "level {depth}");
        }
        assert!(!app.map.open_top_other(&[]));
        app.chart_parent_folder();
        laid_out(&mut app);
        assert_eq!(app.map.other_depth(&[]), 1);
        app.chart_parent_folder();
        laid_out(&mut app);
        assert_eq!((app.map.other_depth(&[]), shown(&app)), (0, MAX_CELLS));
        assert_eq!(app.current_view(), &Vec::<usize>::new());
    }
}
