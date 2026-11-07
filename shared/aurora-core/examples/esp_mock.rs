//! World > ESP layouts to choose from, as a PNG. usage: esp_mock <background.png> <out.png> <a|b|c>

use aurora_core::menuview::{Kind, LAYOUT, Painter, Row, SPACER, View, list_at};

fn row(text: &str, kind: Kind) -> Row {
    Row { text: text.into(), kind, dim: false }
}

fn on(text: &str) -> Row {
    row(text, Kind::Toggle(true))
}

fn view(path: &str, rows: Vec<Row>, help: &str) -> View {
    View {
        path: format!("Aurora 0.1.0 > World > {path}"),
        sections: ["Self", "Weapon", "World", "Game", "Menu"].map(String::from).to_vec(),
        section: 2,
        rows,
        pick: Some(0),
        scroll: 0,
        help: help.into(),
        accent: [140, 60, 255],
        layout: LAYOUT,
        list: list_at(100.0),
        visible: 13,
        spacer: SPACER,
    }
}

fn who() -> Vec<Row> {
    vec![on("Hostile"), on("Friendly"), on("Players"), row("Hide Dead", Kind::Toggle(false))]
}

fn style() -> Vec<Row> {
    vec![
        row("Style", Kind::Choice("Bones".into())),
        row("Lines", Kind::Choice("auto".into())),
        on("Black Edge"),
        on("Distance"),
        row("Colours", Kind::Sub),
    ]
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
    let top = |a: &str, b: &str| vec![on("ESP"), row("Range", Kind::Choice("1500 m".into())), row(a, Kind::Sub), row(b, Kind::Sub)];
    let views = match args[3].as_str() {
        // One list with dividers, as Gear Score.
        "a" => {
            let mut rows = vec![on("ESP"), row("Range", Kind::Choice("1500 m".into())), row("Who", Kind::Header)];
            rows.extend(who());
            rows.push(row("Drawing", Kind::Header));
            rows.extend(style());
            vec![view("ESP", rows, "")]
        }
        // Two lists, named for what they hold.
        "b" => vec![
            view("ESP", top("Targets", "Drawing"), ""),
            view("ESP > Drawing", style(), "Bones follow how they stand and turn; a box where none can be read."),
        ],
        // Two lists, short names.
        _ => vec![view("ESP", top("Who", "Style"), ""), view("ESP > Who", who(), "")],
    };
    let mut x0 = 40u32;
    for v in &views {
        let f = painter.render(v, 1.0);
        let y0 = 40u32;
        for p in &f.panels {
            for y in p[1]..p[3] {
                for x in p[0]..p[2] {
                    let s = ((y * f.w + x) * 4) as usize;
                    let d = (((y0 + y) * w + x0 + x) as usize) * ch;
                    if d + 3 <= bg.len() && x0 + x < w && y0 + y < h {
                        bg[d..d + 3].copy_from_slice(&f.rgba[s..s + 3]);
                    }
                }
            }
        }
        x0 += f.w + 30;
    }
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(&args[2]).unwrap()), w, h);
    enc.set_color(info.color_type);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&bg).unwrap();
}
