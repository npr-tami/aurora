//! Draws the in-game menu over a screenshot, to show the look without the game. Usage: menu_mock
//! <background.png> <out.png>

use aurora_core::menuview::{Kind, LAYOUT, Painter, Row, SPACER, View, list_at};

fn row(text: &str, kind: Kind) -> Row {
    Row { text: text.into(), kind, dim: false }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&args[1]).unwrap()));
    let mut reader = dec.read_info().unwrap();
    let mut bg = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bg).unwrap();
    let (w, h) = (info.width, info.height);
    let ch = info.color_type.samples();
    let painter = Painter::new(std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").unwrap()).unwrap();
    let view = View {
        path: "Aurora 0.1.0 > Self".into(),
        sections: ["Self", "Teleport", "World", "Hotkeys", "Settings"].map(String::from).to_vec(),
        section: 0,
        rows: vec![
            row("God Mode", Kind::Toggle(true)),
            row("Fly", Kind::Toggle(false)),
            row("Fly Speed", Kind::Value("15 m/s".into())),
            row("Safe Fly", Kind::Toggle(true)),
            row("Gear Score", Kind::Sub),
            row("Credits", Kind::Sub),
            row("Where Am I", Kind::Value("-7294, -804, 552".into())),
        ],
        pick: Some(1),
        scroll: 0,
        help: "Fly with W A S D, Space up, Ctrl down, Shift faster.".into(),
        accent: [255, 0, 255],
        layout: LAYOUT,
        list: list_at(100.0),
        visible: 13,
        spacer: SPACER,
    };
    let f = painter.render(&view, h as f32 / 1080.0);
    let (x0, y0) = ((60.0 * h as f32 / 1080.0) as u32, (130.0 * h as f32 / 1080.0) as u32);
    for p in &f.panels {
        for y in p[1]..p[3] {
            for x in p[0]..p[2] {
                let s = ((y * f.w + x) * 4) as usize;
                let d = (((y0 + y) * w + x0 + x) as usize) * ch;
                bg[d..d + 3].copy_from_slice(&f.rgba[s..s + 3]);
            }
        }
    }
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&args[2]).unwrap()), w, h);
    enc.set_color(info.color_type);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&bg).unwrap();
}
