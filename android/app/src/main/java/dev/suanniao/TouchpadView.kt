package dev.suanniao

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.os.Build
import android.os.SystemClock
import android.view.Choreographer
import android.view.MotionEvent
import android.view.View
import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.min
import kotlin.math.pow

/**
 * 触控板：单指移动、轻点左键、双指轻点右键、三指轻点中键、双指滚动（松手带惯性）、轻点后马上按住拖动 = 拖拽。
 * 外观是一整片小圆点，手指附近的点会亮起来、平滑跟着手指走；点按出一圈波纹。
 * 没连上电脑时整块变淡、不画波纹，中间写「未连接电脑」。
 * 系统返回手势的排除区由 MainActivity 设在根布局上（要一直盖到屏幕边），这里只记最近一次按在哪（lastDownY）。
 */
class TouchpadView(context: Context) : View(context) {

    interface Sink {
        /** t：这段移动最后一个触摸点的时间（ms，uptimeMillis 计时，带小数），电脑按它匀速回放 */
        fun move(dx: Double, dy: Double, t: Double)
        fun scroll(dx: Int, dy: Int)
        fun click(button: String)
        fun press(button: String, down: Boolean)
        /** 没连上电脑时摸了触控板 */
        fun offlineTouch()
        /** 一次触摸结束了（MainActivity 据此把返回手势的排除区挪到手常放的位置） */
        fun touchEnded()
    }

    var sink: Sink? = null
    /** 最近一次手指按下的位置（像素，触控板自己的坐标；-1 = 还没按过） */
    var lastDownY = -1f
        private set
    /** 左上角的标题：模块自己的名字 */
    var label = "触控板"
        set(value) {
            field = value
            contentDescription = value
            invalidate()
        }
    /** 没连上电脑：变淡、不画波纹 */
    var offline = false
        set(value) {
            if (field == value) return
            field = value
            invalidate()
        }
    var sensitivity = 2.5
    var acceleration = 1.2
    var scrollSpeed = 1.0
    var naturalScroll = true

    private enum class Mode { MOVE, SCROLL, IDLE }

