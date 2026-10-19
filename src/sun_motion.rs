//! How the sunburst moves: slices glide to their new angles when the order,
//! a filter or the sizes change, and the chart zooms into a folder's slice
//! when it's opened (and back out to it when it's left).

use super::*;
use std::hash::{DefaultHasher, Hash, Hasher};
use voronoi::{ease_in_out, smoothstep};

/// How long a glide takes, in seconds.
const GLIDE_SECS: f32 = 0.25;
/// How long a zoom into or out of a folder takes, in seconds.
const ZOOM_SECS: f32 = 0.3;

/// The key of the item named `name` inside the item keyed `parent`. Keys
/// follow names from the scan's top, so an item keeps its key in a
/// filtered tree, where its index can differ.
pub(crate) fn child_key(parent: u64, name: &str) -> u64 {
    let mut h = DefaultHasher::new();
    parent.hash(&mut h);
    name.hash(&mut h);
    h.finish()
}

/// A slice as drawn: its item's key, its angles, its ring (between two
/// rings while moving) and its colors.
#[derive(Clone)]
pub(crate) struct DrawnSlice {
    pub(crate) key: u64,
    pub(crate) angles: (f32, f32),
    pub(crate) ring: f32,
    pub(crate) colors: (Color32, Color32),
}

impl DrawnSlice {
    fn middle(&self) -> f32 {
        (self.angles.0 + self.angles.1) / 2.0
    }

    /// This slice, of the chart of the folder `focus` is the slice of, in
    /// the chart `focus` is in.
    fn inside(&self, focus: &DrawnSlice) -> DrawnSlice {
        let (a, b) = focus.angles;
        let at = |t: f32| a + t / std::f32::consts::TAU * (b - a);
        DrawnSlice {
            angles: (at(self.angles.0), at(self.angles.1)),
            ring: self.ring + focus.ring + 1.0,
            ..self.clone()
        }
    }
}

/// The part of a chart a zoom shows: the angles from `a0` to `a1` spread
/// over the whole circle, and the rings moved in by `shift`.
#[derive(Clone, Copy)]
struct Camera {
    a0: f32,
    a1: f32,
    shift: f32,
}

impl Camera {
    const WHOLE: Camera = Camera {
        a0: 0.0,
        a1: std::f32::consts::TAU,
        shift: 0.0,
    };

    /// Showing the chart of the folder `focus` is the slice of.
    fn onto(focus: &DrawnSlice) -> Camera {
        Camera {
            a0: focus.angles.0,
            a1: focus.angles.1,
            shift: focus.ring + 1.0,
        }
    }

    fn lerp(self, to: Camera, t: f32) -> Camera {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Camera {
            a0: mix(self.a0, to.a0),
            a1: mix(self.a1, to.a1),
            shift: mix(self.shift, to.shift),
        }
    }

    /// Where `s` is seen; slices outside the angles are squeezed to the
    /// edge.
    fn show(self, s: &DrawnSlice) -> DrawnSlice {
        let tau = std::f32::consts::TAU;
        let span = (self.a1 - self.a0).max(1e-6);
        let angle = |t: f32| ((t - self.a0) / span * tau).clamp(0.0, tau);
        DrawnSlice {
            angles: (angle(s.angles.0), angle(s.angles.1)),
            ring: s.ring - self.shift,
            ..s.clone()
        }
    }
}

enum Motion {
    /// Each slice moves from where it was drawn (`from`) to its new place.
    Glide {
        from: Vec<DrawnSlice>,
        start: Instant,
    },
    /// Into the folder whose slice is `focus` (`inward`), or out to it.
    /// `outgoing` is the chart left, in the outer chart's terms.
    Zoom {
        outgoing: Vec<DrawnSlice>,
        focus: DrawnSlice,
        inward: bool,
        start: Instant,
    },
}

/// The sunburst's movement from one frame to the next.
#[derive(Default)]
pub(crate) struct SunMotion {
    /// The folder shown last frame (its names from the scan's top), the
    /// layout aimed at then, and the slices as drawn.
    view: Option<Vec<Box<str>>>,
    target: Vec<DrawnSlice>,
    drawn: Vec<DrawnSlice>,
    motion: Option<Motion>,
    /// The egui pass the chart was last drawn in.
    pass: u64,
}

