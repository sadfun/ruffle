//! Cached presence of `onXxx` event handler methods on AVM1 movie clips.
//!
//! Dispatching a clip event (`onEnterFrame` every frame, `onMouseMove` on every
//! mouse move, …) used to queue a method call on every MovieClip of the display
//! list and resolve the name through each prototype chain afterwards: thousands
//! of lookups per frame and per mouse move in a crowded room, nearly all of them
//! misses. Instead every clip remembers whether it has the handler, tagged with a
//! global version for that name. The version is bumped whenever any object gains
//! or loses a property with that name, or has its `__proto__` reassigned — the
//! only ways `has_property` can change for an existing clip — so a cached answer
//! is exactly what `has_property` would return.

use crate::avm1::{Object, Value};
use crate::display_object::{DisplayObject, TDisplayObject};
use crate::events::ClipEvent;
use crate::string::{AvmString, StringContext, WStr};
use ruffle_macros::istr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

/// The handler methods whose presence is cached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Handler {
    EnterFrame,
    MouseMove,
    MouseDown,
    MouseUp,
    KeyDown,
    KeyUp,
    Press,
    Release,
    ReleaseOutside,
    RollOver,
    RollOut,
    DragOver,
    DragOut,
    Load,
    Unload,
    HitArea,
}

impl Handler {
    pub const COUNT: usize = 16;

    const ALL: [Handler; Self::COUNT] = [
        Self::EnterFrame,
        Self::MouseMove,
        Self::MouseDown,
        Self::MouseUp,
        Self::KeyDown,
        Self::KeyUp,
        Self::Press,
        Self::Release,
        Self::ReleaseOutside,
        Self::RollOver,
        Self::RollOut,
        Self::DragOver,
        Self::DragOut,
        Self::Load,
        Self::Unload,
        Self::HitArea,
    ];

    /// The handlers that put a MovieClip into button mode
    /// (the same set as `ClipEvent::BUTTON_EVENT_METHODS`).
    pub const BUTTON: [Handler; 7] = [
        Self::DragOver,
        Self::DragOut,
        Self::Press,
        Self::Release,
        Self::ReleaseOutside,
        Self::RollOut,
        Self::RollOver,
    ];

    const NAMES: [&'static [u8]; Self::COUNT] = [
        b"onEnterFrame",
        b"onMouseMove",
        b"onMouseDown",
        b"onMouseUp",
        b"onKeyDown",
        b"onKeyUp",
        b"onPress",
        b"onRelease",
        b"onReleaseOutside",
        b"onRollOver",
        b"onRollOut",
        b"onDragOver",
        b"onDragOut",
        b"onLoad",
        b"onUnload",
        b"hitArea",
    ];

    /// The handler whose method `event` dispatches to, if it is one of the cached ones.
    pub fn for_event(event: ClipEvent) -> Option<Handler> {
        Some(match event {
            ClipEvent::EnterFrame => Self::EnterFrame,
            ClipEvent::MouseMove => Self::MouseMove,
            ClipEvent::MouseDown => Self::MouseDown,
            ClipEvent::MouseUp => Self::MouseUp,
            ClipEvent::KeyDown => Self::KeyDown,
            ClipEvent::KeyUp => Self::KeyUp,
            ClipEvent::Press { .. } => Self::Press,
            ClipEvent::Release { .. } => Self::Release,
            ClipEvent::ReleaseOutside => Self::ReleaseOutside,
            ClipEvent::RollOver { .. } => Self::RollOver,
            ClipEvent::RollOut { .. } => Self::RollOut,
            ClipEvent::DragOver { .. } => Self::DragOver,
            ClipEvent::DragOut { .. } => Self::DragOut,
            ClipEvent::Load => Self::Load,
            ClipEvent::Unload => Self::Unload,
            _ => return None,
        })
    }

