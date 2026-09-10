"""pickbench_gen_crowd.py out.swf [avatars=25] [parts=12] [anims=2] [subs=2] [shapes=2] [text=1] [protodepth=0]

Synthetic crowded Shararam room for core/examples/pickbench.rs:

  root
   └ layers                                 (sprite)
       ├ click_zone_mc (onRelease)          full-stage floor
       ├ avatar × K (onRelease, ~90×200 px) overlapping like a real crowd
       │   ├ body_holder_mc
       │   │   └ part × P                   createEmptyMovieClip per body part
       │   │       └ partroot               loaded part SWF root
       │   │           └ anim × A           animation clip ("D" state)
       │   │               └ sub × Q
       │   │                   └ shape × S  (10–35 px polygons)
       │   └ name_txt (EditText, optional)  the nickname over the head
       └ ui with 6 buttons (onRelease)

Shape depth = 10 (root, layers, avatar, holder, part, partroot, anim, sub, shape).
protodepth=N puts onRelease N levels down the prototype chain instead of on
the clip itself (a class hierarchy like Avatar → ActiveObject → …), so
`is_button_mode` walks the chain the way it does in the client.
"""
import struct, random, sys, math

out = sys.argv[1]
K = int(sys.argv[2]) if len(sys.argv) > 2 else 25
P = int(sys.argv[3]) if len(sys.argv) > 3 else 12
A = int(sys.argv[4]) if len(sys.argv) > 4 else 2
Q = int(sys.argv[5]) if len(sys.argv) > 5 else 2
S = int(sys.argv[6]) if len(sys.argv) > 6 else 2
TEXT = int(sys.argv[7]) if len(sys.argv) > 7 else 1
PROTO = int(sys.argv[8]) if len(sys.argv) > 8 else 0
random.seed(7)
W, H = 815, 495  # px


class Bits:
    def __init__(s): s.b = []
    def ub(s, v, n):
        for i in range(n - 1, -1, -1): s.b.append((v >> i) & 1)
    def sb(s, v, n): s.ub(v & ((1 << n) - 1), n)
    def bytes(s):
        b = s.b + [0] * ((8 - len(s.b) % 8) % 8)
        return bytes(int(''.join(map(str, b[i:i + 8])), 2) for i in range(0, len(b), 8))


def sbits(*vs):
    n = 1
    for v in vs:
        v = abs(v)
        while v >= (1 << (n - 1)): n += 1
    return n


def rect(x0, x1, y0, y1):
    w = Bits(); nb = sbits(x0, x1, y0, y1); w.ub(nb, 5)
    for v in (x0, x1, y0, y1): w.sb(v, nb)
    return w.bytes()


def tag(code, body):
    n = len(body)
    return (struct.pack('<H', (code << 6) | n) if n < 63 else struct.pack('<HI', (code << 6) | 0x3f, n)) + body


def define_shape(sid, pts, rgb):
    xs = [p[0] for p in pts]; ys = [p[1] for p in pts]
    body = struct.pack('<H', sid) + rect(min(xs), max(xs), min(ys), max(ys))
    body += b'\x01\x00' + bytes(rgb)
    body += b'\x00'
    w = Bits(); w.ub(1, 4); w.ub(0, 4)
    x, y = pts[0]; nb = sbits(x, y)
    w.ub(0, 1); w.ub(0b00011, 5); w.ub(nb, 5); w.sb(x, nb); w.sb(y, nb); w.ub(1, 1)
    for (x1, y1) in pts[1:] + pts[:1]:
        dx, dy = x1 - x, y1 - y; nb = max(2, sbits(dx, dy))
        w.ub(1, 1); w.ub(1, 1); w.ub(nb - 2, 4); w.ub(1, 1); w.sb(dx, nb); w.sb(dy, nb); x, y = x1, y1
    w.ub(0, 6)
    return tag(2, body + w.bytes())


