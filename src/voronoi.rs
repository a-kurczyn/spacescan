//! The Voronoi map: the folder shown as a region split into cells whose
//! areas match their items' sizes, folders split again inside, a few levels
//! deep. Each level is a power diagram: every item has a site and a weight,
//! and a point belongs to the item whose site is nearest once the weights
//! are taken off. Sites and weights are adjusted until the cell areas match
//! the sizes (Nocaj and Brandes, 2012).
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
    (0..sites.len())
        .map(|i| {
            let mut cell = bounds.to_vec();
            for j in 0..sites.len() {
                if i == j || cell.is_empty() {
                    continue;
                }
                // Points nearer i than j, weights taken off:
                // 2 (sj - si) · x <= |sj|² - |si|² - wj + wi
                let (si, sj) = (sites[i].to_vec2(), sites[j].to_vec2());
                let normal = (sj - si) * 2.0;
                if normal.length_sq() < 1e-12 {
                    // Same place: the heavier one (or the first) keeps it.
                    if weights[j] > weights[i] || (weights[j] == weights[i] && j < i) {
                        cell.clear();
                    }
                    continue;
                }
                let limit = sj.length_sq() - si.length_sq() - weights[j] + weights[i];
                cell = clip(&cell, normal, limit);
            }
            cell
        })
        .collect()
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

/// Most rounds of adjusting a level's sites and weights.
const MAX_ROUNDS: usize = 400;
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
    // New items start with half an average cell's weight.
    let fresh = total_area / n as f32 / 2.0;
    let (mut sites, mut weights): (Vec<Pos2>, Vec<f32>) = (0..n)
        .map(|i| match start.get(i).copied().flatten() {
            Some(p) => absolute(p, rect, total_area),
            None => (point_inside(bounds, seeds[i]), fresh),
        })
        .unzip();
    if n == 1 {
        return vec![relative(centroid(bounds), 0.0, rect, total_area)];
    }
    let mut cells = power_cells(bounds, &sites, &weights);
    // How strongly weights follow the area error; lowered when the error
    // grows, which damps back-and-forth swings.
    let mut strength = 1.0f32;
    let mut last_error = f32::INFINITY;
    for _ in 0..MAX_ROUNDS {
        // Sites move to their cells' centers...
        for i in 0..n {
            if area(&cells[i]) > 1e-6 {
                sites[i] = centroid(&cells[i]);
            }
        }
        limit_weights(&sites, &mut weights);
        cells = power_cells(bounds, &sites, &weights);
        // ...then each weight grows or shrinks by how far its cell's area is
        // from its share.
        for i in 0..n {
            let a = area(&cells[i]).max(targets[i] * 0.01);
            weights[i] = (weights[i] * (targets[i] / a).powf(strength)).max(1e-3);
        }
        limit_weights(&sites, &mut weights);
        cells = power_cells(bounds, &sites, &weights);
        let worst = (0..n)
            .map(|i| (area(&cells[i]) - targets[i]).abs() / targets[i].max(1e-6))
            .fold(0.0f32, f32::max);
        if worst < AREA_TOLERANCE {
            break;
        }
        // Damping follows the total error, which swings less than one cell's.
        let error = (0..n)
            .map(|i| (area(&cells[i]) - targets[i]).abs())
            .sum::<f32>();
        strength = if error > last_error {
            (strength * 0.8).max(0.25)
        } else {
            (strength * 1.05).min(1.0)
        };
        last_error = error;
    }
    (0..n)
        .map(|i| relative(sites[i], weights[i], rect, total_area))
        .collect()
}

/// Keeps every site inside its own cell: no weight may exceed another's
/// by the squared distance between their sites or more.
fn limit_weights(sites: &[Pos2], weights: &mut [f32]) {
    let n = sites.len();
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let room = (sites[i] - sites[j]).length_sq();
            let over = weights[i] - weights[j] - room;
            if over > 0.0 {
                weights[i] = (weights[i] - over * 1.01).max(1e-3);
            }
        }
    }
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
const MAX_CELLS: usize = 40;
/// How many levels the map shows at once.
const MAP_LEVELS: usize = 3;
/// Smallest area (square points) an item's cell may have; smaller items
/// share the "other" cell.
const MIN_CELL_AREA: f32 = 160.0;
/// Smallest area of a folder's cell for the items inside to be shown.
const MIN_SPLIT_AREA: f32 = 5000.0;
/// The gap between a folder's cell and the cells inside it.
const NEST_GAP: f32 = 3.0;

