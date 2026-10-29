package dev.phonepad

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Rect
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import kotlin.math.roundToInt

/**
 * 按网格摆放子视图：每个子视图占 (x, y, w, h) 个格子（可以是小数，比如 1.25 格），
 * 格子大小随可用空间（比如键盘弹出后）自动变化。
 * 调整布局时（editor 不为空）拦下所有触摸交给 LayoutEditor，并在模块下面、上面画网格点和边框抓手。
 */
class GridLayoutView(context: Context) : ViewGroup(context) {

    /** 一个模块的位置和大小，单位是格；判断重叠的规则和电脑端 geometry.rs 一样 */
    data class Cell(val x: Double, val y: Double, val w: Double, val h: Double) {
        val right get() = x + w
        val bottom get() = y + h

        /** 和另一块有没有重叠（只碰到边不算） */
        fun overlaps(o: Cell) = x < o.right - EPS && o.x < right - EPS && y < o.bottom - EPS && o.y < bottom - EPS

        /** 和另一块几乎一样（差不到 EPS） */
        fun near(o: Cell) = Math.abs(x - o.x) < EPS && Math.abs(y - o.y) < EPS && Math.abs(w - o.w) < EPS && Math.abs(h - o.h) < EPS

        /** 四个数取到 0.01 格 */
        fun tidy() = Cell(fine(x), fine(y), fine(w), fine(h))

        companion object {
            const val EPS = 0.004
            fun fine(v: Double) = Math.round(v * 100.0) / 100.0
        }
    }

    var columns = 4
    var rows = 8
    /** 每个模块四周各留这么多，相邻模块之间就是两倍 */
    val gap = (4 * resources.displayMetrics.density).toInt()

    /** 正在调整布局：不为空时子视图收不到任何触摸 */
    var editor: LayoutEditor? = null
        set(value) {
            field = value
            setWillNotDraw(value == null)
            invalidate()
        }

    fun add(view: View, id: String, cell: Cell) {
        view.setTag(R.id.grid_cell, cell)
        view.setTag(R.id.module_id, id)
        addView(view)
    }

    fun cellOf(child: View) = child.getTag(R.id.grid_cell) as? Cell ?: Cell(0.0, 0.0, 1.0, 1.0)

    fun idOf(child: View) = child.getTag(R.id.module_id) as? String ?: ""

    /** 换一个模块的位置、大小（调整布局时用），马上重新排 */
    fun setCell(child: View, cell: Cell) {
        child.setTag(R.id.grid_cell, cell)
        requestLayout()
    }

    val cellWidth get() = width.toFloat() / columns.coerceAtLeast(1)
    val cellHeight get() = height.toFloat() / rows.coerceAtLeast(1)

    /** 格子换算成像素（四周已经留出 gap）；相邻模块的边按同样方式取整，间距处处一样 */
    fun frameOf(c: Cell, out: Rect, totalW: Int = width, totalH: Int = height) {
        val cw = totalW.toDouble() / columns.coerceAtLeast(1)
        val ch = totalH.toDouble() / rows.coerceAtLeast(1)
        out.set(
            (c.x * cw).roundToInt() + gap,
            (c.y * ch).roundToInt() + gap,
            (c.right * cw).roundToInt() - gap,
            (c.bottom * ch).roundToInt() - gap,
        )
    }

    /** 换了布局：模块从下往上依次浮现 */
    fun animateIn() {
        val shift = 14 * resources.displayMetrics.density
        for (i in 0 until childCount) {
            getChildAt(i).apply {
                alpha = 0f
                translationY = shift
                scaleX = 0.92f
                scaleY = 0.92f
                animate().alpha(1f).translationY(0f).scaleX(1f).scaleY(1f)
                    .setStartDelay(i * 34L).setDuration(600).setInterpolator(Ui.EASE)
                    .withEndAction { animate().setStartDelay(0) }
                    .start()
            }
        }
    }

    private val frame = Rect()

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val width = MeasureSpec.getSize(widthMeasureSpec)
        val height = MeasureSpec.getSize(heightMeasureSpec)
        for (i in 0 until childCount) {
            val child = getChildAt(i)
            frameOf(cellOf(child), frame, width, height)
            child.measure(
                MeasureSpec.makeMeasureSpec(frame.width().coerceAtLeast(0), MeasureSpec.EXACTLY),
                MeasureSpec.makeMeasureSpec(frame.height().coerceAtLeast(0), MeasureSpec.EXACTLY),
            )
        }
        setMeasuredDimension(width, height)
    }

    override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
        for (i in 0 until childCount) {
            val child = getChildAt(i)
            frameOf(cellOf(child), frame)
            child.layout(frame.left, frame.top, frame.left + frame.width().coerceAtLeast(0), frame.top + frame.height().coerceAtLeast(0))
        }
    }

    override fun onInterceptTouchEvent(e: MotionEvent) = editor != null || super.onInterceptTouchEvent(e)

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean = editor?.onTouch(e) ?: super.onTouchEvent(e)

    override fun dispatchDraw(canvas: Canvas) {
        editor?.drawUnder(canvas)
        super.dispatchDraw(canvas)
        editor?.drawOver(canvas)
    }
}
