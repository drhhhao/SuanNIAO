# 蒜鸟 logo（2026-10-29 定稿「白蒜鸟 · 戴耳机」）：白白胖胖的蒜头身子，四道蒜瓣纹、蒜尖一点紫，
# 头顶五片细长蒜苗叶，戴一副带麦克风的耳机（对应语音输入）。坐标都在 256×256 的方块里。
# 打样时的其他几款（黄色、抱手机、别的耳机颜色）在 %TEMP%\sn-logo\v4\logo4.py，没放进项目。
import math


def f(v):
    return f"{v:.1f}".rstrip("0").rstrip(".")


def pt(p):
    return f"{f(p[0])} {f(p[1])}"


class Art:
    """攒 defs 和图形"""

    def __init__(self):
        self.d, self.g = [], []

    @staticmethod
    def url(name):
        return f"url(#{name})"

    def rg(self, name, c, r, stops, fc=None):
        fx = f' fx="{f(fc[0])}" fy="{f(fc[1])}"' if fc else ""
        st = "".join(f'<stop offset="{o}" stop-color="{col}" stop-opacity="{op}"/>' for o, col, op in stops)
        self.d.append(f'<radialGradient id="{name}" gradientUnits="userSpaceOnUse" '
                      f'cx="{f(c[0])}" cy="{f(c[1])}" r="{f(r)}"{fx}>{st}</radialGradient>')

    def lg(self, name, a, b, stops):
        st = "".join(f'<stop offset="{o}" stop-color="{col}" stop-opacity="{op}"/>' for o, col, op in stops)
        self.d.append(f'<linearGradient id="{name}" gradientUnits="userSpaceOnUse" '
                      f'x1="{f(a[0])}" y1="{f(a[1])}" x2="{f(b[0])}" y2="{f(b[1])}">{st}</linearGradient>')

    def blur(self, name, sd):
        self.d.append(f'<filter id="{name}" x="-30%" y="-30%" width="160%" height="160%">'
                      f'<feGaussianBlur stdDeviation="{sd}"/></filter>')

    def clip(self, name, inner):
        self.d.append(f'<clipPath id="{name}">{inner}</clipPath>')

    def add(self, s):
        self.g.append(s)

    def out(self):
        return f'<defs>{"".join(self.d)}</defs>{"".join(self.g)}'


# ---------- 叶子：沿一条三次贝塞尔中线长出来，左右两半分开打光 ----------

def bez(c, t):
    (x0, y0), (x1, y1), (x2, y2), (x3, y3) = c
    m = 1 - t
    return (m ** 3 * x0 + 3 * m * m * t * x1 + 3 * m * t * t * x2 + t ** 3 * x3,
            m ** 3 * y0 + 3 * m * m * t * y1 + 3 * m * t * t * y2 + t ** 3 * y3)


def bez_d(c, t):
    (x0, y0), (x1, y1), (x2, y2), (x3, y3) = c
    m = 1 - t
    return (3 * m * m * (x1 - x0) + 6 * m * t * (x2 - x1) + 3 * t * t * (x3 - x2),
            3 * m * m * (y1 - y0) + 6 * m * t * (y2 - y1) + 3 * t * t * (y3 - y2))


def leaf_pts(c, wbase, wmax, peak=0.42, n=20):
    mid, left, right = [], [], []
    for i in range(n + 1):
        t = i / n
        x, y = bez(c, t)
        dx, dy = bez_d(c, t)
        ln = math.hypot(dx, dy) or 1
        nx, ny = -dy / ln, dx / ln
        if t <= peak:
            w = wbase + (wmax - wbase) * math.sin(math.pi / 2 * t / peak)
        else:
            w = wmax * math.cos(math.pi / 2 * (t - peak) / (1 - peak)) ** 0.75
        mid.append((x, y))
        left.append((x + nx * w / 2, y + ny * w / 2))
        right.append((x - nx * w / 2, y - ny * w / 2))
    return mid, left, right


def poly(pts):
    return "M " + " L ".join(pt(p) for p in pts) + " Z"


def leaf(a, name, c, wbase, wmax, cols, peak=0.42, rib=0.5):
    """cols = (根部, 中段, 叶尖)，每档是 (亮的一半, 暗的一半)"""
    mid, left, right = leaf_pts(c, wbase, wmax, peak)
    base, tip = c[0], c[3]
    lo, md, hi = cols
    a.lg(f"{name}L", base, tip, [(0, lo[0], 1), (0.45, md[0], 1), (1, hi[0], 1)])
    a.lg(f"{name}R", base, tip, [(0, lo[1], 1), (0.45, md[1], 1), (1, hi[1], 1)])
    a.add(f'<path d="{poly(left + right[::-1])}" fill="{a.url(name + "R")}"/>')
    a.add(f'<path d="{poly(left + mid[::-1])}" fill="{a.url(name + "L")}"/>')
    k = int(len(mid) * 0.82)
    a.add(f'<path d="M {" L ".join(pt(p) for p in mid[2:k])}" fill="none" stroke="#FFFFFF" '
          f'stroke-opacity="{rib}" stroke-width="2" stroke-linecap="round"/>')


