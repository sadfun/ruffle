import flash.display.BitmapData;
import flash.geom.Matrix;

// Each of the loaded movies holds a 2x2 shape with a bitmap fill that is not smoothed,
// which every clip scales up to 100x100:
// - filled.swf has it on the main timeline,
// - nested.swf has it inside of a clip called "child",
// - frames.swf has it on the second frame, where it stops.
var loader:MovieClipLoader = new MovieClipLoader();
var listener:Object = new Object();
var queue:Array = [];
var clips:Number = 0;

function makeClip(name:String):MovieClip {
    var clip:MovieClip = createEmptyMovieClip(name, getNextHighestDepth());
    clip._x = 110 * clips++;
    clip._xscale = clip._yscale = 5000;
    return clip;
}

// The movies are loaded one after another, to keep the traces in order.
function load(clip:MovieClip, url:String, loaded:Function):Void {
    queue.push({clip: clip, url: url, loaded: loaded});
}

function loadNext():Void {
    if (queue.length > 0) {
        loader.loadClip(queue[0].url, queue[0].clip);
    }
}

listener.onLoadInit = function(clip:MovieClip):Void {
    trace(clip._name + ": " + clip.forceSmoothing);
    queue.shift().loaded(clip);
    loadNext();
};
loader.addListener(listener);

load(makeClip("untouched"), "filled.swf", function(clip:MovieClip):Void {});

load(makeClip("enabled"), "filled.swf", function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
    trace("after true: " + clip.forceSmoothing);
});

load(makeClip("disabled"), "filled.swf", function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
    clip.forceSmoothing = false;
});

// Loading another movie discards what was set before.
var reloaded:MovieClip = makeClip("reloaded");
load(reloaded, "filled.swf", function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
});
load(reloaded, "filled.swf", function(clip:MovieClip):Void {});

// Only the clip that the shape is placed in can smooth it, loaded or not.
load(makeClip("outer"), "nested.swf", function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
});
load(makeClip("inner"), "nested.swf", function(clip:MovieClip):Void {
    clip.child.forceSmoothing = true;
});

// Shapes that are placed later on are smoothed as well.
load(makeClip("later"), "frames.swf", function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
    trace("set on frame " + clip._currentframe);
});

// Drawings are not shapes.
var bitmap:BitmapData = new BitmapData(2, 2, false, 0xFF0000);
bitmap.setPixel(1, 0, 0x00FF00);
bitmap.setPixel(0, 1, 0x0000FF);
bitmap.setPixel(1, 1, 0xFFFF00);
var drawn:MovieClip = makeClip("drawn");
drawn.beginBitmapFill(bitmap, new Matrix(), false, false);
drawn.lineTo(2, 0);
drawn.lineTo(2, 2);
drawn.lineTo(0, 2);
drawn.endFill();
drawn.forceSmoothing = true;

loadNext();
