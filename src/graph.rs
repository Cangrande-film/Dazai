//! Word graph: one word (or kanji) at the centre, linked through shared kanji to the words around
//! it. Force-directed layout with a spatial grid; drag empty space to pan, scroll to zoom,
//! drag nodes to move them, click a node to re-centre on it.

use crate::dict::{kanji_of, KanjiInfo};
use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Sense, Stroke, Vec2};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Status {
    New,
    Learning,
    Learned,
}

pub struct GraphWord {
    pub id: String,
    pub word: String,
    pub reading: String,
    pub meaning: String,
    pub status: Status,
    /// Steps from the centre (0 = the centre word).
    pub hop: usize,
}

enum Kind {
    Kanji { k: char, words: usize },
    Word { reading: String, meaning: String, status: Status, hop: usize },
}

struct Node {
    key: String,
    label: String,
    kind: Kind,
    pos: Vec2,
    vel: Vec2,
}

pub enum Action {
    OpenKanji(char),
    OpenWord(String),
}

pub struct Graph {
    nodes: Vec<Node>,
    edges: Vec<(usize, usize)>,
    neighbors: Vec<Vec<usize>>,
    pan: Vec2,
    zoom: f32,
    dragging: Option<usize>,
    active: bool,
    signature: u64,
    focused: Option<usize>,
    focus_key: Option<String>,
}

impl Default for Graph {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            neighbors: Vec::new(),
            pan: Vec2::ZERO,
            zoom: 1.0,
            dragging: None,
            active: true,
            signature: 0,
            focused: None,
            focus_key: None,
        }
    }
}

const KANJI_COLOR: Color32 = Color32::from_rgb(214, 170, 96);
const NEW_COLOR: Color32 = Color32::from_rgb(105, 105, 105);
const LEARNING_COLOR: Color32 = Color32::from_rgb(200, 90, 76);
const LEARNED_COLOR: Color32 = Color32::from_rgb(130, 170, 110);
const EDGE_COLOR: Color32 = Color32::from_rgb(70, 70, 70);
const FOCUS_COLOR: Color32 = Color32::from_rgb(110, 150, 200);

fn status_color(s: Status) -> Color32 {
    match s {
        Status::New => NEW_COLOR,
        Status::Learning => LEARNING_COLOR,
        Status::Learned => LEARNED_COLOR,
    }
}

/// Small deterministic hash for initial positions.
fn hash(s: &str) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn jitter(key: &str) -> Vec2 {
    let h = hash(key);
    let a = (h % 6283) as f32 / 1000.0;
    let r = 20.0 + (h >> 16) as f32 % 40.0;
    Vec2::new(a.cos() * r, a.sin() * r)
}

impl Graph {
    pub fn kanji_count(&self) -> usize {
        self.nodes.iter().filter(|n| matches!(n.kind, Kind::Kanji { .. })).count()
    }

