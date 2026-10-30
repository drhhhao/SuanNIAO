# 左上角的品牌字「蒜鸟」：python tools/logo/wordmark.py
# 字形是思源黑体 Heavy（Noto Sans SC，SIL OFL 1.1，fonts/ 里只留了这两个字），和界面上原来的粗黑体一个样子；
# 「蒜」草字头的两竖换成两棵穿过横的绿蒜苗，「鸟」里的点换成一个斜放的蛋黄色的蛋。
# 输出三层，叠在一起用：
#   字身：跟着深浅色变黑/白（电脑端 Slint 里 colorize，手机端 setTint）
#   蒜苗：绿色，深浅色各一个绿（同上，代码里上色）
#   蛋：固定蛋黄色，文件里就是这个颜色，不用上色
#   电脑端 pc/assets/wordmark.svg、wordmark-sprouts.svg、wordmark-egg.svg
#   手机端 res/drawable/wordmark.xml、wordmark_sprouts.xml、wordmark_egg.xml
import math, pathlib
from fontTools.ttLib import TTFont
from fontTools.pens.recordingPen import RecordingPen
from fontTools.pens.transformPen import TransformPen

HERE = pathlib.Path(__file__).parent
ROOT = HERE.parent.parent
FONT = HERE / "fonts" / "NotoSansSC-Black-subset.ttf"
PC = ROOT / "pc" / "assets"
RES = ROOT / "android" / "app" / "src" / "main" / "res"

GAP = 30                      # 两个字之间比字体默认再挤一点
VIEW = (10, 100, 1955, 1162)  # x y 宽 高（1 em = 1000）：两个字连蒜苗一起框住
EGG_COLOR = "#FFC94D"          # 蛋黄色，深浅色一样
EGG_TILT = -30                # 蛋顺着原来那一点的方向斜放：尖头朝左上
SUAN_STROKES = (10, 11)       # 「蒜」草字头的两竖（第 12 个轮廓是那一横，留着）
NIAO_DOT = 3                  # 「鸟」里的点


def f(v):
    return f"{v:.1f}".rstrip("0").rstrip(".")


def glyph_paths(font, ch, dx, skip):
    """把一个字按轮廓拆开，跳过 skip 里的几个，平移 dx 后连同视框原点一起换算好，返回 path 字符串"""
    gs = font.getGlyphSet()
    k = 1000 / font["head"].unitsPerEm
    asc = font["hhea"].ascent * k
    rec = RecordingPen()
    gs[font.getBestCmap()[ord(ch)]].draw(TransformPen(rec, (k, 0, 0, -k, dx - VIEW[0], asc - VIEW[1])))
    out, cur, i = [], [], 0

    def pt(p):
        return f"{f(p[0])} {f(p[1])}"

    for op, args in rec.value:
        if op == "moveTo":
            cur = [f"M {pt(args[0])}"]
        elif op == "lineTo":
            cur.append(f"L {pt(args[0])}")
        elif op == "qCurveTo":
            # TrueType 的二次曲线可以连着好几个控制点，中间的落点取相邻控制点的中点
            pts = list(args)
            if pts[-1] is None:
                raise ValueError("不支持没有落点的闭合曲线")
            ctrl, end = pts[:-1], pts[-1]
            for j, c in enumerate(ctrl):
                nxt = end if j == len(ctrl) - 1 else ((c[0] + ctrl[j + 1][0]) / 2, (c[1] + ctrl[j + 1][1]) / 2)
                cur.append(f"Q {pt(c)} {pt(nxt)}")
        elif op == "curveTo":
            cur.append("C " + " ".join(pt(p) for p in args))
        elif op in ("closePath", "endPath"):
            if i not in skip:
                out.append(" ".join(cur) + " Z")
            i += 1
    return out, gs[font.getBestCmap()[ord(ch)]].width * k


def leaf(base, tip, w, bend):
    """一片尖叶子：base 是根，tip 是尖，w 是半宽，bend 往法线方向弯"""
    (bx, by), (tx, ty) = base, tip
    dx, dy = tx - bx, ty - by
    L = math.hypot(dx, dy)
    nx, ny = -dy / L, dx / L
    mx, my = bx + dx * 0.45 + nx * bend, by + dy * 0.45 + ny * bend
    b1, b2 = (bx + nx * w * 0.35, by + ny * w * 0.35), (bx - nx * w * 0.35, by - ny * w * 0.35)
    c1, c2 = (mx + nx * w, my + ny * w), (mx - nx * w, my - ny * w)
    return f"M {f(b1[0])} {f(b1[1])} Q {f(c1[0])} {f(c1[1])} {f(tx)} {f(ty)} Q {f(c2[0])} {f(c2[1])} {f(b2[0])} {f(b2[1])} Z"


