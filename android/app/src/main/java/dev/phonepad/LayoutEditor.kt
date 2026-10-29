package dev.phonepad

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.graphics.Canvas
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import dev.phonepad.GridLayoutView.Cell
import java.util.Locale
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min

/**
 * 手机上直接调布局（⋯ 菜单 →「调整布局」）。
 *
 * 每个模块显示虚线边框；点一下选中，选中的模块才有抓手：拖四条边、四个角改大小。拖任何模块的中间都是移动（顺便选中它）。
 * 只给选中的画抓手：相邻模块的抓手不会挤成一串，几个键交汇处也不会抓错。点「完成」后由 MainActivity 发给电脑保存。
 * 选中的模块那一段不让系统当返回手势（系统每条边最多让出 200dp，所以只护住选中的那块，没选中时护整个网格）。
 * 规则和电脑端一样（pc/config/src/geometry.rs）：默认对齐 1/4 格（顶栏可以关掉），最小 0.5 格，
 * 模块之间不能重叠、不能出界；改大小碰到别的模块就停在旁边，移动到放不下的地方会在附近找空位，找不到就弹回去。
 * 调整时网格把触摸全拦下来，按键、触控板、语音都收不到，不会往电脑发任何东西。
 */
class LayoutEditor(
    private val grid: GridLayoutView,
    private val snapStep: Double,
    private val minSize: Double,
    private val listener: Listener,
) {
    interface Listener {
        /** 和进来的时候比，改没改过 */
        fun onEditChanged(changed: Boolean)

        /** 放不下之类的提示 */
        fun onEditNotice(text: String)

        /** 选中的模块变了（null = 没选中） */
        fun onEditSelect(id: String?)
    }

    private class Item(val id: String, val view: View, val original: Cell)

    private val items: List<Item> = (0 until grid.childCount).map { i ->
        val view = grid.getChildAt(i)
        Item(grid.idOf(view), view, grid.cellOf(view))
    }

    /** 拖动时对齐到 1/4 格；关掉就随意调（取到 0.01 格） */
    var snap = true
        set(value) {
            field = value
            grid.invalidate()
        }

    private val context = grid.context
    private val palette = Ui.palette(context)
    private val density = context.resources.displayMetrics.density
    private val slop = ViewConfiguration.get(context).scaledTouchSlop.toFloat()
    private val radius = Ui.dp(context, Ui.RADIUS_DP)

    private val dotPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val veilPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val dashed = DashPathEffect(floatArrayOf(5 * density, 4 * density), 0f)
    private val handleFill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val handleLine = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 1.6f * density
    }
    private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val pillText = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 12f)
        textAlign = Paint.Align.CENTER
    }

    /** 边框、抓手、点阵整体的透明度：进出调整时淡入淡出 */
    private var chrome = 0f
    private var chromeAnim: ValueAnimator? = null
    /** 正在退出：退出那一刻各模块的位置（网格马上会按新配置重建，淡出时画这份） */
    private var leaving: List<RectF>? = null

    // 当前手势
    private var active: Item? = null
    private var selected: Item? = null
    private var edges = 0
    private var dragging = false
    private var downX = 0f
    private var downY = 0f
    private var start = Cell(0.0, 0.0, 1.0, 1.0)
    /** 拖动中松手后会在哪；移动时 targetOk = false 表示放不下 */
    private var target: Cell? = null
    private var targetOk = true

    private val frame = Rect()
    private val box = RectF()
    private val pill = RectF()

    fun start() = animateChrome(1f, null)

    /** 退出调整：边框抓手淡出，淡完把自己从网格上摘掉 */
    fun stop() {
        leaving = items.map { visualFrame(it) }
        active = null
        animateChrome(0f) { if (grid.editor === this) grid.editor = null }
    }

    fun selectedId(): String? = selected?.id

    private fun select(item: Item?) {
        if (selected === item) return
        selected = item
        listener.onEditSelect(item?.id)
        grid.invalidate()
    }

    /**
     * 不让系统当返回手势的范围（网格坐标），MainActivity 设到根布局上。系统每条边最多让出 200dp（从下往上算），
     * 所以选中了模块就只护住它中间那一段（左右两条边的抓手在中间）；没选中返回 null = 护整个网格。
     */
    fun guardRect(): Rect? {
        val item = selected ?: return null
        val f = visualFrame(item)
        val reach = (16 * density).toInt()
        val half = min(f.height().toInt() + 2 * reach, (200 * density).toInt()) / 2
        val cy = f.centerY().toInt()
        return Rect(f.left.toInt() - reach, cy - half, f.right.toInt() + reach, cy + half)
    }

    /** 取消：都放回进来时的位置 */
    fun revert() = items.forEach { grid.setCell(it.view, it.original) }

    fun changed() = items.any { !grid.cellOf(it.view).near(it.original) }

    /** 发给电脑的结果：每个模块的 id 和新位置 */
    fun result(): List<Pair<String, Cell>> = items.map { it.id to grid.cellOf(it.view).tidy() }

    // ───────── 触摸 ─────────

    fun onTouch(e: MotionEvent): Boolean {
        if (leaving != null) return true
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                val hit = hitTest(e.x, e.y)
                active = hit?.first
                edges = hit?.second ?: 0
                dragging = false
                downX = e.x
                downY = e.y
                select(hit?.first) // 点空白处 = 取消选中
                hit?.first?.let { item -> start = grid.cellOf(item.view) }
            }
            MotionEvent.ACTION_MOVE -> {
                val item = active ?: return true
                if (!dragging) {
                    if (hypot(e.x - downX, e.y - downY) < slop) return true
                    dragging = true
                    beginDrag(item)
                }
                dragTo(item, e.x, e.y)
            }
            MotionEvent.ACTION_UP -> endDrag(commit = true)
            MotionEvent.ACTION_CANCEL -> endDrag(commit = false)
        }
        return true
    }

    private fun beginDrag(item: Item) {
        Ui.buzz(grid, Ui.Buzz.LONG)
        target = start
        targetOk = true
        if (edges == 0) {
            // 整体移动：拿起来（放大一点、压在别的模块上面），跟着手指走
            grid.bringChildToFront(item.view)
            item.view.animate().scaleX(1.04f).scaleY(1.04f).setStartDelay(0).setDuration(160).setInterpolator(Ui.EASE).start()
        }
    }

    private fun dragTo(item: Item, x: Float, y: Float) {
        val dx = ((x - downX) / grid.cellWidth).toDouble()
        val dy = ((y - downY) / grid.cellHeight).toDouble()
        val others = items.filter { it !== item }.map { grid.cellOf(it.view) }
        val (cell, ok) = drag(start, edges, dx, dy, if (snap) snapStep else null, minSize, others, grid.columns.toDouble(), grid.rows.toDouble())
        val previous = target
        target = cell
        targetOk = ok
        if (edges == 0) {
            item.view.translationX = x - downX
            item.view.translationY = y - downY
        } else if (!cell.near(grid.cellOf(item.view))) {
            grid.setCell(item.view, cell) // 改大小：模块本身马上按新大小重排，看得见改完的样子
        }
        // 对齐时每跳一格轻轻震一下
        if (snap && previous != null && !previous.near(cell)) Ui.buzz(grid, Ui.Buzz.TICK)
        grid.invalidate()
    }

    private fun endDrag(commit: Boolean) {
        val item = active
        active = null
        if (item == null || !dragging) {
            grid.invalidate()
            return
        }
        dragging = false
        val cell = target
        target = null
        if (edges == 0) {
            if (commit && targetOk && cell != null) {
                // 记下现在看到的位置，换到新格子后用位移补上再弹回去：看起来是从手指下面滑进新位置
                val fromX = item.view.left + item.view.translationX
                val fromY = item.view.top + item.view.translationY
                grid.setCell(item.view, cell)
                grid.frameOf(cell, frame)
                item.view.translationX = fromX - frame.left
                item.view.translationY = fromY - frame.top
            } else if (commit) {
                listener.onEditNotice("那里放不下：会和别的模块重叠")
                Ui.buzz(grid, Ui.Buzz.REJECT)
            }
            item.view.animate().translationX(0f).translationY(0f).scaleX(1f).scaleY(1f)
                .setStartDelay(0).setDuration(450).setInterpolator(Ui.SPRING)
                .setUpdateListener { grid.invalidate() } // 边框抓手是网格画的，跟着一起动
                .withEndAction { item.view.animate().setUpdateListener(null) }
                .start()
        } else if (!commit) {
            grid.setCell(item.view, start)
        }
        listener.onEditChanged(changed())
        grid.invalidate()
    }

    /**
     * 手指按在哪个模块的哪儿。选中的模块：角、边（改大小，边框外半个抓手也算）还是中间（移动）；
     * 别的模块只能按在里面（或紧挨着的间隙），算移动、顺便选中。返回 null = 按在空白处
     */
    private fun hitTest(x: Float, y: Float): Pair<Item, Int>? {
        selected?.let { item ->
            val f = visualFrame(item)
            val reach = 14 * density
            if (x >= f.left - reach && x <= f.right + reach && y >= f.top - reach && y <= f.bottom + reach) {
                // 小模块的抓手区也小一点，中间总留得出能拖着移动的地方
                val band = min(16 * density, min(f.width(), f.height()) / 4f)
                val corner = min(26 * density, min(f.width(), f.height()) / 3f)
                val side = if (x < f.centerX()) LEFT else RIGHT
                val end = if (y < f.centerY()) TOP else BOTTOM
                val inCornerX = x < f.left + corner || x > f.right - corner
                val inCornerY = y < f.top + corner || y > f.bottom - corner
                val e = when {
                    inCornerX && inCornerY -> side or end
                    x < f.left + band || x > f.right - band -> side
                    y < f.top + band || y > f.bottom - band -> end
                    else -> 0
                }
                return item to e
            }
        }
        var best: Item? = null
        var bestDistance = 6 * density
        for (item in items) {
            val f = visualFrame(item)
            val distance = hypot(max(0f, max(f.left - x, x - f.right)), max(0f, max(f.top - y, y - f.bottom)))
            if (distance < bestDistance || (distance == 0f && best == null)) {
                best = item
                bestDistance = distance
            }
        }
        return best?.let { it to 0 }
    }

    /** 模块现在在屏幕上的位置（含拖动时的位移） */
    private fun visualFrame(item: Item) = RectF(
        item.view.left + item.view.translationX,
        item.view.top + item.view.translationY,
        item.view.right + item.view.translationX,
        item.view.bottom + item.view.translationY,
    )

    // ───────── 画 ─────────

    /** 模块下面：1/4 格的点阵（整格的点大一点），看得出会对齐到哪 */
    fun drawUnder(canvas: Canvas) {
        val a = chrome
        if (a <= 0f) return
        val cw = grid.cellWidth / 4
        val ch = grid.cellHeight / 4
        dotPaint.color = palette.mut
        for (i in 0..grid.columns * 4) for (j in 0..grid.rows * 4) {
            val major = i % 4 == 0 && j % 4 == 0
            if (!snap && !major) continue
            dotPaint.alpha = ((if (major) 0.6f else 0.3f) * a * 255).toInt()
            canvas.drawCircle(i * cw, j * ch, (if (major) 1.6f else 0.9f) * density, dotPaint)
        }
    }

    /** 模块上面：一层底色（看得出这时按了没用）、边框和抓手；拖动时松手后的位置和尺寸 */
    fun drawOver(canvas: Canvas) {
        val a = chrome
        if (a <= 0f) return
        leaving?.let { frames ->
            frames.forEach { drawChrome(canvas, it, hot = false, a = a) }
            return
        }
        val hot = active ?: selected
        for (item in items) if (item !== hot) drawItem(canvas, item, false, a)
        hot?.let { drawItem(canvas, it, true, a) }
        val t = target
        if (dragging && t != null) {
            grid.frameOf(t, frame)
            box.set(frame)
            if (edges == 0) {
                // 移动：松手后的位置画成虚线框，放不下是红色
                val color = if (targetOk) palette.fg else palette.red
                fillPaint.color = color
                fillPaint.alpha = (0.07f * a * 255).toInt()
                canvas.drawRoundRect(box, radius, radius, fillPaint)
                linePaint.color = color
                linePaint.alpha = (a * 255).toInt()
                linePaint.strokeWidth = 2 * density
                linePaint.pathEffect = dashed
                box.inset(density, density)
                canvas.drawRoundRect(box, radius - density, radius - density, linePaint)
                box.set(frame)
            }
            val text = when {
                edges != 0 -> "宽 ${show(t.w)} × 高 ${show(t.h)}"
                targetOk -> "左 ${show(t.x)} · 上 ${show(t.y)}"
                else -> "放不下"
            }
            drawPill(canvas, box, text, targetOk || edges != 0, a)
        }
    }

    private fun drawItem(canvas: Canvas, item: Item, hot: Boolean, a: Float) {
        val f = visualFrame(item)
        if (!hot) {
            veilPaint.color = palette.bg
            veilPaint.alpha = (0.42f * a * 255).toInt()
            canvas.drawRoundRect(f, radius, radius, veilPaint)
        }
        drawChrome(canvas, f, hot, a)
    }

    /** 边框（平时虚线，选中的实线加粗）；选中的才画八个抓手（角上圆点，边中间短条） */
    private fun drawChrome(canvas: Canvas, f: RectF, hot: Boolean, a: Float) {
        val alpha = (a * 255).toInt()
        linePaint.pathEffect = if (hot) null else dashed
        linePaint.strokeWidth = (if (hot) 2f else 1.2f) * density
        linePaint.color = if (hot) palette.fg else palette.mut
        linePaint.alpha = alpha
        val i = linePaint.strokeWidth / 2
        box.set(f.left + i, f.top + i, f.right - i, f.bottom - i)
        canvas.drawRoundRect(box, radius - i, radius - i, linePaint)
        if (!hot) return

        handleFill.color = palette.tile
        handleFill.alpha = alpha
        handleLine.color = palette.fg
        handleLine.alpha = alpha
        val r = 5.5f * density
        for (cx in floatArrayOf(f.left, f.right)) for (cy in floatArrayOf(f.top, f.bottom)) {
            canvas.drawCircle(cx, cy, r, handleFill)
            canvas.drawCircle(cx, cy, r, handleLine)
        }
        val long = 18f * density
        val thick = 6f * density
        bar(canvas, f.centerX(), f.top, long, thick)
        bar(canvas, f.centerX(), f.bottom, long, thick)
        bar(canvas, f.left, f.centerY(), thick, long)
        bar(canvas, f.right, f.centerY(), thick, long)
    }

    private fun bar(canvas: Canvas, cx: Float, cy: Float, w: Float, h: Float) {
        pill.set(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)
        val r = min(w, h) / 2
        canvas.drawRoundRect(pill, r, r, handleFill)
        canvas.drawRoundRect(pill, r, r, handleLine)
    }

    /** 拖动时的小黑条：放在松手位置的上方，放不下就放下方 */
    private fun drawPill(canvas: Canvas, f: RectF, text: String, ok: Boolean, a: Float) {
        val w = pillText.measureText(text) + 20 * density
        val h = 26 * density
        val margin = 8 * density
        var top = f.top - h - margin
        if (top < 2 * density) top = f.bottom + margin
        if (top + h > grid.height - 2 * density) top = f.top + margin
        val left = (f.centerX() - w / 2).coerceIn(2 * density, max(2 * density, grid.width - w - 2 * density))
        pill.set(left, top, left + w, top + h)
        fillPaint.color = if (ok) palette.fg else palette.red
        fillPaint.alpha = (a * 255).toInt()
        canvas.drawRoundRect(pill, h / 2, h / 2, fillPaint)
        pillText.color = palette.tile
        pillText.alpha = (a * 255).toInt()
        canvas.drawText(text, pill.centerX(), pill.centerY() - (pillText.ascent() + pillText.descent()) / 2, pillText)
    }

    private fun animateChrome(to: Float, end: (() -> Unit)?) {
        chromeAnim?.cancel()
        chromeAnim = ValueAnimator.ofFloat(chrome, to).apply {
            duration = if (to > 0f) 260 else 180
            interpolator = Ui.EASE
            addUpdateListener {
                chrome = it.animatedValue as Float
                grid.invalidate()
            }
            if (end != null) {
                addListener(object : AnimatorListenerAdapter() {
                    override fun onAnimationEnd(animation: Animator) = end()
                })
            }
            start()
        }
    }

    companion object {
        /** 要动的边（两条边组合起来就是角）；0 = 整体移动。和电脑端 geometry.rs 一样 */
        const val LEFT = 1
        const val RIGHT = 2
        const val TOP = 4
        const val BOTTOM = 8
        private const val EPS = Cell.EPS
        /** 移动到放不下的地方时，在这么近（格）的范围里找最近的空位 */
        private const val NUDGE = 0.6

        /** 界面上显示的数：最多两位小数，去掉末尾的 0 */
        fun show(v: Double): String {
            val text = String.format(Locale.US, "%.2f", Cell.fine(v)).trimEnd('0').trimEnd('.')
            return if (text == "-0") "0" else text
        }

        private fun snapTo(v: Double, step: Double?) = if (step != null && step > 0) Math.round(v / step) * step else Cell.fine(v)

        private fun isFree(c: Cell, others: List<Cell>) = others.none { c.overlaps(it) }

        /**
         * 拖动中：模块原来在 start，手指移动了 (dx, dy) 格，它现在应该在哪。和电脑端 geometry::drag 一样：
         * 改大小时动的边对齐后不出网格、不小于 minSize，碰到别的模块就停在旁边（总能放下）；
         * 移动时对齐后不出网格，放不下就在附近找最近的空位，找不到返回 false。
         */
        fun drag(start: Cell, edges: Int, dx: Double, dy: Double, step: Double?, minSize: Double, others: List<Cell>, columns: Double, rows: Double): Pair<Cell, Boolean> {
            if (edges == 0) {
                val want = Cell(
                    snapTo(start.x + dx, step).coerceAtMost(columns - start.w).coerceAtLeast(0.0),
                    snapTo(start.y + dy, step).coerceAtMost(rows - start.h).coerceAtLeast(0.0),
                    start.w,
                    start.h,
                )
                if (isFree(want, others)) return want.tidy() to true
                // 候选位置：紧贴别的模块的四条边、贴着网格边
                val xs = mutableListOf(want.x, 0.0, columns - start.w)
                val ys = mutableListOf(want.y, 0.0, rows - start.h)
                for (o in others) {
                    xs += o.x - start.w
                    xs += o.right
                    ys += o.y - start.h
                    ys += o.bottom
                }
                var best: Cell? = null
                var bestDistance = Double.MAX_VALUE
                for (x in xs) {
                    if (x < -EPS || x + start.w > columns + EPS) continue
                    for (y in ys) {
                        if (y < -EPS || y + start.h > rows + EPS) continue
                        val distance = hypot(x - want.x, y - want.y)
                        val candidate = Cell(x, y, start.w, start.h)
                        if (distance <= NUDGE && distance < bestDistance && isFree(candidate, others)) {
                            best = candidate
                            bestDistance = distance
                        }
                    }
                }
                return best?.let { it.tidy() to true } ?: (want.tidy() to false)
            }

            var l = start.x
            var r = start.right
            var t = start.y
            var b = start.bottom
            if ((edges and LEFT) != 0) l = snapTo(start.x + dx, step).coerceAtLeast(0.0).coerceAtMost(r - minSize)
            if ((edges and RIGHT) != 0) r = snapTo(start.right + dx, step).coerceAtMost(columns).coerceAtLeast(l + minSize)
            if ((edges and TOP) != 0) t = snapTo(start.y + dy, step).coerceAtLeast(0.0).coerceAtMost(b - minSize)
            if ((edges and BOTTOM) != 0) b = snapTo(start.bottom + dy, step).coerceAtMost(rows).coerceAtLeast(t + minSize)

            // 左右两条边：碰到「和 [t0, b0) 竖着有重叠」、原来就在这一侧的模块就停下
            fun clampH(l0: Double, r0: Double, t0: Double, b0: Double): Pair<Double, Double> {
                var nl = l0
                var nr = r0
                for (o in others) {
                    if (!(o.y < b0 - EPS && t0 < o.bottom - EPS)) continue
                    if ((edges and RIGHT) != 0 && o.x >= start.right - EPS) nr = min(nr, o.x)
                    if ((edges and LEFT) != 0 && o.right <= start.x + EPS) nl = max(nl, o.right)
                }
                return nl to nr
            }

            // 上下两条边同理
            fun clampV(l0: Double, r0: Double, t0: Double, b0: Double): Pair<Double, Double> {
                var nt = t0
                var nb = b0
                for (o in others) {
                    if (!(o.x < r0 - EPS && l0 < o.right - EPS)) continue
                    if ((edges and BOTTOM) != 0 && o.y >= start.bottom - EPS) nb = min(nb, o.y)
                    if ((edges and TOP) != 0 && o.bottom <= start.y + EPS) nt = max(nt, o.bottom)
                }
                return nt to nb
            }

            // 拖角时两个方向都要动：先横后竖、先竖后横各算一次，取面积大的
            val (l1, r1) = clampH(l, r, start.y, start.bottom)
            val (t1, b1) = clampV(l1, r1, t, b)
            val a = Cell(l1, t1, r1 - l1, b1 - t1)
            val (t2, b2) = clampV(start.x, start.right, t, b)
            val (l2, r2) = clampH(l, r, t2, b2)
            val c = Cell(l2, t2, r2 - l2, b2 - t2)
            return (if (c.w * c.h > a.w * a.h + EPS) c else a).tidy() to true
        }
    }
}
