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

use crate::avm1::Object;
use crate::events::ClipEvent;
use crate::string::{AvmString, StringContext, WStr};
use ruffle_macros::istr;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

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
}

impl Handler {
    pub const COUNT: usize = 15;

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
        // Every cached name starts with "on"; skip the comparisons for everything else.
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