impl SunMotion {
    /// The slices to draw this frame, each with its opacity, on the way to
    /// `target`, the layout of the folder `view`, in egui pass `pass`.
    /// Without `animate`, the target as it is; after passes without the
    /// chart (another view was shown), nothing moves from before. Also
    /// whether anything is still moving.
    pub(crate) fn frame(
        &mut self,
        view: &[Box<str>],
        view_key: u64,
        target: Vec<DrawnSlice>,
        animate: bool,
        pass: u64,
    ) -> (Vec<(DrawnSlice, f32)>, bool) {
        let now = Instant::now();
        if pass > self.pass + 1 {
            *self = SunMotion::default();
        }
        self.pass = pass;
        let old_view = self.view.replace(view.to_vec());
        if !animate {
            self.motion = None;
        } else if let Some(old) = old_view.filter(|old| old.as_slice() != view) {
            self.motion = self.zoom_between(&old, view, view_key, &target, now);
        } else if !same_places(&self.target, &target) && !self.target.is_empty() {
            self.motion = Some(Motion::Glide {
                from: std::mem::take(&mut self.drawn),
                start: now,
            });
        }
        let shown = match &self.motion {
            Some(Motion::Glide { from, start }) => {
                let t = start.elapsed().as_secs_f32() / GLIDE_SECS;
                (t < 1.0).then(|| glide(from, &target, ease_in_out(t)))
            }
            Some(Motion::Zoom {
                outgoing,
                focus,
                inward,
                start,
            }) => {
                let t = start.elapsed().as_secs_f32() / ZOOM_SECS;
                (t < 1.0).then(|| zoom(outgoing, focus, *inward, &target, ease_in_out(t)))
            }
            None => None,
        };
        let moving = shown.is_some();
        let shown = shown.unwrap_or_else(|| {
            self.motion = None;
            target.iter().map(|s| (s.clone(), 1.0)).collect()
        });
        self.drawn = shown.iter().map(|(s, _)| s.clone()).collect();
        self.target = target;
        (shown, moving)
    }

    /// The zoom from the chart of `old` to that of `new`, when one is the
    /// other's folder and the inner one's slice is in the outer chart.
    fn zoom_between(
        &self,
        old: &[Box<str>],
        new: &[Box<str>],
        new_key: u64,
        target: &[DrawnSlice],
        start: Instant,
    ) -> Option<Motion> {
        if new.len() > old.len() && new.starts_with(old) {
            let focus = self.target.iter().find(|s| s.key == new_key)?;
            Some(Motion::Zoom {
                outgoing: self.target.clone(),
                focus: focus.clone(),
                inward: true,
                start,
            })
        } else if old.len() > new.len() && old.starts_with(new) {
            let old_key = old.iter().fold(0, |k, name| child_key(k, name));
            let focus = target.iter().find(|s| s.key == old_key)?;
            Some(Motion::Zoom {
                outgoing: self.target.iter().map(|s| s.inside(focus)).collect(),
                focus: focus.clone(),
                inward: false,
                start,
            })
        } else {
            None
        }
    }
}

/// True if `a` and `b` have the same slices in the same places (their
/// colors may differ).
fn same_places(a: &[DrawnSlice], b: &[DrawnSlice]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.key == y.key && x.angles == y.angles && x.ring == y.ring)
}

/// Partway (`t`) from the slices `from` to `to`: a slice in both moves, a
/// new one opens from its middle, one that's gone closes to its middle and
/// fades.
fn glide(from: &[DrawnSlice], to: &[DrawnSlice], t: f32) -> Vec<(DrawnSlice, f32)> {
    let mix = |a: f32, b: f32| a + (b - a) * t;
    let before: HashMap<u64, &DrawnSlice> = from.iter().map(|s| (s.key, s)).collect();
    let after: HashSet<u64> = to.iter().map(|s| s.key).collect();
    let closing = from.iter().filter(|s| !after.contains(&s.key)).map(|s| {
        let m = s.middle();
        let angles = (mix(s.angles.0, m), mix(s.angles.1, m));
        (
            DrawnSlice {
                angles,
                ..s.clone()
            },
            1.0 - t,
        )
    });
    let moving = to.iter().map(|s| {
        let (a0, a1, ring) = match before.get(&s.key) {
            Some(b) => (b.angles.0, b.angles.1, b.ring),
            None => (s.middle(), s.middle(), s.ring),
        };
        let slice = DrawnSlice {
            angles: (mix(a0, s.angles.0), mix(a1, s.angles.1)),
            ring: mix(ring, s.ring),
            ..s.clone()
        };
        (slice, 1.0)
    });
    closing.chain(moving).collect()
}

