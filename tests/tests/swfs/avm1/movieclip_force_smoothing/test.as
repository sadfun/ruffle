import flash.display.BitmapData;

// image.png is a 2x2 image, which every clip scales up to 100x100.
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

// The images are loaded one after another, to keep the traces in order.
function load(clip:MovieClip, loaded:Function):Void {
    queue.push({clip: clip, loaded: loaded});
}

function loadNext():Void {
    if (queue.length > 0) {
        loader.loadClip("image.png", queue[0].clip);
    }
}

listener.onLoadInit = function(clip:MovieClip):Void {
    trace(clip._name + ": " + clip.forceSmoothing);
    queue.shift().loaded(clip);
    loadNext();
};
loader.addListener(listener);

trace("own property of the prototype: " + MovieClip.prototype.hasOwnProperty("forceSmoothing"));

load(makeClip("untouched"), function(clip:MovieClip):Void {});

// Loading the image discards what was set before.
var early:MovieClip = makeClip("early");
early.forceSmoothing = true;
load(early, function(clip:MovieClip):Void {});

load(makeClip("enabled"), function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
    trace("after true: " + clip.forceSmoothing);
});

load(makeClip("disabled"), function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
    clip.forceSmoothing = false;
    trace("after false: " + clip.forceSmoothing);
});

var reloaded:MovieClip = makeClip("reloaded");
load(reloaded, function(clip:MovieClip):Void {
    clip.forceSmoothing = true;
});
load(reloaded, function(clip:MovieClip):Void {});

// Only the clip that the image was loaded into can smooth it.
var outer:MovieClip = makeClip("outer");
load(outer.createEmptyMovieClip("inner", 0), function(clip:MovieClip):Void {
    outer.forceSmoothing = true;
});

// Attached bitmaps only follow the `smoothing` argument of `attachBitmap`.
var bitmap:BitmapData = new BitmapData(2, 2, false, 0xFF0000);
bitmap.setPixel(1, 0, 0x00FF00);
bitmap.setPixel(0, 1, 0x0000FF);
bitmap.setPixel(1, 1, 0xFFFF00);
var attached:MovieClip = makeClip("attached");
attached.attachBitmap(bitmap, 0, "auto", false);
attached.forceSmoothing = true;
trace("attached: " + attached.forceSmoothing);

loadNext();
