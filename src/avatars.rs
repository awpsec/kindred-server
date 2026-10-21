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
    let a = crate::avatar_profiles::resolved(profile);
    static PORTRAITS: OnceLock<Value> = OnceLock::new();
    let portraits=PORTRAITS.get_or_init(||serde_json::from_str(include_str!("../ui/avatar-runtime/portraits.json")).expect("avatar portraits"));
    let ink=if let Some(id)=&a.palette {
        if id=="black" {"#ffffff"} else {crate::avatar_profiles::catalog()["palettes"].as_array().unwrap().iter().find(|p|p["id"]==*id).unwrap()["light"].as_str().unwrap()}
    } else {a.custom_color.as_deref().unwrap_or("#2A92FE")};
    let svg=portraits[&a.body][&a.expression].as_str().context("Missing avatar portrait")?;
    let svg=if a.palette.as_deref()==Some("black") {svg.replace("fill=\"#ABCDEF\"", "fill=\"#ffffff\" stroke=\"#777f83\" stroke-width=\"1\"")} else {svg.replace("#ABCDEF",ink)};
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
        assert_eq!(crate::avatar_profiles::catalog()["bodies"].as_array().unwrap().len(),25);
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

#[cfg(test)]
mod artwork_tests {
 use super::*;
 #[test]
 fn all_artwork_portraits_render_and_have_transparent_backgrounds() {
  let c=crate::avatar_profiles::catalog();
  for body in c["bodies"].as_array().unwrap(){for expression in c["expressions"].as_array().unwrap(){let mut p=BotProfile::default();p.avatar=Some(crate::avatar_profiles::AvatarProfile{version:1,body:body.as_str().unwrap().into(),expression:expression.as_str().unwrap().into(),palette:Some("blue".into()),custom_color:None});let image=png(&p,"").unwrap();assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));assert!(image.len()<32768);let decoded=resvg::tiny_skia::Pixmap::decode_png(&image).unwrap();assert_eq!(decoded.pixel(0,0).unwrap().alpha(),0);}}
 }
}
