//! Shararam profiler: a feature-gated, low-overhead event recorder.
//!
//! The Shararam Ruffle profiling build records what the player is doing over
//! time (frame phases, rendering work, SWF loading and parsing, RTMP RPC
//! traffic) so that the events can be lined up against the FPS graph in the
//! external viewer.
//!
//! With the `shararam_profiler` cargo feature disabled, every item in this
//! module is an empty `#[inline(always)]` no-op and argument closures are
//! never evaluated, so the call sites compile to nothing. With the feature
//! enabled, events are appended to a thread-local buffer and drained by the
//! embedder (the web frontend) as one JSON string.
//!
//! Design rules, so that a profiling build stays representative:
//! * a span costs two clock reads and one `Vec` push — no allocation unless
//!   the call site attaches arguments;
//! * nothing here is called per display object except the counters, which
//!   are a single `Cell` increment;
//! * argument strings are built with `format!` at the call site, only when
//!   the feature is on.

pub use imp::*;

#[cfg(feature = "shararam_profiler")]
mod render;
#[cfg(feature = "shararam_profiler")]
pub use render::ProfiledRenderer;

/// Per-frame counters aggregated into the `render` event of each frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Counter {
    /// Display objects whose `render` was invoked this frame.
    DisplayObjectsRendered = 0,
    /// Shapes tessellated/uploaded by the renderer (first use of a shape).
    ShapesRegistered,
    /// Bitmaps decoded and uploaded as textures.
    BitmapsRegistered,
    /// Existing textures updated in place (BitmapData writes).
    TexturesUpdated,
    /// Offscreen renders (cacheAsBitmap redraws, BitmapData.draw, filters).
    OffscreenRenders,
    /// Text fields laid out again.
    TextLayouts,
    /// Display objects instantiated from the timeline or by script.
    ObjectsInstantiated,
    /// Bitmaps decoded from their compressed SWF representation.
    BitmapsDecoded,
    /// Layer blend groups rendered inline instead of through an offscreen
    /// target (see `render_base`).
    LayerBlendsInlined,
}

impl Counter {
    pub const COUNT: usize = 9;

    pub const NAMES: [&'static str; Self::COUNT] = [
        "display_objects",
        "shapes_registered",
        "bitmaps_registered",
        "textures_updated",
        "offscreen_renders",
        "text_layouts",
        "objects_instantiated",
        "bitmaps_decoded",
        "layer_blends_inlined",
    ];
}

#[cfg(feature = "shararam_profiler")]
mod imp {
    use super::Counter;
    use std::cell::{Cell, RefCell};
    use std::fmt::Write as _;

    /// Upper bound on buffered events when nobody drains them (e.g. a build
    /// without the frontend poller), so memory stays bounded.
    const MAX_BUFFERED_EVENTS: usize = 250_000;

    /// Upper bound on the JSON rendering of one AMF payload.
    const MAX_AMF_JSON_BYTES: usize = 48 * 1024;
    const MAX_AMF_DEPTH: usize = 24;

    pub struct Event {
        ts: f64,
        dur: f64,
        cat: &'static str,
        name: &'static str,
        args: Option<String>,
    }

    #[derive(Default)]
    struct State {
        events: Vec<Event>,
        dropped: u64,
        sequence: u64,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
        static MOVIE: RefCell<Option<std::sync::Arc<crate::tag_utils::SwfMovie>>> = const { RefCell::new(None) };
        static CLOCK: Cell<fn() -> f64> = const { Cell::new(default_clock) };
        static COUNTERS: [Cell<u32>; Counter::COUNT] = const { [const { Cell::new(0) }; Counter::COUNT] };
        static START: web_time::Instant = web_time::Instant::now();
    }

    fn default_clock() -> f64 {
        START.with(|start| start.elapsed().as_secs_f64() * 1000.0)
    }