    private val density = resources.displayMetrics.density
    private val palette = Ui.palette(context)
    private val radius = Ui.dp(context, Ui.RADIUS_DP)
    private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = Ui.hairline(context)
    }
    private val dotPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = palette.fg }
    private val labelPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 12f)
        color = palette.sub
    }
    private val offlinePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 14f)
        color = palette.sub
        textAlign = Paint.Align.CENTER
    }

    // 点阵光晕（像素坐标）：亮的位置和亮度都平滑地追手指，不是跳过去
    private var glowX = 0f
    private var glowY = 0f
    private var glowTx = 0f
    private var glowTy = 0f
    private var glow = 0f
    private var glowTarget = 0f
    private var lastDrawAt = 0L
    /** 点按的波纹：位置（像素）和开始时间 */
    private class Ripple(val x: Float, val y: Float, val at: Long)
    private val ripples = ArrayList<Ripple>()

    // 当前手势（坐标单位 dp）
    private var active = false
    private var mode = Mode.IDLE
    private var downTime = 0L
    private var maxFingers = 0
    private var lastX = 0f
    private var lastY = 0f
    private var lastT = 0L
    private var startX = 0f
    private var startY = 0f
    private var maxDist = 0f
    private var speed = 0.0
    private var bufX = 0.0
    private var bufY = 0.0
    private var dragArmed = false
    private var dragging = false
    private var scrollVx = 0.0
    private var scrollVy = 0.0
    private var lastScrollT = 0L
    private var lastTapT = -1_000_000L
    private var lastTapX = 0f
    private var lastTapY = 0f

    // 每帧最多发一次，合并零碎的移动
    private var pendMx = 0.0
    private var pendMy = 0.0
    private var pendT = 0.0
    private var pendSx = 0.0
    private var pendSy = 0.0
    private var frameScheduled = false
    private val choreographer = Choreographer.getInstance()
    private val frame = Choreographer.FrameCallback { frameScheduled = false; flush() }
    private var inertia: Choreographer.FrameCallback? = null

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean {
        val now = e.eventTime
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                stopInertia()
                // 触摸点一到就处理，不等屏幕下一帧再成批送来，延迟更低、采样更密
                if (Build.VERSION.SDK_INT >= 30) requestUnbufferedDispatch(e)
                val (x, y) = centroid(e, -1)
                lastDownY = e.y
                active = true
                mode = Mode.MOVE
                downTime = now
                maxFingers = 1
                lastX = x; lastY = y; lastT = now
                startX = x; startY = y
                maxDist = 0f; speed = 0.0; bufX = 0.0; bufY = 0.0
                if (offline) sink?.offlineTouch()
                // 刚轻点过、又马上在原处按下：这次拖动算按住左键拖拽。
                // 时间和距离卡得紧一点：轻点完接着挪指针时，不容易被误当成拖拽
                dragArmed = !offline && now - lastTapT < 220 && hypot(x - lastTapX, y - lastTapY) < 40f
                dragging = false
                scrollVx = 0.0; scrollVy = 0.0; lastScrollT = 0L
                aimGlow(x, y, jump = glow < 0.03f)
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                maxFingers = maxOf(maxFingers, e.pointerCount)
                mode = Mode.SCROLL
                val (x, y) = centroid(e, -1)
                lastX = x; lastY = y; lastT = now
                startX = x; startY = y
                aimGlow(x, y, jump = false)
            }
            MotionEvent.ACTION_MOVE -> if (active) onMove(e, now)
            MotionEvent.ACTION_POINTER_UP -> {
                // 两指变一指时先不动，免得指针乱跳。
                // 位移起点也换成剩下的手指，否则剩下那根指头会被算成"移动了很远"，双指轻点就不触发右键。
                mode = if (e.pointerCount - 1 > 1) Mode.SCROLL else Mode.IDLE
                val (x, y) = centroid(e, e.actionIndex)
                lastX = x; lastY = y; lastT = now
                startX = x; startY = y
                aimGlow(x, y, jump = false)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (active) onEnd(now, e.actionMasked == MotionEvent.ACTION_CANCEL)
                sink?.touchEnded()
            }
        }
        return true
    }

    private fun onMove(e: MotionEvent, now: Long) {
        val (x, y) = centroid(e, -1)
        aimGlow(x, y, jump = false)
        val dx = (x - lastX).toDouble()
        val dy = (y - lastY).toDouble()
        val dt = (now - lastT).coerceIn(4, 64).toDouble()
        lastX = x; lastY = y; lastT = now
        maxDist = maxOf(maxDist, hypot(x - startX, y - startY))
        if (mode == Mode.MOVE && e.pointerCount == 1) {
            speed = speed * 0.5 + hypot(dx, dy) / dt * 0.5
            val gain = sensitivity * (1 + acceleration * min(speed, 3.0))
            bufX += dx * gain
            bufY += dy * gain
            if (maxDist > 3f) { // 先挪一点点再动指针，轻点时指针就不会抖
                pendMx += bufX; pendMy += bufY; pendT = eventMs(e)
                bufX = 0.0; bufY = 0.0
                if (dragArmed && !dragging) {
                    // 先把挪的这一小段发出去再按下：按下的地方离刚才轻点的地方有几个像素，
                    // 电脑就不会把「轻点 + 按下」当成双击（双击标题栏会最大化窗口）
                    dragging = true
                    flush()
                    sink?.press("left", true)
                    Ui.buzz(this, Ui.Buzz.LONG)
                } else {
                    schedule()
                }
            }
        } else if (mode == Mode.SCROLL && e.pointerCount > 1) {
            val sign = if (naturalScroll) 1 else -1
            val k = 2.5 * scrollSpeed
            val wx = -dx * k * sign
            val wy = dy * k * sign
            pendSx += wx; pendSy += wy
            schedule()
            scrollVx = scrollVx * 0.6 + wx / dt * 0.4
            scrollVy = scrollVy * 0.6 + wy / dt * 0.4
            lastScrollT = now
        }
    }

    private fun onEnd(now: Long, cancelled: Boolean) {
        flush() // 最后一点移动先发出去，再松开拖拽按着的左键
        val still = maxDist < 10f
        val duration = now - downTime
        var tapped = false
        if (dragging) {
            sink?.press("left", false)
        } else if (!cancelled && still) {
            val button = when {
                maxFingers == 1 && duration < 250 -> "left"
                maxFingers == 2 && duration < 350 -> "right"
                maxFingers >= 3 && duration < 400 -> "middle"
                else -> null
            }
            if (button != null && !offline) {
                sink?.click(button)
                Ui.buzz(this, Ui.Buzz.TICK)
                tapped = button == "left"
                ripples += Ripple(startX * density, startY * density, SystemClock.uptimeMillis())
            }
        }
        glowTarget = 0f
        if (tapped) {
            lastTapT = now; lastTapX = startX; lastTapY = startY
        } else {
            lastTapT = -1_000_000L
        }
        if (lastScrollT != 0L && now - lastScrollT < 60) startInertia(scrollVx, scrollVy)
        active = false
        mode = Mode.IDLE
        dragging = false
        invalidate()
    }

    /** 触摸点的时间（ms，带小数）。Android 14 起有纳秒精度 */
    private fun eventMs(e: MotionEvent): Double =
        if (Build.VERSION.SDK_INT >= 34) e.eventTimeNanos / 1e6 else e.eventTime.toDouble()

    private fun centroid(e: MotionEvent, skipIndex: Int): Pair<Float, Float> {
        var x = 0f
        var y = 0f
        var n = 0
        for (i in 0 until e.pointerCount) {
            if (i == skipIndex) continue
            x += e.getX(i); y += e.getY(i); n++
        }
        if (n == 0) return lastX to lastY
        return x / n / density to y / n / density
    }

    private fun schedule() {
        if (!frameScheduled) {
            frameScheduled = true
            choreographer.postFrameCallback(frame)
        }
    }

    private fun flush() {
        if (pendMx != 0.0 || pendMy != 0.0) {
            sink?.move(pendMx, pendMy, pendT)
            pendMx = 0.0; pendMy = 0.0
        }
        // 滚轮按 8 个单位一档发，零头留到下次
        val qx = (pendSx / 8).toInt() * 8
        val qy = (pendSy / 8).toInt() * 8
        if (qx != 0 || qy != 0) {
            sink?.scroll(qx, qy)
            pendSx -= qx; pendSy -= qy
        }
    }

    private fun startInertia(vx0: Double, vy0: Double) {
        if (hypot(vx0, vy0) < 0.25) return
        var vx = vx0
        var vy = vy0
        var last = 0L
        val callback = object : Choreographer.FrameCallback {
            override fun doFrame(frameTimeNanos: Long) {
                val t = frameTimeNanos / 1_000_000
                val dt = if (last == 0L) 16.0 else (t - last).coerceAtMost(40).toDouble()
                last = t
                val k = 0.9965.pow(dt)
                vx *= k; vy *= k
                pendSx += vx * dt; pendSy += vy * dt
                flush()
                if (hypot(vx, vy) > 0.03) choreographer.postFrameCallback(this) else inertia = null
            }
        }
        inertia = callback
        choreographer.postFrameCallback(callback)
    }

    private fun stopInertia() {
        inertia?.let { choreographer.removeFrameCallback(it) }
        inertia = null
    }

    /** 光晕追向手指（坐标单位 dp）。jump = 直接跳过去（手指刚放上来、之前没在亮） */
    private fun aimGlow(xDp: Float, yDp: Float, jump: Boolean) {
        glowTx = xDp * density
        glowTy = yDp * density
        if (jump) {
            glowX = glowTx
            glowY = glowTy
        }
        glowTarget = 1f
        postInvalidateOnAnimation()
    }

    override fun onDraw(canvas: Canvas) {
        val now = SystemClock.uptimeMillis()
        // 按帧间隔算追赶比例，60Hz、120Hz 屏上看起来一样快
        val frames = ((now - lastDrawAt).coerceIn(1, 64)) / 16.7f
        lastDrawAt = now
        val follow = 1f - 0.8f.pow(frames)
        glowX += (glowTx - glowX) * follow
        glowY += (glowTy - glowY) * follow
        glow += (glowTarget - glow) * (1f - 0.86f.pow(frames))
        ripples.removeAll { now - it.at > RIPPLE_MS }

        val w = width.toFloat()
        val h = height.toFloat()
        val dim = offline
        Ui.drawTile(canvas, w, h, radius, if (dim) Ui.blend(palette.tile, palette.bg, 0.55f) else palette.tile, palette.line, 1f, fillPaint, linePaint)
        if (dim) ripples.clear()
        val dotScale = if (dim) 0.4f else 1f

        val step = 11 * density
        val cols = floor(w / step).toInt()
        val rows = floor(h / step).toInt()
        val ox = (w - (cols - 1) * step) / 2
        val oy = (h - (rows - 1) * step) / 2
        val spread = 1300 * density * density
        val ringWidth = 9 * density
        val ringSpeed = 120 * density
        for (i in 0 until cols) for (j in 0 until rows) {
            val x = ox + i * step
            val y = oy + j * step
            val dx = x - glowX
            val dy = y - glowY
            var b = glow * exp(-(dx * dx + dy * dy) / spread)
            for (r in ripples) {
                val p = ((now - r.at) / RIPPLE_MS).coerceIn(0f, 1f)
                val d = hypot(x - r.x, y - r.y)
                b += maxOf(0f, 1 - abs(d - p * ringSpeed) / ringWidth) * (1 - p) * 0.9f
            }
            b = min(1f, b) * dotScale
            dotPaint.alpha = ((0.13f + 0.87f * b) * 255 * dotScale).toInt()
            canvas.drawCircle(x, y, (1.1f + 1.2f * b) * density, dotPaint)
        }
        labelPaint.color = if (dim) palette.mut else palette.sub
        canvas.drawText(label, 13 * density, 10 * density - labelPaint.ascent(), labelPaint)
        if (dim) canvas.drawText("未连接电脑", w / 2, h / 2 - (offlinePaint.ascent() + offlinePaint.descent()) / 2, offlinePaint)

        val settling = abs(glowTarget - glow) > 0.004f || abs(glowTx - glowX) + abs(glowTy - glowY) > 0.5f
        if (settling || ripples.isNotEmpty()) postInvalidateOnAnimation()
    }

    private companion object {
        const val RIPPLE_MS = 800f
    }
}
