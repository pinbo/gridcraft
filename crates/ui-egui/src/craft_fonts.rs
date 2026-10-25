//! Fonts from the optional craft-fonts build input (`CRAFT_FONTS_DIR`, see build.rs): the Carlito
//! faces, which give a build with no system fonts (the web) real bold and italic text. The table is
//! empty in a build without craft-fonts, and every caller works with that.

pub struct CraftFont {
    pub family: &'static str,
    pub style: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/craft_fonts.rs"));

/// The embedded face of `family` in `style` ("Regular", "Bold", "Italic" or "Bold Italic").
pub fn face(family: &str, style: &str) -> Option<&'static [u8]> {
    CRAFT_FONTS.iter().find(|f| f.family == family && f.style == style).map(|f| f.bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_embedded_face_is_a_whole_font_and_a_missing_one_is_none() {
        assert!(super::face("No Such Family", "Regular").is_none());
        for f in super::CRAFT_FONTS {
            assert!(f.bytes.len() > 1000, "{} {} looks truncated", f.family, f.style);
        }
    }
}