    /// Installs the clock used for timestamps. The web frontend installs
    /// `performance.now()` so that Ruffle events share the time base of the
    /// browser's own performance timeline.
    pub fn set_clock(clock: fn() -> f64) {
        CLOCK.with(|cell| cell.set(clock));
    }

    /// Current time in milliseconds on the profiler clock.
    #[inline]
    pub fn now() -> f64 {
        CLOCK.with(|cell| cell.get())()
    }

    fn push(event: Event) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.events.len() >= MAX_BUFFERED_EVENTS {
                state.dropped += 1;
                return;
            }
            state.events.push(event);
        });
    }

    /// A timed region. The event is recorded when the span is dropped.
    #[must_use = "a span records its duration when dropped; bind it to a variable"]
    pub struct Span {
        start: f64,
        cat: &'static str,
        name: &'static str,
        args: Option<String>,
        min_duration: f64,
    }

    impl Span {
        /// Attaches a JSON object (as text) to the event.
        #[inline]
        pub fn args(mut self, args: impl FnOnce() -> String) -> Self {
            self.args = Some(args());
            self
        }

        /// Replaces the attached JSON object, e.g. with values only known at
        /// the end of the region.
        #[inline]
        pub fn set_args(&mut self, args: impl FnOnce() -> String) {
            self.args = Some(args());
        }

        /// Drops the event if the region was shorter than `ms`. Used for
        /// high-frequency regions that are only interesting when slow.
        #[inline]
        pub fn min_duration_ms(mut self, ms: f64) -> Self {
            self.min_duration = ms;
            self
        }

        #[inline]
        pub fn elapsed_ms(&self) -> f64 {
            now() - self.start
        }
    }

    impl Drop for Span {
        fn drop(&mut self) {
            let end = now();
            let dur = end - self.start;
            if dur < self.min_duration {
                return;
            }
            push(Event {
                ts: self.start,
                dur,
                cat: self.cat,
                name: self.name,
                args: self.args.take(),
            });
        }
    }

    #[inline]
    pub fn span(cat: &'static str, name: &'static str) -> Span {
        Span {
            start: now(),
            cat,
            name,
            args: None,
            min_duration: 0.0,
        }
    }

    /// Records an instantaneous event with a JSON object (as text).
    #[inline]
    pub fn instant(cat: &'static str, name: &'static str, args: impl FnOnce() -> String) {
        push(Event {
            ts: now(),
            dur: 0.0,
            cat,
            name,
            args: Some(args()),
        });
    }

    /// Records a region that started at `start_ms` (a value obtained from
    /// [`now`]) and ends now. Useful across `await` points and error paths.
    #[inline]
    pub fn complete(
        cat: &'static str,
        name: &'static str,
        start_ms: f64,
        args: impl FnOnce() -> String,
    ) {
        push(Event {
            ts: start_ms,
            dur: now() - start_ms,
            cat,
            name,
            args: Some(args()),
        });
    }

    /// Records an instantaneous event without arguments.
    #[inline]
    pub fn mark(cat: &'static str, name: &'static str) {
        push(Event {
            ts: now(),
            dur: 0.0,
            cat,
            name,
            args: None,
        });
    }

    /// Marks the SWF whose tags are being processed until the guard drops, so
    /// that renderer events raised from inside (tessellation, texture uploads)
    /// can be attributed to a movie. Character ids are only unique per movie.
    #[must_use]
    pub struct MovieScope(Option<std::sync::Arc<crate::tag_utils::SwfMovie>>);

    impl Drop for MovieScope {
        fn drop(&mut self) {
            MOVIE.with(|movie| *movie.borrow_mut() = self.0.take());
        }
    }

    pub fn movie_scope(movie: std::sync::Arc<crate::tag_utils::SwfMovie>) -> MovieScope {
        MovieScope(MOVIE.with(|current| current.replace(Some(movie))))
    }

    /// The URL of the movie in scope as a JSON string literal (or `null`).
    pub fn movie_json() -> String {
        MOVIE.with(|movie| match movie.borrow().as_ref() {
            Some(movie) => json_str(movie.url()),
            None => "null".to_string(),
        })
    }

    #[inline]
    pub fn inc(counter: Counter) {
        add(counter, 1);
    }

    #[inline]
    pub fn add(counter: Counter, amount: u32) {
        COUNTERS.with(|counters| {
            let cell = &counters[counter as usize];
            cell.set(cell.get().wrapping_add(amount));
        });
    }

    /// Returns and resets the per-frame counters.
    pub fn take_counters() -> [u32; Counter::COUNT] {
        COUNTERS.with(|counters| {
            let mut out = [0; Counter::COUNT];
            for (slot, cell) in out.iter_mut().zip(counters) {
                *slot = cell.replace(0);
            }
            out
        })
    }

    /// Renders the counters as JSON object members (no surrounding braces),
    /// so a call site can splice them into its own argument object.
    pub fn counters_json(counters: &[u32; Counter::COUNT]) -> String {
        let mut out = String::with_capacity(160);
        for (index, name) in Counter::NAMES.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "\"{name}\":{}", counters[index]);
        }
        out
    }

    /// Removes every buffered event and returns them as a JSON array:
    /// `[{"t":<ms>,"d":<ms>,"c":"<category>","n":"<name>","a":{...}}, ...]`.
    /// `t` is on the profiler clock (see [`set_clock`]).
    pub fn drain_json() -> String {
        let (events, dropped, first_sequence) = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let events = std::mem::take(&mut state.events);
            let dropped = std::mem::take(&mut state.dropped);
            let first = state.sequence;
            state.sequence += events.len() as u64;
            (events, dropped, first)
        });
        let mut out = String::with_capacity(events.len() * 96 + 64);
        out.push('[');
        for (index, event) in events.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"s\":{},\"t\":{:.3},\"d\":{:.3},\"c\":\"{}\",\"n\":\"{}\"",
                first_sequence + index as u64,
                event.ts,
                event.dur,
                event.cat,
                event.name
            );
            if let Some(args) = &event.args {
                out.push_str(",\"a\":");
                out.push_str(args);
            }
            out.push('}');
        }
        if dropped > 0 {
            if !events.is_empty() {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"s\":{},\"t\":{:.3},\"d\":0,\"c\":\"profiler\",\"n\":\"events_dropped\",\"a\":{{\"count\":{dropped}}}}}",
                first_sequence + events.len() as u64,
                now()
            );
        }
        out.push(']');
        out
    }

    /// Number of events waiting to be drained.
    pub fn pending() -> usize {
        STATE.with(|state| state.borrow().events.len())
    }

    /// Quotes and escapes a string as a JSON string literal.
    pub fn json_str(value: &str) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "\"?\"".to_string())
    }

    /// Quotes a string that may contain binary garbage, truncated to `max`
    /// characters.
    pub fn json_str_truncated(value: &str, max: usize) -> String {
        if value.chars().count() <= max {
            json_str(value)
        } else {
            let cut: String = value.chars().take(max).collect();
            json_str(&format!("{cut}…"))
        }
    }

    /// Converts an AMF value into readable JSON text. References are left as
    /// `{"$ref":n}` (resolving them is quadratic in flash-lso). Output is cut
    /// off beyond [`MAX_AMF_JSON_BYTES`] (plus one scalar) so that huge
    /// payloads cannot slow the player down; the result stays valid JSON and
    /// is wrapped as `{"$truncated":true,"value":…}` when that happens.
    pub fn amf_to_json(value: &flash_lso::types::Value) -> String {
        let mut out = String::with_capacity(256);
        if write_amf(&mut out, value, 0, MAX_AMF_JSON_BYTES) {
            out
        } else {
            format!("{{\"$truncated\":true,\"value\":{out}}}")
        }
    }

    /// Converts a list of AMF values into a JSON array (see [`amf_to_json`]).
    pub fn amf_list_to_json(values: &[std::rc::Rc<flash_lso::types::Value>]) -> String {
        let mut out = String::with_capacity(256);
        out.push('[');
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            if !write_amf(&mut out, value, 0, MAX_AMF_JSON_BYTES) {
                out.push_str(",\"…truncated\"");
                break;
            }
        }
        out.push(']');
        out
    }

    /// Writes `value` as JSON. Returns `false` once `out` has grown past
    /// `limit`; the value written so far is always structurally complete.
    fn write_amf(
        out: &mut String,
        value: &flash_lso::types::Value,
        depth: usize,
        limit: usize,
    ) -> bool {
        use flash_lso::types::Value;
        if depth > MAX_AMF_DEPTH {
            out.push_str("\"…depth\"");
            return true;
        }
        let ok = match value {
            Value::Number(n) => {
                if n.is_finite() {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{n}");
                    }
                } else {
                    out.push_str("null");
                }
                true
            }
            Value::Integer(n) => {
                let _ = write!(out, "{n}");
                true
            }
            Value::Bool(b) => {
                out.push_str(if *b { "true" } else { "false" });
                true
            }
            Value::String(s) => {
                out.push_str(&json_str_truncated(s, 4096));
                true
            }
            Value::XML(s, _) => {
                out.push_str(&json_str_truncated(s, 4096));
                true
            }
            Value::Null => {
                out.push_str("null");
                true
            }
            Value::Undefined => {
                out.push_str("\"undefined\"");
                true
            }
            Value::Unsupported => {
                out.push_str("\"<unsupported>\"");
                true
            }
            Value::Date(time, _) => {
                let _ = write!(out, "{{\"$date\":{time}}}");
                true
            }
            Value::Reference(reference) => {
                // `Reference` exposes no accessor; its Debug form is `Reference(n)`.
                let _ = write!(out, "{{\"$ref\":{}}}", json_str(&format!("{reference:?}")));
                true
            }
            Value::Amf3ObjectReference(id) => {
                let _ = write!(out, "{{\"$ref\":{}}}", id.0);
                true
            }
            Value::ByteArray(bytes) => {
                let _ = write!(out, "{{\"$bytes\":{}}}", bytes.len());
                true
            }
            Value::StrictArray(_, values) => write_amf_array(out, values, depth, limit),
            Value::ECMAArray(_, dense, associative, _) => {
                if associative.is_empty() {
                    write_amf_array(out, dense, depth, limit)
                } else {
                    let mut ok = true;
                    out.push('{');
                    for (index, value) in dense.iter().enumerate() {
                        if index > 0 {
                            out.push(',');
                        }
                        let _ = write!(out, "\"{index}\":");
                        ok = write_amf(out, value, depth + 1, limit);
                        if !ok {
                            break;
                        }
                    }
                    if ok {
                        ok = write_amf_members(out, associative, !dense.is_empty(), depth, limit);
                    }
                    out.push('}');
                    ok
                }
            }
            Value::Object(_, members, class) => {
                out.push('{');
                let mut first = true;
                if let Some(class) = class
                    && !class.name.is_empty()
                {
                    let _ = write!(out, "\"$class\":{}", json_str(&class.name));
                    first = false;
                }
                let ok = write_amf_members(out, members, !first, depth, limit);
                out.push('}');
                ok
            }
            Value::Custom(custom, members, class) => {
                out.push('{');
                let mut first = true;
                if let Some(class) = class {
                    let _ = write!(out, "\"$class\":{}", json_str(&class.name));
                    first = false;
                }
                let mut ok = write_amf_members(out, custom, !first, depth, limit);
                if ok {
                    ok =
                        write_amf_members(out, members, !first || !custom.is_empty(), depth, limit);
                }
                out.push('}');
                ok
            }
            Value::AMF3(inner) => write_amf(out, inner, depth + 1, limit),
            Value::VectorInt(values, _) => {
                let _ = write!(out, "{values:?}");
                true
            }
            Value::VectorUInt(values, _) => {
                let _ = write!(out, "{values:?}");
                true
            }
            Value::VectorDouble(values, _) => {
                let _ = write!(out, "{values:?}");
                true
            }
            Value::VectorObject(_, values, _, _) => write_amf_array(out, values, depth, limit),
            Value::Dictionary(_, entries, _) => {
                out.push('[');
                let mut ok = true;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push('[');
                    ok = write_amf(out, key, depth + 1, limit);
                    if ok {
                        out.push(',');
                        ok = write_amf(out, value, depth + 1, limit);
                    }
                    out.push(']');
                    if !ok {
                        break;
                    }
                }
                out.push(']');
                ok
            }
        };
        ok && out.len() <= limit
    }

    fn write_amf_array(
        out: &mut String,
        values: &[std::rc::Rc<flash_lso::types::Value>],
        depth: usize,
        limit: usize,
    ) -> bool {
        out.push('[');
        let mut ok = true;
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            ok = write_amf(out, value, depth + 1, limit);
            if !ok {
                break;
            }
        }
        out.push(']');
        ok
    }

    fn write_amf_members(
        out: &mut String,
        members: &[flash_lso::types::Element],
        mut needs_comma: bool,
        depth: usize,
        limit: usize,
    ) -> bool {
        for member in members {
            if needs_comma {
                out.push(',');
            }
            needs_comma = true;
            out.push_str(&json_str(member.name()));
            out.push(':');
            if !write_amf(out, member.value(), depth + 1, limit) {
                return false;
            }
        }
        true
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use flash_lso::types::{Element, ObjectId, Value};
        use std::rc::Rc;

        #[test]
        fn spans_and_instants_drain_as_json() {
            let _ = drain_json();
            {
                let _span = span("frame", "tick").args(|| "{\"dt\":16}".into());
            }
            instant("load", "movie", || "{\"url\":\"a.swf\"}".into());
            mark("frame", "end");
            let json = drain_json();
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            let events = parsed.as_array().unwrap();
            assert_eq!(events.len(), 3);
            assert_eq!(events[0]["c"], "frame");
            assert_eq!(events[0]["n"], "tick");
            assert_eq!(events[0]["a"]["dt"], 16);
            assert_eq!(events[1]["a"]["url"], "a.swf");
            assert!(events[2].get("a").is_none());
            assert!(drain_json().starts_with('['));
        }

        #[test]
        fn short_spans_below_threshold_are_dropped() {
            let _ = drain_json();
            {
                let _span = span("gc", "collect").min_duration_ms(1_000_000.0);
            }
            assert_eq!(drain_json(), "[]");
        }

        #[test]
        fn counters_reset_when_taken() {
            take_counters();
            inc(Counter::ShapesRegistered);
            add(Counter::ShapesRegistered, 2);
            let counters = take_counters();
            assert_eq!(counters[Counter::ShapesRegistered as usize], 3);
            assert_eq!(take_counters()[Counter::ShapesRegistered as usize], 0);
            assert!(counters_json(&counters).contains("\"shapes_registered\":3"));
        }

        #[test]
        fn amf_values_render_as_readable_json() {
            let value = Value::Object(
                ObjectId::INVALID,
                vec![
                    Element::new("name", Rc::new(Value::String("Ёжик \"1\"".into()))),
                    Element::new("x", Rc::new(Value::Number(12.0))),
                    Element::new("y", Rc::new(Value::Number(1.5))),
                    Element::new("ok", Rc::new(Value::Bool(true))),
                    Element::new("nothing", Rc::new(Value::Null)),
                    Element::new(
                        "list",
                        Rc::new(Value::StrictArray(
                            ObjectId::INVALID,
                            vec![Rc::new(Value::Number(1.0)), Rc::new(Value::Undefined)],
                        )),
                    ),
                ],
                None,
            );
            let json = amf_to_json(&value);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed["name"], "Ёжик \"1\"");
            assert_eq!(parsed["x"], 12);
            assert_eq!(parsed["y"], 1.5);
            assert_eq!(parsed["ok"], true);
            assert!(parsed["nothing"].is_null());
            assert_eq!(parsed["list"][1], "undefined");
        }

        #[test]
        fn huge_amf_payloads_are_truncated() {
            let values: Vec<_> = (0..100_000)
                .map(|index| Rc::new(Value::Number(index as f64)))
                .collect();
            let json = amf_list_to_json(&values);
            assert!(json.len() < MAX_AMF_JSON_BYTES + 64, "{}", json.len());
            assert!(json.ends_with(",\"…truncated\"]"));
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(parsed.as_array().unwrap().len() > 1000);

            let nested = Value::Object(
                ObjectId::INVALID,
                vec![Element::new(
                    "items",
                    Rc::new(Value::StrictArray(ObjectId::INVALID, values)),
                )],
                None,
            );
            let json = amf_to_json(&nested);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed["$truncated"], true);
            assert!(parsed["value"]["items"].as_array().unwrap().len() > 1000);
        }
    }
}

