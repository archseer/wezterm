use crate::config::configuration;
use crate::font::ftwrap;
use crate::font::hbwrap as harfbuzz;
use crate::font::locator::FontDataHandle;
use crate::font::shaper::{FallbackIdx, FontMetrics, FontShaper, GlyphInfo};
use crate::font::units::*;
use anyhow::{anyhow, bail};
use log::{debug, error};
use std::cell::{RefCell, RefMut};

fn make_glyphinfo(
    text: &str,
    font_idx: usize,
    info: &harfbuzz::hb_glyph_info_t,
    pos: &harfbuzz::hb_glyph_position_t,
) -> GlyphInfo {
    use termwiz::cell::unicode_column_width;
    let num_cells = unicode_column_width(text) as u8;
    GlyphInfo {
        #[cfg(debug_assertions)]
        text: text.into(),
        num_cells,
        font_idx,
        glyph_pos: info.codepoint,
        cluster: info.cluster,
        x_advance: PixelLength::new(f64::from(pos.x_advance) / 64.0),
        y_advance: PixelLength::new(f64::from(pos.y_advance) / 64.0),
        x_offset: PixelLength::new(f64::from(pos.x_offset) / 64.0),
        y_offset: PixelLength::new(f64::from(pos.y_offset) / 64.0),
    }
}

struct FontPair {
    face: ftwrap::Face,
    font: harfbuzz::Font,
    size: f64,
    dpi: u32,
}

pub struct HarfbuzzShaper {
    handles: Vec<FontDataHandle>,
    fonts: Vec<RefCell<Option<FontPair>>>,
    lib: ftwrap::Library,
}

impl HarfbuzzShaper {
    pub fn new(handles: &[FontDataHandle]) -> anyhow::Result<Self> {
        let lib = ftwrap::Library::new()?;
        let handles = handles.to_vec();
        let mut fonts = vec![];
        for _ in 0..handles.len() {
            fonts.push(RefCell::new(None));
        }
        Ok(Self {
            fonts,
            handles,
            lib,
        })
    }

    fn load_fallback(&self, font_idx: FallbackIdx) -> anyhow::Result<Option<RefMut<FontPair>>> {
        if font_idx >= self.handles.len() {
            return Ok(None);
        }
        match self.fonts.get(font_idx) {
            None => Ok(None),
            Some(opt_pair) => {
                let mut opt_pair = opt_pair.borrow_mut();
                if opt_pair.is_none() {
                    log::trace!("shaper wants {} {:?}", font_idx, &self.handles[font_idx]);
                    let face = self.lib.face_from_locator(&self.handles[font_idx])?;
                    let mut font = harfbuzz::Font::new(face.face);
                    let load_flags = ftwrap::compute_load_flags_from_config();
                    font.set_load_flags(load_flags);
                    *opt_pair = Some(FontPair {
                        face,
                        font,
                        size: 0.,
                        dpi: 0,
                    });
                }

                Ok(Some(RefMut::map(opt_pair, |opt_pair| {
                    opt_pair.as_mut().unwrap()
                })))
            }
        }
    }

