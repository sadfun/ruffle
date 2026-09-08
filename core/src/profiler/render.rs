//! A `RenderBackend` decorator that records the expensive, usually one-off
//! renderer operations (shape tessellation, texture uploads, offscreen
//! renders, GPU readbacks) and the per-frame `submit_frame` cost.
//!
//! Only installed by the `shararam_profiler` feature; see `PlayerBuilder`.

use super::{Counter, inc, span};
use ruffle_render::backend::{
    BitmapCacheEntry, Context3D, Context3DProfile, PixelBenderOutput, PixelBenderTarget,
    RenderBackend, ShapeHandle, ViewportDimensions,
};
use ruffle_render::bitmap::{
    Bitmap, BitmapHandle, BitmapSource, PixelRegion, RgbaBufRead, SyncHandle,
};
use ruffle_render::commands::CommandList;
use ruffle_render::error::Error;
use ruffle_render::filters::Filter;
use ruffle_render::matrix::Matrix;
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::DistilledShape;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::num::NonZeroU32;

/// Screen-cost grid resolution. 24×18 over a 760×600 stage is ~32×33 px per
/// cell — coarse enough to stay cheap, fine enough to point at a screen area.
const GRID_W: usize = 24;
const GRID_H: usize = 18;
const GRID_CELLS: usize = GRID_W * GRID_H;
/// Offscreen-costly subtrees (blends, alpha masks) reported per frame with
/// their screen rects, capped so a pathological frame cannot flood the log.
const MAX_HOT_RECTS: usize = 32;
/// Handle-geometry maps are cleared past this size so memory stays bounded
/// (pointer keys can be reused after a handle is dropped anyway).
const MAX_TRACKED_HANDLES: usize = 100_000;

pub struct ProfiledRenderer {
    inner: Box<dyn RenderBackend>,
    debug_info_reported: bool,
    /// Local-space pixel bounds of registered shapes, keyed by handle pointer.
    shape_bounds: HashMap<usize, [f32; 4]>,
    /// Pixel dimensions of registered bitmaps, keyed by handle pointer.
    bitmap_sizes: HashMap<usize, (f32, f32)>,
}

impl ProfiledRenderer {
    pub fn new(inner: Box<dyn RenderBackend>) -> Self {
        Self {
            inner,
            debug_info_reported: false,
            shape_bounds: HashMap::new(),
            bitmap_sizes: HashMap::new(),
        }
    }

    fn remember_shape(&mut self, handle: &ShapeHandle, bounds: &swf::Rectangle<swf::Twips>) {
        if self.shape_bounds.len() >= MAX_TRACKED_HANDLES {
            self.shape_bounds.clear();
        }
        self.shape_bounds.insert(
            shape_key(handle),
            [
                bounds.x_min.to_pixels() as f32,
                bounds.y_min.to_pixels() as f32,
                bounds.x_max.to_pixels() as f32,
                bounds.y_max.to_pixels() as f32,
            ],
        );
    }

    fn remember_bitmap(&mut self, handle: &BitmapHandle, width: f32, height: f32) {
        if self.bitmap_sizes.len() >= MAX_TRACKED_HANDLES {
            self.bitmap_sizes.clear();
        }
        self.bitmap_sizes
            .insert(bitmap_key(handle), (width, height));
    }
}

fn shape_key(handle: &ShapeHandle) -> usize {
    std::sync::Arc::as_ptr(&handle.0) as *const () as usize
}

fn bitmap_key(handle: &BitmapHandle) -> usize {
    std::sync::Arc::as_ptr(&handle.0) as *const () as usize
}

/// Screen AABB (in viewport pixels) of a local-pixel-space rect pushed
/// through a command matrix. Mesh vertices and bitmap quads are in pixels and
/// command matrices are absolute (the stage's viewport matrix is at the root
/// of the transform stack), so the result is directly in viewport pixels.
fn transformed_aabb(matrix: &Matrix, rect: [f32; 4]) -> [f32; 4] {
    let tx = matrix.tx.to_pixels() as f32;
    let ty = matrix.ty.to_pixels() as f32;
    let mut out = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (x, y) in [
        (rect[0], rect[1]),
        (rect[2], rect[1]),
        (rect[0], rect[3]),
        (rect[2], rect[3]),
    ] {
        let sx = matrix.a * x + matrix.c * y + tx;
        let sy = matrix.b * x + matrix.d * y + ty;
        out[0] = out[0].min(sx);
        out[1] = out[1].min(sy);
        out[2] = out[2].max(sx);
        out[3] = out[3].max(sy);
    }
    out
}

