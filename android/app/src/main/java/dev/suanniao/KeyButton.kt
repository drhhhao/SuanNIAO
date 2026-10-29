package dev.suanniao

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.drawable.Drawable
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.Layout
import android.text.StaticLayout
import android.text.TextPaint
import android.text.TextUtils
import android.view.MotionEvent
import android.view.View
import kotlin.math.min

/**
 * 布局里的一个按键。在手机上判定单击 / 双击 / 长按，判定结果发给电脑，由电脑执行映射好的动作。
 * - 只配了单击：松手立刻触发
 * - 配了双击：松手后等双击间隔，没有第二下才算单击
 * - 配了长按：按住到时间就触发并震动，松手不再算单击
 *
 * 按住时怎么办（hold，电脑上设）：
 * - longPress（长按一次）：到长按时间触发一次长按（默认）
 * - repeatLongPress（重复长按）：到长按时间触发一次长按，隔「长按重复延迟」再一次，之后每「长按重复间隔」一次，松手停
 * - repeatClick（连发）：按下立刻单击一次，按住超过连发延迟后按连发间隔一直单击，松手马上停（不判定双击、长按）
 * - passthrough（直通）：按下时告诉电脑按住单击里的键，松手（或者被打断、App 切到后台）时告诉它松开，像实体键盘一样
 * 计时都在手机上：蓝牙断了电脑那边也不会停不下来。
 *
 * 不假装成功：
 * - 没连上电脑（offline）或电脑上暂停了按键（paused）：键变淡；按下不反色，抖一下、提示为什么没发
 * - 发送失败（刚断线）：马上收回按下的样子，同样抖一下
 * - 手指移出按键（留一点余量）再松手就不发；直通键除外，连发键第一下已经发出去的不追回
 *
 * 外观：白色圆角块，常见的键（方向、回车、删除、换行、Tab）画图标、左下角标小字，其余的直接写字；
 * 配了双击、长按的右上角有小角标。按下整块反色并缩一点，松手弹回来。
 */
