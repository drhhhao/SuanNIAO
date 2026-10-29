package dev.phonepad

import android.animation.ObjectAnimator
import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.content.res.Configuration
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.animation.LinearInterpolator
import android.view.animation.PathInterpolator
import android.widget.FrameLayout
import android.widget.TextView
import kotlin.math.roundToInt

/**
 * 界面的配色、字体、动画曲线、触感：黑白为主，红色只留给「正在录音」；跟随手机的深浅色。
 * 和电脑端确认过的样稿一致（极简黑白 · 圆体）。
 */
object Ui {
    class Palette(
        val bg: Int,
        val tile: Int,
        val line: Int,
        val fg: Int,
        /** 装饰用的浅灰：点阵、虚线、不可用的图标 */
        val mut: Int,
        /** 要读的灰字：状态、说明、键上的小字。在底色和卡片上都至少 4.5:1 */
        val sub: Int,
        val red: Int,
        val dark: Boolean,
    ) {
        /** 浮层后面那层半透明的黑 */
        val scrim: Int get() = if (dark) 0x99000000.toInt() else 0x52000000
    }

    private val LIGHT = Palette(
        0xFFEFEFEC.toInt(), 0xFFFFFFFF.toInt(), 0xFFE2E2DE.toInt(), 0xFF121212.toInt(),
        0xFF8D8D88.toInt(), 0xFF666662.toInt(), 0xFFE5362B.toInt(), false,
    )
    private val DARK = Palette(
        0xFF000000.toInt(), 0xFF111111.toInt(), 0xFF262626.toInt(), 0xFFF2F2F2.toInt(),
        0xFF5E5E5E.toInt(), 0xFF8E8E8E.toInt(), 0xFFFF4032.toInt(), true,
    )

    fun palette(context: Context): Palette {
        val night = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK
        return if (night == Configuration.UI_MODE_NIGHT_YES) DARK else LIGHT
    }

    /** 按下时快速收缩用这个；松手回弹用 SPRING（会冲过头一点再回来） */
    val EASE = PathInterpolator(0.2f, 0.8f, 0.2f, 1f)
    val SPRING = PathInterpolator(0.34f, 1.56f, 0.64f, 1f)
    /** 回弹小一点的：面板、对话框出现用，丝滑不「弹」 */
    val SOFT_SPRING = PathInterpolator(0.3f, 1.25f, 0.5f, 1f)

    /** 模块的圆角半径 */
    const val RADIUS_DP = 18f

    private val fonts = HashMap<Int, Typeface>()

    /** 圆体 Nunito（可变字重）。它没有中文字形，中文由系统字体（小米上是 MiSans）补上 */
    fun font(context: Context, weight: Int): Typeface = fonts.getOrPut(weight) {
        runCatching {
            Typeface.Builder(context.assets, "fonts/Nunito.ttf").setFontVariationSettings("'wght' $weight").build()
        }.getOrNull() ?: Typeface.create(Typeface.DEFAULT, weight, false)
    }

    fun dp(context: Context, v: Float) = v * context.resources.displayMetrics.density

