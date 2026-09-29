//! The sunburst chart: layout, colors and drawing.

use super::*;

pub(crate) fn get_node<'a>(root: &'a Node, idx_path: &[usize]) -> &'a Node {
    let mut n = root;
    for &i in idx_path {
        if i < n.children.len() {
            n = &n.children[i];
        }
    }
    n
}

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
}

/// Slice highlighted in the chart: `rel` is relative to the view `view`.
pub(crate) struct ChartSel {
    pub(crate) view: Vec<usize>,
    pub(crate) rel: Vec<usize>,
}

/// Trailing "index" tagging an "other"-bucket idx_path — see where it's
/// pushed in `layout_sunburst`.
pub(crate) const OTHER_MARKER: usize = usize::MAX;

pub(crate) fn is_other_marker(idx_path: &[usize]) -> bool {
    idx_path.last() == Some(&OTHER_MARKER)
}

/// Like get_node, but None instead of panicking on a stale index path.
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

pub(crate) fn cmp_names(a: &Node, b: &Node) -> std::cmp::Ordering {
    natural_cmp(&a.name, &b.name)
}

pub(crate) fn layout_sunburst(
    node: &Node,
    idx_path: Vec<usize>,
    start_angle: f32,
    end_angle: f32,
    ring: usize,
    hidden: &HashSet<PathBuf>,
    extra_free_bytes: u64,
    total_capacity: u64,
    settings: &Settings,
    order: ChartOrder,
    out: &mut Vec<Segment>,
) {
    if ring >= settings.max_render_depth {
        return;
    }
    let visible_children: Vec<(usize, &Node)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| !hidden.contains(&c.path))
        .collect();
    let real_total: u64 = visible_children.iter().map(|(_, c)| c.size).fold(0u64, u64::saturating_add);

    // Free space gets a *fixed* share of the span, from known filesystem
    // capacity — it doesn't grow or shrink as the scan progresses. Whatever
    // has been discovered so far always divides up the *entire* remaining
    // "content" span among itself (proportional to each other, not to some
    // eventual/unknown final total): otherwise there'd be an unrendered gap
    // between "what's mapped" and "known free space" while a scan is still
    // in progress, since discovered-so-far starts small and grows.
    let full_span = end_angle - start_angle;
    let content_end_angle = if extra_free_bytes > 0 && total_capacity > 0 {
        let free_frac = extra_free_bytes as f32 / total_capacity as f32;
        start_angle + full_span * (1.0 - free_frac)
    } else {
        end_angle
    };

    let total: u64 = real_total.max(1);
    let span_abs = (content_end_angle - start_angle).abs().max(0.0001);
    let min_frac = (settings.min_segment_angle_deg.to_radians() / span_abs).max(0.0);

    // Children are pre-sorted largest-first. Keep showing individual segments
    // only while they'd still be wide enough to render as a real slice; lump
    // the long tail of tiny ones into a single "other" bucket instead of
    // producing hundreds of sub-pixel slivers. Unless `unlimited_slices` is
    // on, in which case every child gets its own slice regardless — zoom
    // (Ctrl+wheel) is the only way to make sliver-thin ones clickable then.
    let mut split = 0;
    if settings.unlimited_slices {
        split = visible_children.len();
    } else {
        for (_, c) in &visible_children {
            if split >= settings.max_children_shown {
                break;
            }
            let frac = c.size as f32 / total as f32;
            if frac < min_frac {
                break;
            }
            split += 1;
        }
    }
    let mut shown: Vec<(usize, &Node)> = visible_children.iter().take(split).cloned().collect();
    // Which children get their own slice is always decided by size (above),
    // so A–Z order never pushes a big folder into "other"; only the drawing
    // order of the shown slices changes. "Other" stays last either way.
    if order == ChartOrder::Name {
        shown.sort_by(|(_, a), (_, b)| cmp_names(a, b));
    }
    let rest: Vec<(usize, &Node)> = visible_children.iter().skip(split).cloned().collect();
    let rest_size: u64 = rest.iter().map(|(_, c)| c.size).fold(0u64, u64::saturating_add);

    // "Other" gets its true share of the ring, never less than the
    // min-slice-angle (so a bucket of many tiny items stays visible and
    // clickable). The shown slices split whatever that leaves in
    // proportion to each other. A fixed-width "other" is wrong: in a folder
    // of 1200 similar-sized movies only the largest clears the threshold,
    // and it would be stretched over the whole ring while the other 1199
    // (nearly all the bytes) were squeezed into a sliver.
    let other_frac = if rest_size > 0 {
        (rest_size as f32 / total as f32).max(min_frac).min(1.0)
    } else {
        0.0
    };
    let available_frac = (1.0 - other_frac).max(0.0);
    let shown_total = shown.iter().map(|(_, c)| c.size as f32).sum::<f32>().max(1.0);

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
        });
        if child.is_dir && !child.children.is_empty() {
            layout_sunburst(child, cp, a0, a1, ring + 1, hidden, 0, 0, settings, order, out);
        }
    }
    if rest_size > 0 {
        // content_end_angle, not cursor + span*other_frac: the exact
        // remaining boundary, so there's no float-drift gap between the
        // last shown slice and "other".
        let a0 = cursor;
        let a1 = content_end_angle;
        // Tagged with OTHER_MARKER as a trailing "index" so its idx_path is
        // the same length as its visual sibling slices (real children get
        // idx_path + their own index appended) — that's what lets arrow-key
        // navigation treat it as a normal sibling to move between. It's not
        // a real child index (get_node silently no-ops on the out-of-range
        // lookup and resolves to the parent, which is what the hover
        // tooltip wants anyway); is_other_marker() is the strict check used
        // wherever code needs to tell it apart from an actual node.
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
        });
    }
    if extra_free_bytes > 0 {
        // Use the precomputed fixed boundary, not `cursor`, so there's no
        // float-drift gap between mapped content and the free-space slice.
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
        });
    }
}