    /// The handler with method name `name` (case-insensitively, like SWF 6).
    pub fn from_name(name: &WStr) -> Option<Handler> {
        if name.eq_ignore_case(WStr::from_units(b"hitArea")) {
            return Some(Self::HitArea);
        }
        // Every other cached name starts with "on"; skip the comparisons for everything else.
        if name.len() < 3
            || name.at(0) | 0x20 != u16::from(b'o')
            || name.at(1) | 0x20 != u16::from(b'n')
        {
            return None;
        }
        Self::ALL
            .into_iter()
            .find(|handler| name.eq_ignore_case(WStr::from_units(Self::NAMES[*handler as usize])))
    }

    /// The interned method name, e.g. `onEnterFrame`.
    pub fn name<'gc>(self, ctx: &StringContext<'gc>) -> AvmString<'gc> {
        match self {
            Self::EnterFrame => istr!(ctx, "onEnterFrame"),
            Self::MouseMove => istr!(ctx, "onMouseMove"),
            Self::MouseDown => istr!(ctx, "onMouseDown"),
            Self::MouseUp => istr!(ctx, "onMouseUp"),
            Self::KeyDown => istr!(ctx, "onKeyDown"),
            Self::KeyUp => istr!(ctx, "onKeyUp"),
            Self::Press => istr!(ctx, "onPress"),
            Self::Release => istr!(ctx, "onRelease"),
            Self::ReleaseOutside => istr!(ctx, "onReleaseOutside"),
            Self::RollOver => istr!(ctx, "onRollOver"),
            Self::RollOut => istr!(ctx, "onRollOut"),
            Self::DragOver => istr!(ctx, "onDragOver"),
            Self::DragOut => istr!(ctx, "onDragOut"),
            Self::Load => istr!(ctx, "onLoad"),
            Self::Unload => istr!(ctx, "onUnload"),
            Self::HitArea => istr!(ctx, "hitArea"),
        }
    }
}

/// Per-name versions; a clip's cached answer for a name is valid while the
/// version it was computed at is still current.
static VERSIONS: [AtomicU32; Handler::COUNT] = [const { AtomicU32::new(1) }; Handler::COUNT];

/// Current version of `handler`'s name.
pub fn version(handler: Handler) -> u32 {
    VERSIONS[handler as usize].load(Relaxed)
}

/// Must be called when a property named `name` is added to or removed from
/// `object`. An object nobody inherits from can only change its own clip's
/// answer; a prototype may sit in any chain, so its change ages every clip's
/// answer for that name.
pub fn property_changed<'gc>(object: Object<'gc>, name: &WStr) {
    let Some(handler) = Handler::from_name(name) else {
        return;
    };
    if object.is_prototype() {
        VERSIONS[handler as usize].fetch_add(1, Relaxed);
    } else if let Some(clip) = object.as_display_object().and_then(|o| o.as_movie_clip()) {
        clip.forget_handler(handler);
    }
}

/// Must be called when `object`'s `__proto__` is assigned: its chain, and the
/// chain of everything inheriting from it, may have changed.
pub fn proto_changed<'gc>(object: Object<'gc>) {
    if object.is_prototype() {
        for version in &VERSIONS {
            version.fetch_add(1, Relaxed);
        }
    } else if let Some(clip) = object.as_display_object().and_then(|o| o.as_movie_clip()) {
        clip.forget_handlers();
    }
}

/// Must be called when a display object is named: a child named like a handler
/// method counts as that property of its parent in AVM1.
pub fn child_named(name: &WStr) {
    if let Some(handler) = Handler::from_name(name) {
        VERSIONS[handler as usize].fetch_add(1, Relaxed);
    }
}

/// Whether a prototype (or another object that is not a clip) defines
/// `hitArea`: every clip inheriting it may have a hit area anywhere, which no
/// per-clip count can express, so picking then walks everything like upstream.
// ponytail: process-wide and sticky; make it per-player if one process hosts several movies.
static PROTO_HIT_AREA: AtomicBool = AtomicBool::new(false);