@SuppressLint("ViewConstructor")
class KeyButton(
    context: Context,
    private val moduleId: String,
    private val label: String,
    private val hasDoubleClick: Boolean,
    private val hasLongPress: Boolean,
    private val doubleClickMs: Long,
    private val longPressMs: Long,
    private val hold: String,
    private val repeatDelayMs: Long,
    private val repeatIntervalMs: Long,
    private val longRepeatDelayMs: Long,
    private val longRepeatIntervalMs: Long,
    private val paused: Boolean,
    /** seq：连发、重复长按的第几下（0 = 第一下）；普通按法是 -1。heldMs：已经按住多久，不用时 -1。返回 true = 真的发出去了 */
    private val onTrigger: (id: String, trigger: String, seq: Int, heldMs: Long) -> Boolean,
    /** 直通的按键按下 / 松开；返回 true = 发出去了 */
    private val onHold: (id: String, down: Boolean) -> Boolean,
    /** 按了但没有发：为什么 */
    private val onRefused: (why: Refusal) -> Unit,
) : View(context) {

    enum class Refusal { OFFLINE, PAUSED, SLID_OFF }

    /** 没连上电脑：键变淡，按了不发（连接状态变了由 MainActivity 改，不用重建按键） */
    var offline = false
        set(value) {
            if (field == value) return
            field = value
            if (value) cancelPress()
            invalidate()
        }

    private val palette = Ui.palette(context)
    private val radius = Ui.dp(context, Ui.RADIUS_DP)
    private val icon: Drawable? = iconFor(label.trim())?.let { context.getDrawable(it)?.mutate() }
    /** 有图标时左下角的小字；方向键的字本身就是箭头，不重复写 */
    private val smallLabel: String? = label.trim().takeIf { icon != null && it !in ARROWS }
    private val bigText: String? = label.takeIf { icon == null }
    /** 右上角的角标：配了双击、长按（连发、直通的键电脑不报这两种） */
    private val badge: String? = when {
        hasDoubleClick && hasLongPress -> "双击·长按"
        hasDoubleClick -> "双击"
        hasLongPress -> "长按"
        else -> null
    }
    private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = Ui.hairline(context)
    }
    private val smallPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 750)
        textSize = Ui.sp(context, 10.5f)
    }
    private val badgePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 9.5f)
        textAlign = Paint.Align.RIGHT
    }
    private val bigPaint = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        val n = label.trim().length
        textSize = Ui.sp(context, if (n <= 3) 19f else if (n <= 6) 15f else 13f)
    }
    private var bigLayout: StaticLayout? = null
    /** 0 = 平时，1 = 按下（反色），中间是过渡 */
    private var pressAmount = 0f
    private var pressAnim: ValueAnimator? = null

    /** 计时都按按下的时刻算，不会越拖越慢 */
    private val ticker = Handler(Looper.getMainLooper())
    private var downAt = 0L
    private fun heldMs() = SystemClock.uptimeMillis() - downAt

    private var longFired = false
    private var longSeq = 0
    private var pendingClick: Runnable? = null
    /** 这次按下手指移出了按键：松手不发 */
    private var slidOff = false
    /** 移出多远才算移出（按键外面留的余量） */
    private val slop = Ui.dp(context, 14f)

    /** 到长按时间：触发一次长按；「重复长按」时接着按延迟、间隔再触发 */
    private val longTick = object : Runnable {
        override fun run() {
            if (!longFired) {
                longFired = true
                longSeq = 0
                Ui.buzz(this@KeyButton, Ui.Buzz.LONG)
                val sent = if (hold == HOLD_REPEAT_LONG) onTrigger(moduleId, "longPress", 0, heldMs()) else onTrigger(moduleId, "longPress", -1, -1)
                if (!sent) return failed()
                if (hold == HOLD_REPEAT_LONG) ticker.postAtTime(this, downAt + longPressMs + longRepeatDelayMs)
            } else {
                longSeq++
                if (!onTrigger(moduleId, "longPress", longSeq, heldMs())) return failed()
                ticker.postAtTime(this, downAt + longPressMs + longRepeatDelayMs + longSeq * longRepeatIntervalMs.coerceAtLeast(10))
            }
        }
    }

    /** 连发：已经连发了几下 */
    private var repeatSeq = 0
    private val repeatTick = object : Runnable {
        override fun run() {
            repeatSeq++
            if (repeatSeq == 1) Ui.buzz(this@KeyButton, Ui.Buzz.TICK) // 开始连发时轻轻一震
            if (!onTrigger(moduleId, "click", repeatSeq, heldMs())) return failed()
            ticker.postAtTime(this, downAt + repeatDelayMs + repeatSeq * repeatIntervalMs.coerceAtLeast(10))
        }
    }

    /** 直通：已经告诉电脑按住了，还没说松开 */
    private var holding = false

    init {
        isClickable = true
        isLongClickable = hasLongPress
        contentDescription = buildString {
            append(label)
            if (hasDoubleClick) append("，可以双击")
            if (hasLongPress) append("，可以长按")
        }
    }

    override fun getAccessibilityClassName(): CharSequence = android.widget.Button::class.java.name

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        val text = bigText ?: return
        val room = (w - Ui.dp(context, 12f)).toInt().coerceAtLeast(1)
        bigLayout = StaticLayout.Builder.obtain(text, 0, text.length, bigPaint, room)
            .setAlignment(Layout.Alignment.ALIGN_CENTER)
            .setMaxLines(2)
            .setEllipsize(TextUtils.TruncateAt.END)
            .setIncludePad(false)
            .build()
    }

    override fun onDraw(canvas: Canvas) {
        val p = palette
        val w = width.toFloat()
        val h = height.toFloat()
        // 没连上、暂停时：底色退向背景、字变灰，深色模式下也看得见边框和字
        val dim = paused || offline
        val fill = if (dim) Ui.blend(p.tile, p.bg, 0.55f) else Ui.blend(p.tile, p.fg, pressAmount)
        val fg = if (dim) p.mut else Ui.blend(p.fg, p.tile, pressAmount)
        val sub = if (dim) p.mut else Ui.blend(p.sub, p.tile, pressAmount)
        Ui.drawTile(canvas, w, h, radius, fill, p.line, if (dim) 1f else 1f - pressAmount, fillPaint, linePaint)
        icon?.let { d ->
            // 图标跟着按键大小走：大键上的图标也大
            val s = (min(w, h) * 0.27f).coerceIn(Ui.dp(context, 22f), Ui.dp(context, 40f)).toInt()
            val l = (width - s) / 2
            val t = (height - s) / 2
            d.setBounds(l, t, l + s, t + s)
            d.setTint(fg)
            d.draw(canvas)
        }
        smallLabel?.let {
            smallPaint.color = sub
            canvas.drawText(it, Ui.dp(context, 11f), h - Ui.dp(context, 8f) - smallPaint.descent(), smallPaint)
        }
        bigLayout?.let { layout ->
            bigPaint.color = fg
            canvas.save()
            canvas.translate((w - layout.width) / 2f, (h - layout.height) / 2f)
            layout.draw(canvas)
            canvas.restore()
        }
        badge?.let {
            val pad = Ui.dp(context, 9f)
            badgePaint.color = sub
            if (badgePaint.measureText(it) < w - Ui.dp(context, 30f)) {
                canvas.drawText(it, w - pad, pad - badgePaint.ascent(), badgePaint)
            } else {
                // 键太窄写不下：画个小点
                fillPaint.color = sub
                canvas.drawCircle(w - pad - Ui.dp(context, 2f), pad + Ui.dp(context, 2f), Ui.dp(context, 2.6f), fillPaint)
            }
        }
    }

    /** 按下：80 毫秒反色、缩到 93%；松手：颜色退回，大小带一点回弹 */
    private fun animatePress(down: Boolean) {
        pressAnim?.cancel()
        pressAnim = ValueAnimator.ofFloat(pressAmount, if (down) 1f else 0f).apply {
            duration = if (down) 80 else 220
            interpolator = Ui.EASE
            addUpdateListener {
                pressAmount = it.animatedValue as Float
                invalidate()
            }
            start()
        }
        val scale = if (down) 0.93f else 1f
        animate().scaleX(scale).scaleY(scale)
            .setStartDelay(0)
            .setDuration(if (down) 90 else 420)
            .setInterpolator(if (down) Ui.EASE else Ui.SOFT_SPRING)
            .start()
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downAt = SystemClock.uptimeMillis()
                longFired = false
                slidOff = false
                // 不反色：直接说为什么没发
                if (offline) return refuse(Refusal.OFFLINE)
                if (paused) return refuse(Refusal.PAUSED)
                isPressed = true
                animatePress(true) // 按住期间一直保持按下的样子
                Ui.buzz(this, Ui.Buzz.KEY)
                when {
                    hold == HOLD_PASSTHROUGH -> {
                        holding = onHold(moduleId, true)
                        if (!holding) failed()
                    }
                    hold == HOLD_REPEAT_CLICK -> {
                        // 按下立刻来一下，到时间后开始连发
                        repeatSeq = 0
                        if (onTrigger(moduleId, "click", 0, 0)) ticker.postAtTime(repeatTick, downAt + repeatDelayMs) else failed()
                    }
                    hasLongPress -> ticker.postAtTime(longTick, downAt + longPressMs)
                }
            }
            MotionEvent.ACTION_MOVE -> {
                if (isPressed && !slidOff && hold != HOLD_PASSTHROUGH && outside(e.x, e.y)) {
                    // 手指移出了按键：这次不算了。按下的样子退回去，告诉用户松手不会发
                    slidOff = true
                    stopTimers()
                    animatePress(false)
                    onRefused(Refusal.SLID_OFF)
                }
            }
            MotionEvent.ACTION_UP -> {
                val wasPressed = isPressed
                isPressed = false
                if (!wasPressed) return true // 没按成（没连上、暂停），或者已经被打断过（切走、调整布局）
                stopTimers()
                if (slidOff) return true
                animatePress(false)
                when (hold) {
                    HOLD_PASSTHROUGH -> releaseHold()
                    HOLD_REPEAT_CLICK -> {}
                    else -> if (!longFired) released()
                }
            }
            MotionEvent.ACTION_CANCEL -> cancelPress()
        }
        return true
    }

    private fun outside(x: Float, y: Float) = x < -slop || y < -slop || x > width + slop || y > height + slop

    /** 没发：抖一下（暂停时轻一点），让 MainActivity 在顶栏说原因 */
    private fun refuse(why: Refusal): Boolean {
        if (why == Refusal.PAUSED) {
            Ui.buzz(this, Ui.Buzz.TICK)
            animate().scaleX(0.97f).scaleY(0.97f).setStartDelay(0).setDuration(80).setInterpolator(Ui.EASE)
                .withEndAction { animate().scaleX(1f).scaleY(1f).setDuration(260).setInterpolator(Ui.SOFT_SPRING).start() }.start()
        } else {
            Ui.buzz(this, Ui.Buzz.REJECT)
            Ui.shake(this)
        }
        onRefused(why)
        return true
    }

    /** 以为连着、其实没发出去（刚断线）：停掉计时，收回按下的样子，抖一下 */
    private fun failed() {
        stopTimers()
        holding = false
        if (isPressed) {
            isPressed = false
            animatePress(false)
        }
        refuse(Refusal.OFFLINE)
    }

    /** 被打断（来电、切走、App 到后台、调整布局、界面重建、断线）：停掉连发，直通的键马上松开 */
    fun cancelPress() {
        stopTimers()
        releaseHold()
        if (isPressed) {
            isPressed = false
            animatePress(false)
        }
    }

    private fun stopTimers() {
        ticker.removeCallbacks(repeatTick)
        ticker.removeCallbacks(longTick)
    }

    private fun releaseHold() {
        if (holding) {
            holding = false
            onHold(moduleId, false)
        }
    }

    override fun onDetachedFromWindow() {
        cancelPress()
        pendingClick?.let(::removeCallbacks)
        pendingClick = null
        super.onDetachedFromWindow()
    }

    override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
        super.onWindowFocusChanged(hasWindowFocus)
        if (!hasWindowFocus && isPressed) cancelPress()
    }

    private fun released() {
        if (!hasDoubleClick) {
            if (!onTrigger(moduleId, "click", -1, -1)) failed()
            return
        }
        val pending = pendingClick
        if (pending != null) {
            removeCallbacks(pending)
            pendingClick = null
            if (!onTrigger(moduleId, "doubleClick", -1, -1)) failed()
        } else {
            val click = Runnable {
                pendingClick = null
                if (!onTrigger(moduleId, "click", -1, -1)) failed()
            }
            pendingClick = click
            postDelayed(click, doubleClickMs)
        }
    }

    /** 无障碍（TalkBack 双击屏幕）= 单击这个键 */
    override fun performClick(): Boolean {
        super.performClick()
        when {
            offline -> refuse(Refusal.OFFLINE)
            paused -> refuse(Refusal.PAUSED)
            hold == HOLD_PASSTHROUGH -> if (onHold(moduleId, true)) onHold(moduleId, false) else failed()
            !onTrigger(moduleId, "click", -1, -1) -> failed()
        }
        return true
    }

    /** 无障碍的长按 = 长按这个键 */
    override fun performLongClick(): Boolean {
        if (!hasLongPress) return false
        when {
            offline -> refuse(Refusal.OFFLINE)
            paused -> refuse(Refusal.PAUSED)
            !onTrigger(moduleId, "longPress", -1, -1) -> failed()
        }
        return true
    }

    companion object {
        const val HOLD_LONG = "longPress"
        const val HOLD_REPEAT_LONG = "repeatLongPress"
        const val HOLD_REPEAT_CLICK = "repeatClick"
        const val HOLD_PASSTHROUGH = "passthrough"

        private val ARROWS = setOf("↑", "↓", "←", "→", "上", "下", "左", "右")

        /** 常见按键用图标；别的按键（比如 Esc、^C、自己起名的）直接写字 */
        private fun iconFor(label: String): Int? = when (label) {
            "↑", "上" -> R.drawable.ic_key_up
            "↓", "下" -> R.drawable.ic_key_down
            "←", "左" -> R.drawable.ic_key_left
            "→", "右" -> R.drawable.ic_key_right
            "回车", "Enter", "↵", "⏎" -> R.drawable.ic_key_enter
            "删除", "退格", "Backspace", "⌫" -> R.drawable.ic_key_backspace
            "换行" -> R.drawable.ic_key_newline
            "Tab" -> R.drawable.ic_key_tab
            "Shift+Tab", "⇤" -> R.drawable.ic_key_shift_tab
            else -> null
        }
    }
}