def sprout(cx, bottom, fork, top, w, stem):
    """一棵小蒜苗：一根茎从横下面穿过横，在横上面分成两片叶子，一片高一片矮"""
    h = fork - top
    half, neck = stem / 2, stem * 0.36
    body = (f"M {f(cx - half)} {f(bottom)} L {f(cx + half)} {f(bottom)} "
            f"C {f(cx + half)} {f(fork + 60)} {f(cx + neck)} {f(fork + 30)} {f(cx + neck)} {f(fork)} "
            f"L {f(cx - neck)} {f(fork)} C {f(cx - neck)} {f(fork + 30)} {f(cx - half)} {f(fork + 60)} {f(cx - half)} {f(bottom)} Z")
    return [body,
            leaf((cx - 4, fork + 10), (cx - 0.66 * h, top + 0.24 * h), w, 0.18 * h),
            leaf((cx + 4, fork + 10), (cx + 0.44 * h, top), w, -0.14 * h)]


def egg(cx, cy, w, h, tilt, n=48):
    """蛋：上尖下圆，绕中心转 tilt 度（负数往左歪）"""
    a = math.radians(tilt)
    ca, sa = math.cos(a), math.sin(a)
    pts = []
    for i in range(n):
        t = 2 * math.pi * i / n
        y = math.cos(t) * h / 2
        x = math.sin(t) * (1 + 0.18 * math.cos(t)) / 1.18 * w / 2
        pts.append(f"{f(cx + x * ca - y * sa)} {f(cy + x * sa + y * ca)}")
    return "M " + " L ".join(pts) + " Z"


def build():
    font = TTFont(str(FONT))
    suan, w1 = glyph_paths(font, "蒜", 0, SUAN_STROKES)
    nx = w1 - GAP
    niao, _ = glyph_paths(font, "鸟", nx, (NIAO_DOT,))
    ox, oy = VIEW[0], VIEW[1]
    eggs = egg(nx + 452 - ox, 628 - oy, 186, 236, EGG_TILT)
    sprouts = []
    for cx in (323, 670):
        sprouts += sprout(cx - ox, 539 - oy, 330 - oy, 105 - oy, 66, 116)
    return " ".join(suan + niao), " ".join(sprouts), eggs


def svg(d, color="#000000"):
    w, h = VIEW[2], VIEW[3]
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">'
            f'<path fill="{color}" d="{d}"/></svg>\n')


def vector(d, what, color=None):
    w, h = VIEW[2], VIEW[3]
    # 高按 26dp 算（一个字约 22dp 大，原来的字是 18sp），宽按比例
    hdp = 26
    return f'''<?xml version="1.0" encoding="utf-8"?>
<!-- 左上角品牌字的{what}（tools/logo/wordmark.py 生成，别手改；{"颜色固定" if color else "代码里 setTint 上色"}） -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="{f(hdp * w / h)}dp" android:height="{hdp}dp"
    android:viewportWidth="{w}" android:viewportHeight="{h}">
    <path android:fillColor="{('#FF' + color[1:]) if color else '#FFFFFFFF'}" android:pathData="{d}" />
</vector>
'''


def write(path, text):
    path.write_text(text, encoding="utf-8", newline="\n")
    print(path.relative_to(ROOT))


def main():
    body, sprouts, eggs = build()
    write(PC / "wordmark.svg", svg(body))
    write(PC / "wordmark-sprouts.svg", svg(sprouts))
    write(PC / "wordmark-egg.svg", svg(eggs, EGG_COLOR))
    write(RES / "drawable" / "wordmark.xml", vector(body, "字身"))
    write(RES / "drawable" / "wordmark_sprouts.xml", vector(sprouts, "蒜苗"))
    write(RES / "drawable" / "wordmark_egg.xml", vector(eggs, "蛋", EGG_COLOR))


if __name__ == "__main__":
    main()
