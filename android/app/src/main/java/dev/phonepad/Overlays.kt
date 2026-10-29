package dev.phonepad

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.ViewTreeObserver
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import kotlin.math.min

/**
 * 自绘的浮层，和主界面同一套黑白圆角，不用系统的对话框、菜单、Toast：
 * - 底部面板：⋯ 菜单、选择电脑、没发出去的话
 * - 对话框：确认、说明为什么要权限
 * - 整屏：使用教程（Tutorial.kt）
 * - 提示卡片：顶栏一行放不下的话（同一时间只有一张，新的顶掉旧的）
 * - 屏幕中间的大字：切换了方案
 * 都挂在 MainActivity 根布局最上面的 host 里。面板、对话框可以叠（教程上面再开选择电脑），返回键先关最上面的。
 */
class Overlays(private val host: FrameLayout) {
    private val context: Context = host.context
    val palette = Ui.palette(context)
    private val density = context.resources.displayMetrics.density
    private fun dp(v: Float) = (v * density).toInt()
    private fun dpf(v: Float) = v * density

    /** 系统栏占的位置，MainActivity 收到 insets 时更新 */
    var insetTop = 0
    var insetBottom = 0
    /** 提示卡片离屏幕顶的距离（顶栏下面） */
    var noticeTop = 0

    private val modals = ArrayList<Modal>()
    val isOpen get() = modals.isNotEmpty()
    fun top(): Modal? = modals.lastOrNull()

    /** 返回键：关掉最上面那个（不让关的交给它的 onBack）。没有开着的返回 false */
    fun back(): Boolean {
        val m = modals.lastOrNull() ?: return false
        if (m.cancellable) m.dismiss() else m.onBack?.invoke()
        return true
    }

    enum class Style { PRIMARY, SECONDARY, DANGER }

    class Btn(val text: String, val style: Style = Style.SECONDARY, val enabled: Boolean = true, val onClick: () -> Unit = {})

    // ───────── 浮层的共同部分：半透明遮罩 + 内容，进出带动画 ─────────