    fun sp(context: Context, v: Float) =
        TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, v, context.resources.displayMetrics)

    /** 细线：大约 0.6dp，至少 1 个像素 */
    fun hairline(context: Context) = maxOf(1f, dp(context, 0.6f).roundToInt().toFloat())

    /** 两种颜色按 t（0～1）混合 */
    fun blend(a: Int, b: Int, t: Float): Int {
        if (t <= 0f) return a
        if (t >= 1f) return b
        fun ch(shift: Int) = (((a ushr shift) and 0xFF) + (((b ushr shift) and 0xFF) - ((a ushr shift) and 0xFF)) * t).roundToInt()
        return (ch(24) shl 24) or (ch(16) shl 16) or (ch(8) shl 8) or ch(0)
    }

    /** 颜色换个透明度（0～1） */
    fun alpha(color: Int, a: Float) = (color and 0x00FFFFFF) or ((a.coerceIn(0f, 1f) * 255).roundToInt() shl 24)

    /** 画一块模块底板：圆角、底色、一圈细线（lineAlpha 为 0 时不画线） */
    fun drawTile(canvas: Canvas, w: Float, h: Float, radius: Float, fill: Int, line: Int, lineAlpha: Float, fillPaint: Paint, linePaint: Paint) {
        fillPaint.color = fill
        canvas.drawRoundRect(0f, 0f, w, h, radius, radius, fillPaint)
        if (lineAlpha > 0f) {
            linePaint.color = line
            linePaint.alpha = (lineAlpha * ((line ushr 24) and 0xFF)).roundToInt()
            val i = linePaint.strokeWidth / 2
            canvas.drawRoundRect(i, i, w - i, h - i, radius - i, radius - i, linePaint)
        }
    }

    /** 圆角底色（可带一圈描边） */
    fun round(color: Int, radius: Float, stroke: Int = 0, strokeColor: Int = 0) = GradientDrawable().apply {
        cornerRadius = radius
        setColor(color)
        if (stroke > 0) setStroke(stroke, strokeColor)
    }

    /** 按下缩一点、松手弹回来（小按钮、面板里的行用） */
    @SuppressLint("ClickableViewAccessibility")
    fun springy(v: View, scale: Float = 0.94f) {
        v.setOnTouchListener { view, e ->
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> view.animate().scaleX(scale).scaleY(scale).setStartDelay(0).setDuration(90).setInterpolator(EASE).start()
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL ->
                    view.animate().scaleX(1f).scaleY(1f).setStartDelay(0).setDuration(380).setInterpolator(SOFT_SPRING).start()
            }
            false
        }
    }

    /** 左右抖一下：「没有发出去」「不行」 */
    fun shake(v: View) {
        val d = dp(v.context, 6f)
        ObjectAnimator.ofFloat(v, View.TRANSLATION_X, 0f, -d, d * 0.85f, -d * 0.6f, d * 0.35f, -d * 0.15f, 0f).apply {
            duration = 360
            interpolator = LinearInterpolator()
            start()
        }
    }

    // ───────── 触感：走系统的 performHapticFeedback，关了系统「触摸反馈」就不震 ─────────

    enum class Buzz {
        /** 按键按下 */
        KEY,
        /** 很轻的一下：开始连发、对齐跳格、触控板点按 */
        TICK,
        /** 点了个小东西：开关、选项、连上电脑 */
        TAP,
        /** 长按到了、拿起模块 */
        LONG,
        /** 开始说话 */
        START,
        /** 成了：锁定录音、发出去了、存好了 */
        CONFIRM,
        /** 不行：没连上、放不下 */
        REJECT,
        /** 明显的两下：切换了方案 */
        STRONG,
    }

    fun buzz(view: View, kind: Buzz) {
        val api30 = Build.VERSION.SDK_INT >= 30
        val constant = when (kind) {
            Buzz.KEY -> HapticFeedbackConstants.KEYBOARD_TAP
            Buzz.TICK -> HapticFeedbackConstants.CLOCK_TICK
            Buzz.TAP -> HapticFeedbackConstants.CONTEXT_CLICK
            Buzz.LONG -> HapticFeedbackConstants.LONG_PRESS
            Buzz.START -> if (api30) HapticFeedbackConstants.GESTURE_START else HapticFeedbackConstants.VIRTUAL_KEY
            Buzz.CONFIRM -> if (api30) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.CONTEXT_CLICK
            Buzz.REJECT -> if (api30) HapticFeedbackConstants.REJECT else HapticFeedbackConstants.LONG_PRESS
            Buzz.STRONG -> HapticFeedbackConstants.LONG_PRESS
        }
        view.performHapticFeedback(constant)
        if (kind == Buzz.STRONG) {
            view.postDelayed({ view.performHapticFeedback(if (api30) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.LONG_PRESS) }, 150)
        }
    }

    /** 文字太长时只留结尾那段，前面用「…」，语音识别的字最新的在后面 */
    fun tail(text: String, paint: Paint, maxWidth: Float): String {
        if (paint.measureText(text) <= maxWidth) return text
        var start = 0
        while (start < text.length && paint.measureText("…" + text.substring(start)) > maxWidth) start++
        return "…" + text.substring(start)
    }

    /**
     * 顶栏右边的一行字：换内容时旧的往上滑走、新的从下面滑进来。
     * 同一类内容只是数字变了就原地改，不滑。
     */
    class Readout(context: Context, private val palette: Palette) : FrameLayout(context) {
        private var current: TextView? = null
        private val measure = TextView(context).apply {
            typeface = font(context, 800)
            textSize = 13f
        }

        /** 这行字放得下（不会被截断） */
        fun fits(text: String): Boolean {
            val room = width - paddingLeft - paddingRight
            return room <= 0 || measure.paint.measureText(text) <= room
        }

        fun set(text: String, hot: Boolean, animate: Boolean) {
            val color = if (hot) palette.fg else palette.sub
            val old = current
            if (old != null && !animate) {
                old.text = text
                old.setTextColor(color)
                return
            }
            if (old != null && old.text.toString() == text) {
                old.setTextColor(color)
                return
            }
            val view = TextView(context).apply {
                this.text = text
                setTextColor(color)
                typeface = font(context, 800)
                textSize = 13f
                maxLines = 1
                ellipsize = TextUtils.TruncateAt.END
                gravity = Gravity.END or Gravity.CENTER_VERTICAL
            }
            addView(view, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
            current = view
            if (old == null || height == 0) {
                old?.let(::removeView)
                return
            }
            val shift = height * 0.9f
            view.translationY = shift
            view.alpha = 0f
            view.animate().translationY(0f).alpha(1f).setDuration(420).setInterpolator(SOFT_SPRING).start()
            old.animate().translationY(-shift).alpha(0f).setDuration(260).setInterpolator(EASE)
                .withEndAction { removeView(old) }.start()
        }
    }

    /** 自绘的开关：黑白胶囊，滑块带一点回弹 */
    class Toggle(context: Context, private val palette: Palette) : View(context) {
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        private var t = 0f
        private var anim: ValueAnimator? = null
        var checked = false
            private set

        fun set(on: Boolean, animate: Boolean) {
            checked = on
            anim?.cancel()
            if (!animate) {
                t = if (on) 1f else 0f
                invalidate()
                return
            }
            anim = ValueAnimator.ofFloat(t, if (on) 1f else 0f).apply {
                duration = 240
                interpolator = SOFT_SPRING
                addUpdateListener {
                    t = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            setMeasuredDimension(dp(context, 44f).toInt(), dp(context, 26f).toInt())
        }

        override fun onDraw(canvas: Canvas) {
            val w = width.toFloat()
            val h = height.toFloat()
            val c = t.coerceIn(0f, 1f)
            paint.color = blend(if (palette.dark) 0xFF3A3A3A.toInt() else 0xFFD6D6D2.toInt(), palette.fg, c)
            canvas.drawRoundRect(0f, 0f, w, h, h / 2, h / 2, paint)
            val r = h / 2 - dp(context, 3f)
            val x = h / 2 + (w - h) * t
            paint.color = palette.tile
            canvas.drawCircle(x, h / 2, r, paint)
        }
    }

    /** 自绘的单选圆点 */
    class Radio(context: Context, private val palette: Palette) : View(context) {
        private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = dp(context, 1.8f)
        }
        private val dot = Paint(Paint.ANTI_ALIAS_FLAG)
        var checked = false
            set(value) {
                field = value
                invalidate()
            }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            val s = dp(context, 22f).toInt()
            setMeasuredDimension(s, s)
        }

        override fun onDraw(canvas: Canvas) {
            val c = width / 2f
            ring.color = if (checked) palette.fg else palette.mut
            canvas.drawCircle(c, c, c - ring.strokeWidth, ring)
            if (checked) {
                dot.color = palette.fg
                canvas.drawCircle(c, c, c * 0.45f, dot)
            }
        }
    }
}