pub(crate) fn hue_for_branch(i: usize) -> f32 {
    // evenly distributed hues using the golden angle
    ((i as f32) * 137.50776_f32) % 360.0
}

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

/// Brightens `c` with a gamma curve (gamma < 1 lightens) while keeping its
/// character, instead of flatly blending toward white.
pub(crate) fn gamma_lighten(c: Color32, gamma: f32) -> Color32 {
    let f = |v: u8| ((v as f32 / 255.0).powf(gamma) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

pub(crate) fn segment_color(seg: &Segment, top_branch_hue: f32, settings: &Settings) -> Color32 {
    if seg.is_other {
        // A pale tint of the *same* branch hue, so the aggregate bucket
        // reads as "more of this folder", not an unrelated color/glitch.
        return hsv_to_rgb(top_branch_hue, settings.other_sat, settings.other_val);
    }
    let val = (settings.ring_val_base - (seg.ring as f32) * settings.ring_val_falloff)
        .max(settings.ring_val_floor);
    hsv_to_rgb(top_branch_hue, settings.ring_sat, val)
}


pub(crate) fn draw_hub_text(painter: &egui::Painter, center: Pos2, hub_radius: f32, name: &str, size: u64) {
    // Width of a rectangle comfortably inscribed in the circle, with a
    // little margin so wrapped lines don't touch the ring.
    let wrap_width = (hub_radius * 1.3).max(24.0);

    let display_name = if name.is_empty() { "/" } else { name };
    let name_job = egui::text::LayoutJob::simple(
        display_name.to_string(),
        egui::FontId::proportional(14.0),
        Color32::WHITE,
        wrap_width,
    );
    let name_galley = painter.layout_job(name_job);

    let size_job = egui::text::LayoutJob::simple(
        human_size(size),
        egui::FontId::proportional(18.0),
        Color32::WHITE,
        wrap_width,
    );
    let size_galley = painter.layout_job(size_job);

    let gap = 4.0;
    let total_height = name_galley.size().y + gap + size_galley.size().y;
    let top = center.y - total_height / 2.0;

    let name_pos = Pos2::new(center.x - name_galley.size().x / 2.0, top);
    painter.galley(name_pos, name_galley.clone(), Color32::WHITE);

    let size_pos = Pos2::new(
        center.x - size_galley.size().x / 2.0,
        top + name_galley.size().y + gap,
    );
    painter.galley(size_pos, size_galley.clone(), Color32::WHITE);
}

/// Arc outline points, traced outer-arc-forward then inner-arc-backward,
/// suitable for both mesh fill (as a triangle strip) and a closed stroke.
pub(crate) fn arc_dir(t: f32) -> Vec2 {
    let (s, c) = t.sin_cos();
    Vec2::new(s, -c) // angle 0 = straight up, increasing clockwise
}

/// Outline of one sunburst slice (same geometry as draw_arc_mesh).
pub(crate) fn draw_arc_outline(painter: &egui::Painter, center: Pos2, r0: f32, r1: f32, a0: f32, a1: f32, stroke: egui::Stroke) {
    let steps = (((a1 - a0).abs() * r1.max(1.0) / 3.0).ceil() as usize).clamp(1, 512);
    let arc = |r: f32| (0..=steps).map(move |i| center + arc_dir(a0 + (a1 - a0) * (i as f32 / steps as f32)) * r);
    let mut pts: Vec<Pos2> = arc(r1).collect();
    let mut inner: Vec<Pos2> = arc(r0).collect();
    inner.reverse();
    pts.extend(inner);
    painter.add(egui::Shape::closed_line(pts, stroke));
}

pub(crate) fn draw_arc_mesh(
    painter: &egui::Painter,
    center: Pos2,
    r0: f32,
    r1: f32,
    a0: f32,
    a1: f32,
    color: Color32,
    settings: &Settings,
) {
    let span = (a1 - a0).abs();
    // Tessellate based on actual on-screen arc length (at the outer radius)
    // so outer rings stay smooth instead of getting faceted/pixelated.
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

    // Crisp, consistent border regardless of theme/background, instead
    // of relying on a radial gap that only sometimes shows through.
    let stroke = egui::Stroke::new(
        settings.stroke_width,
        Color32::from_black_alpha(settings.stroke_alpha),
    );
    let mut outline = Vec::with_capacity(2 * steps + 2);
    for i in 0..=steps {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        outline.push(center + arc_dir(t) * r1);
    }
    for i in (0..=steps).rev() {
        let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
        outline.push(center + arc_dir(t) * r0);
    }
    painter.add(egui::Shape::closed_line(outline, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of many similar items where only the largest clears the
    /// min-slice threshold: it must keep its true share, not the whole ring.
    #[test]
    fn other_bucket_keeps_its_true_share() {
        let mut kids = vec![test_node("/m/big", 171, false, vec![])];
        kids.extend((0..1198).map(|i| test_node(&format!("/m/{i}"), 30, false, vec![])));
        let root = test_node("/m", 171 + 1198 * 30, true, kids);
        let mut segs = Vec::new();
        let settings = Settings::default();
        let span = 160f32.to_radians();
        layout_sunburst(&root, vec![], 0.0, span, 0, &HashSet::new(), 0, 0, &settings, ChartOrder::Size, &mut segs);
        let width = |s: &Segment| (s.end_angle - s.start_angle) / span;
        let big = segs.iter().find(|s| s.name == "big").unwrap();
        let other = segs.iter().find(|s| s.is_other).unwrap();
        assert!(width(big) < 0.01, "biggest item drawn over {:.1}% of the ring", width(big) * 100.0);
        assert!(width(other) > 0.99);
        assert!((other.end_angle - span).abs() < 1e-4);
    }

    /// A handful of tiny leftovers still get a visible, clickable sliver.
    #[test]
    fn tiny_other_bucket_gets_min_width() {
        let root = test_node("/m", 1_000_001, true, vec![
            test_node("/m/a", 1_000_000, false, vec![]),
            test_node("/m/b", 1, false, vec![]),
        ]);
        let mut segs = Vec::new();
        let settings = Settings::default();
        layout_sunburst(&root, vec![], 0.0, std::f32::consts::TAU, 0, &HashSet::new(), 0, 0, &settings, ChartOrder::Size, &mut segs);
        let other = segs.iter().find(|s| s.is_other).unwrap();
        let min = settings.min_segment_angle_deg.to_radians();
        assert!(other.end_angle - other.start_angle >= min * 0.999);
    }
}