    abstract inner class Modal {
        protected val scrim = View(context)
        protected lateinit var panel: View
        /** 点遮罩、按返回能不能关 */
        var cancellable = true
        /** 不让关的时候按返回键做这个 */
        var onBack: (() -> Unit)? = null
        var onDismiss: (() -> Unit)? = null
        var closed = false
            private set

        protected fun attach(params: FrameLayout.LayoutParams, withScrim: Boolean = true) {
            if (withScrim) {
                scrim.setBackgroundColor(palette.scrim)
                scrim.alpha = 0f
                scrim.isClickable = true
                scrim.setOnClickListener { if (cancellable) dismiss() }
                host.addView(scrim, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
                scrim.animate().alpha(1f).setDuration(200).setInterpolator(Ui.EASE).start()
            }
            host.addView(panel, params)
            modals += this
            spot?.bringToFront() // 方案大字、提示卡片总在最上面
            noticeView?.bringToFront()
            beforeFirstDraw(panel) { animateIn() }
        }

        protected abstract fun animateIn()
        protected abstract fun animateOut(end: () -> Unit)

        fun dismiss(animate: Boolean = true) {
            if (closed) return
            closed = true
            modals.remove(this)
            if (animate) {
                scrim.animate().alpha(0f).setDuration(180).setInterpolator(Ui.EASE).withEndAction { host.removeView(scrim) }.start()
                animateOut { host.removeView(panel) }
            } else {
                host.removeView(scrim)
                host.removeView(panel)
            }
            onDismiss?.invoke()
        }
    }

    /** 第一次要画之前（这时已经量好大小）做 block，用来从「藏起来」的位置开始动画，不闪一帧 */
    private fun beforeFirstDraw(v: View, block: () -> Unit) {
        v.viewTreeObserver.addOnPreDrawListener(object : ViewTreeObserver.OnPreDrawListener {
            override fun onPreDraw(): Boolean {
                v.viewTreeObserver.removeOnPreDrawListener(this)
                block()
                return true
            }
        })
    }

    // ───────── 底部面板 ─────────

    /** 底部面板。build 里用 header / row / toggle / radio 等往里加东西；内容变了调 refresh() 原地重建 */
    fun sheet(build: Sheet.() -> Unit): Sheet = Sheet(build)

    inner class Sheet(private val build: Sheet.() -> Unit) : Modal() {
        val body = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }

        init {
            val scroll = object : ScrollView(context) {
                override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
                    val max = ((host.height.takeIf { it > 0 } ?: context.resources.displayMetrics.heightPixels) * 0.88f).toInt() - insetTop
                    super.onMeasure(widthMeasureSpec, MeasureSpec.makeMeasureSpec(max.coerceAtLeast(dp(220f)), MeasureSpec.AT_MOST))
                }
            }.apply {
                isVerticalScrollBarEnabled = false
                overScrollMode = View.OVER_SCROLL_NEVER
                addView(body)
            }
            val r = dpf(28f)
            panel = LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                background = GradientDrawable().apply {
                    cornerRadii = floatArrayOf(r, r, r, r, 0f, 0f, 0f, 0f)
                    setColor(palette.tile)
                }
                setPadding(0, dp(10f), 0, insetBottom + dp(10f))
                isClickable = true // 点面板的空白处不关
                addView(View(context).apply {
                    background = Ui.round(palette.line, dpf(2f))
                }, LinearLayout.LayoutParams(dp(36f), dp(4f)).apply {
                    gravity = Gravity.CENTER_HORIZONTAL
                    bottomMargin = dp(6f)
                })
                addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
            }
            build()
            attach(FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT, Gravity.BOTTOM))
        }

        fun refresh() {
            if (closed) return
            body.removeAllViews()
            build()
        }

        override fun animateIn() {
            panel.translationY = panel.height.toFloat()
            panel.animate().translationY(0f).setDuration(300).setInterpolator(Ui.EASE).start()
        }

        override fun animateOut(end: () -> Unit) {
            panel.animate().translationY(panel.height.toFloat()).setDuration(220).setInterpolator(Ui.EASE).withEndAction(end).start()
        }

        fun header(title: String, subtitle: String? = null) {
            body.addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(dp(24f), dp(6f), dp(24f), dp(10f))
                addView(text(title, 19f, 900, palette.fg))
                subtitle?.let { addView(text(it, 13f, 650, palette.sub).apply { setPadding(0, dp(4f), 0, 0) }) }
            })
        }

        fun section(title: String) {
            body.addView(text(title, 12.5f, 850, palette.sub).apply { setPadding(dp(24f), dp(14f), dp(24f), dp(4f)) })
        }

        fun divider() {
            body.addView(View(context).apply { setBackgroundColor(palette.line) },
                LinearLayout.LayoutParams(MATCH_PARENT, Ui.hairline(context).toInt()).apply {
                    setMargins(dp(24f), dp(6f), dp(24f), dp(6f))
                })
        }

        /** 普通的一行：点了做 onClick；trailing 是右边的东西（默认一个「›」，null 就不画） */
        fun row(title: String, subtitle: String? = null, enabled: Boolean = true, trailing: View? = chevron(), onClick: (() -> Unit)?): View =
            addRow(rowView(title, subtitle, enabled, leading = null, trailing = trailing, onClick = onClick))

        /** 带开关的一行：点整行就切换 */
        fun toggle(title: String, subtitle: String?, checked: Boolean, enabled: Boolean = true, onChange: (Boolean) -> Unit): Ui.Toggle {
            val t = Ui.Toggle(context, palette).apply { set(checked, animate = false) }
            addRow(rowView(title, subtitle, enabled, leading = null, trailing = t) {
                val now = !t.checked
                t.set(now, animate = true)
                Ui.buzz(t, Ui.Buzz.TAP)
                onChange(now)
            })
            return t
        }

        /** 单选的一行：左边一个圆点 */
        fun radio(title: String, subtitle: String?, checked: Boolean, enabled: Boolean = true, onClick: () -> Unit) {
            val r = Ui.Radio(context, palette).apply { this.checked = checked }
            addRow(rowView(title, subtitle, enabled, leading = r, trailing = null) {
                if (!r.checked) Ui.buzz(r, Ui.Buzz.TAP)
                onClick()
            })
        }

        fun view(v: View, params: LinearLayout.LayoutParams = LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT)) {
            body.addView(v, params)
        }

        fun buttons(vararg list: Btn) {
            body.addView(buttonRow(list.toList()), LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply {
                setMargins(dp(20f), dp(14f), dp(20f), dp(4f))
            })
        }

        private fun addRow(v: View): View {
            body.addView(v, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { setMargins(dp(10f), 0, dp(10f), 0) })
            return v
        }
    }

    // ───────── 对话框 ─────────

    fun dialog(title: String, message: CharSequence?, buttons: List<Btn>, extra: View? = null, cancellable: Boolean = true): Dialog =
        Dialog(title, message, buttons, extra).also { it.cancellable = cancellable }

    inner class Dialog(title: String, message: CharSequence?, buttons: List<Btn>, extra: View?) : Modal() {
        init {
            panel = LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                background = Ui.round(palette.tile, dpf(26f))
                setPadding(dp(22f), dp(22f), dp(22f), dp(18f))
                isClickable = true
                addView(text(title, 18f, 900, palette.fg))
                message?.let {
                    addView(text("", 14f, 600, palette.sub).apply {
                        this.text = it
                        setLineSpacing(dpf(3f), 1f)
                        setPadding(0, dp(8f), 0, 0)
                    })
                }
                extra?.let { addView(it, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(12f) }) }
                addView(buttonRow(buttons.map { b -> Btn(b.text, b.style, b.enabled) { dismiss(); b.onClick() } }),
                    LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(20f) })
            }
            val width = min(context.resources.displayMetrics.widthPixels - dp(44f), dp(360f))
            attach(FrameLayout.LayoutParams(width, WRAP_CONTENT, Gravity.CENTER))
        }

        override fun animateIn() {
            panel.alpha = 0f
            panel.scaleX = 0.94f
            panel.scaleY = 0.94f
            panel.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(200).setInterpolator(Ui.EASE).start()
        }

        override fun animateOut(end: () -> Unit) {
            panel.animate().alpha(0f).scaleX(0.96f).scaleY(0.96f).setDuration(150).setInterpolator(Ui.EASE).withEndAction(end).start()
        }
    }

    // ───────── 整屏（使用教程） ─────────

    /** 盖住整个界面的一屏，内容自己铺满、自己画底色；返回键交给 onBack */
    fun screen(content: View): Screen = Screen(content)

    inner class Screen(content: View) : Modal() {
        init {
            cancellable = false
            panel = content
            attach(FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT), withScrim = false)
        }

        override fun animateIn() {
            panel.alpha = 0f
            panel.animate().alpha(1f).setDuration(260).setInterpolator(Ui.EASE).withLayer().start()
        }

        // 关的时候微微放大着淡出，像是「走进」了主界面
        override fun animateOut(end: () -> Unit) {
            panel.animate().alpha(0f).scaleX(1.04f).scaleY(1.04f).setDuration(280).setInterpolator(Ui.EASE)
                .withLayer().withEndAction(end).start()
        }
    }

    // ───────── 提示卡片 ─────────

    private var noticeView: View? = null
    private var noticeHide: Runnable? = null

    /** 顶栏下面滑出一张黑色卡片（顶掉上一张）；可以带一个按钮。ms = 0 按字数定显示多久 */
    fun notice(message: CharSequence, action: String? = null, ms: Long = 0, onAction: (() -> Unit)? = null) {
        hideNotice(fast = true)
        val card = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.round(palette.fg, dpf(16f))
            setPadding(dp(16f), dp(12f), dp(if (action != null) 8f else 16f), dp(12f))
            addView(text("", 13.5f, 750, palette.tile).apply {
                text = message
                maxLines = 5
                ellipsize = TextUtils.TruncateAt.END
                setLineSpacing(dpf(2f), 1f)
            }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            if (action != null) {
                addView(text(action, 13.5f, 900, palette.fg).apply {
                    gravity = Gravity.CENTER
                    background = Ui.round(palette.tile, dpf(14f))
                    setPadding(dp(12f), dp(6f), dp(12f), dp(6f))
                    Ui.springy(this)
                    setOnClickListener {
                        hideNotice(fast = false)
                        onAction?.invoke()
                    }
                }, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginStart = dp(10f) })
            }
            setOnClickListener { hideNotice(fast = false) }
        }
        host.addView(card, FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT, Gravity.TOP).apply {
            setMargins(dp(12f), noticeTop, dp(12f), 0)
        })
        noticeView = card
        card.alpha = 0f
        card.translationY = -dpf(10f)
        card.animate().alpha(1f).translationY(0f).setDuration(220).setInterpolator(Ui.EASE).start()
        val shown = if (ms > 0) ms else (2400L + message.length * 70L).coerceAtMost(6500L)
        val hide = Runnable { hideNotice(fast = false) }
        noticeHide = hide
        host.postDelayed(hide, shown)
    }

    fun hideNotice(fast: Boolean) {
        noticeHide?.let(host::removeCallbacks)
        noticeHide = null
        val old = noticeView ?: return
        noticeView = null
        old.animate().alpha(0f).translationY(if (fast) 0f else -dpf(8f)).setDuration(if (fast) 100 else 180)
            .setInterpolator(Ui.EASE).withEndAction { host.removeView(old) }.start()
    }

    // ───────── 屏幕中间的大字 ─────────

    private var spot: View? = null

    /** 屏幕中间浮出一块大字（切换了方案），一会儿自己淡掉；不挡触摸 */
    fun spotlight(caption: String, title: String) {
        spot?.let { old -> old.animate().cancel(); host.removeView(old) }
        val maxText = context.resources.displayMetrics.widthPixels - dp(124f)
        val card = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            background = Ui.round(palette.fg, dpf(30f))
            setPadding(dp(30f), dp(20f), dp(30f), dp(24f))
            addView(text(caption, 13f, 850, Ui.alpha(palette.tile, 0.72f)).apply { gravity = Gravity.CENTER })
            addView(text(title, 30f, 900, palette.tile).apply {
                gravity = Gravity.CENTER
                maxLines = 2
                maxWidth = maxText
                ellipsize = TextUtils.TruncateAt.END
                setPadding(0, dp(4f), 0, 0)
            })
        }
        host.addView(card, FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.CENTER))
        spot = card
        card.alpha = 0f
        card.scaleX = 0.86f
        card.scaleY = 0.86f
        card.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(320).setInterpolator(Ui.SOFT_SPRING).start()
        card.postDelayed({
            card.animate().alpha(0f).scaleX(0.96f).scaleY(0.96f).setDuration(260).setInterpolator(Ui.EASE)
                .withEndAction {
                    host.removeView(card)
                    if (spot === card) spot = null
                }.start()
        }, 1300)
    }

    // ───────── 零件 ─────────

    fun text(s: String, size: Float, weight: Int, color: Int) = TextView(context).apply {
        text = s
        textSize = size
        typeface = Ui.font(context, weight)
        setTextColor(color)
        includeFontPadding = false
    }

    /** 一行：左边可选的圆点（单选），中间标题和小字，右边可选的东西（开关、箭头） */
    @SuppressLint("ClickableViewAccessibility")
    fun rowView(title: String, subtitle: String?, enabled: Boolean, leading: View?, trailing: View?, onClick: (() -> Unit)?): View {
        val row = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(54f)
            setPadding(dp(14f), dp(9f), dp(14f), dp(9f))
            alpha = if (enabled) 1f else 0.42f
        }
        leading?.let { row.addView(it, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginEnd = dp(14f) }) }
        val texts = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            addView(text(title, 15.5f, 750, palette.fg))
            subtitle?.let { addView(text(it, 12.5f, 650, palette.sub).apply { setPadding(0, dp(3f), 0, 0); setLineSpacing(dpf(1.5f), 1f) }) }
        }
        row.addView(texts, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
        trailing?.let { row.addView(it, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginStart = dp(12f) }) }
        row.contentDescription = listOfNotNull(title, subtitle).joinToString("，")
        if (enabled && onClick != null) {
            val bg = Ui.round(0, dpf(16f))
            row.background = bg
            row.setOnTouchListener { _, e ->
                when (e.actionMasked) {
                    MotionEvent.ACTION_DOWN -> bg.setColor(palette.bg)
                    MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> row.postDelayed({ bg.setColor(0) }, 110)
                }
                false
            }
            row.setOnClickListener { onClick() }
        }
        return row
    }

    fun buttonRow(buttons: List<Btn>): LinearLayout = LinearLayout(context).apply {
        orientation = LinearLayout.HORIZONTAL
        buttons.forEachIndexed { i, b ->
            addView(button(b), LinearLayout.LayoutParams(0, dp(46f), 1f).apply { if (i > 0) marginStart = dp(10f) })
        }
    }

    fun button(b: Btn): TextView = text(b.text, 14.5f, 850, palette.fg).apply {
        gravity = Gravity.CENTER
        maxLines = 1
        ellipsize = TextUtils.TruncateAt.END
        setPadding(dp(10f), 0, dp(10f), 0)
        val r = dpf(23f)
        when (b.style) {
            Style.PRIMARY -> {
                background = Ui.round(palette.fg, r)
                setTextColor(palette.tile)
            }
            Style.SECONDARY -> background = Ui.round(palette.tile, r, Ui.hairline(context).toInt() + 1, palette.line)
            Style.DANGER -> {
                background = Ui.round(palette.tile, r, Ui.hairline(context).toInt() + 1, palette.line)
                setTextColor(palette.red)
            }
        }
        alpha = if (b.enabled) 1f else 0.4f
        if (b.enabled) {
            Ui.springy(this)
            setOnClickListener { b.onClick() }
        }
    }

    /** 行尾的「›」 */
    fun chevron(): View = object : View(context) {
        private val p = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = dpf(1.8f)
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
            color = palette.mut
        }
        private val path = Path()
        override fun onMeasure(w: Int, h: Int) = setMeasuredDimension(dp(16f), dp(20f))
        override fun onDraw(canvas: Canvas) {
            val cx = width / 2f
            val cy = height / 2f
            val s = dpf(4f)
            path.reset()
            path.moveTo(cx - s / 2, cy - s)
            path.lineTo(cx + s / 2, cy)
            path.lineTo(cx - s / 2, cy + s)
            canvas.drawPath(path, p)
        }
    }

    /** 行尾的对勾（当前选中的电脑） */
    fun check(): View = object : View(context) {
        private val p = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = dpf(2.2f)
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
            color = palette.fg
        }
        private val path = Path()
        override fun onMeasure(w: Int, h: Int) = setMeasuredDimension(dp(22f), dp(22f))
        override fun onDraw(canvas: Canvas) {
            val s = width.toFloat()
            path.reset()
            path.moveTo(s * 0.2f, s * 0.52f)
            path.lineTo(s * 0.42f, s * 0.72f)
            path.lineTo(s * 0.8f, s * 0.3f)
            canvas.drawPath(path, p)
        }
    }

    /** 右边的小字（比如「当前」「没装」） */
    fun tag(s: String): TextView = text(s, 12f, 800, palette.sub)
}
