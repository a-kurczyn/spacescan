# Sankey view experiment — lessons learned

An alternate disk-usage view was explored: a Sankey-diagram style layout
(columns = depth rings, folders as blocks, curved ribbons connecting parent
to child, sized to disk usage), as an alternative to the sunburst. The
experiment was reverted (see bottom), but the algorithmic lessons are worth
keeping for next time.

## What actually worked

### 1. It's a strict tree, not a general Sankey graph
A file/folder always has exactly one parent — two parents never point at the
same child. That means the classic d3-sankey algorithm (iterative left-right/
right-left relaxation, ~6 passes, needed to reconcile a node fed by multiple
flows) is unnecessary complexity. A single deterministic layout pass is
enough.

### 2. Curvature requires source ≠ target — literally
A ribbon drawn between an *identical* source and target y-range is
mathematically flat no matter what easing function draws it; two identical
endpoints cannot show curvature. The fix: compute two different y-ranges per
node.
- **source** = a tight, gapless proportional slice of the parent's span
  (sums exactly to the parent's height, byte-accurate).
- **target** = the node's own padded position among its siblings (gaps
  between blocks for legibility).

The divergence between these two ranges *is* the curve. This also gives a
clean way to reconcile "block height should reflect disk size" with "blocks
need breathing room for readable labels": source stays exactly proportional,
target gets a legibility floor.

### 3. Centering an ancestor on its children's combined span needs two passes
A naive top-down layout (parent's span fixed by its own parent, subdivide
forward) never revisits the parent once its children turn out to need more
room than it "deserves" by simple proportional slicing. Getting real
centering (an ancestor with a much taller subtree should sit vertically
centered on that subtree, not pinned flush to the top) requires:

- **Bottom-up** (`sankey_required_height`): each node's *required* height =
  `max(its own byte-proportional height, sum of children's required heights
  + padding)`. This only works without circular dependency on a parent's
  span because the proportional term uses one *fixed global* bytes-per-pixel
  scale, pinned once from the root — not a fraction of the immediate
  parent's own span.
- **Top-down** (`layout_sankey`): every node carries two separate ranges — a
  small **block** (the drawn rectangle, sized purely by byte count, never
  grown to fit descendants) and a larger **reserved** span (the bottom-up
  value, used only for stacking/centering children). A node's own block is
  centered within its reserved span. Recursing this all the way up means the
  root itself ends up centered on the full computed tree height, for free.

**Trap to avoid**: if the *same* value is used both for "this node's own
rendered size" and "the space its children need," the centering slack is
*always* mathematically zero (reserved height already *is* children-combined
by definition) — centering silently becomes a no-op. First attempt at this
looked like nothing had changed (screenshot showed every block filling its
column edge-to-edge with zero gaps) specifically because of this trap.

### 4. Node-count blowup is a global problem, not a per-parent one
A per-parent cap on children shown (like the sunburst's own per-parent
limit) still combinatorially explodes: dozens of independent parents each
showing up to their own limit multiplies out fast on a deep/wide tree —
produced a canvas literally thousands of pixels tall.

**Fix**: one *global* best-first-by-size budget (`select_sankey_nodes`) —
a priority queue (`BinaryHeap`) over the *whole* tree. Always expand the
single largest not-yet-shown item anywhere next; once the budget (e.g. 120
nodes) runs out, everything left unexpanded folds into its parent's
"(N other items)" bucket. This favors fully detailing a few genuinely large
branches over shallowly listing many small ones — and keeps the total
rendered node count (and thus canvas height) bounded regardless of how deep
or wide the real tree is.

### 5. The scalable answer is cap + click-to-navigate, not "render everything"
Tried dropping the budget entirely — folders-only, no aggregation, with
wheel-zoom + drag-pan to navigate the resulting (much larger) canvas.

Failed hard on any real-world folder with thousands of direct
subdirectories: vertical extent becomes enormous (thousands × min-height
floor + padding) while horizontal extent (bounded by max depth) stays tiny.
A single *uniform* zoom-to-fit crushed the columns into an unreadable
one-pixel sliver. Splitting into independent X/Y zoom factors fixed that
specific aspect-ratio symptom, but not the underlying issue: a real
filesystem can have hundreds of thousands of entries, and laying out /
hit-testing that many blocks every frame does not stay legible or fast
regardless of how you pan or zoom.

**What actually worked instead**: keep the global budget cap (§4) so every
single view is bounded and legible by construction, and make the "there's
more here" indicator itself the way to get more:
- Clicking a shown folder's block drills into it (pushes `view_stack`, the
  same mechanism the sunburst already uses for click-to-zoom).
- Clicking the "(N other items)" bucket **re-roots the whole view** at the
  folder that bucket belongs to — that folder's own children get a *fresh*
  budget instead of competing with the rest of the tree for the shared one.
- Clicking the root block goes back up a level (mirrors the sunburst's
  hub-click-to-go-up).

This is the right mental model for a bounded-budget diagram: don't try to
render an unbounded tree in one view — bound each view, and let navigation
(not zoom) be how you go deeper.

## Parameters that worked (starting points for next time)
- `node_width` = 150.0, `column_spacing` = 320.0, sibling `padding` = 18.0
- `min_node_height` = 34.0 (floor for a legible label)
- `SANKEY_NODE_BUDGET` = 120 (global, best-first-by-size)
- `global_scale` = viewport_height / root.size (bytes-per-pixel, pinned once
  from the root, never recomputed per-parent)

## Ideas for next time
- Make the budget a `Settings` field (tunable), not a hard-coded constant.
- A breadcrumb trail for the current Sankey root, since click-to-drill can
  nest quickly and there's currently no visible "path so far" in this view.
- A small always-uncapped "minimap" (folders-only, no labels) for overall
  orientation, alongside the budget-capped detailed view.
- Reconsider whether wheel-zoom/pan is worth keeping at all now that
  click-to-navigate exists for going deeper — it added real implementation
  complexity (independent X/Y zoom, cursor-anchored zoom math, viewport
  culling) without ever solving the core scalability problem on its own.
  If reintroduced, keep it secondary to navigation, not a substitute for it.
- The bottom-up/top-down centering algorithm and the tight-source/
  padded-target curvature technique (§2, §3) are solid and reusable
  regardless of whatever selection/navigation strategy comes next.

## Reverted implementation, for reference
Removed from `src/main.rs` when this experiment was shelved:
- Types/functions: `SankeyNode`, `select_sankey_nodes`,
  `sankey_required_height`, `layout_sankey`, `draw_sankey_ribbon`.
- `DiskScanApp` fields: `sankey_view`, `sankey_pan`, `sankey_zoom_x`,
  `sankey_zoom_y`, `sankey_fit_for`.
- The "Sankey view (experiment)" toggle button in the left panel and its
  render block in `ui()`.

The Icicle view (experiment) is unaffected by this revert.
