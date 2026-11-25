use crate::theme::{CANVAS, LANES, SELECTED, TEXT_3, soft};
use gpui::{
    BorderStyle, Bounds, ContentMask, IntoElement, PathBuilder, Pixels, Point, Styled, Window, canvas, fill,
    linear_color_stop, linear_gradient, point, px, quad, rgb, size,
};

pub const LEFT: f32 = 16.;
pub const LANE: f32 = 14.;
const RIGHT: f32 = 22.;
const NARROWEST: f32 = 52.;
pub const LANE_CAP: usize = 12;
const FADE: f32 = 18.;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lanes {
    pub width: f32,
    pub offset: f32,
}

#[derive(Default)]
pub struct Graph {
    lanes: Vec<Option<String>>,
    rows: Vec<LaneRow>,
    columns: usize,
}

#[derive(Clone)]
struct LaneRow {
    node: usize,
    incoming: bool,
    through: Vec<usize>,
    outgoing: Vec<usize>,
}

impl Graph {
    fn vacant_lane(&mut self) -> usize {
        if let Some(index) = self.lanes.iter().position(Option::is_none) {
            index
        } else {
            self.lanes.push(None);
            self.lanes.len() - 1
        }
    }

    fn find(&self, oid: &str) -> Option<usize> {
        self.lanes
            .iter()
            .position(|slot| slot.as_deref() == Some(oid))
    }

    pub fn push(&mut self, oid: &str, parents: &[String]) {
        let pending = self.find(oid);
        let node = pending.unwrap_or_else(|| self.vacant_lane());
        self.lanes[node] = None;
        let through = self
            .lanes
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
            .collect();
        let mut outgoing = Vec::with_capacity(parents.len());
        for (i, parent) in parents.iter().enumerate() {
            let lane = match self.find(parent) {
                Some(existing) => existing,
                None if i == 0 => node,
                None => self.vacant_lane(),
            };
            self.lanes[lane] = Some(parent.clone());
            outgoing.push(lane);
        }
        self.columns = self.columns.max(self.lanes.len());
        self.rows.push(LaneRow {
            node,
            incoming: pending.is_some(),
            through,
            outgoing,
        });
        while self.lanes.last() == Some(&None) {
            self.lanes.pop();
        }
    }

    pub fn content_width(&self) -> f32 {
        LEFT + self.columns.saturating_sub(1) as f32 * LANE + RIGHT
    }

    pub fn auto_width(&self) -> f32 {
        self.content_width().clamp(NARROWEST, LEFT + (LANE_CAP - 1) as f32 * LANE + RIGHT)
    }

    pub fn node_x(&self, row: usize) -> Option<f32> {
        self.rows.get(row).map(|row| LEFT + row.node as f32 * LANE)
    }

    pub fn element(
        &self,
        row: usize,
        checked_out: bool,
        selected: bool,
        stash_above: bool,
        lanes: Lanes,
    ) -> impl IntoElement + use<> {
        let row = self.rows[row].clone();
        let content = self.content_width();
        let ground = if selected { SELECTED } else { CANVAS };
        canvas(
            |_, _, _| (),
            move |b, _, window, _| window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                let x = |lane: usize| b.left() + px(LEFT + lane as f32 * LANE - lanes.offset);
                let mid = b.center().y;
                let color = |lane: usize| rgb(LANES[lane % LANES.len()]);
                let straight = |lane: usize, y1, y2| {
                    fill(
                        Bounds::new(point(x(lane) - px(1.), y1), size(px(2.), y2 - y1)),
                        color(lane),
                    )
                };
                for lane in &row.through {
                    window.paint_quad(straight(*lane, b.top(), b.bottom()));
                }
                if row.incoming {
                    window.paint_quad(straight(row.node, b.top(), mid));
                } else if stash_above {
                    dashes(window, point(x(row.node) - px(1.), b.top()), mid - b.top(), false, LANES[row.node % LANES.len()]);
                }
                for parent in &row.outgoing {
                    if *parent == row.node {
                        window.paint_quad(straight(row.node, mid, b.bottom()));
                        continue;
                    }
                    let (from, to) = (point(x(row.node), mid), point(x(*parent), b.bottom()));
                    let bend = (b.bottom() - mid) * 0.7;
                    let mut path = PathBuilder::stroke(px(2.));
                    path.move_to(from);
                    path.cubic_bezier_to(
                        to,
                        point(from.x, from.y + bend),
                        point(to.x, to.y - bend),
                    );
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color(*parent));
                    }
                }
                let centre = point(x(row.node), mid);
                let disc = |radius: f32| {
                    Bounds::new(
                        centre - point(px(radius), px(radius)),
                        size(px(radius * 2.), px(radius * 2.)),
                    )
                };
                let ink = color(row.node);
                let disc_ground = rgb(ground);
                if checked_out {
                    window.paint_quad(quad(disc(6.5), px(6.5), disc_ground, px(2.), ink, BorderStyle::Solid));
                    window.paint_quad(fill(disc(2.5), ink).corner_radii(px(2.5)));
                } else if row.outgoing.len() > 1 {
                    window.paint_quad(quad(disc(4.5), px(4.5), disc_ground, px(2.), ink, BorderStyle::Solid));
                } else {
                    window.paint_quad(fill(disc(6.5), disc_ground).corner_radii(px(6.5)));
                    window.paint_quad(fill(disc(4.5), ink).corner_radii(px(4.5)));
                }
                fades(window, b, lanes, content, ground);
            }),
        )
        .w(px(lanes.width))
        .h_full()
        .flex_shrink_0()
    }

    pub fn stash_element(&self, base: usize, first: bool, selected: bool, lanes: Lanes) -> impl IntoElement + use<> {
        let row = self.rows[base].clone();
        let content = self.content_width();
        let ground = if selected { SELECTED } else { CANVAS };
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let x = |lane: usize| bounds.left() + px(LEFT + lane as f32 * LANE - lanes.offset);
                let mid = bounds.center().y;
                let marker = bounds.right() - px(10.);
                let stem = x(row.node);
                dashes(window, point(stem + px(1.), mid - px(1.)), marker - stem - px(4.), true, TEXT_3);
                for lane in row.through.iter().copied().chain(row.incoming.then_some(row.node)) {
                    window.paint_quad(fill(
                        Bounds::new(point(x(lane) - px(1.), bounds.top()), size(px(2.), bounds.size.height)),
                        rgb(LANES[lane % LANES.len()]),
                    ));
                }
                if !row.incoming {
                    let top = if first { mid } else { bounds.top() };
                    dashes(window, point(stem - px(1.), top), bounds.bottom() - top, false, LANES[row.node % LANES.len()]);
                }
                window.paint_quad(quad(
                    Bounds::new(point(marker - px(3.5), mid - px(3.5)), size(px(7.), px(7.))),
                    px(1.5),
                    rgb(ground),
                    px(1.),
                    rgb(TEXT_3),
                    BorderStyle::Solid,
                ));
                fades(window, bounds, lanes, content, ground);
            }),
        )
        .w(px(lanes.width))
        .h_full()
        .flex_shrink_0()
    }
}