fn union_into(union: &mut Option<[f32; 4]>, rect: [f32; 4]) {
    match union {
        Some(u) => {
            u[0] = u[0].min(rect[0]);
            u[1] = u[1].min(rect[1]);
            u[2] = u[2].max(rect[2]);
            u[3] = u[3].max(rect[3]);
        }
        None => *union = Some(rect),
    }
}

/// Per-frame accumulation of where on screen the draw commands land.
struct ScreenGrid {
    /// Number of draw commands covering each cell (overdraw).
    draws: [u32; GRID_CELLS],
    /// Same, but only for commands inside blend/alpha-mask subtrees, which
    /// cost full offscreen passes in the wgpu backend.
    heavy: [u32; GRID_CELLS],
    cells_per_px_x: f32,
    cells_per_px_y: f32,
    /// Blend/alpha-mask subtrees with their union screen rect.
    hot: Vec<([f32; 4], &'static str)>,
}

impl ScreenGrid {
    fn new(viewport_w: f32, viewport_h: f32) -> Self {
        Self {
            draws: [0; GRID_CELLS],
            heavy: [0; GRID_CELLS],
            cells_per_px_x: GRID_W as f32 / viewport_w.max(1.0),
            cells_per_px_y: GRID_H as f32 / viewport_h.max(1.0),
            hot: Vec::new(),
        }
    }

    fn add_rect(&mut self, rect: [f32; 4], heavy: bool) {
        if !rect.iter().all(|v| v.is_finite()) || rect[2] <= rect[0] || rect[3] <= rect[1] {
            return;
        }
        let x0 = ((rect[0] * self.cells_per_px_x).floor() as i32).clamp(0, GRID_W as i32 - 1);
        let x1 = ((rect[2] * self.cells_per_px_x).ceil() as i32).clamp(1, GRID_W as i32);
        let y0 = ((rect[1] * self.cells_per_px_y).floor() as i32).clamp(0, GRID_H as i32 - 1);
        let y1 = ((rect[3] * self.cells_per_px_y).ceil() as i32).clamp(1, GRID_H as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let cell = y as usize * GRID_W + x as usize;
                self.draws[cell] += 1;
                if heavy {
                    self.heavy[cell] += 1;
                }
            }
        }
    }

    fn to_json(&self, viewport_w: u32, viewport_h: u32) -> String {
        let mut out = String::with_capacity(GRID_CELLS * 3 + 256);
        let _ = write!(
            out,
            "{{\"vw\":{viewport_w},\"vh\":{viewport_h},\"gw\":{GRID_W},\"gh\":{GRID_H},\"draws\":["
        );
        for (index, count) in self.draws.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "{count}");
        }
        out.push(']');
        if self.heavy.iter().any(|&count| count > 0) {
            out.push_str(",\"heavy\":[");
            let mut first = true;
            for (index, &count) in self.heavy.iter().enumerate() {
                if count > 0 {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    let _ = write!(out, "[{index},{count}]");
                }
            }
            out.push(']');
        }
        if !self.hot.is_empty() {
            out.push_str(",\"hot\":[");
            for (index, (rect, kind)) in self.hot.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let _ = write!(
                    out,
                    "[{:.0},{:.0},{:.0},{:.0},\"{kind}\"]",
                    rect[0],
                    rect[1],
                    rect[2] - rect[0],
                    rect[3] - rect[1]
                );
            }
            out.push(']');
        }
        out.push('}');
        out
    }
}

/// Per-frame breakdown of the command tree. Offscreen-heavy constructs
/// (`Blend`, `RenderAlphaMask`) are the interesting part: in the wgpu
/// backend each costs full-target-sized intermediate textures per frame.
#[derive(Default)]
struct CommandStats {
    shapes: u32,
    bitmaps: u32,
    rects: u32,
    stencil_masks: u32,
    alpha_masks: u32,
    blend_layer: u32,
    blend_alpha_erase: u32,
    blend_complex: u32,
    blend_shader: u32,
    max_depth: u32,
}

impl CommandStats {
    fn json_fields(&self) -> String {
        format!(
            "\"shapes\":{},\"bitmaps\":{},\"rects\":{},\"stencil_masks\":{},\"alpha_masks\":{},\"blend_layer\":{},\"blend_alpha_erase\":{},\"blend_complex\":{},\"blend_shader\":{},\"max_depth\":{}",
            self.shapes,
            self.bitmaps,
            self.rects,
            self.stencil_masks,
            self.alpha_masks,
            self.blend_layer,
            self.blend_alpha_erase,
            self.blend_complex,
            self.blend_shader,
            self.max_depth
        )
    }
}

