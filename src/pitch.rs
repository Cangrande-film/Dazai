//! Draws a reading with its pitch-accent line (high/low per mora, like dictionaries do).

use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Sense, Stroke, Vec2};

const SMALL: &str = "ゃゅょぁぃぅぇぉゎャュョァィゥェォヮ";

/// Splits kana into morae: small ゃ/ゅ/ょ etc. join the previous kana; っ and ー count on their own.
pub fn morae(reading: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in reading.chars() {
        match out.last_mut() {
            Some(last) if SMALL.contains(c) => last.push(c),
            _ => out.push(c.to_string()),
        }
    }
    out
}

/// Whether mora `i` is high, for a word of `n` morae with downstep `accent` (0 = heiban).
fn is_high(i: usize, accent: usize) -> bool {
    match accent {
        0 => i > 0,
        1 => i == 0,
        a => i > 0 && i < a,
    }
}

/// Reading with a pitch line over it. Returns the widget response.
pub fn show(ui: &mut egui::Ui, reading: &str, accent: u8, size: f32, color: Color32) -> egui::Response {
    let font = FontId::new(size, FontFamily::Proportional);
    let morae = morae(reading);
    let galleys: Vec<_> = morae
        .iter()
        .map(|m| ui.painter().layout_no_wrap(m.clone(), font.clone(), color))
        .collect();
    let line_gap = (size * 0.42).max(5.0);
    let width: f32 = galleys.iter().map(|g| g.size().x).sum::<f32>() + size * 0.4;
    let text_h = galleys.iter().map(|g| g.size().y).fold(size, f32::max);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, text_h + line_gap * 2.0 + 2.0), Sense::hover());

    let painter = ui.painter_at(rect.expand(2.0));
    let line = Stroke::new((size / 16.0).max(1.2), color.gamma_multiply(0.75));
    let hi_y = rect.top() + 1.0;
    let lo_y = rect.bottom() - 1.0;
    let accent = accent as usize;
    let n = morae.len();

    let mut x = rect.left();
    let mut prev: Option<bool> = None;
    for (i, g) in galleys.iter().enumerate() {
        let w = g.size().x;
        painter.galley(Pos2::new(x, rect.top() + line_gap + 1.0), g.clone(), color);
        let high = is_high(i, accent);
        let y = if high { hi_y } else { lo_y };
        if let Some(p) = prev {
            if p != high {
                painter.line_segment([Pos2::new(x, hi_y), Pos2::new(x, lo_y)], line);
            }
        }
        painter.line_segment([Pos2::new(x, y), Pos2::new(x + w, y)], line);
        prev = Some(high);
        x += w;
    }
    // The particle after the word: high for heiban, low after a final-mora drop (odaka).
    if n > 0 {
        let particle_high = accent == 0;
        let y = if particle_high { hi_y } else { lo_y };
        if prev != Some(particle_high) {
            painter.line_segment([Pos2::new(x, hi_y), Pos2::new(x, lo_y)], line);
        }
        painter.line_segment([Pos2::new(x, y), Pos2::new(x + size * 0.3, y)], Stroke::new(line.width, line.color.gamma_multiply(0.4)));
    }
    resp
}

/// Short name of the accent pattern.
pub fn pattern_name(reading: &str, accent: u8) -> &'static str {
    let n = morae(reading).len();
    match accent as usize {
        0 => "heiban",
        1 => "atamadaka",
        a if a == n => "odaka",
        _ => "nakadaka",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mora_split() {
        assert_eq!(morae("きょう"), vec!["きょ", "う"]);
        assert_eq!(morae("がっこう").len(), 4);
        assert_eq!(pattern_name("ねこ", 1), "atamadaka");
        assert_eq!(pattern_name("にほん", 2), "nakadaka");
        assert_eq!(pattern_name("がっこう", 0), "heiban");
    }
}