# ---------- 各部件 ----------

BODY = ("M 128 62 C 96 64, 30 96, 30 158 C 30 214, 76 236, 128 236 "
        "C 180 236, 226 214, 226 158 C 226 96, 160 64, 128 62 Z")
WINGS = ('<ellipse cx="32" cy="194" rx="13" ry="22" transform="rotate(46 32 194)"/>'
         '<ellipse cx="224" cy="194" rx="13" ry="22" transform="rotate(-46 224 194)"/>')
RIDGES = ["M 117 72 C 90 84, 66 108, 58 146", "M 123 72 C 109 88, 101 112, 99 136",
          "M 133 72 C 147 88, 155 112, 157 136", "M 139 72 C 166 84, 190 108, 198 146"]
EYES = (102, 154)
EYE_Y = 158
BEAK_Y = EYE_Y + 8
BEAK = (f"M 118.6 {BEAK_Y + 3} C 124 {f(BEAK_Y - 0.6)}, 132 {f(BEAK_Y - 0.6)}, 137.4 {BEAK_Y + 3} "
        f"C 135.2 {BEAK_Y + 10}, 131.6 {BEAK_Y + 14}, 128 {f(BEAK_Y + 15.5)} "
        f"C 124.4 {BEAK_Y + 14}, 120.8 {BEAK_Y + 10}, 118.6 {BEAK_Y + 3} Z")
# 耳机：头带在蒜苗后面，耳罩压在身子两边，左边耳罩伸出一根麦克风
BAND = "M 27 132 C 24 84, 74 54, 128 54 C 182 54, 232 84, 229 132"
CUPS = (28, 228)
BOOM = "M 30 176 C 36 200, 64 210, 92 202"
INK, INK_HI, CUP_HI, CUP_LO = "#2B3240", "#56627A", "#3C4556", "#1E242F"
LIME = "#9BE03A"

# 五片蒜苗叶：后面两片短、颜色深，前面两片往外弯，中间一片最高
LEAF_FRONT = (("#F1FBD9", "#E2F3BE"), ("#AEE458", "#93CF3E"), ("#72BE2C", "#5BA222"))
LEAF_BACK = (("#DDF2B0", "#CFE9A0"), ("#8FCB3E", "#7DBB33"), ("#5DA526", "#4E921E"))
LEAVES = [
    ("lbL", ((124, 76), (116, 56), (104, 38), (88, 24)), 11, 22, LEAF_BACK),
    ("lbR", ((132, 76), (142, 56), (156, 38), (174, 24)), 11, 22, LEAF_BACK),
    ("lfL", ((122, 78), (108, 56), (84, 44), (52, 44)), 12, 25, LEAF_FRONT),
    ("lfR", ((134, 78), (148, 56), (174, 44), (206, 46)), 12, 25, LEAF_FRONT),
    ("lfC", ((128, 80), (124, 56), (128, 32), (138, 10)), 14, 27, LEAF_FRONT),
]

# 底板（电脑图标、手机老式图标带着；手机自适应图标的底板是单独一层）
PLATE_TOP, PLATE_BOTTOM = "#CDEBFF", "#98CDF6"
GLOW = ((128, 96), 150, 0.55)


def plate(a):
    a.lg("plate", (0, 0), (0, 256), [(0, PLATE_TOP, 1), (1, PLATE_BOTTOM, 1)])
    (cx, cy), r, op = GLOW
    a.rg("glow", (cx, cy), r, [(0, "#FFFFFF", op), (1, "#FFFFFF", 0)])
    a.add(f'<rect width="256" height="256" rx="56" fill="{a.url("plate")}"/>')
    a.add(f'<rect width="256" height="256" rx="56" fill="{a.url("glow")}"/>')


def ground(a):
    """脚下的影子，蓝色调，只在蓝底板上画"""
    a.blur("b4", 4)
    a.add(f'<ellipse cx="128" cy="242" rx="84" ry="9" fill="#3F7FB8" opacity="0.34" filter="{a.url("b4")}"/>')


def feet(a):
    a.lg("foot", (0, 230), (0, 248), [(0, "#FFB64E", 1), (1, "#EE8420", 1)])
    for cx in (108, 148):
        a.add(f'<ellipse cx="{cx}" cy="238" rx="12.5" ry="8.5" fill="{a.url("foot")}"/>')