struct CommandWalk<'a> {
    stats: CommandStats,
    grid: ScreenGrid,
    shape_bounds: &'a HashMap<usize, [f32; 4]>,
    bitmap_sizes: &'a HashMap<usize, (f32, f32)>,
}

impl CommandWalk<'_> {
    /// Returns the union screen rect of the subtree's draw commands.
    fn walk(&mut self, list: &CommandList, depth: u32, heavy: bool) -> Option<[f32; 4]> {
        use ruffle_render::commands::{Command, RenderBlendMode};
        if depth > self.stats.max_depth {
            self.stats.max_depth = depth;
        }
        let mut union: Option<[f32; 4]> = None;
        for command in &list.commands {
            match command {
                Command::RenderShape { shape, transform } => {
                    self.stats.shapes += 1;
                    if let Some(bounds) = self.shape_bounds.get(&shape_key(shape)) {
                        let rect = transformed_aabb(&transform.matrix, *bounds);
                        self.grid.add_rect(rect, heavy);
                        union_into(&mut union, rect);
                    }
                }
                Command::RenderBitmap {
                    bitmap, transform, ..
                }
                | Command::RenderStage3D {
                    bitmap, transform, ..
                } => {
                    self.stats.bitmaps += 1;
                    if let Some(&(width, height)) = self.bitmap_sizes.get(&bitmap_key(bitmap)) {
                        let rect = transformed_aabb(&transform.matrix, [0.0, 0.0, width, height]);
                        self.grid.add_rect(rect, heavy);
                        union_into(&mut union, rect);
                    }
                }
                Command::DrawRect { .. }
                | Command::DrawLine { .. }
                | Command::DrawLineRect { .. } => self.stats.rects += 1,
                Command::PushMask => self.stats.stencil_masks += 1,
                Command::ActivateMask | Command::DeactivateMask | Command::PopMask => {}
                Command::RenderAlphaMask {
                    maskee_commands,
                    mask_commands,
                } => {
                    self.stats.alpha_masks += 1;
                    let mut sub = self.walk(maskee_commands, depth + 1, true);
                    if let Some(rect) = self.walk(mask_commands, depth + 1, true) {
                        union_into(&mut sub, rect);
                    }
                    if let Some(rect) = sub {
                        if self.grid.hot.len() < MAX_HOT_RECTS {
                            self.grid.hot.push((rect, "alpha_mask"));
                        }
                        union_into(&mut union, rect);
                    }
                }
                Command::Blend(inner, mode, _) => {
                    let kind = match mode {
                        RenderBlendMode::Builtin(swf::BlendMode::Layer) => {
                            self.stats.blend_layer += 1;
                            "blend_layer"
                        }
                        RenderBlendMode::Builtin(swf::BlendMode::Alpha | swf::BlendMode::Erase) => {
                            self.stats.blend_alpha_erase += 1;
                            "blend_alpha_erase"
                        }
                        RenderBlendMode::Builtin(_) => {
                            self.stats.blend_complex += 1;
                            "blend"
                        }
                        RenderBlendMode::Shader(_) => {
                            self.stats.blend_shader += 1;
                            "blend_shader"
                        }
                    };
                    if let Some(rect) = self.walk(inner, depth + 1, true) {
                        if self.grid.hot.len() < MAX_HOT_RECTS {
                            self.grid.hot.push((rect, kind));
                        }
                        union_into(&mut union, rect);
                    }
                }
            }
        }
        union
    }
}

fn filter_name(filter: &Filter) -> &'static str {
    match filter {
        Filter::BevelFilter(_) => "bevel",
        Filter::BlurFilter(_) => "blur",
        Filter::ColorMatrixFilter(_) => "color_matrix",
        Filter::ConvolutionFilter(_) => "convolution",
        Filter::DisplacementMapFilter(_) => "displacement_map",
        Filter::DropShadowFilter(_) => "drop_shadow",
        Filter::GlowFilter(_) => "glow",
        Filter::GradientBevelFilter(_) => "gradient_bevel",
        Filter::GradientGlowFilter(_) => "gradient_glow",
        Filter::ShaderFilter(_) => "shader",
    }
}

impl RenderBackend for ProfiledRenderer {
    fn viewport_dimensions(&self) -> ViewportDimensions {
        self.inner.viewport_dimensions()
    }