    /// Rebuilds nodes/edges when the input changes, keeping existing positions.
    /// Only kanji in `keep` get a node.
    pub fn sync(&mut self, words: &[GraphWord], keep: &HashSet<char>) {
        let mut sig = words.len() as u64 ^ ((keep.len() as u64) << 40);
        for w in words {
            sig = sig.wrapping_mul(31).wrapping_add(hash(&w.id) ^ w.status as u64 ^ ((w.hop as u64) << 8));
        }
        if sig == self.signature && !self.nodes.is_empty() {
            return;
        }
        self.signature = sig;

        let old: HashMap<String, Vec2> = self.nodes.iter().map(|n| (n.key.clone(), n.pos)).collect();
        let mut nodes: Vec<Node> = Vec::new();
        let mut index: HashMap<char, usize> = HashMap::new();
        let mut order: Vec<char> = Vec::new();
        let mut uses: HashMap<char, usize> = HashMap::new();
        for w in words {
            for k in kanji_of(&w.word).into_iter().filter(|k| keep.contains(k)) {
                *uses.entry(k).or_insert_with(|| {
                    order.push(k);
                    0
                }) += 1;
            }
        }
        for (i, k) in order.iter().enumerate() {
            let key = format!("k:{k}");
            let angle = i as f32 / order.len().max(1) as f32 * std::f32::consts::TAU;
            let pos = old.get(&key).copied().unwrap_or(Vec2::new(angle.cos(), angle.sin()) * (60.0 + 25.0 * (i % 4) as f32));
            index.insert(*k, nodes.len());
            nodes.push(Node { key, label: k.to_string(), kind: Kind::Kanji { k: *k, words: uses[k] }, pos, vel: Vec2::ZERO });
        }
        let mut edges = Vec::new();
        for w in words {
            let ks: Vec<usize> = kanji_of(&w.word).iter().filter_map(|k| index.get(k).copied()).collect();
            let key = format!("w:{}", w.id);
            let anchor = if ks.is_empty() {
                Vec2::ZERO
            } else {
                ks.iter().map(|&i| nodes[i].pos).fold(Vec2::ZERO, |a, b| a + b) / ks.len() as f32
            };
            let pos = old.get(&key).copied().unwrap_or(anchor + jitter(&key) * (1.0 + w.hop as f32));
            let wi = nodes.len();
            nodes.push(Node {
                key,
                label: w.word.clone(),
                kind: Kind::Word { reading: w.reading.clone(), meaning: w.meaning.clone(), status: w.status, hop: w.hop },
                pos,
                vel: Vec2::ZERO,
            });
            for ki in ks {
                edges.push((wi, ki));
            }
        }
        let mut neighbors = vec![Vec::new(); nodes.len()];
        for &(a, b) in &edges {
            neighbors[a].push(b);
            neighbors[b].push(a);
        }
        self.nodes = nodes;
        self.edges = edges;
        self.neighbors = neighbors;
        self.active = true;
        self.resolve_focus();
    }

    /// Pins the node with this key ("k:日" or "w:<card id>") at the centre.
    pub fn set_focus(&mut self, key: Option<String>) {
        if key == self.focus_key {
            return;
        }
        self.focus_key = key;
        self.resolve_focus();
        if self.focused.is_some() {
            self.recenter();
        }
        self.active = true;
    }

    fn resolve_focus(&mut self) {
        self.focused = self.focus_key.as_ref().and_then(|k| self.nodes.iter().position(|n| &n.key == k));
        if let Some(f) = self.focused {
            let shift = self.nodes[f].pos;
            for n in &mut self.nodes {
                n.pos -= shift;
            }
        }
    }