pub fn hidden_sides(lanes: Lanes, content: f32) -> (bool, bool) {
    (lanes.offset > 0.5, lanes.offset + lanes.width < content - 0.5)
}

fn fades(window: &mut Window, bounds: Bounds<Pixels>, lanes: Lanes, content: f32, ground: u32) {
    let (left, right) = hidden_sides(lanes, content);
    let band = px(FADE.min(lanes.width / 3.));
    let (solid, clear) = (rgb(ground), soft(ground, 0));
    if left {
        let area = Bounds::new(bounds.origin, size(band, bounds.size.height));
        window.paint_quad(fill(area, linear_gradient(90., linear_color_stop(solid, 0.), linear_color_stop(clear, 1.))));
    }
    if right {
        let area = Bounds::new(point(bounds.right() - band, bounds.top()), size(band, bounds.size.height));
        window.paint_quad(fill(area, linear_gradient(90., linear_color_stop(clear, 0.), linear_color_stop(solid, 1.))));
    }
}

fn dashes(window: &mut Window, start: Point<Pixels>, length: Pixels, horizontal: bool, color: u32) {
    let mut offset = px(0.);
    while offset < length {
        let segment = (length - offset).min(px(2.));
        let (origin, dimensions) = if horizontal {
            (point(start.x + offset, start.y), size(segment, px(1.)))
        } else {
            (point(start.x, start.y + offset), size(px(2.), segment))
        };
        window.paint_quad(fill(Bounds::new(origin, dimensions), rgb(color)));
        offset += px(5.);
    }
}

#[cfg(test)]
mod tests {
    use super::{Graph, LANE, LANE_CAP, LEFT, Lanes, hidden_sides};

    fn fanned(lanes: usize) -> Graph {
        let mut graph = Graph::default();
        let tips: Vec<String> = (0..lanes).map(|lane| format!("tip{lane}")).collect();
        graph.push("gather", &tips);
        for tip in &tips {
            graph.push(tip, &["base".into()]);
        }
        graph.push("base", &[]);
        graph
    }

    #[test]
    fn the_graph_column_fits_its_lanes_up_to_twelve_and_keeps_full_spacing() {
        let narrow = fanned(1);
        assert_eq!(narrow.auto_width(), 52.);
        let five = fanned(5);
        assert_eq!(five.content_width(), LEFT + 4. * LANE + 22.);
        assert_eq!(five.auto_width(), five.content_width());
        let wide = fanned(20);
        assert_eq!(wide.content_width(), LEFT + 19. * LANE + 22.);
        assert_eq!(wide.auto_width(), LEFT + (LANE_CAP - 1) as f32 * LANE + 22.);
        assert_eq!(wide.node_x(0), Some(LEFT));
        assert_eq!(wide.node_x(20), Some(LEFT + 19. * LANE));
        assert_eq!(wide.node_x(99), None);
    }

    #[test]
    fn a_faded_edge_marks_each_side_with_lanes_out_of_view() {
        let content = 300.;
        assert_eq!(hidden_sides(Lanes { width: 300., offset: 0. }, content), (false, false));
        assert_eq!(hidden_sides(Lanes { width: 200., offset: 0. }, content), (false, true));
        assert_eq!(hidden_sides(Lanes { width: 200., offset: 50. }, content), (true, true));
        assert_eq!(hidden_sides(Lanes { width: 200., offset: 100. }, content), (true, false));
    }

    #[test]
    fn lanes_fork_at_a_merge_and_join_at_the_common_ancestor() {
        let mut graph = Graph::default();
        graph.push("merge", &["left".into(), "right".into()]);
        graph.push("left", &["base".into()]);
        graph.push("right", &["base".into()]);
        graph.push("base", &[]);
        assert_eq!(graph.rows[0].outgoing, [0, 1]);
        assert_eq!(graph.rows[1].through, [1]);
        assert_eq!(graph.rows[2].node, 1);
        assert_eq!(graph.rows[2].outgoing, [0]);
        assert_eq!(graph.rows[3].node, 0);
        assert!(graph.rows[3].incoming && graph.lanes.is_empty());
    }
}
