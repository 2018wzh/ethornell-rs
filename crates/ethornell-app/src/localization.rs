//! Display-only localization. Script opcodes, resource names, branches and VM
//! string operations keep their original byte representation and CP932 rules.
use ab_glyph::{Font, FontArc};
use ethornell_core::{EthornellError, Result};
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplayEncoding {
    #[default]
    Cp932,
    Gbk,
}
impl DisplayEncoding {
    pub fn encoding(self) -> &'static encoding_rs::Encoding {
        match self { Self::Cp932 => encoding_rs::SHIFT_JIS, Self::Gbk => encoding_rs::GBK }
    }
    pub(crate) fn is_lead_byte(self, byte: u8) -> bool {
        match self {
            Self::Cp932 => byte >= 0x80 && !(0xA0..0xE0).contains(&byte),
            Self::Gbk => (0x81..=0xFE).contains(&byte),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayConfiguration {
    pub text_encoding: DisplayEncoding,
    /// Game-root-relative font file; an empty path retains the existing font.
    pub font_path: String,
    /// Zero-based SFNT/TTC face index. Never substitute another face on failure.
    pub font_index: u32,
}
impl DisplayConfiguration {
    pub(crate) fn load_font(&self, root: &Path) -> Result<Option<FontArc>> {
        if self.font_path.is_empty() {
            return if self.font_index == 0 { Ok(None) } else {
                Err(EthornellError::Other("font_index requires font_path".into()))
            };
        }
        let relative = Path::new(&self.font_path);
        if relative.components().any(|part| !matches!(part, Component::Normal(_)))
            || self.font_path.contains(':') || self.font_path.contains('\0')
        {
            return Err(EthornellError::Other("font_path must be game-root-relative".into()));
        }
        let root = root.canonicalize()?;
        let path = root.join(relative).canonicalize()?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(EthornellError::Other("font_path does not resolve to a game font file".into()));
        }
        ethornell_render::load_font_file(&path, self.font_index).map(Some)
    }
}

/// Check only presentation operands, after the VM has decoded them and before
/// native handlers consume the call frame. Resource and choice targets stay intact.
pub(crate) fn validate_display_font(font: Option<&FontArc>, call: &ethornell_vm::NativeCallFrame) -> ethornell_vm::VmResult<()> {
    let Some(font) = font else { return Ok(()); };
    let from_top = match (call.group(), call.id()) {
        (0x90, 0x90) | (0x91, 0x93) => 4,
        (0x91, 0x91) => 3,
        (0x91, 0x9C) => 10,
        (0x91, 0x9D) => 11,
        (0x92, 0x1C) => 6,
        (0x92, 0x1D) => 7,
        (0x92, 0x1E) => 5,
        (0x92, 0x1F) => 0,
        (0x92, 0x90) => 13,
        (0x92, 0x91) => 9,
        (0x92, 0x9C) => 17,
        _ => return Ok(()),
    };
    let text = call.args().len().checked_sub(from_top + 1).and_then(|index| call.args().get(index));
    if let Some(ethornell_vm::Value::Str(text)) = text {
        if text.chars().any(|ch| !ch.is_control() && !ch.is_whitespace() && font.glyph_id(ch).0 == 0) {
            return Err(ethornell_vm::VmError::Runtime("selected display font lacks a required glyph".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_defaults_preserve_cp932_and_default_font() {
        let config = DisplayConfiguration::default();
        assert_eq!(config.text_encoding, DisplayEncoding::Cp932);
        assert!(config.load_font(Path::new("does-not-need-to-exist")).unwrap().is_none());
    }
    #[test]
    fn selected_font_never_falls_back_when_missing_or_invalid() {
        let root = std::env::temp_dir().join(format!("ethornell-font-test-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        let config = DisplayConfiguration { font_path: "absent.ttf".into(), ..Default::default() };
        assert!(config.load_font(&root).is_err());
        std::fs::write(root.join("invalid.ttf"), b"public invalid font").unwrap();
        let config = DisplayConfiguration { font_path: "invalid.ttf".into(), ..Default::default() };
        assert!(config.load_font(&root).is_err());
        std::fs::remove_file(root.join("invalid.ttf")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn font_face_and_relative_path_are_explicit() {
        for path in ["../font.ttf", "/font.ttf", "C:/font.ttf"] {
            let config = DisplayConfiguration { font_path: path.into(), ..Default::default() };
            assert!(config.load_font(Path::new(".")).is_err());
        }
        let config = DisplayConfiguration { font_index: 1, ..Default::default() };
        assert!(config.load_font(Path::new(".")).is_err());
    }
    #[test]
    fn gbk_leads_include_bytes_that_cp932_treats_as_halfwidth() {
        assert!(DisplayEncoding::Gbk.is_lead_byte(0xD6));
        assert!(!DisplayEncoding::Cp932.is_lead_byte(0xD6));
        assert!(!DisplayEncoding::Gbk.is_lead_byte(0x80));
        assert!(!DisplayEncoding::Gbk.is_lead_byte(0xFF));
    }

    // Original public SFNT fixture: one square outline, mapped to A and 中.
    // No system, commercial, or redistributed third-party font data is used.
    fn square_font(advance: u16) -> Vec<u8> {
        fn be16(bytes: &mut [u8], at: usize, value: u16) { bytes[at..at+2].copy_from_slice(&value.to_be_bytes()); }
        fn be32(bytes: &mut [u8], at: usize, value: u32) { bytes[at..at+4].copy_from_slice(&value.to_be_bytes()); }
        let mut head = vec![0; 54];
        be32(&mut head, 0, 0x00010000); be32(&mut head, 12, 0x5F0F3CF5); be16(&mut head, 18, 1000);
        be16(&mut head, 40, 500); be16(&mut head, 42, 700);
        let mut hhea = vec![0; 36];
        be32(&mut hhea, 0, 0x00010000); be16(&mut hhea, 4, 800); be16(&mut hhea, 6, (-200_i16) as u16);
        be16(&mut hhea, 10, advance.max(500)); be16(&mut hhea, 34, 2);
        let mut maxp = vec![0; 32]; be32(&mut maxp, 0, 0x00010000); be16(&mut maxp, 4, 2);
        let mut hmtx = vec![0; 8]; be16(&mut hmtx, 0, 500); be16(&mut hmtx, 4, advance);
        let mut glyf = vec![0; 34];
        be16(&mut glyf, 0, 1); be16(&mut glyf, 6, 500); be16(&mut glyf, 8, 700); be16(&mut glyf, 10, 3);
        glyf[14..18].fill(1);
        for (index, value) in [0_i16, 500, 0, -500, 0, 0, 700, 0].into_iter().enumerate() {
            be16(&mut glyf, 18 + index * 2, value as u16);
        }
        let mut loca = vec![0; 6]; be16(&mut loca, 4, 17);
        let mut cmap = vec![0; 52]; be16(&mut cmap, 2, 1); be16(&mut cmap, 4, 3);
        be16(&mut cmap, 6, 1); be32(&mut cmap, 8, 12);
        be16(&mut cmap, 12, 4); be16(&mut cmap, 14, 40); be16(&mut cmap, 18, 6);
        be16(&mut cmap, 20, 4); be16(&mut cmap, 22, 1); be16(&mut cmap, 24, 2);
        for (index, code) in [0x41_u16, 0x4E2D, 0xFFFF].into_iter().enumerate() {
            be16(&mut cmap, 26 + index * 2, code); be16(&mut cmap, 34 + index * 2, code);
            be16(&mut cmap, 40 + index * 2, 1_u16.wrapping_sub(code));
        }
        let mut tables = vec![(*b"head", head), (*b"hhea", hhea), (*b"maxp", maxp),
            (*b"hmtx", hmtx), (*b"glyf", glyf), (*b"loca", loca), (*b"cmap", cmap)];
        tables.sort_by_key(|(tag, _)| *tag);
        let mut result = vec![0; 12 + tables.len() * 16];
        be32(&mut result, 0, 0x00010000); be16(&mut result, 4, 7);
        be16(&mut result, 6, 64); be16(&mut result, 8, 2); be16(&mut result, 10, 48);
        for (index, (tag, data)) in tables.into_iter().enumerate() {
            let at = 12 + index * 16; let offset = result.len();
            result[at..at+4].copy_from_slice(&tag); be32(&mut result, at+8, offset as u32);
            be32(&mut result, at+12, data.len() as u32);
            result.extend(data); result.resize((result.len()+3) & !3, 0);
        }
        result
    }

    fn font_collection() -> Vec<u8> {
        let fonts = [square_font(700), square_font(400)];
        let mut collection = b"ttcf\0\x01\0\0\0\0\0\x02".to_vec();
        collection.resize(20, 0);
        for (index, mut font) in fonts.into_iter().enumerate() {
            let base = collection.len() as u32;
            collection[12 + index * 4..16 + index * 4].copy_from_slice(&base.to_be_bytes());
            let count = u16::from_be_bytes(font[4..6].try_into().unwrap()) as usize;
            for table in 0..count {
                let at = 12 + table * 16 + 8;
                let offset = u32::from_be_bytes(font[at..at+4].try_into().unwrap()) + base;
                font[at..at+4].copy_from_slice(&offset.to_be_bytes());
            }
            collection.extend(font);
        }
        collection
    }

    #[test]
    fn selected_public_font_face_drives_cpu_metrics_pixels_and_coverage() {
        let root = std::env::temp_dir().join(format!("ethornell-public-font-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("wide.ttf"), square_font(700)).unwrap();
        std::fs::write(root.join("narrow.ttf"), square_font(400)).unwrap();
        std::fs::write(root.join("faces.ttc"), font_collection()).unwrap();
        let config = DisplayConfiguration { font_path: "wide.ttf".into(), ..Default::default() };
        let wide = config.load_font(&root).unwrap().unwrap();
        let narrow = DisplayConfiguration { font_path: "narrow.ttf".into(), ..Default::default() }.load_font(&root).unwrap().unwrap();
        assert_eq!(wide.glyph_id('中').0, 1);
        assert!(crate::snapshot::glyph_advance(Some(&wide), '中', 20.0, 100.0)
            > crate::snapshot::glyph_advance(Some(&narrow), '中', 20.0, 100.0));
        let mut image = ethornell_image::DecodedImage { width: 40, height: 40, rgba: vec![0; 40*40*4] };
        crate::snapshot::rasterize_bitmap_text(&mut image, Some(&wide), "中", 0, 0, 20.0, 0.0, 100.0, [1.0; 4]);
        assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
        let opcode = ethornell_vm::native_call::NativeOpcode { group: 0x92, id: 0x1F };
        let call = ethornell_vm::NativeCallFrame::new(opcode, vec![ethornell_vm::Value::Str("中".into())]);
        assert!(validate_display_font(Some(&wide), &call).is_ok());
        let call = ethornell_vm::NativeCallFrame::new(opcode, vec![ethornell_vm::Value::Str("文".into())]);
        assert!(validate_display_font(Some(&wide), &call).is_err());
        assert_eq!(call.args(), &[ethornell_vm::Value::Str("文".into())]);
        let resource_call = ethornell_vm::NativeCallFrame::new(
            ethornell_vm::native_call::NativeOpcode { group: 0x90, id: 0x10 },
            vec![ethornell_vm::Value::Str("文".into())],
        );
        assert!(validate_display_font(Some(&wide), &resource_call).is_ok());
        let face = DisplayConfiguration { font_path: "faces.ttc".into(), font_index: 1, ..Default::default() }
            .load_font(&root).unwrap().unwrap();
        assert_eq!(crate::snapshot::glyph_advance(Some(&face), '中', 20.0, 100.0),
            crate::snapshot::glyph_advance(Some(&narrow), '中', 20.0, 100.0));
        let config = DisplayConfiguration { font_index: 1, ..config };
        assert!(config.load_font(&root).is_err());
        std::fs::remove_file(root.join("wide.ttf")).unwrap();
        std::fs::remove_file(root.join("narrow.ttf")).unwrap();
        std::fs::remove_file(root.join("faces.ttc")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