pub fn proto_hit_area() -> bool {
    PROTO_HIT_AREA.load(Relaxed)
}

/// Whether `name` is `hitArea`.
pub fn is_hit_area(name: &WStr) -> bool {
    Handler::from_name(name) == Some(Handler::HitArea)
}

/// Must be called when `value` is stored as `object`'s `hitArea`. `resolve`
/// turns a `Value::MovieClip` path into its object; `None` (unresolvable now,
/// or no activation to try) counts as "anywhere". A hit area inside the clip's
/// own subtree is covered by its pick bounds; anything else marks the clip
/// (`TDisplayObject::set_hit_area_unbounded`).
pub fn hit_area_assigned<'gc>(
    object: Object<'gc>,
    value: Value<'gc>,
    resolve: impl FnOnce(Value<'gc>) -> Option<Object<'gc>>,
) {
    let Some(clip) = object.as_display_object() else {
        if !matches!(value, Value::Undefined | Value::Null) {
            PROTO_HIT_AREA.store(true, Relaxed);
        }
        return;
    };
    let unbounded = match value {
        Value::Object(area) => outside(area, clip),
        Value::MovieClip(_) => resolve(value).is_none_or(|area| outside(area, clip)),
        _ => false,
    };
    clip.set_hit_area_unbounded(unbounded);
}

/// Must be called when `hitArea` becomes a getter on `object`: what it will
/// return is unknown.
pub fn hit_area_virtual<'gc>(object: Object<'gc>) {
    match object.as_display_object() {
        Some(clip) => clip.set_hit_area_unbounded(true),
        None => PROTO_HIT_AREA.store(true, Relaxed),
    }
}

/// Must be called when `hitArea` is deleted from `object`.
pub fn hit_area_deleted<'gc>(object: Object<'gc>) {
    if let Some(clip) = object.as_display_object() {
        clip.set_hit_area_unbounded(false);
    }
}

/// Must be called with what `clip`'s `hitArea` resolved to at pick time: a
/// path reference may resolve elsewhere than it did when assigned. Only ever
/// marks the clip — a getter may answer differently next time.
pub fn hit_area_resolved<'gc>(clip: DisplayObject<'gc>, area: DisplayObject<'gc>) {
    if !clip.hit_area_unbounded() && outside_subtree(area, clip) {
        clip.set_hit_area_unbounded(true);
    }
}

fn outside<'gc>(area: Object<'gc>, clip: DisplayObject<'gc>) -> bool {
    area.as_display_object()
        .is_some_and(|area| outside_subtree(area, clip))
}

fn outside_subtree<'gc>(area: DisplayObject<'gc>, clip: DisplayObject<'gc>) -> bool {
    let mut node = area;
    loop {
        if DisplayObject::ptr_eq(node, clip) {
            return false;
        }
        match node.parent() {
            Some(parent) => node = parent,
            None => return true,
        }
    }
}

/// Whether `name` is `__proto__`.
pub fn is_proto(name: &WStr) -> bool {
    name.eq_ignore_case(WStr::from_units(b"__proto__"))
}

/// A movie clip's remembered answers.
#[derive(Clone, Copy, Debug, Default)]
pub struct HandlerCache {
    versions: [u32; Handler::COUNT],
    present: u16,
}

impl HandlerCache {
    /// The cached answer for `handler`, if still valid.
    pub fn get(&self, handler: Handler) -> Option<bool> {
        let i = handler as usize;
        (self.versions[i] == VERSIONS[i].load(Relaxed)).then(|| self.present & (1 << i) != 0)
    }

    /// Remembers `present` for `handler`, computed while `version` was current.
    pub fn set(&mut self, handler: Handler, present: bool, version: u32) {
        let i = handler as usize;
        self.versions[i] = version;
        if present {
            self.present |= 1 << i;
        } else {
            self.present &= !(1 << i);
        }
    }

