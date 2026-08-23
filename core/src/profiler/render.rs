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
use ruffle_render::pixel_bender::{PixelBenderShader, PixelBenderShaderHandle};
use ruffle_render::pixel_bender_support::PixelBenderShaderArgument;
use ruffle_render::quality::StageQuality;
use ruffle_render::shape_utils::DistilledShape;
use std::borrow::Cow;
use std::num::NonZeroU32;

pub struct ProfiledRenderer {
    inner: Box<dyn RenderBackend>,
    debug_info_reported: bool,
}

impl ProfiledRenderer {
    pub fn new(inner: Box<dyn RenderBackend>) -> Self {
        Self {
            inner,
            debug_info_reported: false,
        }
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

fn walk_commands(list: &CommandList, depth: u32, stats: &mut CommandStats) {
    use ruffle_render::commands::{Command, RenderBlendMode};
    if depth > stats.max_depth {
        stats.max_depth = depth;
    }
    for command in &list.commands {
        match command {
            Command::RenderShape { .. } => stats.shapes += 1,
            Command::RenderBitmap { .. } | Command::RenderStage3D { .. } => stats.bitmaps += 1,
            Command::DrawRect { .. } | Command::DrawLine { .. } | Command::DrawLineRect { .. } => {
                stats.rects += 1
            }
            Command::PushMask => stats.stencil_masks += 1,
            Command::ActivateMask | Command::DeactivateMask | Command::PopMask => {}
            Command::RenderAlphaMask {
                maskee_commands,
                mask_commands,
            } => {
                stats.alpha_masks += 1;
                walk_commands(maskee_commands, depth + 1, stats);
                walk_commands(mask_commands, depth + 1, stats);
            }
            Command::Blend(inner, mode) => {
                match mode {
                    RenderBlendMode::Builtin(swf::BlendMode::Layer) => stats.blend_layer += 1,
                    RenderBlendMode::Builtin(
                        swf::BlendMode::Alpha | swf::BlendMode::Erase,
                    ) => stats.blend_alpha_erase += 1,
                    RenderBlendMode::Builtin(_) => stats.blend_complex += 1,
                    RenderBlendMode::Shader(_) => stats.blend_shader += 1,
                }
                walk_commands(inner, depth + 1, stats);
            }
        }
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
        self.inner.register_shape(shape, bitmap_source)
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
        self.inner
            .register_shape_with_scale(shape, bitmap_source, scale)
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
        let _span = span("render", "submit_frame").args(|| {
            let mut stats = CommandStats::default();
            walk_commands(&commands, 0, &mut stats);
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
        self.inner.create_empty_texture(width, height)
    }

    fn register_bitmap(&mut self, bitmap: Bitmap<'_>) -> Result<BitmapHandle, Error> {
        inc(Counter::BitmapsRegistered);
        let _span = span("render", "register_bitmap").args(|| {
            format!(
                "{{\"w\":{},\"h\":{},\"bytes\":{},\"movie\":{}}}",
                bitmap.width(),
                bitmap.height(),
                bitmap.data().len(),
                super::movie_json()
            )
        });
        self.inner.register_bitmap(bitmap)
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
