#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod check;
mod dict;
mod extract;
mod levels;
mod graph;
mod pitch;
mod srs;
mod storage;

use eframe::egui;
use std::sync::Arc;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dazai")
            .with_inner_size([760.0, 660.0])
            .with_min_inner_size([420.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Dazai",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            // egui may re-run a frame with the *same* input after a layout change; that would
            // apply one key press twice (e.g. answer two cards). Allow a single pass only.
            cc.egui_ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
            Ok(Box::new(app::DazaiApp::new(&cc.egui_ctx)))
        }),
    )
}

fn read_font(names: &[&str]) -> Option<Vec<u8>> {
    let fonts = std::env::var("WINDIR")
        .map(|w| std::path::PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|_| "C:\\Windows\\Fonts".into());
    names.iter().find_map(|n| std::fs::read(fonts.join(n)).ok())
}

/// Klee One sits a little high next to Garamond; this nudges it onto the same baseline.
const KLEE_Y_OFFSET: f32 = 0.1;

/// Garamond for Latin text, Klee One for Japanese (egui ships no CJK glyphs).
fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // EB Garamond (SIL OFL, bundled) for Latin text. It runs small for its size, so scale it up a bit.
    fonts.font_data.insert(
        "garamond".into(),
        Arc::new(
            egui::FontData::from_static(include_bytes!("../assets/EBGaramond.ttf")).tweak(egui::FontTweak {
                scale: 1.12,
                y_offset_factor: -0.02,
                ..Default::default()
            }),
        ),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "garamond".into());

    // Klee One (SIL OFL, bundled) for Japanese: Regular for text, SemiBold for the big study word.
    let klee_tweak = egui::FontTweak { y_offset_factor: KLEE_Y_OFFSET, ..Default::default() };
    fonts.font_data.insert(
        "klee".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/KleeOne-Regular.ttf")).tweak(klee_tweak)),
    );
    fonts.font_data.insert(
        "klee_semibold".into(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/KleeOne-SemiBold.ttf")).tweak(klee_tweak)),
    );
    let mut jp = vec!["klee".to_string()];
    // Yu Gothic covers the rare kanji Klee One doesn't have.
    if let Some(bytes) = read_font(&["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"]) {
        fonts.font_data.insert(
            "jp_fallback".into(),
            Arc::new(egui::FontData::from_owned(bytes).tweak(egui::FontTweak {
                y_offset_factor: 0.2,
                ..Default::default()
            })),
        );
        jp.push("jp_fallback".into());
    }
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().extend(jp.iter().cloned());
    }

    let mut display = vec!["klee_semibold".to_string()];
    display.extend(
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name("display".into()), display);

    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    use egui::{Color32, FontFamily, FontId, TextStyle};

    // Don't follow the OS light/dark setting; Dazai is always dark.
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    let bg = Color32::from_rgb(20, 20, 20);
    visuals.panel_fill = bg;
    visuals.window_fill = bg;
    visuals.extreme_bg_color = Color32::from_rgb(30, 30, 30);
    visuals.override_text_color = Some(Color32::from_rgb(226, 222, 214));
    visuals.selection.bg_fill = Color32::from_rgb(120, 52, 44);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 34, 34);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 46, 46);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(58, 58, 58);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(15.0, FontFamily::Monospace)),
        ]
        .into();
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        // Rows as tall as the padded buttons, so labels line up with them.
        style.spacing.interact_size.y = 34.0;
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    });
}