#[cfg(not(feature = "shararam_profiler"))]
mod imp {
    use super::Counter;

    #[must_use = "a span records its duration when dropped; bind it to a variable"]
    pub struct Span;

    impl Span {
        #[inline(always)]
        pub fn args(self, _args: impl FnOnce() -> String) -> Self {
            self
        }
        #[inline(always)]
        pub fn set_args(&mut self, _args: impl FnOnce() -> String) {}
        #[inline(always)]
        pub fn min_duration_ms(self, _ms: f64) -> Self {
            self
        }
        #[inline(always)]
        pub fn elapsed_ms(&self) -> f64 {
            0.0
        }
    }

    #[inline(always)]
    pub fn set_clock(_clock: fn() -> f64) {}
    #[inline(always)]
    pub fn now() -> f64 {
        0.0
    }
    #[inline(always)]
    pub fn span(_cat: &'static str, _name: &'static str) -> Span {
        Span
    }
    #[inline(always)]
    pub fn instant(_cat: &'static str, _name: &'static str, _args: impl FnOnce() -> String) {}
    #[inline(always)]
    pub fn complete(
        _cat: &'static str,
        _name: &'static str,
        _start_ms: f64,
        _args: impl FnOnce() -> String,
    ) {
    }
    #[inline(always)]
    pub fn mark(_cat: &'static str, _name: &'static str) {}
    #[must_use]
    pub struct MovieScope;
    #[inline(always)]
    pub fn movie_scope(_movie: std::sync::Arc<crate::tag_utils::SwfMovie>) -> MovieScope {
        MovieScope
    }
    #[inline(always)]
    pub fn movie_json() -> String {
        String::new()
    }
    #[inline(always)]
    pub fn inc(_counter: Counter) {}
    #[inline(always)]
    pub fn add(_counter: Counter, _amount: u32) {}
    #[inline(always)]
    pub fn take_counters() -> [u32; Counter::COUNT] {
        [0; Counter::COUNT]
    }
    #[inline(always)]
    pub fn counters_json(_counters: &[u32; Counter::COUNT]) -> String {
        String::new()
    }
    #[inline(always)]
    pub fn drain_json() -> String {
        "[]".to_string()
    }
    #[inline(always)]
    pub fn pending() -> usize {
        0
    }
    #[inline(always)]
    pub fn json_str(_value: &str) -> String {
        String::new()
    }
    #[inline(always)]
    pub fn json_str_truncated(_value: &str, _max: usize) -> String {
        String::new()
    }
    #[inline(always)]
    pub fn amf_to_json(_value: &flash_lso::types::Value) -> String {
        String::new()
    }
    #[inline(always)]
    pub fn amf_list_to_json(_values: &[std::rc::Rc<flash_lso::types::Value>]) -> String {
        String::new()
    }
}