def wings(a):
    a.lg("wing", (0, 176), (0, 222), [(0, "#FFFFFF", 1), (1, "#EADCC2", 1)])
    a.clip("wingclip", WINGS)
    a.add(WINGS.replace("/>", f' fill="{a.url("wing")}"/>'))
    a.blur("b1", 1.2)
    a.add(f'<path d="{BODY}" fill="none" stroke="#9C7A45" stroke-opacity="0.3" stroke-width="7" '
          f'clip-path="{a.url("wingclip")}" filter="{a.url("b1")}"/>')


def body(a):
    a.rg("body", (110, 140), 150, [(0, "#FFFFFF", 1), (0.45, "#FFFCF2", 1), (0.8, "#F3E9D5", 1), (1, "#D9C7A6", 1)],
         fc=(96, 120))
    a.add(f'<path d="{BODY}" fill="{a.url("body")}"/>')
    a.lg("bottom", (0, 180), (0, 236), [(0, "#B8935A", 0), (1, "#B8935A", 0.32)])
    a.add(f'<path d="{BODY}" fill="{a.url("bottom")}"/>')


def ridges(a):
    """蒜尖一点紫皮；四道蒜瓣纹从蒜尖往下，到眼睛旁边就淡没了"""
    a.rg("purple", (128, 66), 78, [(0, "#C47BD6", 0.5), (0.5, "#D59BE2", 0.24), (1, "#E6C2EE", 0)])
    a.add(f'<path d="{BODY}" fill="{a.url("purple")}"/>')
    a.lg("ridge", (0, 70), (0, 146), [(0, "#D2BF9C", 0.9), (0.55, "#D2BF9C", 0.45), (1, "#D2BF9C", 0)])
    a.lg("ridgehi", (0, 70), (0, 146), [(0, "#FFFFFF", 0.9), (1, "#FFFFFF", 0)])
    for r in RIDGES:
        a.add(f'<path d="{r}" fill="none" stroke="{a.url("ridgehi")}" stroke-width="3" stroke-linecap="round" '
              f'transform="translate(-2.5 0)"/>')
        a.add(f'<path d="{r}" fill="none" stroke="{a.url("ridge")}" stroke-width="2.2" stroke-linecap="round"/>')


def headset_band(a):
    a.add(f'<path d="{BAND}" fill="none" stroke="{INK}" stroke-width="10" stroke-linecap="round"/>')
    a.add(f'<path d="{BAND}" fill="none" stroke="{INK_HI}" stroke-width="3" stroke-linecap="round" '
          f'transform="translate(0 -2)"/>')


def leaves(a):
    a.blur("b3", 3)
    a.add(f'<ellipse cx="128" cy="80" rx="20" ry="6" fill="#8A7440" opacity="0.28" filter="{a.url("b3")}"/>')
    for name, c, wb, wm, cols in LEAVES:
        leaf(a, name, c, wb, wm, cols)


def face(a):
    a.blur("b1f", 1.2)
    for cx in (80, 176):
        a.add(f'<ellipse cx="{cx}" cy="{EYE_Y + 20}" rx="13" ry="7.5" fill="#FF8AA2" opacity="0.6" '
              f'filter="{a.url("b1f")}"/>')
    for cx in EYES:
        a.add(f'<ellipse cx="{cx}" cy="{EYE_Y}" rx="9.5" ry="11.5" fill="#211913"/>')
        a.add(f'<circle cx="{f(cx + 3.4)}" cy="{f(EYE_Y - 4.6)}" r="3.6" fill="#FFFFFF"/>')
        a.add(f'<circle cx="{f(cx - 3.2)}" cy="{f(EYE_Y + 5.2)}" r="1.7" fill="#FFFFFF" fill-opacity="0.85"/>')
    y = BEAK_Y
    a.lg("beak", (0, y), (0, y + 15), [(0, "#FFB84E", 1), (1, "#F07A12", 1)])
    a.add(f'<path d="{BEAK}" fill="{a.url("beak")}"/>')
    a.add(f'<path d="M 123 {f(y + 2.6)} C 126 {f(y + 1.2)}, 130 {f(y + 1.2)}, 133 {f(y + 2.6)}" fill="none" '
          f'stroke="#FFFFFF" stroke-opacity="0.55" stroke-width="1.6" stroke-linecap="round"/>')