/// An item of the map: its slice data (as for the sunburst), its weight
/// and, for a folder shown split, its items.
pub(crate) struct MapItem {
    pub(crate) seg: Segment,
    weight: f64,
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
/// will share about `area` square points.
pub(crate) fn map_tree(
    view: &Node,
    only: Option<&[usize]>,
    opts: LayoutOpts,
    area: f32,
) -> MapTree {
    MapTree {
        items: map_items(view, only, &[], 0, area, opts),
    }
}

/// The items of `node` (at `idx_path` from the view), largest first, that
/// get a cell of their own in `area`, then "other" for the rest.
fn map_items(
    node: &Node,
    only: Option<&[usize]>,
    idx_path: &[usize],
    depth: usize,
    area: f32,
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
    let share = |w: u64| area * (w as f64 / total as f64) as f32;
    let shown = visible
        .iter()
        .take(MAX_CELLS)
        .take_while(|(_, c)| share(weight(c)) >= MIN_CELL_AREA)
        .count()
        // With "other" taking a cell, one item alone isn't worth a split.
        .max(visible.len().min(1));
    let mut items: Vec<MapItem> = visible[..shown]
        .iter()
        .map(|&(i, c)| {
            let mut path = idx_path.to_vec();
            path.push(i);
            let child_area = share(weight(c));
            let children = if c.is_dir && depth + 1 < MAP_LEVELS && child_area >= MIN_SPLIT_AREA {
                map_items(c, None, &path, depth + 1, child_area, opts)
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
                weight: weight(c) as f64,
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
            key: OTHER_KEY.into(),
            children: Vec::new(),
        });
    }
    items
}

/// Where an item is drawn: from its folder's key and its name.
type PlaceOf<'a> = &'a dyn Fn(&[Box<str>], &str) -> Option<Place>;

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
    let sizes: Vec<f64> = items.iter().map(|i| i.weight).collect();
    let seeds: Vec<u64> = items.iter().map(|i| name_seed(&i.key)).collect();
    let start: Vec<Option<Place>> = items
        .iter()
        .map(|i| before.and_then(|b| b.get(&i.key)).copied())
        .collect();
    let places = place_items(bounds, &sizes, &seeds, &start);
    let cells = cells_of(bounds, &places);
    for (item, cell) in items.iter().zip(&cells) {
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
    let rect = egui::Rect::from_points(bounds);
    let places: Vec<Place> = items
        .iter()
        .map(|i| {
            place_of(key, &i.key).unwrap_or_else(|| {
                let p = point_inside(bounds, name_seed(&i.key));
                relative(p, 0.0, rect, area(bounds))
            })
        })
        .collect();
    let cells = cells_of(bounds, &places);
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

/// The width of the strip left of the map.
const STRIP_WIDTH: f32 = 190.0;

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
            let pad = ui.allocate_ui_with_layout(
                Vec2::new(STRIP_WIDTH, size.y),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_min_size(Vec2::new(STRIP_WIDTH, size.y));
                    ui.set_max_width(STRIP_WIDTH);
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
                    .wrap(),
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
            self.map.next_tree = Some(Arc::new(map_tree(view_node, only, opts, area(&bounds))));
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

    /// Items too small for a cell share "other", which knows which they
    /// are; hidden and empty items get none; folders are split only a few
    /// levels deep and only when there's room.
    #[test]
    fn items_get_cells_by_size() {
        let mut files: Vec<Node> = (0..60)
            .map(|i| test_node(&format!("/t/small{i}"), 10, false, vec![]))
            .collect();
        files.push(test_node("/t/big", 100_000, false, vec![]));
        files.push(test_node("/t/hidden", 50_000, false, vec![]));
        files.push(test_node("/t/empty", 0, false, vec![]));
        let deep = test_node(
            "/t/d1",
            100_000,
            true,
            vec![test_node(
                "/t/d1/d2",
                100_000,
                true,
                vec![test_node(
                    "/t/d1/d2/d3",
                    100_000,
                    true,
                    vec![test_node("/t/d1/d2/d3/f", 100_000, false, vec![])],
                )],
            )],
        );
        files.push(deep);
        let root = test_node("/t", 0, true, files);
        let hidden: HashSet<PathBuf> = [PathBuf::from("/t/hidden")].into();
        let settings = Settings::default();
        let tree = map_tree(&root, None, opts(&hidden, &settings), 800.0 * 600.0);
        let names: Vec<&str> = tree.items.iter().map(|i| &*i.key).collect();
        assert_eq!(names, ["big", "d1", OTHER_KEY]);
        let other = &tree.items[2].seg;
        assert!(other.is_other && other.rest.len() == 60);
        assert_eq!(other.idx_path, [OTHER_MARKER]);
        // d1 → d2 → d3: three levels, the third not split.
        let d2 = &tree.items[1].children[0];
        assert_eq!(d2.seg.idx_path, [60 + 3, 0]);
        assert!(d2.children.len() == 1 && d2.children[0].children.is_empty());
        // In a small area, folders aren't split at all.
        let tree = map_tree(&root, None, opts(&hidden, &settings), 3000.0);
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
        let (hidden, settings) = (HashSet::new(), Settings::default());
        let bounds = rect_poly(egui::Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(900.0, 600.0),
        ));
        let tree = map_tree(&root, None, opts(&hidden, &settings), area(&bounds));
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
        let mut files: Vec<Node> = (0..100)
            .map(|i| test_node(&format!("/t/s{i:03}"), 10, false, vec![]))
            .collect();
        files.push(test_node("/t/big", 10_000_000, false, vec![]));
        app.full_root = Some(Arc::new(test_node("/t", 10_001_000, true, files)));
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
            for _ in 0..300 {
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
        assert_eq!(shown(&app), 2);
        for (depth, cells) in [
            (1, MAX_CELLS + 1),
            (2, MAX_CELLS + 1),
            (3, 100 - 2 * MAX_CELLS),
        ] {
            assert!(app.map.open_top_other(&[]));
            laid_out(&mut app);
            assert_eq!(app.map.other_depth(&[]), depth);
            assert_eq!(shown(&app), cells, "level {depth}");
        }
        assert!(!app.map.open_top_other(&[]) || shown(&app) == 0);
        app.chart_parent_folder();
        laid_out(&mut app);
        assert_eq!(app.map.other_depth(&[]), 2);
        app.chart_parent_folder();
        app.chart_parent_folder();
        laid_out(&mut app);
        assert_eq!((app.map.other_depth(&[]), shown(&app)), (0, 2));
        assert_eq!(app.current_view(), &Vec::<usize>::new());
    }
}
