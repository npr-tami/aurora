//! The Slots page over a background, as a PNG. usage: slots_mock <background.png> <out.png> <a|b>

use aurora_core::menuview::{Kind, LAYOUT, Painter, Row, SPACER, View, list_at};

fn row(text: &str, kind: Kind) -> Row {
    Row { text: text.into(), kind, dim: false }
}

fn value(text: &str, v: &str) -> Row {
    row(text, Kind::Choice(v.into()))
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
    let mut rows = vec![
        value("Total Gear Score", "549"),
        row("Weapons", Kind::Header),
        value("Primary", "666"),
        value("Secondary", "212"),
        value("Pistol", "666"),
        row("Gear", Kind::Header),
        value("Head", "666"),
        value("Hands", "665"),
        value("Vest", "419"),
        value("Pants", "666"),
        value("Shoes", "666"),
    ];
    let b = args[3] == "b";
    if b {
        rows.push(row("Everything You Own", Kind::Header));
        rows.push(value("Set All Owned Items", "600"));
    }
    let view = View {
        path: "Aurora 0.1.0 > Self > Progress > Slots".into(),
        sections: ["Self", "Weapon", "World", "Game", "Menu"].map(String::from).to_vec(),
        section: 0,
        rows,
        pick: Some(if b { 12 } else { 0 }),
        scroll: 0,
        help: if b {
            "Command: setallowneditems [0 - 1000]\nSets every weapon and piece of gear you own, stash too, when you press Enter.".into()
        } else {
            "Command: totalgearscore [0 - 1000]\nSets everything you have on to this score.\nClick to input a value.".into()
        },
        accent: [140, 60, 255],
        layout: LAYOUT,
        list: list_at(100.0),
        visible: 13,
        spacer: SPACER,
    };
    let f = painter.render(&view, 1.4);
    let (x0, y0) = (20u32, 20u32);
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
