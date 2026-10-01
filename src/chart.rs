//! The sunburst chart: layout, colors and drawing.

use super::*;

/// The node at `idx_path` (child indices from `root`), stopping early at an
/// index that doesn't exist.
pub(crate) fn get_node<'a>(root: &'a Node, idx_path: &[usize]) -> &'a Node {
    let mut n = root;
    for &i in idx_path {
        if i < n.children.len() {
            n = &n.children[i];
        }
    }
    n
}

/// One slice of the sunburst.
pub(crate) struct Segment {
    pub(crate) idx_path: Vec<usize>,
    pub(crate) start_angle: f32,
    pub(crate) end_angle: f32,
    pub(crate) ring: usize,
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) file_count: u64,
    pub(crate) is_dir: bool,
    pub(crate) is_other: bool,
    pub(crate) is_free: bool,
    pub(crate) mode: Option<u32>,
    /// For an "other" slice: indices of the children it groups.
    pub(crate) rest: Vec<usize>,
}

/// Slice highlighted in the chart: `rel` is relative to the view `view`.
pub(crate) struct ChartSel {
    pub(crate) view: Vec<usize>,
    pub(crate) rel: Vec<usize>,
}

/// Last entry of an "other" slice's `idx_path` (it has no child index).
pub(crate) const OTHER_MARKER: usize = usize::MAX;

/// True if `idx_path` is an "other" slice's.
pub(crate) fn is_other_marker(idx_path: &[usize]) -> bool {
    idx_path.last() == Some(&OTHER_MARKER)
}

/// The node at `idx_path`, or None if the path no longer exists.
pub(crate) fn try_get_node<'a>(root: &'a Node, idx_path: &[usize]) -> Option<&'a Node> {
    idx_path.iter().try_fold(root, |n, &i| n.children.get(i))
}

/// A step of the arrow keys, the same in the chart and the table:
/// ⬆⬇ previous / next item, ➡ in (a level deeper), ⬅ out (a level up).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NavDir {
    Prev,
    Next,
    In,
    Out,
}

/// The arrow key pressed this frame (without Ctrl/Alt), as a step.
pub(crate) fn arrow_nav(i: &egui::InputState) -> Option<NavDir> {
    if i.modifiers.command || i.modifiers.alt {
        return None;
    }
    [
        (egui::Key::ArrowUp, NavDir::Prev),
        (egui::Key::ArrowDown, NavDir::Next),
        (egui::Key::ArrowRight, NavDir::In),
        (egui::Key::ArrowLeft, NavDir::Out),
    ]
    .into_iter()
    .find(|(k, _)| i.key_pressed(*k))
    .map(|(_, d)| d)
}

/// Clockwise order of slices around each ring of the sunburst.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ChartOrder {
    /// Largest first (how children are stored).
    Size,
    /// Alphabetical, case-insensitive.
    Name,
}

/// Natural name order ("file2" before "file10").
pub(crate) fn cmp_names(a: &Node, b: &Node) -> std::cmp::Ordering {
    natural_cmp(&a.name, &b.name)
}

/// What the chart layout needs besides the tree.
#[derive(Clone, Copy)]
pub(crate) struct LayoutOpts<'a> {
    /// Items hidden from the chart.
    pub(crate) hidden: &'a HashSet<PathBuf>,
    pub(crate) settings: &'a Settings,
    pub(crate) order: ChartOrder,
}

/// The slices of the chart of `node`, around the full circle. `free` is the
/// drive's (capacity, free bytes), shown as a free-space slice.
pub(crate) fn layout_sunburst(
    node: &Node,
    free: Option<(u64, u64)>,
    opts: LayoutOpts,
) -> Vec<Segment> {
    let mut out = Vec::new();
    layout_ring(
        node,
        vec![],
        (0.0, std::f32::consts::TAU),
        0,
        free.unwrap_or((0, 0)),
        opts,
        &mut out,
    );
    out
}

