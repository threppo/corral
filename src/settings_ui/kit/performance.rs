//! Headless measurements of the production settings render path (no HWND or desktop).
use super::super::*;
#[test]
fn production_slider_row_matches_full_render_and_reports_cost() {
    let out = std::path::Path::new("target/qa/settings-redesign/u3");
    std::fs::create_dir_all(out).unwrap();
    let mut report = String::from(
        "Production Personalization full-page vs cached slider and live-sample repaint, offscreen; not WM_MOUSEMOVE latency.\n",
    );
    for dpi in [96, 240] {
        let settings = Settings::default();
        let theme = Theme::dark();
        let view = View {
            page: Page::Personalization,
            hover: None,
            scroll: 0,
        };
        let mut full = Surface::new(sc(1000.0, dpi) as u32, sc(720.0, dpi) as u32).unwrap();
        render_settings_state(&mut full, &view, &settings, &theme, dpi).unwrap();
        let slider = look_pages::SliderKind::SeeThrough;
        let mut partial = Surface::new(full.width(), full.height()).unwrap();
        partial.pixels_mut().copy_from_slice(full.pixels());
        render_look_slider_row(&mut partial, &view, &settings, &theme, dpi, None, slider)
            .unwrap()
            .unwrap();
        let diffs: Vec<_> = partial
            .pixels()
            .chunks_exact(4)
            .zip(full.pixels().chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| (i % full.width() as usize, i / full.width() as usize))
            .collect();
        if !diffs.is_empty() {
            partial
                .save_png(&out.join(format!("partial-debug-{dpi}.png")))
                .unwrap();
            full.save_png(&out.join(format!("full-debug-{dpi}.png")))
                .unwrap();
        }
        assert!(
            diffs.is_empty(),
            "row repaint diffs dpi={dpi}: count={} first={:?} last={:?}",
            diffs.len(),
            diffs.first(),
            diffs.last()
        );
        let start = std::time::Instant::now();
        for _ in 0..8 {
            render_settings_state(&mut full, &view, &settings, &theme, dpi).unwrap();
        }
        let page = start.elapsed().as_secs_f64() * 1000.0 / 8.0;
        let start = std::time::Instant::now();
        for _ in 0..64 {
            render_look_slider_row(&mut partial, &view, &settings, &theme, dpi, None, slider)
                .unwrap();
        }
        let row = start.elapsed().as_secs_f64() * 1000.0 / 64.0;
        report.push_str(&format!(
            "{dpi} dpi: full_page_ms={page:.4}, slider_and_sample_ms={row:.4}, iterations=8/64\n"
        ));
    }
    eprintln!("{report}");
    std::fs::write(out.join("production-look-slider-benchmark.txt"), report).unwrap();
}