/// Partway (`t`) through a zoom into the folder whose slice is `focus`
/// (`inward`), or out to it: the chart left fades under the one shown
/// (`target`), which fades in, both seen through a camera moving between
/// the whole outer chart and the folder's slice.
fn zoom(
    outgoing: &[DrawnSlice],
    focus: &DrawnSlice,
    inward: bool,
    target: &[DrawnSlice],
    t: f32,
) -> Vec<(DrawnSlice, f32)> {
    let (from, to) = (Camera::WHOLE, Camera::onto(focus));
    let camera = if inward {
        from.lerp(to, t)
    } else {
        to.lerp(from, t)
    };
    let fade_out = 1.0 - smoothstep(0.5, 1.0, t);
    let fade_in = smoothstep(0.0, 0.5, t);
    let left = outgoing.iter().map(|s| (camera.show(s), fade_out));
    let shown = target.iter().map(|s| {
        let s = if inward { s.inside(focus) } else { s.clone() };
        (camera.show(&s), fade_in)
    });
    left.chain(shown).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn slice(key: u64, angles: (f32, f32), ring: f32) -> DrawnSlice {
        DrawnSlice {
            key,
            angles,
            ring,
            colors: (Color32::RED, Color32::RED),
        }
    }

    /// A slice keeps moving from where it was drawn; a new one opens from
    /// its middle; one that's gone closes and fades.
    #[test]
    fn slices_glide() {
        let from = [slice(1, (0.0, 1.0), 0.0), slice(2, (1.0, 2.0), 0.0)];
        let to = [slice(1, (0.0, 3.0), 0.0), slice(3, (3.0, 4.0), 0.0)];
        let half = glide(&from, &to, 0.5);
        let get = |k| half.iter().find(|(s, _)| s.key == k).unwrap();
        assert_eq!(get(1).0.angles, (0.0, 2.0));
        assert_eq!(get(3).0.angles, (3.25, 3.75));
        assert_eq!(get(2).0.angles, (1.25, 1.75));
        assert_eq!(get(2).1, 0.5);
        let done = glide(&from, &to, 1.0);
        assert!(done.iter().any(|(s, o)| s.key == 2 && *o == 0.0));
    }

    /// Zooming in, the folder's slice ends as the whole circle one ring in,
    /// and the new chart ends where it's laid out.
    #[test]
    fn zoom_ends_on_the_new_chart() {
        let focus = slice(7, (TAU / 4.0, TAU / 2.0), 1.0);
        let inner = slice(8, (0.0, TAU / 2.0), 0.0);
        let start = zoom(
            std::slice::from_ref(&focus),
            &focus,
            true,
            std::slice::from_ref(&inner),
            0.0,
        );
        let end = zoom(
            std::slice::from_ref(&focus),
            &focus,
            true,
            std::slice::from_ref(&inner),
            1.0,
        );
        // At the start, the new chart's slice sits inside the folder's,
        // one ring out.
        let (s, opacity) = &start[1];
        assert_eq!(s.angles, (TAU / 4.0, TAU * 3.0 / 8.0));
        assert_eq!((s.ring, *opacity), (2.0, 0.0));
        // At the end: as laid out, the folder's slice spread over the
        // whole circle inside the hub.
        let (s, opacity) = &end[1];
        assert!((s.angles.0 - inner.angles.0).abs() < 1e-4);
        assert!((s.angles.1 - inner.angles.1).abs() < 1e-4);
        assert_eq!((s.ring, *opacity), (0.0, 1.0));
        assert_eq!(end[0].0.angles, (0.0, TAU));
        assert_eq!((end[0].0.ring, end[0].1), (-1.0, 0.0));
        // Zooming out runs the same way back.
        let back = zoom(
            &[inner.inside(&focus)],
            &focus,
            false,
            std::slice::from_ref(&focus),
            0.0,
        );
        assert!((back[0].0.angles.1 - inner.angles.1).abs() < 1e-4);
        assert_eq!(back[0].0.ring, 0.0);
    }

    /// A change of folder zooms only between a folder and one inside it
    /// whose slice was shown, and not across frames without the chart;
    /// with animations off, the target is drawn.
    #[test]
    fn zooms_only_into_shown_folders() {
        let names = |n: &[&str]| -> Vec<Box<str>> { n.iter().map(|s| (*s).into()).collect() };
        let key = |n: &[&str]| n.iter().fold(0, |k, name| child_key(k, name));
        let mut m = SunMotion::default();
        let top = names(&["t"]);
        let a = key(&["t", "a"]);
        m.frame(&top, key(&["t"]), vec![slice(a, (0.0, 1.0), 0.0)], true, 1);
        let (_, moving) = m.frame(
            &names(&["t", "a"]),
            a,
            vec![slice(9, (0.0, TAU), 0.0)],
            true,
            2,
        );
        assert!(moving && matches!(m.motion, Some(Motion::Zoom { inward: true, .. })));
        let (_, moving) = m.frame(&names(&["u"]), key(&["u"]), vec![], true, 3);
        assert!(!moving);
        let (shown, moving) = m.frame(&top, key(&["t"]), vec![slice(a, (0.0, 2.0), 0.0)], false, 4);
        assert!(!moving && shown.len() == 1 && shown[0].1 == 1.0);
        // Back from another view: no zoom from what was shown before it.
        let (_, moving) = m.frame(
            &names(&["t", "a"]),
            a,
            vec![slice(9, (0.0, TAU), 0.0)],
            true,
            9,
        );
        assert!(!moving);
    }
}