def headset_cups(a):
    a.lg("cup", (0, 122), (0, 178), [(0, CUP_HI, 1), (1, CUP_LO, 1)])
    for cx in CUPS:
        a.add(f'<rect x="{cx - 16}" y="124" width="32" height="54" rx="15" fill="{a.url("cup")}"/>')
        a.add(f'<rect x="{cx - 10}" y="130" width="8" height="28" rx="4" fill="#FFFFFF" fill-opacity="0.2"/>')
    for x in (34, 214):
        a.add(f'<rect x="{x}" y="134" width="8" height="34" rx="4" fill="{LIME}"/>')
    a.add(f'<path d="{BOOM}" fill="none" stroke="{INK}" stroke-width="6" stroke-linecap="round"/>')
    a.add(f'<ellipse cx="96" cy="201" rx="10" ry="8" fill="{INK}"/>')
    a.add(f'<ellipse cx="97" cy="200" rx="6" ry="4.6" fill="{LIME}"/>')


def art(with_plate=True, with_ground=None):
    """彩色的鸟；with_plate=False 时不带底板（托盘、侧边栏用），with_ground 默认跟着底板走"""
    a = Art()
    if with_plate:
        plate(a)
    if with_ground if with_ground is not None else with_plate:
        ground(a)
    feet(a)
    wings(a)
    body(a)
    ridges(a)
    headset_band(a)
    leaves(a)
    face(a)
    headset_cups(a)
    return a.out()


# ---------- 单色剪影：手机主题图标、通知栏小图标用（只有白色和透明） ----------

def shapes(part, color, grow=0):
    """各部件的形状；grow>0 时整圈往外胖 grow，用来在后面的部件上挖缝"""
    s = f' stroke="{color}" stroke-width="{f(2 * grow)}" stroke-linejoin="round"' if grow else ""
    if part == "feet":
        return "".join(f'<ellipse cx="{cx}" cy="238" rx="12.5" ry="8.5" fill="{color}"{s}/>' for cx in (108, 148))
    if part == "wings":
        return WINGS.replace("/>", f' fill="{color}"{s}/>')
    if part == "body":
        return f'<path d="{BODY}" fill="{color}"{s}/>'
    if part == "band":
        return f'<path d="{BAND}" fill="none" stroke="{color}" stroke-width="{f(10 + 2 * grow)}" stroke-linecap="round"/>'
    if part == "leaves":
        out = []
        for _, c, wb, wm, _ in LEAVES:
            _, left, right = leaf_pts(c, wb, wm)
            out.append(f'<path d="{poly(left + right[::-1])}" fill="{color}"{s}/>')
        return "".join(out)
    if part == "headset":
        cups = "".join(f'<rect x="{cx - 16}" y="124" width="32" height="54" rx="15" fill="{color}"{s}/>' for cx in CUPS)
        return (cups + f'<path d="{BOOM}" fill="none" stroke="{color}" stroke-width="{f(6 + 2 * grow)}" '
                f'stroke-linecap="round"/><ellipse cx="96" cy="201" rx="10" ry="8" fill="{color}"{s}/>')
    raise ValueError(part)


def mono(gap=4, shine=True, limbs=True):
    """前面的部件在后面的部件上挖一圈 gap 宽的缝，叠在一起也分得清；眼睛和嘴挖空。
    shine：眼睛里留高光点（大图才看得清）；limbs：画不画脚和翅膀（小图上只剩碎渣，就不画）"""
    # 部件从后往前，后面跟着要在它身上挖缝的部件
    order = [("feet", ["body"]), ("wings", ["body", "headset"])] if limbs else []
    order += [("body", ["band", "leaves", "headset"]), ("band", ["leaves"]), ("leaves", []), ("headset", [])]
    holes = "".join(f'<ellipse cx="{cx}" cy="{EYE_Y}" rx="9.5" ry="11.5" fill="#000"/>' for cx in EYES)
    holes += f'<path d="{BEAK}" fill="#000"/>'
    out = []
    for part, cutters in order:
        cut = "".join(shapes(c, "#000", gap) for c in cutters) + (holes if part == "body" else "")
        if cut:
            out.append(f'<mask id="m{part}" maskUnits="userSpaceOnUse" x="-80" y="-80" width="416" height="416">'
                       f'<rect x="-80" y="-80" width="416" height="416" fill="#fff"/>{cut}</mask>'
                       f'<g mask="url(#m{part})">{shapes(part, "#FFFFFF")}</g>')
        else:
            out.append(shapes(part, "#FFFFFF"))
    if shine:
        out += [f'<circle cx="{f(cx + 3.4)}" cy="{f(EYE_Y - 4.6)}" r="3.6" fill="#FFFFFF"/>' for cx in EYES]
    return "".join(out)


def svg(inner, view="0 0 256 256", size=256):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="{view}">'
            f"{inner}</svg>")
