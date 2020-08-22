//pub use text_size::{TextSize, TextRange}; // ~Range<u32> with impl SliceIndex for String (fixme: const)

#[derive(derive_more::Deref)] pub(crate) struct LineRange<'t> { #[deref] line: &'t str, pub(crate) range: std::ops::Range<usize> }

pub(crate) fn line_ranges(text: &'t str) -> impl Iterator<Item=LineRange<'t>> {
	let mut iter = text.char_indices().peekable();
	std::iter::from_fn(move || {
		let &(start,_) = iter.peek()?;
		let range = start .. iter.find(|&(_,c)| c=='\n').map_or(text.len(), |(end,_)| end);
		Some(LineRange{line: &text[range.clone()], range})
	})
}

use {std::cmp::{min, max}, ttf_parser::{Face,GlyphId,Rect}, fehler::throws, error::Error, num::Ratio, crate::font::{self, Rasterize}};
pub mod unicode_segmentation;
use self::unicode_segmentation::{GraphemeIndex, UnicodeSegmentation};

pub struct Glyph {pub index: GraphemeIndex, pub x: i32, pub id: GlyphId }
pub fn layout<'t>(font: &'t Face<'t>, iter: impl Iterator<Item=(GraphemeIndex, &'t str)>+'t) -> impl 't+Iterator<Item=Glyph> {
	iter.scan((None, 0), move |(last_id, x), (index, g)| {
		use iter::Single;
		let c = g.chars().single().unwrap();
		let id = font.glyph_index(if c == '\t' { ' ' } else { c }).unwrap_or_else(||panic!("Missing glyph for '{:?}'",c));
		if let Some(last_id) = *last_id { *x += font.kerning_subtables().next().map_or(0, |x| x.glyphs_kerning(last_id, id).unwrap_or(0) as i32); }
		*last_id = Some(id);
		let next = Glyph{index, x: *x, id};
		*x += font.glyph_hor_advance(id)? as i32;
		Some(next)
	})
}

pub(crate) fn bbox<'t>(font: &'t Face<'t>, iter: impl Iterator<Item=Glyph>+'t) -> impl 't+Iterator<Item=(Rect, Glyph)> {
	iter.filter_map(move |g| Some((font.glyph_bounding_box(g.id)?, g)))
}

#[derive(Default)] pub(crate) struct LineMetrics {pub width: u32, pub ascent: i16, pub descent: i16}
pub(crate) fn metrics(font: &Face<'_>, iter: impl Iterator<Item=Glyph>) -> LineMetrics {
	bbox(font, iter).fold(Default::default(), |metrics: LineMetrics, (bbox, Glyph{x, id, ..})| LineMetrics{
		width: (x + font.glyph_hor_side_bearing(id).unwrap() as i32 + bbox.x_max as i32) as u32,
		ascent: max(metrics.ascent, bbox.y_max),
		descent: min(metrics.descent, bbox.y_min)
	})
}

pub type Color = image::bgrf;
#[derive(Clone,Copy)] pub enum FontStyle { Normal, Bold, /*Italic, BoldItalic*/ }
impl Default for FontStyle { fn default() -> Self { Self::Normal } }
#[derive(Clone,Copy,Default)] pub struct Style { pub color: Color, pub style: FontStyle }
pub type TextRange = std::ops::Range<u32>;
#[derive(Clone,derive_more::Deref)] pub struct Attribute<T> { #[deref] pub range: TextRange, pub attribute: T }

use std::lazy::SyncLazy;
#[allow(non_upper_case_globals)] pub static default_font : SyncLazy<font::File<'static>> = SyncLazy::new(|| font::open(
		["/usr/share/fonts/noto/NotoSans-Regular.ttf","/usr/share/fonts/liberation-fonts/LiberationSans-Regular.ttf"].iter().map(std::path::Path::new)
			.filter(|x| std::path::Path::exists(x))
			.next().unwrap()
	).unwrap());
#[allow(non_upper_case_globals)]
pub const default_style: [Attribute::<Style>; 1] = [Attribute{range: 0..u32::MAX, attribute: Style{color: Color{b:1.,r:1.,g:1.}, style: FontStyle::Normal}}];
use std::sync::Mutex;
pub static CACHE: SyncLazy<Mutex<Vec<((Ratio, GlyphId),Image<Vec<u8>>)>>> = SyncLazy::new(|| Mutex::new(Vec::new()));

pub struct View<'f, D> {
    pub font : &'f Face<'f>,
    pub data: D,
    //size : Option<size2>
}

use {::xy::{xy, size, uint2}, image::{Image, bgra8}, num::{IsZero, Zero, div_ceil, clamp}};