    fn step(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            self.active = false;
            return;
        }
        let mut force = vec![Vec2::ZERO; n];
        // Repulsion only between nearby nodes (spatial grid), so big graphs stay smooth.
        const CELL: f32 = 110.0;
        let cell_of = |p: Vec2| ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32);
        let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            grid.entry(cell_of(node.pos)).or_default().push(i);
        }
        for i in 0..n {
            let (cx, cy) = cell_of(self.nodes[i].pos);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    let Some(cell) = grid.get(&(cx + dx, cy + dy)) else { continue };
                    for &j in cell {
                        if j <= i {
                            continue;
                        }
                        let mut d = self.nodes[i].pos - self.nodes[j].pos;
                        if d.length_sq() < 0.01 {
                            d = jitter(&self.nodes[j].key) * 0.05;
                        }
                        let dist2 = d.length_sq().max(100.0);
                        let f = d * (2600.0 / (dist2 * dist2.sqrt()));
                        force[i] += f;
                        force[j] -= f;
                    }
                }
            }
        }
        for &(a, b) in &self.edges {
            let d = self.nodes[b].pos - self.nodes[a].pos;
            let dist = d.length().max(0.1);
            let f = d / dist * (dist - 70.0) * 0.04;
            force[a] += f;
            force[b] -= f;
        }
        let mut energy = 0.0;
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if Some(i) == self.dragging {
                node.vel = Vec2::ZERO;
                continue;
            }
            if Some(i) == self.focused {
                node.pos = Vec2::ZERO;
                node.vel = Vec2::ZERO;
                continue;
            }
            let f = force[i] - node.pos * 0.012;
            node.vel = (node.vel + f) * 0.6;
            let len = node.vel.length();
            if len > 15.0 {
                node.vel *= 15.0 / len;
            }
            node.pos += node.vel;
            energy += node.vel.length();
        }
        if energy / (n as f32) < 0.03 && self.dragging.is_none() {
            self.active = false;
        }
    }

    fn radius(&self, i: usize) -> f32 {
        match &self.nodes[i].kind {
            Kind::Kanji { words, .. } => 12.0 + 2.5 * (*words as f32).sqrt(),
            Kind::Word { hop: 0, .. } => 9.0,
            Kind::Word { .. } => 5.5,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, kanji_info: &HashMap<String, KanjiInfo>) -> Option<Action> {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, Color32::from_rgb(17, 17, 17));

        let center = rect.center().to_vec2();
        let to_screen = |p: Vec2, pan: Vec2, zoom: f32| (center + pan + p * zoom).to_pos2();
        let to_world = |s: Pos2, pan: Vec2, zoom: f32| (s.to_vec2() - center - pan) / zoom;

        // Zoom around the cursor.
        if let Some(hover) = resp.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let before = to_world(hover, self.pan, self.zoom);
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.15, 4.0);
                self.pan = hover.to_vec2() - center - before * self.zoom;
            }
        }

        let pointer = ui.input(|i| i.pointer.hover_pos());
        let node_at = |g: &Graph, p: Pos2| -> Option<usize> {
            (0..g.nodes.len())
                .filter(|&i| {
                    let s = to_screen(g.nodes[i].pos, g.pan, g.zoom);
                    s.distance(p) <= (g.radius(i) * g.zoom.sqrt()).max(6.0) + 3.0
                })
                .min_by(|&a, &b| {
                    let da = to_screen(g.nodes[a].pos, g.pan, g.zoom).distance(p);
                    let db = to_screen(g.nodes[b].pos, g.pan, g.zoom).distance(p);
                    da.total_cmp(&db)
                })
        };

        if resp.drag_started() {
            self.dragging = resp.interact_pointer_pos().and_then(|p| node_at(self, p));
        }
        if resp.dragged() {
            match (self.dragging, resp.interact_pointer_pos()) {
                (Some(i), Some(p)) => {
                    self.nodes[i].pos = to_world(p, self.pan, self.zoom);
                    self.active = true;
                }
                _ => self.pan += resp.drag_delta(),
            }
        }
        if resp.drag_stopped() {
            self.dragging = None;
        }

        if self.active {
            for _ in 0..2 {
                self.step();
            }
            ui.ctx().request_repaint();
        }

        let hovered = if resp.hovered() || self.dragging.is_some() {
            self.dragging.or_else(|| pointer.and_then(|p| node_at(self, p)))
        } else {
            None
        };
        let lit = |i: usize| match hovered {
            None => true,
            Some(h) => h == i || self.neighbors[h].contains(&i),
        };

        // Edges
        for &(a, b) in &self.edges {
            let on = hovered.is_some_and(|h| h == a || h == b);
            let color = if on {
                KANJI_COLOR.gamma_multiply(0.8)
            } else if hovered.is_some() {
                EDGE_COLOR.gamma_multiply(0.35)
            } else {
                EDGE_COLOR
            };
            painter.line_segment(
                [to_screen(self.nodes[a].pos, self.pan, self.zoom), to_screen(self.nodes[b].pos, self.pan, self.zoom)],
                Stroke::new(if on { 1.6_f32 } else { 1.0 }, color),
            );
        }

        // Nodes and labels: words near the centre are always labelled, the rest when zoomed in.
        for i in 0..self.nodes.len() {
            let node = &self.nodes[i];
            let p = to_screen(node.pos, self.pan, self.zoom);
            if !rect.expand(40.0).contains(p) {
                continue;
            }
            let r = self.radius(i) * self.zoom.sqrt();
            let dim: f32 = if lit(i) { 1.0 } else { 0.25 };
            match &node.kind {
                Kind::Kanji { .. } => {
                    painter.circle(p, r, Color32::from_rgb(28, 26, 22).gamma_multiply(dim.max(0.6)), Stroke::new(1.5_f32, KANJI_COLOR.gamma_multiply(dim)));
                    painter.text(
                        p,
                        egui::Align2::CENTER_CENTER,
                        &node.label,
                        FontId::new((r * 1.15).max(9.0), FontFamily::Name("display".into())),
                        Color32::from_rgb(235, 225, 205).gamma_multiply(dim),
                    );
                }
                Kind::Word { status, hop, .. } => {
                    painter.circle_filled(p, r, status_color(*status).gamma_multiply(dim));
                    let labelled = *hop <= 1 || self.zoom >= 1.2 || (hovered.is_some() && lit(i));
                    if labelled {
                        let size = if *hop == 0 { 17.0 } else { 13.0 * self.zoom.sqrt().clamp(0.8, 1.4) };
                        painter.text(
                            p + Vec2::new(0.0, r + 3.0),
                            egui::Align2::CENTER_TOP,
                            &node.label,
                            FontId::new(size, FontFamily::Proportional),
                            Color32::from_rgb(200, 196, 188).gamma_multiply(dim),
                        );
                    }
                }
            }
        }
        if let Some(f) = self.focused {
            let p = to_screen(self.nodes[f].pos, self.pan, self.zoom);
            painter.circle_stroke(p, self.radius(f) * self.zoom.sqrt() + 5.0, Stroke::new(1.5_f32, FOCUS_COLOR));
        }

        let mut action = None;
        if let Some(h) = hovered {
            let node = &self.nodes[h];
            let degree = self.neighbors[h].len();
            let resp = resp.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(260.0);
                match &node.kind {
                    Kind::Kanji { k, .. } => {
                        ui.label(egui::RichText::new(k.to_string()).size(28.0));
                        if let Some(info) = kanji_info.get(&k.to_string()) {
                            ui.label(info.meanings.join(", "));
                            if !info.on_readings.is_empty() {
                                ui.label(egui::RichText::new(format!("on: {}", info.on_readings.join("、"))).small());
                            }
                            if !info.kun_readings.is_empty() {
                                ui.label(egui::RichText::new(format!("kun: {}", info.kun_readings.join("、"))).small());
                            }
                        }
                        ui.label(egui::RichText::new(format!("{degree} words here · click to centre it")).small().weak());
                    }
                    Kind::Word { reading, meaning, status, .. } => {
                        ui.label(egui::RichText::new(&node.label).size(22.0));
                        ui.label(reading);
                        ui.label(meaning);
                        let s = match status {
                            Status::New => "not studied yet",
                            Status::Learning => "learning",
                            Status::Learned => "learned",
                        };
                        ui.label(egui::RichText::new(s).small().color(status_color(*status)));
                        ui.label(egui::RichText::new("click to centre it").small().weak());
                    }
                }
            });
            if resp.clicked() {
                action = Some(match &node.kind {
                    Kind::Kanji { k, .. } => Action::OpenKanji(*k),
                    Kind::Word { .. } => Action::OpenWord(node.key.trim_start_matches("w:").to_string()),
                });
            }
        }

        // Legend
        let mut y = rect.top() + 10.0;
        for (label, color) in [("kanji", KANJI_COLOR), ("learned", LEARNED_COLOR), ("learning", LEARNING_COLOR), ("new", NEW_COLOR)] {
            painter.circle_filled(Pos2::new(rect.left() + 18.0, y + 7.0), 4.5, color);
            painter.text(Pos2::new(rect.left() + 30.0, y), egui::Align2::LEFT_TOP, label, FontId::proportional(13.0), Color32::from_gray(140));
            y += 18.0;
        }
        painter.text(
            rect.right_bottom() + Vec2::new(-10.0, -8.0),
            egui::Align2::RIGHT_BOTTOM,
            "drag to pan · scroll to zoom · click a node to centre it",
            FontId::proportional(12.0),
            Color32::from_gray(90),
        );
        action
    }

    pub fn recenter(&mut self) {
        self.pan = Vec2::ZERO;
        self.zoom = if self.nodes.len() > 120 { 0.6 } else { 1.0 };
    }
}