    fn set_viewport_dimensions(&mut self, dimensions: ViewportDimensions) {
        super::instant("render", "viewport", || {
            format!(
                "{{\"width\":{},\"height\":{},\"scale\":{}}}",
                dimensions.width, dimensions.height, dimensions.scale_factor
            )
        });
        self.inner.set_viewport_dimensions(dimensions)
    }

    fn register_shape(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
    ) -> ShapeHandle {
        inc(Counter::ShapesRegistered);
        let bounds = shape.shape_bounds;
        let _span = span("render", "register_shape").args(|| {
            format!(
                "{{\"id\":{},\"paths\":{},\"w\":{:.0},\"h\":{:.0},\"movie\":{}}}",
                shape.id,
                shape.paths.len(),
                bounds.width().to_pixels(),
                bounds.height().to_pixels(),
                super::movie_json()
            )
        });
        let handle = self.inner.register_shape(shape, bitmap_source);
        self.remember_shape(&handle, &bounds);
        handle
    }

    fn register_shape_with_scale(
        &mut self,
        shape: DistilledShape,
        bitmap_source: &dyn BitmapSource,
        scale: f32,
    ) -> ShapeHandle {
        inc(Counter::ShapesRegistered);
        let bounds = shape.shape_bounds;
        let _span = span("render", "register_shape").args(|| {
            format!(
                "{{\"id\":{},\"paths\":{},\"w\":{:.0},\"h\":{:.0},\"scale\":{scale},\"movie\":{}}}",
                shape.id,
                shape.paths.len(),
                bounds.width().to_pixels(),
                bounds.height().to_pixels(),
                super::movie_json()
            )
        });
        let handle = self
            .inner
            .register_shape_with_scale(shape, bitmap_source, scale);
        self.remember_shape(&handle, &bounds);
        handle
    }

    fn render_offscreen(
        &mut self,
        handle: BitmapHandle,
        commands: CommandList,
        quality: StageQuality,
        bounds: PixelRegion,
    ) -> Option<Box<dyn SyncHandle>> {
        inc(Counter::OffscreenRenders);
        let _span = span("render", "render_offscreen").args(|| {
            format!(
                "{{\"commands\":{},\"w\":{},\"h\":{}}}",
                commands.commands.len(),
                bounds.width(),
                bounds.height()
            )
        });
        self.inner
            .render_offscreen(handle, commands, quality, bounds)
    }

    fn apply_filter(
        &mut self,
        source: BitmapHandle,
        source_point: (u32, u32),
        source_size: (u32, u32),
        destination: BitmapHandle,
        dest_point: (i32, i32),
        filter: Filter,
    ) -> Option<Box<dyn SyncHandle>> {
        inc(Counter::OffscreenRenders);
        let _span = span("render", "apply_filter").args(|| {
            format!(
                "{{\"filter\":\"{}\",\"w\":{},\"h\":{}}}",
                filter_name(&filter),
                source_size.0,
                source_size.1
            )
        });
        self.inner.apply_filter(
            source,
            source_point,
            source_size,
            destination,
            dest_point,
            filter,
        )
    }

    fn is_filter_supported(&self, filter: &Filter) -> bool {
        self.inner.is_filter_supported(filter)
    }

    fn is_offscreen_supported(&self) -> bool {
        self.inner.is_offscreen_supported()
    }

    fn submit_frame(
        &mut self,
        clear: swf::Color,
        commands: CommandList,
        cache_entries: Vec<BitmapCacheEntry>,
    ) {
        if !self.debug_info_reported {
            self.debug_info_reported = true;
            let info = self.inner.debug_info().replace(['\n', '"'], " ");
            crate::profiler::instant("render", "debug_info", || {
                format!("{{\"info\":{}}}", crate::profiler::json_str(&info))
            });
        }
        let dimensions = self.inner.viewport_dimensions();
        let mut walk = CommandWalk {
            stats: CommandStats::default(),
            grid: ScreenGrid::new(dimensions.width as f32, dimensions.height as f32),
            shape_bounds: &self.shape_bounds,
            bitmap_sizes: &self.bitmap_sizes,
        };
        walk.walk(&commands, 0, false);
        let CommandWalk { stats, grid, .. } = walk;
        if !commands.commands.is_empty() {
            super::instant("render", "screen_grid", || {
                grid.to_json(dimensions.width, dimensions.height)
            });
        }
        let _span = span("render", "submit_frame").args(|| {
            let cache_commands: usize = cache_entries
                .iter()
                .map(|entry| entry.commands.commands.len())
                .sum();
            format!(
                "{{\"commands\":{},\"cache_entries\":{},\"cache_commands\":{cache_commands},{}}}",
                commands.commands.len(),
                cache_entries.len(),
                stats.json_fields()
            )
        });
        self.inner.submit_frame(clear, commands, cache_entries)
    }

