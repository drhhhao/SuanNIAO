# 用 logo.py 生成电脑端和手机端的全部图标文件：python tools/logo/make_icons.py
# 渲染用 render/ 里的小工具（和 Slint 是同一个 resvg 0.48.1），第一次用会自动编译
import pathlib, struct, subprocess, sys, tempfile

HERE = pathlib.Path(__file__).parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import logo as L

RENDER_DIR = HERE / "render"
RENDER = RENDER_DIR / "target" / "release" / "sn-render.exe"
PC = ROOT / "pc" / "assets"
RES = ROOT / "android" / "app" / "src" / "main" / "res"
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}

MARK_VIEW = "8 8 240 240"     # 不带底板时按鸟的外框裁
# 手机自适应图标的画布是 108dp，系统只露中间 72dp，再按机型裁成圆角方形或圆形；
# 鸟要落在中间 66dp 的安全区里，所以 256 的方块缩成 69dp 左右：整块画布对应 400 个单位
ADAPTIVE_VIEW = "-72 -72 400 400"
STAT_VIEW = "-1 -1 258 258"   # 通知栏小图标：鸟占满 24dp 里的 22dp 左右


def render(svg_text, px, py=None):
    with tempfile.TemporaryDirectory() as tmp:
        sp, out = pathlib.Path(tmp) / "a.svg", pathlib.Path(tmp) / "a.png"
        sp.write_text(svg_text, encoding="utf-8")
        subprocess.run([str(RENDER), str(sp), str(out), str(px)] + ([str(py)] if py else []), check=True)
        return out.read_bytes()


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        data = data.encode("utf-8")
    path.write_bytes(data)
    print(path.relative_to(ROOT))


def ico(frames):
    """frames: [(边长, png 字节)]，拼成 ico；每张都按 png 存，Vista 以后的 Windows 都认"""
    blobs, entries = b"", b""
    offset = 6 + 16 * len(frames)
    for px, png in frames:
        entries += struct.pack("<BBBBHHII", px % 256, px % 256, 0, 0, 1, 32, len(png), offset + len(blobs))
        blobs += png
    return struct.pack("<HHH", 0, 1, len(frames)) + entries + blobs


def adaptive_background():
    """自适应图标的底板那层：和电脑端图标底板一样的上浅下深蓝加一团白光，写成矢量，各密度都清楚"""
    x0, _, w, _ = map(float, ADAPTIVE_VIEW.split())
    k = 108 / w

    def dp(v):
        return L.f((v - x0) * k)

    (gx, gy), r, op = L.GLOW
    top, bottom = (f"#FF{c[1:]}" for c in (L.PLATE_TOP, L.PLATE_BOTTOM))
    return f'''<?xml version="1.0" encoding="utf-8"?>
<!-- 自适应图标的底板：和电脑端图标一样的蓝色渐变加一团白光（tools/logo/make_icons.py 生成，别手改） -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <path android:pathData="M0,0h108v108h-108z">
        <aapt:attr name="android:fillColor">
            <gradient android:type="linear"
                android:startX="0" android:startY="{dp(0)}" android:endX="0" android:endY="{dp(256)}"
                android:startColor="{top}" android:endColor="{bottom}" />
        </aapt:attr>
    </path>
    <path android:pathData="M0,0h108v108h-108z">
        <aapt:attr name="android:fillColor">
            <gradient android:type="radial"
                android:centerX="{dp(gx)}" android:centerY="{dp(gy)}" android:gradientRadius="{L.f(r * k)}"
                android:startColor="#{round(op * 255):02X}FFFFFF" android:endColor="#00FFFFFF" />
        </aapt:attr>
    </path>
</vector>
'''


def main():
    if not RENDER.exists():
        subprocess.run(["cargo", "build", "--release"], cwd=RENDER_DIR, check=True)
    icon = L.svg(L.art())
    mark = L.svg(L.art(False), MARK_VIEW)

    # 电脑端：exe 图标（任务栏、桌面快捷方式、托盘都用它），窗口图标，侧边栏和「关于」页的小鸟
    write(PC / "icon.ico", ico([(px, render(icon, px)) for px in (16, 20, 24, 32, 40, 48, 64, 256)]))
    write(PC / "icon.png", render(icon, 256))
    write(PC / "logo.svg", mark)

    # 手机端：自适应图标三层（底板、前景、主题图标用的单色剪影），老式图标，通知栏小图标
    write(RES / "drawable" / "ic_launcher_bg.xml", adaptive_background())
    fg = L.svg(L.art(False, True), ADAPTIVE_VIEW)
    mono = L.svg(L.mono(gap=4), ADAPTIVE_VIEW)
    stat = L.svg(L.mono(gap=8, shine=False, limbs=False), STAT_VIEW)
    for name, scale in DENSITIES.items():
        write(RES / f"mipmap-{name}" / "ic_launcher_fg.png", render(fg, round(108 * scale)))
        write(RES / f"mipmap-{name}" / "ic_launcher_mono.png", render(mono, round(108 * scale)))
        write(RES / f"mipmap-{name}" / "ic_launcher.png", render(icon, round(48 * scale)))
        write(RES / f"drawable-{name}" / "ic_stat_pad.png", render(stat, round(24 * scale)))


if __name__ == "__main__":
    main()