def define_edit_text(tid, w, h, text):
    # DefineEditText: id, bounds, flags(u16), [font], [color], [maxlen], [layout], varname, [text]
    flags = 0x0080 | 0x0008 | 0x1000  # HasText | ReadOnly | NoSelect
    body = struct.pack('<H', tid) + rect(0, w, 0, h) + struct.pack('<H', flags) + b'\x00' + text.encode() + b'\x00'
    return tag(37, body)


def matrix_bits(tx, ty):
    m = Bits(); m.ub(0, 1); m.ub(0, 1)
    nt = sbits(tx, ty); m.ub(nt, 5); m.sb(tx, nt); m.sb(ty, nt)
    return m.bytes()


def place(depth, cid, tx, ty, name=None):
    flags = 0x06
    if name is not None: flags |= 0x20
    body = bytes([flags]) + struct.pack('<H', depth) + struct.pack('<H', cid) + matrix_bits(tx, ty)
    if name is not None: body += name.encode() + b'\x00'
    return tag(26, body)


def push_str(s):
    b = s.encode() + b'\x00'
    return bytes([0x96]) + struct.pack('<H', 1 + len(b)) + b'\x00' + b


def push_int(v):
    return bytes([0x96]) + struct.pack('<H', 5) + b'\x07' + struct.pack('<i', v)


DEFINE_EMPTY_FN = bytes([0x9B]) + struct.pack('<H', 5) + b'\x00' + struct.pack('<H', 0) + struct.pack('<H', 0)
SET_VARIABLE = bytes([0x1D]); GET_VARIABLE = bytes([0x1C]); SET_MEMBER = bytes([0x4F]); GET_MEMBER = bytes([0x4E])
INIT_OBJECT = bytes([0x43])


def do_action_button_mode(proto_depth):
    """this.onRelease = fn  — or, with proto_depth>0, this.__proto__ = {__proto__: {... {onRelease: fn}}}
    with the innermost object inheriting from the original prototype so MovieClip methods keep working."""
    if proto_depth == 0:
        return tag(12, push_str('onRelease') + DEFINE_EMPTY_FN + SET_VARIABLE + b'\x00')
    # var p = this.__proto__;  (original MovieClip prototype)
    code = push_str('p') + push_str('this') + GET_VARIABLE + push_str('__proto__') + GET_MEMBER + SET_VARIABLE
    # innermost: o = {onRelease: fn}; o.__proto__ = p; p = o
    code += push_str('o') + push_str('onRelease') + DEFINE_EMPTY_FN + push_int(1) + INIT_OBJECT + SET_VARIABLE
    code += push_str('o') + GET_VARIABLE + push_str('__proto__') + push_str('p') + GET_VARIABLE + SET_MEMBER
    code += push_str('p') + push_str('o') + GET_VARIABLE + SET_VARIABLE
    for i in range(proto_depth - 1):
        # o = {someMethod_i: fn}; o.__proto__ = p; p = o
        code += push_str('o') + push_str(f'method{i}') + DEFINE_EMPTY_FN + push_int(1) + INIT_OBJECT + SET_VARIABLE
        code += push_str('o') + GET_VARIABLE + push_str('__proto__') + push_str('p') + GET_VARIABLE + SET_MEMBER
        code += push_str('p') + push_str('o') + GET_VARIABLE + SET_VARIABLE
    # this.__proto__ = p
    code += push_str('this') + GET_VARIABLE + push_str('__proto__') + push_str('p') + GET_VARIABLE + SET_MEMBER
    return tag(12, code + b'\x00')


def define_sprite(sid, frames_tags):
    body = struct.pack('<HH', sid, 1) + b''.join(frames_tags) + tag(1, b'') + tag(0, b'')
    return tag(39, body)


def poly(r, k):
    return [(int(r * math.cos(2 * math.pi * j / k) * random.uniform(0.7, 1.3)),
             int(r * math.sin(2 * math.pi * j / k) * random.uniform(0.7, 1.3))) for j in range(k)]


tags = [tag(9, b'\x72\xcc\xec')]
next_id = [1]


def new_id():
    i = next_id[0]; next_id[0] += 1; return i