/// Lays out ring `ring` (and the rings beyond it) for `node`'s children
/// between two angles, appending one Segment per slice to `out`. On the
/// first ring, free space (`free` = capacity, free bytes) takes its share
/// at the end.
fn layout_ring(
    node: &Node,
    idx_path: Vec<usize>,
    (start_angle, end_angle): (f32, f32),
    ring: usize,
    (total_capacity, extra_free_bytes): (u64, u64),
    opts: LayoutOpts,
    out: &mut Vec<Segment>,
) {
    let LayoutOpts {
        hidden,
        settings,
        order,
    } = opts;
    if ring >= settings.max_render_depth {
        return;
    }
    let visible_children: Vec<(usize, &Node)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| !hidden.contains(&c.path))
        .collect();

    // Free space takes its share of the drive's capacity; the content found
    // so far fills the rest of the ring (also while a scan is running).
    let full_span = end_angle - start_angle;
    let content_end_angle = if extra_free_bytes > 0 && total_capacity > 0 {
        let free_frac = extra_free_bytes as f32 / total_capacity as f32;
        start_angle + full_span * (1.0 - free_frac)
    } else {
        end_angle
    };

    let span_abs = (content_end_angle - start_angle).abs().max(0.0001);
    let min_frac = (settings.min_segment_angle_deg.to_radians() / span_abs).max(0.0);

    // How many children (largest first) get their own slice. The ring has
    // room for span / min-angle slices: if all children fit, each gets one;
    // otherwise the largest fill all slots but the last, which is "other".
    // `unlimited_slices` shows every child.
    let split = if settings.unlimited_slices {
        visible_children.len()
    } else {
        // At most 360 slices per full ring, whatever the min angle.
        let slots = ((span_abs / settings.min_segment_angle_deg.to_radians().max(1e-6)) as usize)
            .min((360.0 * span_abs / std::f32::consts::TAU) as usize);
        let n = if visible_children.len() <= slots {
            visible_children.len()
        } else {
            slots.saturating_sub(1)
        };
        // Shown slices are stretched to fill the ring. Stop at the first
        // child that would still be drawn narrower than the min angle;
        // "other" takes it and the rest.
        let n = n.min(settings.max_children_shown);
        let mut shown_sum = 0u64;
        visible_children
            .iter()
            .take(n)
            .enumerate()
            .take_while(|(k, (_, c))| {
                shown_sum = shown_sum.saturating_add(c.size);
                let room = if k + 1 < visible_children.len() {
                    1.0 - min_frac.min(0.5)
                } else {
                    1.0
                };
                c.size > 0 && (c.size as f32 / shown_sum as f32) * room >= min_frac * 0.999
            })
            .count()
    };
    let mut shown: Vec<(usize, &Node)> = visible_children.iter().take(split).cloned().collect();
    // A–Z order only changes the drawing order; "other" stays last.
    if order == ChartOrder::Name {
        shown.sort_by(|(_, a), (_, b)| cmp_names(a, b));
    }
    let rest: Vec<(usize, &Node)> = visible_children.iter().skip(split).cloned().collect();
    let rest_size: u64 = rest
        .iter()
        .map(|(_, c)| c.size)
        .fold(0u64, u64::saturating_add);

    // "Other" is one min-angle slot; the shown slices split the rest in
    // proportion to their sizes.
    let other_frac = if rest_size > 0 {
        min_frac.min(0.5)
    } else {
        0.0
    };
    let available_frac = (1.0 - other_frac).max(0.0);
    let shown_total = shown
        .iter()
        .map(|(_, c)| c.size as f32)
        .sum::<f32>()
        .max(1.0);

    let span = content_end_angle - start_angle;
    let mut cursor = start_angle;

    for (i, child) in &shown {
        let frac = (child.size as f32 / shown_total) * available_frac;
        let a0 = cursor;
        let a1 = cursor + span * frac;
        cursor = a1;
        let mut cp = idx_path.clone();
        cp.push(*i);
        out.push(Segment {
            idx_path: cp.clone(),
            start_angle: a0,
            end_angle: a1,
            ring,
            name: child.name.clone(),
            size: child.size,
            file_count: child.file_count,
            is_dir: child.is_dir,
            is_other: false,
            is_free: false,
            mode: Some(child.mode),
            rest: Vec::new(),
        });
        if child.is_dir && !child.children.is_empty() {
            layout_ring(child, cp, (a0, a1), ring + 1, (0, 0), opts, out);
        }
    }
    if rest_size > 0 {
        // Ends exactly at content_end_angle, so rounding leaves no gap.
        let a0 = cursor;
        let a1 = content_end_angle;
        // OTHER_MARKER in place of a child index keeps its idx_path as long
        // as its siblings', so arrow keys move onto it like any slice.
        let mut other_path = idx_path.clone();
        other_path.push(OTHER_MARKER);
        out.push(Segment {
            idx_path: other_path,
            start_angle: a0,
            end_angle: a1,
            ring,
            name: trf("SEG_OTHER_ITEMS", &[&rest.len().to_string()]),
            size: rest_size,
            file_count: rest.iter().map(|(_, c)| c.file_count).sum(),
            is_dir: false,
            is_other: true,
            is_free: false,
            mode: None,
            rest: rest.iter().map(|(i, _)| *i).collect(),
        });
    }
    if extra_free_bytes > 0 {
        out.push(Segment {
            idx_path: idx_path.clone(),
            start_angle: content_end_angle,
            end_angle,
            ring,
            name: tr("SEG_FREE_SPACE"),
            size: extra_free_bytes,
            file_count: 0,
            is_dir: false,
            is_other: false,
            is_free: true,
            mode: None,
            rest: Vec::new(),
        });
    }
}