fn fit_width(width: u32, from : size) -> size { if from.is_zero() { return Zero::zero(); } xy{x: width, y: div_ceil(width * from.y, from.x)} }
fn fit_height(height: u32, from : size) -> size { if from.is_zero() { return Zero::zero(); } xy{x: div_ceil(height * from.x, from.y), y: height} }
fn fit(size: size, from: size) -> size { if size.x*from.y < size.y*from.x { fit_width(size.x, from) } else { fit_height(size.y, from) } }

impl<D:AsRef<str>> View<'_, D> {
	pub fn size(&/*mut*/ self) -> size {
		let Self{font, data, /*ref mut size,*/ ..} = self;
		//*size.get_or_insert_with(||{
			let text = data.as_ref();
			let (line_count, max_width) = line_ranges(&text).fold((0,0),|(line_count, width), line| (line_count+1, max(width, metrics(font, layout(font, line.graphemes(true).enumerate())).width)));
			xy{x: max_width, y: line_count * (font.height() as u32)}
		//})
	}
	pub fn scale(&/*mut*/ self, fit: size) -> Ratio {
		let size = Self::size(&self);
		if fit.x*size.y < fit.y*fit.x { Ratio{num: fit.x-1, div: size.x-1} } else { Ratio{num: fit.y-1, div: size.y-1} } // todo: scroll
	}
}

#[derive(PartialEq,Eq,PartialOrd,Ord,Clone,Copy)] pub struct LineColumn {
	pub line: usize,
	pub column: GraphemeIndex // May be on the right of the corresponding line (preserves horizontal affinity during line up/down movement)
}
impl Zero for LineColumn { fn zero() -> Self { Self{line: 0, column: 0} } }

impl<D:AsRef<str>> View<'_, D> {
	pub fn cursor(&self, size: size, position: uint2) -> LineColumn {
		let position = position / self.scale(size);
		let View{font, data, ..} = self;
		let text = data.as_ref();
		let line = clamp(0, (position.y/font.height() as u32) as usize, line_ranges(text).count()-1);
		LineColumn{line, column:
			layout(font, line_ranges(text).nth(line).unwrap().graphemes(true).enumerate())
			.map(|Glyph{index, x, id}| (index, x+font.glyph_hor_advance(id).unwrap() as i32/2))
			.take_while(|&(_, x)| x <= position.x as i32).last().map(|(index,_)| index+1).unwrap_or(0)
		}
	}
}

impl<D:AsRef<str>+AsRef<[Attribute<Style>]>> View<'_, D> {
	pub fn paint(&mut self, target : &mut Image<&mut[bgra8]>, scale: Ratio) {
		if target.size < self.size(target.size) { return; }
		//assert!(target.size >= self.size(target.size), "{:?} {:?} ", target.size, self.size(target.size));
		let Self{font, data} = &*self;
		let (mut style, mut styles) = (None, AsRef::<[Attribute<Style>]>::as_ref(&data).iter().peekable());
		for (line_index, line) in line_ranges(&data.as_ref()).enumerate() {
			for (bbox, Glyph{index, x, id}) in bbox(font, layout(font, line.graphemes(true).enumerate())) {
				use iter::{PeekableExt, Single};
				style = style.filter(|style:&&Attribute<Style>| style.contains(&(index as u32))).or_else(|| styles.peeking_take_while(|style| style.contains(&(index as u32))).single());
				let mut cache = CACHE.lock().unwrap();
				if cache.iter().find(|(key,_)| key == &(scale, id)).is_none() {
					cache.push( ((scale, id), image::from_linear(&self.font.rasterize(scale, id, bbox).as_ref())) );
				};
				let coverage = &cache.iter().find(|(key,_)| key == &(scale, id)).unwrap().1;
				let position = xy{
					x: (x+font.glyph_hor_side_bearing(id).unwrap() as i32) as u32,
					y: (line_index as u32)*(font.height() as u32) + (font.ascender()-bbox.y_max) as u32
				};
				image::fill_mask(&mut target.slice_mut(scale*position, coverage.size), style.map(|x|x.attribute.color).unwrap_or((1.).into()), &coverage.as_ref());
			}
		}
	}
}
use crate::widget::{Widget, Target};
impl<'f, D:AsRef<str>+AsRef<[Attribute<Style>]>> Widget for View<'f, D> {
    fn size(&mut self, size: size) -> size { fit(size, Self::size(&self)) }
    #[throws] fn paint(&mut self, target : &mut Target) {
        let scale = self.scale(target.size);
        Self::paint(self, target, scale)
    }
}