    /// Drops the answer for `handler`.
    pub fn forget(&mut self, handler: Handler) {
        self.versions[handler as usize] = 0;
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::navigator::{NullExecutor, NullNavigatorBackend};
    use crate::display_object::{TDisplayObject, TDisplayObjectContainer, TInteractiveObject};
    use crate::events::PlayerEvent;
    use crate::player::run_mouse_pick;
    use crate::tag_utils::movie_from_path;
    use crate::{Player, PlayerBuilder};
    use ruffle_wstr::WStr;
    use std::sync::{Arc, Mutex};
    use swf::avm1::types::{Action, DefineFunction, Push, Value};
    use swf::{
        Color, Compression, FillStyle, Fixed8, Header, Matrix, PlaceObject, PlaceObjectAction,
        Point, PointDelta, Rectangle, Shape, ShapeFlag, ShapeRecord, ShapeStyles, Sprite,
        StyleChangeData, SwfStr, Tag, Twips,
    };

    fn px(pixels: i32) -> Twips {
        Twips::from_pixels_i32(pixels)
    }

    fn actions(actions: &[Action]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = swf::avm1::write::Writer::new(&mut out, 8);
            for action in actions {
                writer.write_action(action).unwrap();
            }
            writer.write_action(&Action::End).unwrap();
        }
        out
    }