/// Hue (degrees) of top-level slice `i`: golden-angle steps, so neighbors
/// differ clearly.
pub(crate) fn hue_for_branch(i: usize) -> f32 {
    ((i as f32) * 137.50776_f32) % 360.0
}

/// HSV (hue in degrees, saturation and value 0–1) as a color.
pub(crate) fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color32 {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = if hp < 1.0 {
        (c, x, 0.0)
    } else if hp < 2.0 {
        (x, c, 0.0)
    } else if hp < 3.0 {
        (0.0, c, x)
    } else if hp < 4.0 {
        (0.0, x, c)
    } else if hp < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = v - c;
    Color32::from_rgb(
        (((r1 + m) * 255.0) as i32).clamp(0, 255) as u8,
        (((g1 + m) * 255.0) as i32).clamp(0, 255) as u8,
        (((b1 + m) * 255.0) as i32).clamp(0, 255) as u8,
    )
}

/// `c` with a gamma curve applied: gamma < 1 lightens it.
pub(crate) fn gamma_lighten(c: Color32, gamma: f32) -> Color32 {
    let f = |v: u8| {
        ((v as f32 / 255.0).powf(gamma) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Color of the free-space slice: the panel background, lightened by
/// `gamma` on a dark theme and darkened by as much on a light one.
pub(crate) fn free_space_color(visuals: &egui::Visuals, gamma: f32) -> Color32 {
    let gamma = if visuals.dark_mode {
        gamma
    } else {
        1.0 / gamma.max(0.01)
    };
    gamma_lighten(visuals.panel_fill, gamma)
}

/// Folder name and size in the center hub, in `color`, wrapped to fit
/// inside it.
pub(crate) fn draw_hub_text(
    painter: &egui::Painter,
    center: Pos2,
    hub_radius: f32,
    name: &str,
    size: u64,
    color: Color32,
) {
    // About the width of a rectangle inside the circle, with a margin.
    let wrap_width = (hub_radius * 1.3).max(24.0);

    let display_name = if name.is_empty() { "/" } else { name };
    let name_job = egui::text::LayoutJob::simple(
        display_name.to_string(),
        egui::FontId::proportional(14.0),
        color,
        wrap_width,
    );
    let name_galley = painter.layout_job(name_job);

    let size_job = egui::text::LayoutJob::simple(
        human_size(size),
        egui::FontId::proportional(18.0),
        color,
        wrap_width,
    );
    let size_galley = painter.layout_job(size_job);

    let gap = 4.0;
    let total_height = name_galley.size().y + gap + size_galley.size().y;
    let top = center.y - total_height / 2.0;

    let name_pos = Pos2::new(center.x - name_galley.size().x / 2.0, top);
    painter.galley(name_pos, name_galley.clone(), color);

    let size_pos = Pos2::new(
        center.x - size_galley.size().x / 2.0,
        top + name_galley.size().y + gap,
    );
    painter.galley(size_pos, size_galley.clone(), color);
}

/// Inner and outer radius of ring `ring`.
pub(crate) fn ring_radii(ring: usize, hub_radius: f32, ring_thickness: f32) -> (f32, f32) {
    let r0 = hub_radius + ring_thickness * ring as f32;
    (r0, r0 + ring_thickness)
}

/// Unit vector at angle `t`: 0 points straight up, angles grow clockwise.
pub(crate) fn arc_dir(t: f32) -> Vec2 {
    let (s, c) = t.sin_cos();
    Vec2::new(s, -c)
}

/// Outline of a slice between two radii and two angles: the outer arc
/// forward, then the inner arc back, `steps` segments each.
fn slice_outline(
    center: Pos2,
    (r0, r1): (f32, f32),
    (a0, a1): (f32, f32),
    steps: usize,
) -> Vec<Pos2> {
    let arc = |r: f32| {
        (0..=steps).map(move |i| center + arc_dir(a0 + (a1 - a0) * (i as f32 / steps as f32)) * r)
    };
    let mut pts: Vec<Pos2> = arc(r1).collect();
    let mut inner: Vec<Pos2> = arc(r0).collect();
    inner.reverse();
    pts.extend(inner);
    pts
}

/// Draws the outline of one slice.
pub(crate) fn draw_arc_outline(
    painter: &egui::Painter,
    center: Pos2,
    radii: (f32, f32),
    angles: (f32, f32),
    stroke: egui::Stroke,
) {
    let steps =
        (((angles.1 - angles.0).abs() * radii.1.max(1.0) / 3.0).ceil() as usize).clamp(1, 512);
    painter.add(egui::Shape::closed_line(
        slice_outline(center, radii, angles, steps),
        stroke,
    ));
}

/// Draws one slice, filled with `color` and with a thin dark border.
pub(crate) fn draw_arc_mesh(
    painter: &egui::Painter,
    center: Pos2,
    radii: (f32, f32),
    angles: (f32, f32),
    color: Color32,
    settings: &Settings,
) {
    let ((r0, r1), (a0, a1)) = (radii, angles);
    let span = (a1 - a0).abs();
    // One segment every few pixels of the outer arc, so curves stay smooth.
    let arc_len_px = span * r1.max(1.0);
    let px_per_step = settings.tess_px_per_step.max(0.5);
    let steps = ((arc_len_px / px_per_step).ceil() as usize).clamp(1, 512);

    let mut mesh = egui::epaint::Mesh::default();
    let base = mesh.vertices.len() as u32;
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        let dir = arc_dir(t);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + dir * r0,
            uv: egui::epaint::WHITE_UV,
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + dir * r1,
            uv: egui::epaint::WHITE_UV,
            color,
        });
    }
    for i in 0..steps as u32 {
        let i0 = base + i * 2;
        let i1 = i0 + 1;
        let i2 = i0 + 2;
        let i3 = i0 + 3;
        mesh.indices.extend_from_slice(&[i0, i1, i2, i1, i3, i2]);
    }
    painter.add(egui::Shape::mesh(mesh));

    let stroke = egui::Stroke::new(
        settings.stroke_width,
        Color32::from_black_alpha(settings.stroke_alpha),
    );
    painter.add(egui::Shape::closed_line(
        slice_outline(center, radii, angles, steps),
        stroke,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(root: &Node, span: f32) -> Vec<Segment> {
        layout_with(root, span, &Settings::default())
    }

    fn layout_with(root: &Node, span: f32, settings: &Settings) -> Vec<Segment> {
        let mut segs = Vec::new();
        let opts = LayoutOpts {
            hidden: &HashSet::new(),
            settings,
            order: ChartOrder::Size,
        };
        layout_ring(root, vec![], (0.0, span), 0, (0, 0), opts, &mut segs);
        segs
    }

    /// Similar-sized items (movies of 171, 158, 156, … GB among 1199) get
    /// comparable slices; "other" takes one min-width slot at the end.
    #[test]
    fn similar_items_fill_the_ring_slots() {
        let mut kids: Vec<Node> = [171u64, 158, 156, 150]
            .iter()
            .enumerate()
            .map(|(i, &gb)| test_node(&format!("/m/top{i}"), gb, false, vec![]))
            .collect();
        kids.extend((0..1195).map(|i| test_node(&format!("/m/{i}"), 30, false, vec![])));
        let root = test_node("/m", kids.iter().map(|k| k.size).sum(), true, kids);
        let settings = Settings {
            min_segment_angle_deg: 1.0,
            max_children_shown: 360,
            ..Settings::default()
        };
        let span = 160f32.to_radians();
        let segs = layout_with(&root, span, &settings);
        let shown: Vec<&Segment> = segs.iter().filter(|s| !s.is_other).collect();
        // 159 slots, but the 30-GB movies would be drawn under 1° by then.
        let w = |s: &Segment| s.end_angle - s.start_angle;
        assert!(
            shown.len() > 100 && shown.len() < 159,
            "{} shown",
            shown.len()
        );
        assert!(shown.iter().all(|s| w(s) >= 1f32.to_radians() * 0.999));
        let (a, b) = (w(shown[0]), w(shown[1]));
        assert!(
            (a / b - 171.0 / 158.0).abs() < 1e-3,
            "171 GB vs 158 GB drawn {a} vs {b}"
        );
        let other = segs.iter().find(|s| s.is_other).unwrap();
        assert!((w(other) - 1f32.to_radians()).abs() < 1e-4);
        assert!((other.end_angle - span).abs() < 1e-4);
    }

    /// A few big items and a long tail of small ones: no slice is drawn
    /// narrower than the min angle; the thin tail goes into "other".
    #[test]
    fn uneven_tail_goes_to_other() {
        let mut kids: Vec<Node> = (0..10)
            .map(|i| test_node(&format!("/m/big{i}"), 100, false, vec![]))
            .collect();
        kids.extend((0..100).map(|i| test_node(&format!("/m/{i}"), 10, false, vec![])));
        let root = test_node("/m", 2000, true, kids);
        let settings = Settings::default();
        let segs = layout(&root, 120f32.to_radians());
        let min = settings.min_segment_angle_deg.to_radians();
        assert!(
            segs.iter()
                .all(|s| s.end_angle - s.start_angle >= min * 0.999)
        );
        assert!(segs.iter().any(|s| s.is_other));
        assert!(segs.iter().filter(|s| !s.is_other).count() > 10);
    }

    /// When every child fits, there's no "other" at all.
    #[test]
    fn no_other_when_everything_fits() {
        let root = test_node(
            "/m",
            6,
            true,
            (1..=3)
                .map(|i| test_node(&format!("/m/{i}"), i, false, vec![]))
                .collect(),
        );
        let segs = layout(&root, std::f32::consts::TAU);
        assert_eq!(segs.len(), 3);
        assert!(segs.iter().all(|s| !s.is_other));
    }

    /// Too many children for the slots: "other" gets one min-width slot.
    #[test]
    fn overflow_goes_to_a_min_width_other() {
        let settings = Settings::default();
        let n = 1000;
        let root = test_node(
            "/m",
            n,
            true,
            (0..n)
                .map(|i| test_node(&format!("/m/{i}"), 1, false, vec![]))
                .collect(),
        );
        let segs = layout(&root, std::f32::consts::TAU);
        let other = segs.iter().find(|s| s.is_other).unwrap();
        let min = settings.min_segment_angle_deg.to_radians();
        assert!((other.end_angle - other.start_angle - min).abs() < 1e-4);
        assert_eq!(segs.len() - 1, settings.max_children_shown);
    }

    /// Never more than 360 slices around a full ring, even at a tiny min angle.
    #[test]
    fn at_most_360_slices_per_ring() {
        let settings = Settings {
            min_segment_angle_deg: 0.1,
            max_children_shown: usize::MAX,
            ..Settings::default()
        };
        let n = 5000;
        let root = test_node(
            "/m",
            n,
            true,
            (0..n)
                .map(|i| test_node(&format!("/m/{i}"), 1, false, vec![]))
                .collect(),
        );
        let segs = layout_with(&root, std::f32::consts::TAU, &settings);
        assert_eq!(segs.len(), 360);
    }
}