part_shapes = []
for i in range(32):
    sid = new_id()
    tags.append(define_shape(sid, poly(random.randint(200, 700), random.randint(5, 9)),
                             (random.randint(0, 255), random.randint(0, 255), random.randint(0, 255))))
    part_shapes.append(sid)
floor_id = new_id()
tags.append(define_shape(floor_id, [(0, 0), (W * 20, 0), (W * 20, H * 20), (0, H * 20)], (60, 80, 60)))
btn_face = new_id()
tags.append(define_shape(btn_face, [(0, 0), (1600, 0), (1600, 600), (0, 600)], (200, 200, 60)))
text_id = None
if TEXT:
    text_id = new_id()
    tags.append(define_edit_text(text_id, 1800, 400, 'Player125489'))

sub_ids = []
for q in range(Q):
    sid = new_id()
    tags.append(define_sprite(sid, [place(d + 1, random.choice(part_shapes), random.randint(-300, 300), random.randint(-300, 300))
                                   for d in range(S)]))
    sub_ids.append(sid)
anim_ids = []
for a in range(A):
    sid = new_id()
    tags.append(define_sprite(sid, [place(d + 1, sub_ids[d % Q], random.randint(-200, 200), random.randint(-200, 200)) for d in range(Q)]))
    anim_ids.append(sid)
partroot_id = new_id()
tags.append(define_sprite(partroot_id, [place(d + 1, anim_ids[d], random.randint(-100, 100), random.randint(-100, 100)) for d in range(A)]))
part_id = new_id()
tags.append(define_sprite(part_id, [place(1, partroot_id, 0, 0, name='partroot')]))
holder_id = new_id()
# body ~90 px wide, 200 px tall: parts along the body
tags.append(define_sprite(holder_id, [place(p + 1, part_id, random.randint(-700, 700), -3800 + int(p * 3800 / max(P - 1, 1)), name=f'p{p}') for p in range(P)]))
avatar_id = new_id()
avatar_tags = [do_action_button_mode(PROTO), place(1, holder_id, 0, 0, name='body_holder_mc')]
if TEXT:
    avatar_tags.append(place(2, text_id, -900, -4400, name='name_txt'))
tags.append(define_sprite(avatar_id, avatar_tags))

zone_id = new_id()
tags.append(define_sprite(zone_id, [do_action_button_mode(0), place(1, floor_id, 0, 0)]))
btn_id = new_id()
tags.append(define_sprite(btn_id, [do_action_button_mode(0), place(1, btn_face, 0, 0)]))
ui_id = new_id()
tags.append(define_sprite(ui_id, [place(i + 1, btn_id, 400 + i * 2000, 400, name=f'btn{i}') for i in range(6)]))

layer_tags = [place(1, zone_id, 0, 0, name='click_zone_mc')]
for i in range(K):
    x = int(random.gauss(W * 10, W * 3)); y = int(random.gauss(H * 12, H * 2.5))
    x = max(1000, min(W * 20 - 1000, x)); y = max(4500, min(H * 20 - 500, y))
    layer_tags.append(place(2 + i, avatar_id, x, y, name=f'a{i}'))
layer_tags.append(place(2 + K, ui_id, 0, 0, name='ui'))
layers_id = new_id()
tags.append(define_sprite(layers_id, layer_tags))

tags += [place(1, layers_id, 0, 0, name='layers'), tag(1, b''), tag(0, b'')]
body = rect(0, W * 20, 0, H * 20) + struct.pack('<BBH', 0, 24, 1) + b''.join(tags)
data = b'FWS' + bytes([8]) + struct.pack('<I', 8 + len(body)) + body
open(out, 'wb').write(data)
nodes = 1 + 1 + 2 + K * (2 + TEXT + P * (2 + A * (1 + Q * (1 + S)))) + 1 + 6 * 2
print(f'{out}: avatars={K} parts={P} anims={A} subs={Q} shapes={S} text={TEXT} proto={PROTO} ~{nodes} display objects, {len(data)} bytes')