    fn create_empty_texture(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<BitmapHandle, Error> {
        let _span = span("render", "create_empty_texture")
            .args(|| format!("{{\"w\":{width},\"h\":{height}}}"));
        let handle = self.inner.create_empty_texture(width, height);
        if let Ok(handle) = &handle {
            self.remember_bitmap(handle, width.get() as f32, height.get() as f32);
        }
        handle
    }

    fn register_bitmap(&mut self, bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        inc(Counter::BitmapsRegistered);
        let (width, height) = (bitmap.width(), bitmap.height());
        let _span = span("render", "register_bitmap").args(|| {
            format!(
                "{{\"w\":{width},\"h\":{height},\"bytes\":{},\"movie\":{}}}",
                bitmap.data().len(),
                super::movie_json()
            )
        });
        let handle = self.inner.register_bitmap(bitmap);
        if let Ok(handle) = &handle {
            self.remember_bitmap(handle, width as f32, height as f32);
        }
        handle
    }

    fn update_texture(
        &mut self,
        handle: &BitmapHandle,
        bitmap: Bitmap<'_>,
        region: PixelRegion,
    ) -> Result<(), Error> {
        inc(Counter::TexturesUpdated);
        let _span = span("render", "update_texture")
            .min_duration_ms(0.05)
            .args(|| {
                format!(
                    "{{\"w\":{},\"h\":{},\"region_w\":{},\"region_h\":{}}}",
                    bitmap.width(),
                    bitmap.height(),
                    region.width(),
                    region.height()
                )
            });
        self.inner.update_texture(handle, bitmap, region)
    }

    fn create_context3d(&mut self, profile: Context3DProfile) -> Result<Box<dyn Context3D>, Error> {
        self.inner.create_context3d(profile)
    }

    fn debug_info(&self) -> Cow<'static, str> {
        self.inner.debug_info()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn set_quality(&mut self, quality: StageQuality) {
        self.inner.set_quality(quality)
    }

    fn compile_pixelbender_shader(
        &mut self,
        shader: PixelBenderShader,
    ) -> Result<PixelBenderShaderHandle, Error> {
        let _span = span("render", "compile_pixelbender_shader");
        self.inner.compile_pixelbender_shader(shader)
    }

    fn run_pixelbender_shader(
        &mut self,
        handle: PixelBenderShaderHandle,
        arguments: &[PixelBenderShaderArgument],
        target: &PixelBenderTarget,
    ) -> Result<PixelBenderOutput, Error> {
        let _span = span("render", "run_pixelbender_shader");
        self.inner.run_pixelbender_shader(handle, arguments, target)
    }

    fn resolve_sync_handle(
        &mut self,
        handle: Box<dyn SyncHandle>,
        with_rgba: RgbaBufRead,
    ) -> Result<(), Error> {
        // A GPU readback stalls the pipeline; always worth seeing.
        let _span = span("render", "gpu_readback");
        self.inner.resolve_sync_handle(handle, with_rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_grid_accumulates_rects() {
        let mut grid = ScreenGrid::new(240.0, 180.0); // 10×10 px cells
        grid.add_rect([0.0, 0.0, 10.0, 10.0], false); // exactly the first cell
        grid.add_rect([0.0, 0.0, 240.0, 180.0], true); // full screen, heavy
        grid.hot.push(([12.0, 34.0, 56.0, 78.0], "blend_layer"));
        let json = grid.to_json(240, 180);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let draws = parsed["draws"].as_array().unwrap();
        assert_eq!(draws.len(), GRID_CELLS);
        assert_eq!(draws[0], 2);
        assert_eq!(draws[GRID_CELLS - 1], 1);
        assert_eq!(parsed["heavy"].as_array().unwrap().len(), GRID_CELLS);
        assert_eq!(parsed["hot"][0][4], "blend_layer");
        assert_eq!(parsed["vw"], 240);
    }

    #[test]
    fn transformed_aabb_handles_rotation() {
        // 90° rotation: (a,b,c,d) = (0,1,-1,0), so a w×h rect becomes h×w.
        let matrix = Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            tx: swf::Twips::from_pixels(100.0),
            ty: swf::Twips::from_pixels(50.0),
        };
        let rect = transformed_aabb(&matrix, [0.0, 0.0, 30.0, 10.0]);
        assert_eq!(rect, [90.0, 50.0, 100.0, 80.0]);
    }
}