    fn string(s: &str) -> Action<'_> {
        Action::Push(Push {
            values: vec![Value::Str(SwfStr::from_utf8_str(s))],
        })
    }

    /// A 100×100 px square.
    fn square(id: u16) -> Tag<'static> {
        let bounds = Rectangle {
            x_min: Twips::ZERO,
            x_max: px(100),
            y_min: Twips::ZERO,
            y_max: px(100),
        };
        let edge = |dx, dy| ShapeRecord::StraightEdge {
            delta: PointDelta::new(dx, dy),
        };
        Tag::DefineShape(Box::new(Shape {
            version: 1,
            id,
            shape_bounds: bounds,
            edge_bounds: bounds,
            flags: ShapeFlag::empty(),
            styles: ShapeStyles {
                fill_styles: vec![FillStyle::Color(Color::from_rgb(0xff0000, 255))],
                line_styles: vec![],
            },
            shape: vec![
                ShapeRecord::StyleChange(Box::new(StyleChangeData {
                    move_to: Some(Point::new(Twips::ZERO, Twips::ZERO)),
                    fill_style_0: None,
                    fill_style_1: Some(1),
                    line_style: None,
                    new_styles: None,
                })),
                edge(px(100), Twips::ZERO),
                edge(Twips::ZERO, px(100)),
                edge(px(-100), Twips::ZERO),
                edge(Twips::ZERO, px(-100)),
            ],
        }))
    }

    fn place(depth: u16, id: u16, name: Option<&'static str>, x: i32) -> Tag<'static> {
        Tag::PlaceObject(Box::new(PlaceObject {
            version: 2,
            action: PlaceObjectAction::Place(id),
            depth,
            matrix: Some(Matrix::translate(px(x), Twips::ZERO)),
            color_transform: None,
            ratio: None,
            name: name.map(SwfStr::from_utf8_str),
            clip_depth: None,
            class_name: None,
            filters: None,
            background_color: None,
            blend_mode: None,
            clip_actions: None,
            has_image: false,
            is_bitmap_cached: None,
            is_visible: None,
            amf_data: None,
        }))
    }

    fn sprite(id: u16, tags: Vec<Tag<'static>>) -> Tag<'static> {
        let mut tags = tags;
        tags.push(Tag::ShowFrame);
        Tag::DefineSprite(Sprite {
            id,
            num_frames: 1,
            tags,
        })
    }

    fn pick(player: &Arc<Mutex<Player>>, x: f64, y: f64) -> Option<String> {
        let mut player = player.lock().unwrap();
        player.handle_event(PlayerEvent::MouseMove { x, y });
        player.mutate_with_update_context(|context| {
            run_mouse_pick(context, true).map(|hit| hit.as_displayobject().path().to_string())
        })
    }

    fn unbounded_hit_areas(player: &Arc<Mutex<Player>>, name: &str) -> u32 {
        player
            .lock()
            .unwrap()
            .mutate_with_update_context(|context| {
                let root = context.stage.root_clip().unwrap();
                let child = root
                    .as_container()
                    .unwrap()
                    .child_by_name(WStr::from_units(name.as_bytes()), false)
                    .unwrap();
                child.unbounded_hit_areas()
            })
    }

    /// `box.btn` sits in a container whose pick bounds end at x = 100, while its
    /// `hitArea` is the sibling `zone` at x = 500: the container must not be
    /// pruned on the way to `btn` while the hit area is external, and must be
    /// again once it is cleared.
    #[test]
    fn external_hit_area_is_reached_through_a_pruned_container() {
        let frame1 = actions(&[
            string("box.btn"),
            Action::GetVariable,
            string("onRelease"),
            Action::DefineFunction(DefineFunction {
                name: SwfStr::from_utf8_str(""),
                params: vec![],
                actions: &[],
            }),
            Action::SetMember,
            string("box.btn"),
            Action::GetVariable,
            string("hitArea"),
            string("_root.zone"),
            Action::GetVariable,
            Action::SetMember,
        ]);
        let frame2 = actions(&[
            string("box.btn"),
            Action::GetVariable,
            string("hitArea"),
            Action::Push(Push {
                values: vec![Value::Null],
            }),
            Action::SetMember,
        ]);
        let tags = vec![
            square(1),
            sprite(2, vec![place(1, 1, None, 0)]), // zone
            sprite(3, vec![place(1, 1, None, 0)]), // btn
            sprite(4, vec![place(1, 3, Some("btn"), 0)]), // box
            place(1, 4, Some("box"), 0),
            place(2, 2, Some("zone"), 500),
            Tag::DoAction(&frame1),
            Tag::ShowFrame,
            Tag::DoAction(&frame2),
            Tag::ShowFrame,
        ];
        let header = Header {
            compression: Compression::None,
            version: 8,
            stage_size: Rectangle {
                x_min: Twips::ZERO,
                x_max: px(800),
                y_min: Twips::ZERO,
                y_max: px(600),
            },
            frame_rate: Fixed8::from_f32(24.0),
            num_frames: 2,
        };
        let mut data = Vec::new();
        swf::write_swf(&header, &tags, &mut data).unwrap();
        let dir = std::env::temp_dir().join(format!("ruffle-hitarea-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.swf"), data).unwrap();

        let executor = NullExecutor::new();
        let navigator = NullNavigatorBackend::with_base_path(&dir, &executor).unwrap();
        let movie = movie_from_path(dir.join("test.swf"), None).unwrap();
        let player = PlayerBuilder::new()
            .with_navigator(navigator)
            .with_movie(movie)
            .with_viewport_dimensions(800, 600, 1.0)
            .build();

        // Frame 1: `box.btn.hitArea = _root.zone`.
        player.lock().unwrap().run_frame();
        assert_eq!(unbounded_hit_areas(&player, "box"), 1);
        assert_eq!(
            pick(&player, 550.0, 50.0).as_deref(),
            Some("_level0.box.btn")
        );
        // The hit area replaces the clip's own shape.
        assert_eq!(pick(&player, 50.0, 50.0), None);

        // Frame 2: `box.btn.hitArea = null`.
        player.lock().unwrap().run_frame();
        assert_eq!(unbounded_hit_areas(&player, "box"), 0);
        assert_eq!(pick(&player, 550.0, 50.0), None);
        assert_eq!(
            pick(&player, 50.0, 50.0).as_deref(),
            Some("_level0.box.btn")
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
