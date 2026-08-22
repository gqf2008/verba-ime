//! 渲染候选窗为 PNG（无窗口可视化验证）。
//! 用法: cargo run -p verba-candidate --example render_png --release
//! 输出: crates/verba-candidate/render-preview.png

use std::io::Write;

use verba_candidate::renderer::CpuCandidateRenderer;
use verba_candidate::{CandidateWindowController, Theme};

fn main() {
    let theme = Theme::default();
    let mut ctrl = CandidateWindowController::new(theme);
    ctrl.set_candidates(vec![
        "你好".into(),
        "您好".into(),
        "你号".into(),
        "尼豪".into(),
        "你们".into(),
        "你我".into(),
        "你个头".into(),
        "呢".into(),
        "拟".into(),
    ]);
    ctrl.show();
    ctrl.set_position(100, 200);

    let mut renderer = CpuCandidateRenderer::new();
    let out = renderer.render(&ctrl);

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/render-preview.png");
    let mut file = std::fs::File::create(path).expect("创建 PNG");
    let mut encoder = png::Encoder::new(&mut file, out.width, out.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG 头");
    writer.write_image_data(&out.pixels).expect("写像素");
    drop(writer);
    println!(
        "已渲染候选窗: {}x{} → {}",
        out.width, out.height, path
    );
    std::io::stdout().flush().unwrap();
}
