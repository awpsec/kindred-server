//! Static notification portraits share their geometry and colours with chat.
use crate::db::BotProfile;
use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::OnceLock;

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(
            include_str!("../ui/avatar-data.js")
                .trim()
                .strip_prefix("export const avatarData = ")
                .unwrap()
                .strip_suffix(';')
                .unwrap(),
        )
        .expect("Bundled avatar data")
    })
}

pub fn png(profile: &BotProfile, name: &str) -> Result<Vec<u8>> {
    let data = data();
    let shape = match profile.shape.as_str() {
        "bean" => "hexagon",
        "ghost" => "cloud",
        other => other,
    };
    let shape = if data["paths"][shape].is_string() {
        shape
    } else {
        "round"
    };
    let color = profile.color.to_lowercase();
    let color = data["vivid"][&color].as_str().unwrap_or(&color);
    let color = if color.len() == 7
        && color.starts_with('#')
        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        color
    } else {
        "#2475ff"
    };
    for tribute in data["tributes"].as_object().unwrap().values() {
        if name
            .trim()
            .eq_ignore_ascii_case(tribute["name"].as_str().unwrap())
            && profile.shape == tribute["shape"].as_str().unwrap()
            && color == tribute["color"].as_str().unwrap()
        {
            return render(&format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='128' height='128' viewBox='0 0 128 128'>{}</svg>",
                tribute["art"].as_str().unwrap()
            ));
        }
    }
    let channels: Vec<u8> = [1, 3, 5]
        .into_iter()
        .map(|i| u8::from_str_radix(&color[i..i + 2], 16).unwrap())
        .collect();
    let ink = if channels.iter().map(|v| *v as u32).sum::<u32>() < 150 {
        "#ffffff"
    } else {
        "#171b1d"
    };
    let f = &data["faces"][shape];
    let x = f["x"].as_f64().unwrap();
    let y = f["y"].as_f64().unwrap();
    let spread = f["spread"].as_f64().unwrap();
    let mut eyes = String::new();
    for cx in [x - spread, x + spread] {
        eyes.push_str(&match profile.eyes.as_str(){
            "happy"=>format!("<path d='M{} {}q4 -6 8 0' fill='none' stroke='{ink}' stroke-width='4.5' stroke-linecap='round'/>",cx-4.,y+1.),
            "sleepy"=>format!("<rect x='{}' y='{}' width='8' height='3.6' rx='1.8' fill='{ink}'/>",cx-4.,y-1.8),
            _=>{let (rx,ry)=if profile.eyes=="wide"{(4.5,7.6)}else{(3.7,7.1)};format!("<ellipse cx='{cx}' cy='{y}' rx='{rx}' ry='{ry}' fill='{ink}'/>")}
        });
    }
    let outline = if channels.iter().all(|v| *v >= 210) {
        " stroke='#777f83' stroke-width='1'"
    } else {
        ""
    };
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='128' height='128' viewBox='0 0 100 110'><path d='{}' fill='{color}'{outline}/>{eyes}</svg>",
        data["paths"][shape].as_str().unwrap()
    );
    render(&svg)
}

fn render(svg: &str) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(128, 128).context("Avatar image allocation")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portraits_are_bounded_pngs_with_actual_bot_identity() {
        let mut profile = BotProfile::default();
        let first = png(&profile, "").unwrap();
        assert!(first.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(first.len() < 32 * 1024);
        profile.shape = "triangle".into();
        profile.color = "#24d5a4".into();
        profile.eyes = "happy".into();
        assert_ne!(first, png(&profile, "").unwrap());
        profile.shape = "<script>".into();
        profile.color = "url(https://invalid.test)".into();
        assert!(png(&profile, "").is_ok());
        assert_eq!(data()["paths"].as_object().unwrap().len(), 8);
    }

    #[test]
    fn tribute_portraits_require_name_shape_and_color() {
        for (name, shape, color, legacy) in [
            ("Oliver", "pebble", "#ffbe16", "#dfc174"),
            ("Vivienne", "triangle", "#2ec767", "#91c18f"),
        ] {
            let mut profile = BotProfile::default();
            profile.shape = shape.into();
            profile.color = color.into();
            let tribute = png(&profile, name).unwrap();
            assert!(tribute.len() < 32 * 1024);
            assert_ne!(tribute, png(&profile, "Another bot").unwrap());
            assert_eq!(
                tribute,
                png(&profile, &format!(" {} ", name.to_uppercase())).unwrap()
            );
            profile.color = legacy.to_uppercase();
            assert_eq!(tribute, png(&profile, name).unwrap());
            profile.color = "#2475ff".into();
            assert_eq!(
                png(&profile, name).unwrap(),
                png(&profile, "Another bot").unwrap()
            );
            profile.color = color.into();
            profile.shape = "round".into();
            assert_eq!(
                png(&profile, name).unwrap(),
                png(&profile, "Another bot").unwrap()
            );
        }
    }
}