    fn do_shape(
        &self,
        font_idx: FallbackIdx,
        s: &str,
        font_size: f64,
        dpi: u32,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        let config = configuration();
        let features: Vec<harfbuzz::hb_feature_t> = config
            .harfbuzz_features
            .iter()
            .filter_map(|s| harfbuzz::feature_from_string(s).ok())
            .collect();

        let mut buf = harfbuzz::Buffer::new()?;
        buf.set_script(harfbuzz::hb_script_t::HB_SCRIPT_LATIN);
        buf.set_direction(harfbuzz::hb_direction_t::HB_DIRECTION_LTR);
        buf.set_language(harfbuzz::language_from_string("en")?);
        buf.add_str(s);

        {
            match self.load_fallback(font_idx)? {
                #[allow(clippy::float_cmp)]
                Some(mut pair) => {
                    if pair.size != font_size || pair.dpi != dpi {
                        pair.face.set_font_size(font_size, dpi)?;
                        pair.size = font_size;
                        pair.dpi = dpi;
                    }
                    pair.font.shape(&mut buf, Some(features.as_slice()));
                }
                None => {
                    let chars: Vec<u32> = s.chars().map(|c| c as u32).collect();
                    bail!("No more fallbacks while shaping {:x?}", chars);
                }
            }
        }

        let infos = buf.glyph_infos();
        let positions = buf.glyph_positions();

        let mut cluster = Vec::new();

        let mut last_text_pos = None;
        let mut first_fallback_pos = None;

        // Compute the lengths of the text clusters.
        // Ligatures and combining characters mean
        // that a single glyph can take the place of
        // multiple characters.  The 'cluster' member
        // of the glyph info is set to the position
        // in the input utf8 text, so we make a pass
        // over the set of clusters to look for differences
        // greater than 1 and backfill the length of
        // the corresponding text fragment.  We need
        // the fragments to properly handle fallback,
        // and they're handy to have for debugging
        // purposes too.
        let mut sizes = Vec::with_capacity(s.len());
        for (i, info) in infos.iter().enumerate() {
            let pos = info.cluster as usize;
            let mut size = 1;
            if let Some(last_pos) = last_text_pos {
                let diff = pos - last_pos;
                if diff > 1 {
                    sizes[i - 1] = diff;
                }
            } else if pos != 0 {
                size = pos;
            }
            last_text_pos = Some(pos);
            sizes.push(size);
        }
        if let Some(last_pos) = last_text_pos {
            let diff = s.len() - last_pos;
            if diff > 1 {
                let last = sizes.len() - 1;
                sizes[last] = diff;
            }
        }
        //debug!("sizes: {:?}", sizes);

        // Now make a second pass to determine if we need
        // to perform fallback to a later font.
        // We can determine this by looking at the codepoint.
        for (i, info) in infos.iter().enumerate() {
            let pos = info.cluster as usize;
            if info.codepoint == 0 {
                if first_fallback_pos.is_none() {
                    // Start of a run that needs fallback
                    first_fallback_pos = Some(pos);
                }
            } else if let Some(start_pos) = first_fallback_pos {
                // End of a fallback run
                //debug!("range: {:?}-{:?} needs fallback", start, pos);

                let substr = &s[start_pos..pos];
                let mut shape = match self.do_shape(font_idx + 1, substr, font_size, dpi) {
                    Ok(shape) => Ok(shape),
                    Err(e) => {
                        error!("{:?} for {:?}", e, substr);
                        if font_idx == 0 && s == "?" {
                            bail!("unable to find any usable glyphs for `?` in font_idx 0");
                        }
                        self.do_shape(0, "?", font_size, dpi)
                    }
                }?;

                // Fixup the cluster member to match our current offset
                for mut info in &mut shape {
                    info.cluster += start_pos as u32;
                }
                cluster.append(&mut shape);

                first_fallback_pos = None;
            }
            if info.codepoint != 0 {
                if s.is_char_boundary(pos) && s.is_char_boundary(pos + sizes[i]) {
                    let text = &s[pos..pos + sizes[i]];
                    //debug!("glyph from `{}`", text);
                    cluster.push(make_glyphinfo(text, font_idx, info, &positions[i]));
                } else {
                    cluster.append(&mut self.do_shape(0, "?", font_size, dpi)?);
                }
            }
        }

        // Check to see if we started and didn't finish a
        // fallback run.
        if let Some(start_pos) = first_fallback_pos {
            let substr = &s[start_pos..];
            if false {
                debug!(
                    "at end {:?}-{:?} needs fallback {}",
                    start_pos,
                    s.len() - 1,
                    substr,
                );
            }
            let mut shape = match self.do_shape(font_idx + 1, substr, font_size, dpi) {
                Ok(shape) => Ok(shape),
                Err(e) => {
                    error!("{:?} for {:?}", e, substr);
                    if font_idx == 0 && s == "?" {
                        bail!("unable to find any usable glyphs for `?` in font_idx 0");
                    }
                    self.do_shape(0, "?", font_size, dpi)
                }
            }?;
            // Fixup the cluster member to match our current offset
            for mut info in &mut shape {
                info.cluster += start_pos as u32;
            }
            cluster.append(&mut shape);
        }

        //debug!("shaped: {:#?}", cluster);

        Ok(cluster)
    }
}

impl FontShaper for HarfbuzzShaper {
    fn shape(&self, text: &str, size: f64, dpi: u32) -> anyhow::Result<Vec<GlyphInfo>> {
        let start = std::time::Instant::now();
        let result = self.do_shape(0, text, size, dpi);
        metrics::value!("shape.harfbuzz", start.elapsed());
        result
    }

    fn metrics(&self, size: f64, dpi: u32) -> anyhow::Result<FontMetrics> {
        let mut pair = self
            .load_fallback(0)?
            .ok_or_else(|| anyhow!("unable to load first font!?"))?;
        let (cell_width, cell_height) = pair.face.set_font_size(size, dpi)?;
        let y_scale = unsafe { (*(*pair.face.face).size).metrics.y_scale as f64 / 65536.0 };
        let metrics = FontMetrics {
            cell_height: PixelLength::new(cell_height),
            cell_width: PixelLength::new(cell_width),
            // Note: face.face.descender is useless, we have to go through
            // face.face.size.metrics to get to the real descender!
            descender: PixelLength::new(
                unsafe { (*(*pair.face.face).size).metrics.descender as f64 } / 64.0,
            ),
            underline_thickness: PixelLength::new(
                unsafe { (*pair.face.face).underline_thickness as f64 } * y_scale / 64.,
            ),
            underline_position: PixelLength::new(
                unsafe { (*pair.face.face).underline_position as f64 } * y_scale / 64.,
            ),
        };

        log::trace!("metrics: {:?}", metrics);

        Ok(metrics)
    }
}
