package dev.suanniao

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PathMeasure
import android.graphics.RectF
import android.graphics.Shader
import android.os.SystemClock
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextUtils
import android.text.style.ForegroundColorSpan
import android.text.style.TypefaceSpan
import android.view.Gravity
import android.view.MotionEvent
import android.view.VelocityTracker
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewTreeObserver
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import dev.suanniao.Overlays.Btn
import dev.suanniao.Overlays.Style
import java.util.Random
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.cos
import kotlin.math.exp
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * 使用教程：整屏，左右滑或点下面的按钮翻页。每页上面一幅会动的小画，下面几句大白话。
 * 页：0 欢迎 · 1 在电脑上打开蒜鸟 · 2 蓝牙配对 · 3 选电脑 · 4 触控板 · 5 按键 · 6 语音 · 7 ⋯ 菜单 · 8 准备好了
 * - 第一次打开时自动出来（firstRun）：连电脑那几步就在这里做，做完的打上勾；「跳过」「开始使用」都进主界面
 * - ⋯ 菜单里能再看：右上角是「关闭」，最后一页是「完成」
 * 小画按 300×240 的设计尺寸画，等比缩放放在正中；关了系统动画时停在有代表性的一帧
 */
class Tutorial(private val host: Setup.Host, private val overlays: Overlays, private val setup: Setup) {
    private val context get() = host.context
    private val palette = overlays.palette
    private val density = context.resources.displayMetrics.density
    private fun dp(v: Float) = (v * density).toInt()
    private fun dpf(v: Float) = v * density
    private val prefs get() = context.getSharedPreferences(LinkService.PREFS, Context.MODE_PRIVATE)

    private var screen: Overlays.Screen? = null

    /** 第一次打开时自动出来的那次（不是从 ⋯ 菜单打开的） */
    var firstRun = false
        private set
    val isOpen get() = screen?.closed == false

    /** 看过（或跳过）了，以后不再自动出来 */
    val seen get() = prefs.getBoolean(KEY_SEEN, false)

    private var current = 0

    /** 翻到过触控板那页：回到主界面就不用再弹手势说明 */
    private var reachedPad = false

    private val birdImage: Bitmap by lazy {
        BitmapFactory.decodeResource(context.resources, R.drawable.tutorial_bird).apply { setHasMipMap(true) }
    }

    private lateinit var root: FrameLayout
    private lateinit var column: LinearLayout
    private lateinit var topBar: FrameLayout
    private lateinit var dots: Dots
    private lateinit var corner: TextView
    private lateinit var pager: Pager
    private lateinit var nav: NavBar
    private val pages = ArrayList<PageView>()

    // 连电脑那几页里跟着状态变的部分
    private lateinit var phoneStep: TextView
    private lateinit var pairHolder: FrameLayout
    private lateinit var pickHolder: FrameLayout
    private lateinit var doneNote: View
    private lateinit var pairScene: PairScene
    private lateinit var pickScene: PickScene
    private var pairKey: String? = null
    private var pickKey: String? = null

    // ───────── 打开、关上 ─────────

    fun show(firstRun: Boolean, page: Int = 0) {
        val target = page.coerceIn(0, LAST)
        if (isOpen) {
            pager.settle(target)
            return
        }
        this.firstRun = firstRun
        current = target
        reachedPad = target >= PAD
        build()
        update(animate = false)
        screen = overlays.screen(root).apply {
            onBack = { back() }
            onDismiss = { closed() }
        }
        pager.jump(target)
        nav.setLabel(labelFor(target), animate = false)
        intro()
    }

    fun close() {
        screen?.dismiss()
        screen = null
    }

    /** 权限、配对、选好的电脑可能变了：连电脑那几页跟着变 */
    fun refresh() {
        if (isOpen) update(animate = true)
    }

    private fun closed() {
        val edit = prefs.edit().putBoolean(KEY_SEEN, true)
        if (reachedPad) edit.putBoolean(KEY_GUIDE_SEEN, true)
        edit.apply()
        screen = null
    }

    /** 返回键：往前翻一页。第一页上：第一次打开时退到后台（还没进过主界面），从菜单打开的就关掉 */
    private fun back() {
        when {
            current > 0 -> pager.settle(current - 1)
            firstRun -> (context as? Activity)?.moveTaskToBack(true)
            else -> close()
        }
    }

    /** 「跳过」「开始使用」「关闭」「完成」 */
    private fun finish() {
        if (firstRun) host.finishOnboarding() else close()
    }

    private fun primary() {
        if (current < LAST) pager.settle(current + 1) else finish()
    }

    private fun labelFor(i: Int) = when {
        i == 0 -> "开始"
        i < LAST -> "下一步"
        firstRun -> "开始使用"
        else -> "完成"
    }

    /** 翻到了第 i 页（翻页动画一开始就算） */
    private fun select(i: Int) {
        if (i == current) return
        current = i
        if (i >= PAD) reachedPad = true
        nav.setLabel(labelFor(i), animate = true)
        Ui.buzz(root, Ui.Buzz.TICK)
    }

    /** 翻页位置变了：圆点、底下按钮、右上角跟着变（pos 是小数，翻到一半时两页各占一半） */
    private fun moved(pos: Float) {
        dots.pos = pos
        nav.t = pos.coerceIn(0f, 1f)
        val a = (LAST - pos).coerceIn(0f, 1f)
        corner.alpha = a
        corner.isEnabled = a > 0.5f
        corner.visibility = if (a > 0f) View.VISIBLE else View.INVISIBLE
    }

    // ───────── 搭界面 ─────────

    private fun build() {
        pages.clear()
        pairKey = null
        pickKey = null
        root = object : FrameLayout(context) {
            // 状态栏、导航条有多高，可能等教程出来以后才知道（刚启动、换深浅色重建界面时）：每次排版完对一下
            private val barsWatch = ViewTreeObserver.OnGlobalLayoutListener { fitBars() }

            override fun onAttachedToWindow() {
                super.onAttachedToWindow()
                viewTreeObserver.addOnGlobalLayoutListener(barsWatch)
            }

            override fun onDetachedFromWindow() {
                viewTreeObserver.removeOnGlobalLayoutListener(barsWatch)
                super.onDetachedFromWindow()
            }

            // 从蓝牙设置、权限框回来时窗口重新拿到焦点：刷新连电脑那几步
            override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
                super.onWindowFocusChanged(hasWindowFocus)
                if (hasWindowFocus) refresh()
            }
        }.apply {
            setBackgroundColor(palette.bg)
            isClickable = true
        }
        column = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        fitBars()
        root.addView(column, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))

        topBar = FrameLayout(context)
        dots = Dots(context)
        topBar.addView(dots, FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.START or Gravity.CENTER_VERTICAL).apply {
            marginStart = dp(28f)
        })
        corner = overlays.text(if (firstRun) "跳过" else "关闭", 14f, 850, palette.sub).apply {
            gravity = Gravity.CENTER
            background = Ui.round(palette.tile, dpf(18f))
            setPadding(dp(16f), dp(8f), dp(16f), dp(8f))
            Ui.springy(this)
            setOnClickListener { finish() }
        }
        topBar.addView(corner, FrameLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT, Gravity.END or Gravity.CENTER_VERTICAL).apply {
            marginEnd = dp(16f)
        })
        column.addView(topBar, LinearLayout.LayoutParams(MATCH_PARENT, dp(56f)))

        pager = Pager(context)
        buildPages()
        pages.forEach { pager.addView(it, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT)) }
        column.addView(pager, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))

        nav = NavBar(context)
        column.addView(nav, LinearLayout.LayoutParams(MATCH_PARENT, dp(54f)).apply { setMargins(dp(20f), dp(8f), dp(20f), dp(16f)) })
    }

    /** 上下让出状态栏和导航条（MainActivity 收到 insets 时记在 overlays 里） */
    private fun fitBars() {
        if (column.paddingTop != overlays.insetTop || column.paddingBottom != overlays.insetBottom) {
            column.setPadding(0, overlays.insetTop, 0, overlays.insetBottom)
        }
    }

    private fun buildPages() {
        page(HelloScene(context, done = false), 0.56f, center = true) {
            title("欢迎使用蒜鸟")
            para("蒜鸟能把这部手机变成电脑的**触控板**、**快捷键**和**语音输入**。手机和电脑用蓝牙连在一起，不用网络，也不用数据线。", 14f)
            para("跟着往下看，一两分钟就能学会。")
        }
        page(PcScene(context), 0.5f) {
            eyebrow("连接电脑 · 第 1 步")
            title("先在电脑上打开蒜鸟")
            para("电脑上也要装好蒜鸟。打开以后，屏幕右下角的托盘里会出现这只小鸟（没看到就点一下托盘的「^」），说明它在等手机来连。")
            para("电脑上的窗口关掉也没关系，托盘里还有小鸟就行。")
        }
        pairScene = PairScene(context)
        page(pairScene, 0.4f) {
            eyebrow("连接电脑 · 第 2 步")
            title("让手机和电脑蓝牙配对")
            para("配对只要做一次，以后不用再配。")
            step(1, "**手机**：打开蓝牙设置页，停在那一页别退出，电脑才找得到这部手机。", 14f)
            phoneStep = step(2, "")
            step(3, "两边会弹出同一串数字，核对一样后两边都点确认（「配对」或「连接」）。")
            pairHolder = holder()
        }
        pickScene = PickScene(context)
        page(pickScene, 0.46f) {
            eyebrow("连接电脑 · 第 3 步")
            title("在手机上选你的电脑")
            para("从配对过的设备里点你的电脑。选好以后，只要两边都开着蒜鸟，就会自动连上。")
            pickHolder = holder()
        }
        val grid = GestureGrid(context)
        page(PadScene(context) { grid.highlight(it) }, 0.44f) {
            eyebrow("怎么用 · 触控板")
            title("手指一滑，鼠标就跟着动")
            para("和笔记本电脑的触控板一样用：")
            extra(grid, 14f)
        }
        page(KeysScene(context), 0.5f) {
            eyebrow("怎么用 · 按键")
            title("一个键，就是一个快捷键")
            para("点「复制」，电脑就按下 **Ctrl + C**。键角写着「长按」「双击」的，长按、双击还能做另一件事；删除这类键按住不放，会一直删。")
            tip("刚装好时手机上只有语音和触控板。想加按键，在电脑上的蒜鸟里点「**添加按键**」，加好会自动出现在手机上。")
        }
        page(VoiceScene(context), 0.5f) {
            eyebrow("怎么用 · 语音输入")
            title("按住说话，字直接打到电脑上")
            para("按住语音键说话，说的话会变成文字，打在电脑上光标所在的地方。")
            para("要说很久，就按住往**上滑**锁定，之后可以松手，说完**点一下**结束。")
            tip("识别在手机上完成，录音不会上传。")
        }
        page(MenuScene(context), 0.52f) {
            eyebrow("怎么用 · 更多")
            title("其他功能都在右上角的「⋯」里")
            para("调整按键的位置和大小、暂停按键、换一台电脑、再看一遍这个教程，都在这里。")
        }
        page(HelloScene(context, done = true), 0.56f, center = true) {
            title("准备好了！")
            para("以后只要电脑上开着蒜鸟，手机打开蒜鸟就会自动连上。", 14f)
            doneNote = tip("还没选电脑也没关系，可以先看看界面，之后在顶上的提示条或 ⋯ 菜单里选。", 18f)
        }
    }

    private fun page(scene: Scene, share: Float, center: Boolean = false, fill: PageView.() -> Unit) {
        pages += PageView(context, scene, share, center).apply(fill)
    }

    /** 刚打开：字一行行浮上来，按钮最后到 */
    private fun intro() {
        if (!ValueAnimator.areAnimatorsEnabled()) return
        val texts = pages[current].texts
        for (i in 0 until texts.childCount) {
            val v = texts.getChildAt(i)
            v.alpha = 0f
            v.translationY = dpf(16f)
            v.animate().alpha(1f).translationY(0f).setStartDelay(120L + 70L * i).setDuration(560).setInterpolator(Ui.EASE).start()
        }
        nav.alpha = 0f
        nav.translationY = dpf(16f)
        nav.animate().alpha(1f).translationY(0f).setStartDelay(300).setDuration(560).setInterpolator(Ui.EASE).start()
        topBar.alpha = 0f
        topBar.animate().alpha(1f).setStartDelay(180).setDuration(500).setInterpolator(Ui.EASE).start()
    }

    // ───────── 连电脑那几步的状态 ─────────

    private fun update(animate: Boolean) {
        val perm = host.hasBluetoothPermission()
        val paired = setup.pairedComputers().size
        val address = host.savedAddress()
        val name = host.savedName()
        val phone = setup.phoneName(perm) ?: "这部手机"
        phoneStep.text = rich("**电脑**：打开「设置 → 蓝牙和其他设备 → 添加设备 → 蓝牙」，在列表里点「**$phone**」。")
        pairScene.phoneName = phone
        pickScene.name = name

        val pairNow = when {
            !perm -> "perm"
            paired == 0 -> "none"
            else -> "done $paired"
        }
        if (pairNow != pairKey) {
            if (animate && pairNow.startsWith("done") && pairKey?.startsWith("done") == false) Ui.buzz(root, Ui.Buzz.CONFIRM)
            swap(pairHolder, pairCard(perm, paired), animate && pairKey != null)
            pairKey = pairNow
        }
        val pickNow = when {
            !perm -> "perm"
            address == null -> "none ${paired == 0}"
            else -> "done $address $name"
        }
        if (pickNow != pickKey) {
            if (animate && pickNow.startsWith("done") && pickKey?.startsWith("done") == false) Ui.buzz(root, Ui.Buzz.CONFIRM)
            swap(pickHolder, pickCard(perm, paired, address, name), animate && pickKey != null)
            pickKey = pickNow
        }
        doneNote.visibility = if (address == null) View.VISIBLE else View.GONE
    }

    /** 换卡片：旧的淡出，新的稍后从下面浮上来 */
    private fun swap(holder: FrameLayout, next: View, animate: Boolean) {
        val old = List(holder.childCount) { holder.getChildAt(it) }
        holder.addView(next, FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        if (!animate || !ValueAnimator.areAnimatorsEnabled()) {
            old.forEach(holder::removeView)
            return
        }
        old.forEach { v ->
            v.animate().alpha(0f).setStartDelay(0).setDuration(140).setInterpolator(Ui.EASE).withEndAction { holder.removeView(v) }.start()
        }
        next.alpha = 0f
        next.translationY = dpf(8f)
        next.animate().alpha(1f).translationY(0f).setStartDelay(90).setDuration(360).setInterpolator(Ui.EASE).start()
    }

    private fun pairCard(perm: Boolean, paired: Int): View = when {
        !perm -> permCard()
        paired == 0 -> action(Btn("打开手机蓝牙设置", Style.PRIMARY) { host.openBluetoothSettings() }, "配好以后回到这里，这一步会打上勾。")
        else -> doneCard("已和 $paired 台电脑配对", "可以下一步了", Btn("再配一台") { host.openBluetoothSettings() })
    }

    private fun pickCard(perm: Boolean, paired: Int, address: String?, name: String?): View = when {
        !perm -> permCard()
        address == null -> action(
            Btn("选择电脑", Style.PRIMARY) { pick() },
            if (paired == 0) "还没配对过电脑的话，先回上一步配对。" else "列表里只有配对过的电脑。",
        )
        else -> doneCard(if (name != null) "已选：$name" else "已选好电脑", "连上后通知栏会显示连接状态", Btn("换一台") { pick() })
    }

    private fun pick() {
        setup.showPicker { address, name ->
            host.chooseComputer(address, name)
            refresh()
        }
    }

    /** 一个大按钮，下面一行小字 */
    private fun action(btn: Btn, note: String) = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        addView(overlays.button(btn), LinearLayout.LayoutParams(MATCH_PARENT, dp(48f)))
        addView(overlays.text(note, 12.5f, 650, palette.sub).apply {
            gravity = Gravity.CENTER_HORIZONTAL
            setLineSpacing(dpf(2f), 1f)
        }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(10f) })
    }

    private fun card() = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        background = Ui.round(palette.tile, dpf(20f), Ui.hairline(context).toInt(), palette.line)
        setPadding(dp(16f), dp(14f), dp(16f), dp(14f))
    }

    /** 要权限前先说清楚拿来干什么 */
    private fun permCard() = card().apply {
        addView(overlays.text("先允许使用蓝牙", 15.5f, 850, palette.fg))
        addView(overlays.text("蒜鸟要用「附近的设备」权限，才能通过蓝牙连上你选的那台电脑；不会扫描、连接别的设备。", 13f, 650, palette.sub).apply {
            setLineSpacing(dpf(3f), 1f)
        }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(6f) })
        addView(overlays.button(Btn("允许使用蓝牙", Style.PRIMARY) { host.askBluetoothPermission() }),
            LinearLayout.LayoutParams(MATCH_PARENT, dp(46f)).apply { topMargin = dp(14f) })
    }

    /** 这一步做完了：勾 + 一句话 + 一个小按钮（再来一次） */
    private fun doneCard(title: String, sub: String, btn: Btn): View = card().apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        addView(DoneBadge(context), LinearLayout.LayoutParams(dp(30f), dp(30f)))
        addView(LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            addView(overlays.text(title, 15.5f, 850, palette.fg).apply {
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.END
            })
            addView(overlays.text(sub, 12.5f, 650, palette.sub).apply { setPadding(0, dp(3f), 0, 0) })
        }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginStart = dp(12f) })
        addView(overlays.button(btn).apply {
            textSize = 13.5f
            setPadding(dp(14f), 0, dp(14f), 0)
        }, LinearLayout.LayoutParams(WRAP_CONTENT, dp(36f)).apply { marginStart = dp(10f) })
    }

    /** **粗体** 的部分用黑字、粗一点 */
    private fun rich(s: String): CharSequence {
        val out = SpannableStringBuilder()
        s.split("**").forEachIndexed { i, part ->
            val start = out.length
            out.append(if (i % 2 == 1) glue(part) else part)
            if (i % 2 == 1) {
                out.setSpan(ForegroundColorSpan(palette.fg), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                out.setSpan(TypefaceSpan(Ui.font(context, 850)), start, out.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        return out
    }

    /** 加粗的词别从中间断行（「语／音输入」）：空格换成不断行的，字和字之间夹上 WORD JOINER。带 emoji 的（手机名）不动 */
    private fun glue(s: String): String {
        if (s.any { it.isSurrogate() || it == '‍' }) return s
        return s.replace(' ', ' ').toList().joinToString("⁠")
    }

    // ───────── 翻页 ─────────

    /** 左右翻页：跟手拖，松手按速度和位置决定翻不翻；两头拉过了有阻尼 */
    @SuppressLint("ViewConstructor")
    private inner class Pager(ctx: Context) : FrameLayout(ctx) {
        /** 现在停在第几页（翻页途中是小数） */
        var pos = 0f
            private set
        private var anim: ValueAnimator? = null
        private val slop = ViewConfiguration.get(ctx).scaledTouchSlop
        private var tracker: VelocityTracker? = null
        private var downX = 0f
        private var downY = 0f
        private var dragging = false
        private var dragX = 0f
        private var dragPos = 0f
        private var dragPage = 0

        init {
            clipChildren = false // 每页的画会画到页外（视差），整个翻页区在 dispatchDraw 里裁
        }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            val h = View.MeasureSpec.getSize(heightMeasureSpec)
            pages.forEach { it.fitScene(h) }
            super.onMeasure(widthMeasureSpec, heightMeasureSpec)
        }

        override fun onLayout(changed: Boolean, left: Int, top: Int, right: Int, bottom: Int) {
            super.onLayout(changed, left, top, right, bottom)
            layoutPages()
        }

        override fun dispatchDraw(canvas: Canvas) {
            canvas.save()
            canvas.clipRect(0, 0, width, height)
            super.dispatchDraw(canvas)
            canvas.restore()
        }

        fun jump(page: Int) {
            anim?.cancel()
            pos = page.toFloat()
            layoutPages()
        }

        fun layoutPages() {
            val w = width.toFloat()
            pages.forEachIndexed { i, p ->
                val d = i - pos
                val near = abs(d) < 1f
                p.visibility = if (near) View.VISIBLE else View.INVISIBLE
                p.translationX = d * w
                p.onScroll(d, near)
            }
            moved(pos)
        }

        fun settle(target: Int, velocity: Float? = null) {
            val to = target.coerceIn(0, LAST)
            anim?.cancel()
            select(to)
            val from = pos
            val dist = abs(to - from)
            if (dist < 0.001f) {
                pos = to.toFloat()
                layoutPages()
                return
            }
            val w = width.coerceAtLeast(1)
            val ms = if (velocity != null) (4000f * dist * w / abs(velocity)).toLong().coerceIn(220L, 460L)
            else (260f + 220f * dist).toLong().coerceAtMost(480L)
            anim = ValueAnimator.ofFloat(from, to.toFloat()).apply {
                duration = ms
                interpolator = Ui.EASE
                addUpdateListener {
                    pos = it.animatedValue as Float
                    layoutPages()
                }
                start()
            }
        }

        override fun onInterceptTouchEvent(e: MotionEvent): Boolean {
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    downX = e.x
                    downY = e.y
                    dragging = false
                    tracker?.recycle()
                    tracker = VelocityTracker.obtain().apply { addMovement(e) }
                    // 正在翻页时按住：直接接着拖，不用等滑够距离
                    if (anim?.isRunning == true) {
                        anim?.cancel()
                        beginDrag(e.x)
                    }
                }
                MotionEvent.ACTION_MOVE -> if (!dragging) {
                    tracker?.addMovement(e)
                    val dx = abs(e.x - downX)
                    val dy = abs(e.y - downY)
                    if (dx > slop && dx > dy * 1.2f) beginDrag(e.x)
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> if (!dragging) {
                    tracker?.recycle()
                    tracker = null
                }
            }
            return dragging
        }

        private fun beginDrag(x: Float) {
            dragging = true
            dragX = x
            dragPos = pos
            dragPage = current
            parent?.requestDisallowInterceptTouchEvent(true)
        }

        @SuppressLint("ClickableViewAccessibility")
        override fun onTouchEvent(e: MotionEvent): Boolean {
            when (e.actionMasked) {
                MotionEvent.ACTION_MOVE -> {
                    tracker?.addMovement(e)
                    if (!dragging) {
                        val dx = abs(e.x - downX)
                        val dy = abs(e.y - downY)
                        if (dx > slop && dx > dy * 1.2f) beginDrag(e.x)
                    }
                    if (dragging) drag(e.x)
                }
                MotionEvent.ACTION_UP -> {
                    tracker?.addMovement(e)
                    if (dragging) release()
                    endTouch()
                }
                MotionEvent.ACTION_CANCEL -> {
                    if (dragging) settle(pos.roundToInt())
                    endTouch()
                }
            }
            return true
        }

        private fun endTouch() {
            dragging = false
            tracker?.recycle()
            tracker = null
        }

        private fun drag(x: Float) {
            var p = dragPos - (x - dragX) / width.coerceAtLeast(1)
            // 两头拉过了：越拉越费劲
            if (p < 0f) p = -band(-p) else if (p > LAST) p = LAST + band(p - LAST)
            pos = p
            layoutPages()
        }

        private fun band(x: Float) = x * 0.33f / (1f + x * 0.9f)

        private fun release() {
            val v = tracker?.run {
                computeCurrentVelocity(1000)
                xVelocity
            } ?: 0f
            val fling = dpf(400f)
            val target = when {
                v < -fling -> floor(pos).toInt() + 1
                v > fling -> ceil(pos).toInt() - 1
                else -> pos.roundToInt()
            }.coerceIn(dragPage - 1, dragPage + 1)
            settle(target, if (abs(v) > fling) v else null)
        }
    }

    /** 一页：上面的画，下面的字。字多、屏幕矮时可以上下滑 */
    @SuppressLint("ViewConstructor")
    private inner class PageView(ctx: Context, val scene: Scene, private val share: Float, private val center: Boolean) : ScrollView(ctx) {
        val texts = LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            clipChildren = false
            setPadding(dp(28f), dp(6f), dp(28f), dp(20f))
        }

        init {
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            overScrollMode = View.OVER_SCROLL_NEVER
            clipChildren = false
            clipToPadding = false
            addView(LinearLayout(ctx).apply {
                orientation = LinearLayout.VERTICAL
                clipChildren = false
                addView(scene, LinearLayout.LayoutParams(MATCH_PARENT, dp(240f)))
                addView(texts, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
            })
        }

        /** 画占翻页区高度的 share，太矮、太高都不好看 */
        fun fitScene(pagerHeight: Int) {
            val h = (pagerHeight * share).toInt().coerceIn(dp(190f), dp(380f))
            val lp = scene.layoutParams
            if (lp.height != h) lp.height = h
        }

        /** d：这页离当前位置差几页（-1～1）。画慢一点移、淡出、缩一点；字跟着页走、淡得快一点 */
        fun onScroll(d: Float, near: Boolean) {
            val w = width.toFloat()
            val a = abs(d)
            scene.translationX = -d * w * 0.7f
            scene.alpha = (1f - a / 0.6f).coerceIn(0f, 1f)
            val s = 1f - 0.08f * min(a, 1f)
            scene.scaleX = s
            scene.scaleY = s
            scene.shown = near && a < 0.6f
            texts.translationX = d * w * 0.12f
            texts.alpha = (1f - 1.3f * a).coerceIn(0f, 1f)
        }

        private fun add(v: View, top: Float, wrap: Boolean = false): View {
            texts.addView(v, LinearLayout.LayoutParams(if (wrap) WRAP_CONTENT else MATCH_PARENT, WRAP_CONTENT).apply {
                topMargin = dp(top)
                if (wrap && center) gravity = Gravity.CENTER_HORIZONTAL
            })
            return v
        }

        /** 标题上面的小标签：「连接电脑 · 第 1 步」 */
        fun eyebrow(s: String) {
            add(overlays.text(s, 12.5f, 850, palette.sub).apply {
                background = Ui.round(palette.tile, dpf(12f))
                setPadding(dp(10f), dp(5f), dp(10f), dp(5f))
            }, 0f, wrap = true)
        }

        fun title(s: String) {
            add(overlays.text(s, if (center) 30f else 25f, 900, palette.fg).apply {
                setLineSpacing(dpf(3f), 1f)
                if (center) gravity = Gravity.CENTER_HORIZONTAL
            }, if (texts.childCount > 0) 12f else 0f)
        }

        fun para(s: String, top: Float = 10f): TextView = add(overlays.text("", 15f, 650, palette.sub).apply {
            text = rich(s)
            setLineSpacing(dpf(5f), 1f)
            if (center) gravity = Gravity.CENTER_HORIZONTAL
        }, top) as TextView

        /** 灰底的小提示 */
        fun tip(s: String, top: Float = 16f): TextView = add(overlays.text("", 13f, 650, palette.sub).apply {
            text = rich(s)
            setLineSpacing(dpf(3f), 1f)
            background = Ui.round(palette.tile, dpf(16f))
            setPadding(dp(14f), dp(12f), dp(14f), dp(12f))
        }, top) as TextView

        /** 带圆圈序号的一步；返回那段字，好随时改 */
        fun step(n: Int, s: String, top: Float = 12f): TextView {
            val body = overlays.text("", 14.5f, 650, palette.sub).apply {
                text = rich(s)
                setLineSpacing(dpf(4f), 1f)
            }
            add(LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                addView(overlays.text("$n", 12f, 900, palette.fg).apply {
                    gravity = Gravity.CENTER
                    background = Ui.round(palette.tile, dpf(11f), Ui.hairline(context).toInt(), palette.line)
                }, LinearLayout.LayoutParams(dp(22f), dp(22f)))
                addView(body, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply {
                    marginStart = dp(12f)
                    topMargin = dp(1f)
                })
            }, top)
            return body
        }

        /** 放随状态换��卡片 */
        fun holder(top: Float = 18f): FrameLayout = add(FrameLayout(context), top) as FrameLayout

        fun extra(v: View, top: Float = 16f) {
            add(v, top)
        }
    }

    /** 顶上的页码点：当前页拉长成一小段 */
    @SuppressLint("ViewConstructor")
    private inner class Dots(ctx: Context) : View(ctx) {
        var pos = 0f
            set(v) {
                if (field != v) {
                    field = v
                    invalidate()
                }
            }
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        private val box = RectF()
        private val dot = dpf(6f)
        private val wide = dpf(20f)
        private val gap = dpf(6f)

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            setMeasuredDimension(ceil(dot * (PAGES - 1) + wide + gap * (PAGES - 1)).toInt(), ceil(dot).toInt())
        }

        override fun onDraw(canvas: Canvas) {
            var x = 0f
            for (i in 0 until PAGES) {
                val k = (1f - abs(i - pos)).coerceIn(0f, 1f)
                val w = dot + (wide - dot) * k
                paint.color = Ui.blend(Ui.alpha(palette.fg, 0.18f), palette.fg, k)
                box.set(x, 0f, x + w, dot)
                canvas.drawRoundRect(box, dot / 2f, dot / 2f, paint)
                x += w + gap
            }
        }
    }

    /**
     * 底下的按钮：第一页只有一个大按钮；往后翻，左边滑出一个圆的「返回」，大按钮让出位置。
     * 字换的时候旧的往上淡出、新的从下面浮上来
     */
    @SuppressLint("ViewConstructor")
    private inner class NavBar(ctx: Context) : View(ctx) {
        /** 0：第一页（没有返回键）；1：后面几页 */
        var t = 0f
            set(v) {
                if (field != v) {
                    field = v
                    invalidate()
                }
            }
        private var label = ""
        private var oldLabel: String? = null
        private var swap = 1f
        private var swapAnim: ValueAnimator? = null
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
        private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = Ui.hairline(ctx)
        }
        private val ink = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = dpf(2f)
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
        }
        private val words = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply {
            typeface = Ui.font(ctx, 850)
            textSize = Ui.sp(ctx, 16f)
            textAlign = Paint.Align.CENTER
        }
        private val box = RectF()
        private val path = Path()

        /** 返回、大按钮按下的程度（0～1）；松手弹回去时会略微冲过头 */
        private val press = floatArrayOf(0f, 0f)
        private val pressAnim = arrayOfNulls<ValueAnimator>(2)
        private var target = -1

        init {
            isFocusable = true
        }

        fun setLabel(s: String, animate: Boolean) {
            if (s == label) return
            contentDescription = s
            swapAnim?.cancel()
            if (!animate || label.isEmpty() || !ValueAnimator.areAnimatorsEnabled()) {
                label = s
                oldLabel = null
                swap = 1f
                invalidate()
                return
            }
            oldLabel = label
            label = s
            swap = 0f
            swapAnim = ValueAnimator.ofFloat(0f, 1f).apply {
                duration = 340
                interpolator = Ui.EASE
                addUpdateListener {
                    swap = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }

        private fun mainLeft() = (height + dpf(10f)) * t

        override fun onDraw(canvas: Canvas) {
            val w = width.toFloat()
            val h = height.toFloat()
            val r = h / 2f
            if (t > 0f) {
                val s = 1f - 0.05f * press[0]
                canvas.save()
                canvas.translate(r - dpf(18f) * (1f - t), r)
                canvas.scale(s, s)
                fill.color = Ui.alpha(palette.tile, t)
                canvas.drawCircle(0f, 0f, r, fill)
                ring.color = Ui.alpha(palette.line, t)
                canvas.drawCircle(0f, 0f, r - ring.strokeWidth / 2f, ring)
                ink.color = Ui.alpha(palette.fg, t)
                val k = dpf(5.5f)
                path.reset()
                path.moveTo(k * 0.45f, -k)
                path.lineTo(-k * 0.55f, 0f)
                path.lineTo(k * 0.45f, k)
                canvas.drawPath(path, ink)
                canvas.restore()
            }
            val left = mainLeft()
            val s = 1f - 0.05f * press[1]
            canvas.save()
            canvas.scale(s, s, (left + w) / 2f, r)
            fill.color = palette.fg
            box.set(left, 0f, w, h)
            canvas.drawRoundRect(box, r, r, fill)
            canvas.clipRect(box)
            val cx = (left + w) / 2f
            val base = r - (words.descent() + words.ascent()) / 2f
            val shift = dpf(12f)
            oldLabel?.let {
                words.color = Ui.alpha(palette.tile, 1f - swap)
                canvas.drawText(it, cx, base - shift * swap, words)
            }
            words.color = Ui.alpha(palette.tile, swap)
            canvas.drawText(label, cx, base + shift * (1f - swap), words)
            canvas.restore()
        }

        private fun inside(which: Int, x: Float, y: Float, pad: Float): Boolean {
            val h = height.toFloat()
            if (y < -pad || y > h + pad) return false
            return if (which == 0) t > 0.5f && x >= -pad && x <= h + pad else x >= mainLeft() - pad && x <= width + pad
        }

        @SuppressLint("ClickableViewAccessibility")
        override fun onTouchEvent(e: MotionEvent): Boolean {
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    target = when {
                        inside(1, e.x, e.y, 0f) -> 1
                        inside(0, e.x, e.y, 0f) -> 0
                        else -> -1
                    }
                    if (target < 0) return false
                    pressTo(target, true)
                }
                MotionEvent.ACTION_MOVE -> if (target >= 0 && !inside(target, e.x, e.y, dpf(16f))) {
                    pressTo(target, false)
                    target = -1
                }
                MotionEvent.ACTION_UP -> if (target >= 0) {
                    val which = target
                    pressTo(which, false)
                    target = -1
                    if (which == 0) back() else performClick()
                }
                MotionEvent.ACTION_CANCEL -> if (target >= 0) {
                    pressTo(target, false)
                    target = -1
                }
            }
            return true
        }

        override fun performClick(): Boolean {
            super.performClick()
            primary()
            return true
        }

        private fun pressTo(which: Int, down: Boolean) {
            pressAnim[which]?.cancel()
            pressAnim[which] = ValueAnimator.ofFloat(press[which], if (down) 1f else 0f).apply {
                duration = if (down) 90L else 380L
                interpolator = if (down) Ui.EASE else Ui.SOFT_SPRING
                addUpdateListener {
                    press[which] = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }
    }

    /** 黑底白勾的小圆：出现时弹一下，勾一笔画出来 */
    @SuppressLint("ViewConstructor")
    private inner class DoneBadge(ctx: Context) : View(ctx) {
        private var k = 0f
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = palette.fg }
        private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
            color = palette.tile
        }
        private val path = Path()
        private val part = Path()
        private val measure = PathMeasure()

        override fun onAttachedToWindow() {
            super.onAttachedToWindow()
            scaleX = 0.6f
            scaleY = 0.6f
            animate().scaleX(1f).scaleY(1f).setStartDelay(60).setDuration(420).setInterpolator(Ui.SPRING).start()
            ValueAnimator.ofFloat(0f, 1f).apply {
                duration = 520
                startDelay = 160
                interpolator = Ui.EASE
                addUpdateListener {
                    k = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }

        override fun onDraw(canvas: Canvas) {
            val s = width.toFloat()
            canvas.drawCircle(s / 2f, s / 2f, s / 2f, fill)
            path.reset()
            path.moveTo(s * 0.29f, s * 0.52f)
            path.lineTo(s * 0.44f, s * 0.66f)
            path.lineTo(s * 0.72f, s * 0.37f)
            measure.setPath(path, false)
            part.reset()
            measure.getSegment(0f, measure.length * k, part, true)
            stroke.strokeWidth = s * 0.1f
            canvas.drawPath(part, stroke)
        }
    }

    /** 触控板那页的六个手势，2 列 3 行；上面的画演示到哪个，哪个就反色 */
    @SuppressLint("ViewConstructor")
    private inner class GestureGrid(ctx: Context) : LinearLayout(ctx) {
        private val cells = ArrayList<GestureCell>()
        private var lit = -1

        init {
            orientation = LinearLayout.VERTICAL
            GESTURES.chunked(2).forEachIndexed { r, two ->
                addView(LinearLayout(ctx).apply {
                    orientation = LinearLayout.HORIZONTAL
                    two.forEachIndexed { i, (kind, title, sub) ->
                        val cell = GestureCell(ctx, kind, title, sub)
                        cells += cell
                        addView(cell, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { if (i > 0) marginStart = dp(8f) })
                    }
                }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { if (r > 0) topMargin = dp(6f) })
            }
        }

        fun highlight(i: Int) {
            if (i == lit) return
            cells.getOrNull(lit)?.light(false)
            lit = i
            cells.getOrNull(i)?.light(true)
        }
    }

    @SuppressLint("ViewConstructor")
    private inner class GestureCell(ctx: Context, kind: Int, title: String, sub: String) : LinearLayout(ctx) {
        private val icon = GestureIcon(ctx, palette, kind, palette.tile)
        private val bg = Ui.round(palette.tile, dpf(16f))
        private var anim: ValueAnimator? = null
        private var k = 0f

        init {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(8f), dp(8f), dp(8f), dp(8f))
            bg.alpha = 0
            background = bg
            addView(icon, LinearLayout.LayoutParams(dp(40f), dp(40f)))
            addView(LinearLayout(ctx).apply {
                orientation = LinearLayout.VERTICAL
                addView(overlays.text(title, 12.5f, 850, palette.fg).apply {
                    maxLines = 2
                    ellipsize = TextUtils.TruncateAt.END
                })
                addView(overlays.text(sub, 11.5f, 650, palette.sub).apply { setPadding(0, dp(2f), 0, 0) })
            }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginStart = dp(10f) })
        }

        fun light(on: Boolean) {
            anim?.cancel()
            anim = ValueAnimator.ofFloat(k, if (on) 1f else 0f).apply {
                duration = 300
                interpolator = Ui.EASE
                addUpdateListener {
                    k = it.animatedValue as Float
                    icon.active = k
                    bg.alpha = (k * 255).roundToInt()
                }
                start()
            }
        }
    }

    // ───────── 小画 ─────────

    /**
     * 每页上面那幅会动的画。按 300×240 的设计尺寸画，等比缩放后放在正中（一个单位最大 1.35dp）。
     * shown 时每帧重画，t 是出现以后过了多少毫秒；关了系统动画时 t 固定成 still，停在有代表性的一帧
     */
    @SuppressLint("ViewConstructor")
    private abstract inner class Scene(ctx: Context, private val still: Float) : View(ctx) {
        /** 翻到这页了：从头开始动 */
        var shown = false
            set(v) {
                if (field == v) return
                field = v
                if (v) {
                    born = SystemClock.uptimeMillis()
                    restart()
                    invalidate()
                }
            }
        private var born = 0L

        /** 出现以后过了多少毫秒 */
        protected var t = 0f
        protected lateinit var c: Canvas
        protected val fill = Paint(Paint.ANTI_ALIAS_FLAG)
        protected val line = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
        }
        protected val path = Path()
        protected val box = RectF()
        private val words = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG)
        private val image = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
        private val plate = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            shader = LinearGradient(0f, -9f, 0f, 9f, 0xFFCDEBFF.toInt(), 0xFF98CDF6.toInt(), Shader.TileMode.CLAMP)
        }
        private val measure = PathMeasure()
        private val part = Path()

        init {
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }

        override fun onDraw(canvas: Canvas) {
            val moving = ValueAnimator.areAnimatorsEnabled()
            t = if (moving) (SystemClock.uptimeMillis() - born).toFloat() else still
            val u = min(width / 300f, height / 240f).coerceAtMost(dpf(1.35f))
            c = canvas
            canvas.save()
            canvas.translate((width - 300f * u) / 2f, (height - 240f * u) / 2f)
            canvas.scale(u, u)
            render()
            canvas.restore()
            if (shown && moving) postInvalidateOnAnimation()
        }

        protected abstract fun render()

        /** 重新出现时清掉上一轮的状态 */
        protected open fun restart() {}

        // 时间

        /** x 在 a 到 b 之间走到哪了（0～1） */
        protected fun seg(a: Float, b: Float, x: Float = t) = ((x - a) / (b - a)).coerceIn(0f, 1f)
        protected fun ease(k: Float) = Ui.EASE.getInterpolation(k)
        protected fun pop(k: Float) = Ui.SPRING.getInterpolation(k)
        protected fun smooth(k: Float) = k * k * (3f - 2f * k)
        protected fun lerp(a: Float, b: Float, k: Float) = a + (b - a) * k

        /** -1～1 的正弦，period 毫秒一圈 */
        protected fun wave(period: Float, phase: Float = 0f) = sin((t / period + phase) * 2f * PI.toFloat())

        /** 进场 intro 毫秒以后，每 length 毫秒循环一次：这一轮过了多久（进场时是 0） */
        protected fun loop(intro: Float, length: Float) = if (t < intro) 0f else (t - intro) % length

        /** 按下的程度：down 前 50ms 按下去，up 后 90ms 松开 */
        protected fun hold(x: Float, down: Float, up: Float) = seg(down - 50f, down, x) * (1f - seg(up, up + 90f, x))

        // 画

        protected fun rr(l: Float, top: Float, r: Float, b: Float, radius: Float, p: Paint) {
            box.set(l, top, r, b)
            c.drawRoundRect(box, radius, radius, p)
        }

        protected fun text(s: String, x: Float, y: Float, size: Float, weight: Int, color: Int, align: Paint.Align = Paint.Align.LEFT) {
            words.typeface = Ui.font(context, weight)
            words.textSize = size
            words.textAlign = align
            words.color = color
            c.drawText(s, x, y, words)
        }

        /** 竖直方向以 cy 为中线 */
        protected fun textMid(s: String, x: Float, cy: Float, size: Float, weight: Int, color: Int, align: Paint.Align = Paint.Align.CENTER) {
            words.typeface = Ui.font(context, weight)
            words.textSize = size
            words.textAlign = align
            words.color = color
            c.drawText(s, x, cy - (words.descent() + words.ascent()) / 2f, words)
        }

        protected fun textWidth(s: String, size: Float, weight: Int): Float {
            words.typeface = Ui.font(context, weight)
            words.textSize = size
            return words.measureText(s)
        }

        /** 放不下就截掉结尾加「…」 */
        protected fun fit(s: String, size: Float, weight: Int, max: Float): String {
            if (textWidth(s, size, weight) <= max) return s
            var end = s.length
            while (end > 1 && textWidth(s.substring(0, end) + "…", size, weight) > max) end--
            return s.substring(0, end) + "…"
        }

        /** 手指：一个半透明的圆点，按下去稍微变小变深；描一圈底色，按在黑色的键上也看得见 */
        protected fun finger(x: Float, y: Float, press: Float, a: Float, s: Float = 1f) {
            if (a <= 0f) return
            val r = (9f - 1.5f * press) * s
            fill.color = Ui.alpha(palette.fg, (0.08f + 0.08f * press) * a)
            c.drawCircle(x, y, r + 6f * s, fill)
            fill.color = Ui.alpha(palette.fg, (0.42f + 0.18f * press) * a)
            c.drawCircle(x, y, r, fill)
            line.color = Ui.alpha(palette.tile, 0.9f * a)
            line.strokeWidth = 1.4f * s
            c.drawCircle(x, y, r, line)
        }

        /** 点下去散开的一圈 */
        protected fun ripple(x: Float, y: Float, k: Float, r0: Float = 8f, r1: Float = 24f, a: Float = 1f) {
            if (k <= 0f || k >= 1f || a <= 0f) return
            line.color = Ui.alpha(palette.fg, 0.5f * (1f - k) * a)
            line.strokeWidth = 1.5f
            c.drawCircle(x, y, lerp(r0, r1, ease(k)), line)
        }

        /** 黑底白字的小气泡 */
        protected fun bubble(s: String, x: Float, y: Float, a: Float, align: Paint.Align = Paint.Align.CENTER, size: Float = 8f) {
            if (a <= 0f) return
            val w = textWidth(s, size, 850) + size * 1.5f
            val h = size + 8f
            val l = when (align) {
                Paint.Align.LEFT -> x
                Paint.Align.RIGHT -> x - w
                else -> x - w / 2f
            }
            fill.color = Ui.alpha(palette.fg, a)
            rr(l, y - h / 2f, l + w, y + h / 2f, h / 2f, fill)
            textMid(s, l + w / 2f, y, size, 850, Ui.alpha(palette.tile, a))
        }

        /** 一笔画出来的勾，k 是画到哪了；s 是整个勾的大小 */
        protected fun check(cx: Float, cy: Float, s: Float, k: Float, color: Int, width: Float) {
            if (k <= 0f) return
            path.reset()
            path.moveTo(cx - s * 0.36f, cy + s * 0.02f)
            path.lineTo(cx - s * 0.1f, cy + s * 0.26f)
            path.lineTo(cx + s * 0.38f, cy - s * 0.24f)
            measure.setPath(path, false)
            part.reset()
            measure.getSegment(0f, measure.length * k.coerceAtMost(1f), part, true)
            line.color = color
            line.strokeWidth = width
            c.drawPath(part, line)
        }

        /** 电脑上的鼠标箭头，尖在 (x, y) */
        protected fun cursor(x: Float, y: Float, a: Float, s: Float = 1f) {
            if (a <= 0f) return
            path.reset()
            path.moveTo(x, y)
            path.lineTo(x, y + 15f * s)
            path.lineTo(x + 3.8f * s, y + 11.6f * s)
            path.lineTo(x + 6.6f * s, y + 17.4f * s)
            path.lineTo(x + 9.2f * s, y + 16.2f * s)
            path.lineTo(x + 6.5f * s, y + 10.6f * s)
            path.lineTo(x + 11f * s, y + 10.6f * s)
            path.close()
            fill.color = Ui.alpha(palette.fg, a)
            c.drawPath(path, fill)
            line.color = Ui.alpha(palette.tile, a)
            line.strokeWidth = 1.1f * s
            c.drawPath(path, line)
        }

        /** 笔记本电脑：屏幕（l..r × top..b）+ 下面一条键盘底座 */
        protected fun laptop(l: Float, top: Float, r: Float, b: Float, a: Float, inset: Float = 6f) {
            if (a <= 0f) return
            fill.color = Ui.alpha(palette.tile, a)
            rr(l, top, r, b, 9f, fill)
            line.color = Ui.alpha(palette.fg, a)
            line.strokeWidth = 2f
            rr(l, top, r, b, 9f, line)
            fill.color = Ui.alpha(palette.bg, a)
            rr(l + inset, top + inset, r - inset, b - inset, 4f, fill)
            fill.color = Ui.alpha(palette.fg, a)
            rr(l - 14f, b + 1.5f, r + 14f, b + 8f, 3.5f, fill)
            fill.color = Ui.alpha(palette.tile, 0.45f * a)
            val m = (l + r) / 2f
            rr(m - 13f, b + 1.5f, m + 13f, b + 3.6f, 1.2f, fill)
        }

        protected fun phoneBody(l: Float, top: Float, r: Float, b: Float, radius: Float, a: Float, inset: Float = 4f) {
            if (a <= 0f) return
            fill.color = Ui.alpha(palette.tile, a)
            rr(l, top, r, b, radius, fill)
            line.color = Ui.alpha(palette.fg, a)
            line.strokeWidth = 2f
            rr(l, top, r, b, radius, line)
            fill.color = Ui.alpha(palette.bg, a)
            rr(l + inset, top + inset, r - inset, b - inset, radius - inset * 0.7f, fill)
        }

        protected fun bird(cx: Float, cy: Float, size: Float, a: Float = 1f) {
            if (a <= 0f) return
            image.alpha = (a.coerceIn(0f, 1f) * 255).roundToInt()
            box.set(cx - size / 2f, cy - size / 2f, cx + size / 2f, cy + size / 2f)
            c.drawBitmap(birdImage, null, box, image)
        }

        /** 蒜鸟的图标：浅蓝圆角方块上的小鸟（和桌面、托盘上的一样） */
        protected fun appIcon(cx: Float, cy: Float, size: Float, a: Float = 1f) {
            if (a <= 0f) return
            c.save()
            c.translate(cx, cy)
            c.scale(size / 18f, size / 18f)
            plate.alpha = (a.coerceIn(0f, 1f) * 255).roundToInt()
            rr(-9f, -9f, 9f, 9f, 4.2f, plate)
            bird(0f, 0.6f, 16f, a)
            c.restore()
        }

        protected fun mic(x: Float, y: Float, color: Int, s: Float = 1f) {
            fill.color = color
            rr(x - 3.4f * s, y - 9f * s, x + 3.4f * s, y + 2f * s, 3.4f * s, fill)
            line.color = color
            line.strokeWidth = 1.5f * s
            box.set(x - 6.5f * s, y - 6.5f * s, x + 6.5f * s, y + 5f * s)
            c.drawArc(box, 15f, 150f, false, line)
            c.drawLine(x, y + 5f * s, x, y + 8.5f * s, line)
        }

        protected fun chevron(x: Float, y: Float, a: Float) {
            if (a <= 0f) return
            line.color = Ui.alpha(palette.mut, a)
            line.strokeWidth = 1.3f
            path.reset()
            path.moveTo(x - 1.8f, y - 3.6f)
            path.lineTo(x + 1.8f, y)
            path.lineTo(x - 1.8f, y + 3.6f)
            c.drawPath(path, line)
        }

        /** 列表里的小电脑图标 */
        protected fun miniLaptop(cx: Float, cy: Float, a: Float, s: Float = 1f) {
            if (a <= 0f) return
            line.color = Ui.alpha(palette.fg, a)
            line.strokeWidth = 0.9f * s
            rr(cx - 4f * s, cy - 3.5f * s, cx + 4f * s, cy + 2f * s, 0.8f * s, line)
            c.drawLine(cx - 5.5f * s, cy + 3.6f * s, cx + 5.5f * s, cy + 3.6f * s, line)
        }

        /** 输入框左上角压在边框上的小标签「电脑」 */
        protected fun legend(cx: Float, y: Float, a: Float) {
            if (a <= 0f) return
            val w = textWidth("电脑", 5.5f, 850) + 9f
            fill.color = Ui.alpha(palette.tile, a)
            rr(cx - w / 2f, y - 5f, cx + w / 2f, y + 5f, 5f, fill)
            line.color = Ui.alpha(palette.line, a)
            line.strokeWidth = 0.9f
            rr(cx - w / 2f, y - 5f, cx + w / 2f, y + 5f, 5f, line)
            textMid("电脑", cx, y, 5.5f, 850, Ui.alpha(palette.sub, a))
        }
    }

    /** 欢迎页、最后一页：蓝色圆盘上的蒜鸟，周围漂着三样本事（触控板、按键、麦克风）；最后一页换成撒彩纸、打勾 */
    @SuppressLint("ViewConstructor")
    private inner class HelloScene(ctx: Context, private val done: Boolean) : Scene(ctx, if (done) 1400f else 1700f) {
        private val disc = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            shader = if (palette.dark) {
                LinearGradient(0f, 36f, 0f, 188f, 0xFF2B4256.toInt(), 0xFF1A2A39.toInt(), Shader.TileMode.CLAMP)
            } else {
                LinearGradient(0f, 36f, 0f, 188f, 0xFFCDEBFF.toInt(), 0xFF98CDF6.toInt(), Shader.TileMode.CLAMP)
            }
        }
        private val confetti = if (done) Confetti(26) else null

        override fun render() {
            val cx = 150f
            val cy = 112f
            val kd = seg(0f, 760f)
            if (kd > 0f) {
                val s = lerp(0.62f, 1f, pop(kd))
                disc.alpha = (255 * seg(0f, 300f)).roundToInt()
                c.save()
                c.scale(s, s, cx, cy)
                c.drawCircle(cx, cy, 76f, disc)
                c.restore()
            }
            sparkles()
            confetti?.let { burst(it) }
            val kb = seg(140f, 900f)
            if (kb > 0f) {
                val s = lerp(0.55f, 1f, pop(kb))
                val bob = 3.2f * wave(2000f) * seg(800f, 1400f)
                c.save()
                c.translate(cx, cy + bob)
                c.scale(s, s)
                bird(0f, 0f, 124f, seg(140f, 360f))
                c.restore()
            }
            if (done) badge() else bubbles()
        }

        /** 四颗一闪一闪的小星 */
        private fun sparkles() {
            val a0 = seg(500f, 1100f)
            if (a0 <= 0f) return
            for (i in 0 until 4) {
                val x = SPARKS[i * 2]
                val y = SPARKS[i * 2 + 1]
                val tw = 0.5f + 0.5f * wave(1800f, i * 0.27f)
                val s = 3.5f + 3f * tw
                fill.color = Ui.alpha(palette.fg, a0 * (0.1f + 0.16f * tw))
                path.reset()
                path.moveTo(x, y - s)
                path.quadTo(x, y, x + s, y)
                path.quadTo(x, y, x, y + s)
                path.quadTo(x, y, x - s, y)
                path.quadTo(x, y, x, y - s)
                path.close()
                c.drawPath(path, fill)
            }
        }

        /** 欢迎页：三个小圆泡，依次弹出来，然后轻轻上下漂 */
        private fun bubbles() {
            for (i in 0 until 3) {
                val k = seg(520f + 140f * i, 1020f + 140f * i)
                if (k <= 0f) continue
                val a = seg(520f + 140f * i, 720f + 140f * i)
                val s = pop(k)
                c.save()
                c.translate(BUBBLES[i * 2], BUBBLES[i * 2 + 1] + 2.6f * wave(2600f, i * 0.33f) * seg(1000f, 1600f))
                c.scale(s, s)
                fill.color = Ui.alpha(palette.tile, a)
                c.drawCircle(0f, 0f, 22f, fill)
                line.color = Ui.alpha(palette.line, a)
                line.strokeWidth = 1.2f
                c.drawCircle(0f, 0f, 22f, line)
                val ink = Ui.alpha(palette.fg, a)
                line.color = ink
                line.strokeWidth = 1.7f
                when (i) {
                    0 -> { // 触控板
                        rr(-10f, -7.5f, 10f, 7.5f, 3f, line)
                        fill.color = ink
                        c.drawCircle(1.5f, 0.5f, 2.3f, fill)
                    }
                    1 -> { // 按键
                        rr(-9f, -9f, 9f, 9f, 3.5f, line)
                        textMid("C", 0f, -0.3f, 9f, 900, ink)
                    }
                    else -> mic(0f, 1f, ink) // 语音
                }
                c.restore()
            }
        }

        /** 最后一页：鸟右下角弹出一个勾 */
        private fun badge() {
            val k = seg(650f, 1150f)
            if (k <= 0f) return
            val s = pop(k)
            c.save()
            c.translate(196f, 156f)
            c.scale(s, s)
            fill.color = palette.bg
            c.drawCircle(0f, 0f, 18.5f, fill)
            fill.color = palette.fg
            c.drawCircle(0f, 0f, 15f, fill)
            check(0f, 0f, 18f, seg(850f, 1300f), palette.tile, 2.6f)
            c.restore()
        }

        /** 彩纸从鸟身后炸开，往上飞、慢下来、飘落、淡掉；每 4.2 秒一次，单数次左右翻过来 */
        private fun burst(set: Confetti) {
            if (t < 380f) return
            val round = ((t - 380f) / 4200f).toInt()
            val ms = (t - 380f) % 4200f
            val a = 1f - seg(1500f, 2300f, ms)
            if (a <= 0f) return
            val s = ms / 1000f
            val e = (1f - exp(-2.6f * s)) / 2.6f
            val mirror = if (round % 2 == 1) -1f else 1f
            for (i in 0 until set.count) {
                c.save()
                c.translate(150f + mirror * set.vx[i] * e, 112f + set.vy[i] * e + 46f * s * s)
                c.rotate(set.turn[i] + set.spin[i] * s)
                c.scale(cos(s * set.flip[i]), 1f)
                fill.color = Ui.alpha(set.color[i], a)
                rr(-set.w[i] / 2f, -set.h[i] / 2f, set.w[i] / 2f, set.h[i] / 2f, 1f, fill)
                c.restore()
            }
        }
    }

    /** 彩纸：一把固定的种子，每次撒的样子一样 */
    private class Confetti(val count: Int) {
        val vx = FloatArray(count)
        val vy = FloatArray(count)
        val spin = FloatArray(count)
        val turn = FloatArray(count)
        val flip = FloatArray(count)
        val w = FloatArray(count)
        val h = FloatArray(count)
        val color = IntArray(count)

        init {
            val r = Random(7)
            for (i in 0 until count) {
                val ang = -PI / 2 + (r.nextDouble() - 0.5) * PI * 1.6
                val speed = 170.0 + 130.0 * r.nextDouble()
                vx[i] = (cos(ang) * speed).toFloat()
                vy[i] = (sin(ang) * speed).toFloat()
                spin[i] = (r.nextFloat() - 0.5f) * 540f
                turn[i] = r.nextFloat() * 360f
                flip[i] = 5f + 7f * r.nextFloat()
                w[i] = 3f + 2f * r.nextFloat()
                h[i] = 5f + 3f * r.nextFloat()
                color[i] = CONFETTI[i % CONFETTI.size]
            }
        }
    }

    /** 第 1 步：桌面上双击蒜鸟，窗口打开，托盘里出现小鸟；关掉窗口，小鸟还在（后台运行） */
    @SuppressLint("ViewConstructor")
    private inner class PcScene(ctx: Context) : Scene(ctx, 3520f) {
        private val desk = Path()

        override fun render() {
            val intro = ease(seg(0f, 520f))
            val tt = loop(520f, 6000f)
            c.save()
            c.translate(0f, 26f + 10f * (1f - intro)) // 下面这些按偏上的位置画的，整体挪到正中
            laptop(46f, 20f, 254f, 150f, intro)
            c.save()
            desk.reset()
            desk.addRoundRect(52f, 26f, 248f, 144f, 4f, 4f, Path.Direction.CW)
            c.clipPath(desk)
            // 任务栏：中间几个图标，右边时间
            fill.color = Ui.alpha(palette.fg, 0.07f * intro)
            c.drawRect(52f, 131f, 248f, 144f, fill)
            fill.color = Ui.alpha(palette.fg, 0.2f * intro)
            for (i in 0 until 4) rr(131f + 10f * i, 134.5f, 137f + 10f * i, 140.5f, 1.6f, fill)
            text("10:30", 244f, 139.6f, 5f, 750, Ui.alpha(palette.sub, intro), Paint.Align.RIGHT)
            // 桌面上的蒜鸟：双击时有一层选中的底
            val sel = seg(950f, 1000f, tt) * (1f - seg(1300f, 1500f, tt))
            if (sel > 0f) {
                fill.color = Ui.alpha(palette.fg, 0.08f * sel)
                rr(59f, 37f, 85f, 72f, 3f, fill)
            }
            appIcon(72f, 50f, 18f, intro)
            textMid("蒜鸟", 72f, 66f, 5.6f, 750, Ui.alpha(palette.fg, intro))
            // 托盘里的小鸟：窗口打开时出现，关了窗口也还在，一轮结束才淡掉
            if (tt >= 1500f && tt < 5850f) {
                val a = 1f - seg(5500f, 5850f, tt)
                if (tt > 1900f && tt < 5400f) ripple(213f, 137.5f, ((tt - 1900f) % 1400f) / 1400f, 5f, 12f, a * 0.8f)
                val s = pop(seg(1500f, 1900f, tt))
                c.save()
                c.scale(s, s, 213f, 137.5f)
                appIcon(213f, 137.5f, 9f, a)
                c.restore()
            }
            // 蒜鸟的窗口：从图标的位置长出来，点 × 缩一点淡掉
            val open = ease(seg(1250f, 1650f, tt))
            val gone = ease(seg(4100f, 4380f, tt))
            if (open > 0f && gone < 1f) {
                val l = lerp(63f, 98f, open)
                val r = lerp(81f, 218f, open)
                val b = lerp(59f, 119f, open)
                val s = 1f - 0.08f * gone
                c.save()
                c.scale(s, s, (l + r) / 2f, (41f + b) / 2f)
                window(l, 41f, r, b, seg(1250f, 1400f, tt) * (1f - gone), seg(0.55f, 1f, open))
                c.restore()
            }
            // 「后台运行中」指着托盘
            val tip = seg(4400f, 4600f, tt) * (1f - seg(5200f, 5400f, tt))
            if (tip > 0f) {
                val s = lerp(0.85f, 1f, pop(seg(4400f, 4700f, tt)))
                c.save()
                c.scale(s, s, 213f, 130f)
                fill.color = Ui.alpha(palette.fg, tip)
                path.reset()
                path.moveTo(209f, 127.5f)
                path.lineTo(217f, 127.5f)
                path.lineTo(213f, 131.5f)
                path.close()
                c.drawPath(path, fill)
                bubble("后台运行中", 207f, 120f, tip, size = 7f)
                c.restore()
            }
            // 鼠标：去双击图标 → 去点 × → 回到原处
            val px: Float
            val py: Float
            when {
                tt < 3500f -> {
                    val k = ease(seg(0f, 900f, tt))
                    px = lerp(204f, 75f, k)
                    py = lerp(112f, 53f, k)
                }
                tt < 4300f -> {
                    val k = ease(seg(3500f, 3950f, tt))
                    px = lerp(75f, 210f, k)
                    py = lerp(53f, 47f, k)
                }
                else -> {
                    val k = ease(seg(4300f, 5300f, tt))
                    px = lerp(210f, 204f, k)
                    py = lerp(47f, 112f, k)
                }
            }
            ripple(75f, 53f, seg(950f, 1250f, tt), 2f, 10f)
            ripple(75f, 53f, seg(1110f, 1410f, tt), 2f, 10f)
            ripple(210f, 47f, seg(4000f, 4300f, tt), 2f, 10f)
            cursor(px, py, intro, 0.72f)
            c.restore()
            c.restore()
        }

        /** 蒜鸟电脑端的窗口，inner 是里面内容的显现程度（窗口还小时先不画） */
        private fun window(l: Float, top: Float, r: Float, b: Float, a: Float, inner: Float) {
            fill.color = Ui.alpha(palette.tile, a)
            rr(l, top, r, b, 4f, fill)
            line.color = Ui.alpha(palette.line, a)
            line.strokeWidth = 0.8f
            rr(l, top, r, b, 4f, line)
            val k = a * inner
            if (k <= 0f) return
            bird(l + 6.5f, top + 6f, 7f, k)
            text("蒜鸟", l + 11.5f, top + 8f, 4.6f, 850, Ui.alpha(palette.fg, k))
            line.color = Ui.alpha(palette.fg, k)
            line.strokeWidth = 0.9f
            c.drawLine(r - 10f, top + 4f, r - 6f, top + 8f, line)
            c.drawLine(r - 6f, top + 4f, r - 10f, top + 8f, line)
            fill.color = Ui.alpha(palette.fg, 0.045f * k)
            c.drawRect(l + 0.5f, top + 12f, l + 22f, b - 0.5f, fill)
            fill.color = Ui.alpha(palette.fg, 0.14f * k)
            for (i in 0 until 4) rr(l + 5f, top + 17f + 7f * i, l + 17f, top + 19.5f + 7f * i, 1.2f, fill)
            fill.color = Ui.alpha(palette.fg, 0.06f * k)
            rr(l + 28f, top + 16f, r - 7f, top + 38f, 3f, fill)
            rr(l + 28f, top + 43f, (l + r) / 2f + 10f, b - 7f, 3f, fill)
            rr((l + r) / 2f + 14f, top + 43f, r - 7f, b - 7f, 3f, fill)
        }
    }

    /** 第 2 步：手机停在蓝牙设置页，电脑「添加设备」里点这部手机，两边核对同一串数字，连上打勾 */
    @SuppressLint("ViewConstructor")
    private inner class PairScene(ctx: Context) : Scene(ctx, 4360f) {
        var phoneName = "这部手机"
            set(v) {
                if (field != v) {
                    field = v
                    shortName = null
                }
            }
        private var shortName: String? = null
        private val screenClip = Path()

        override fun render() {
            val intro = ease(seg(0f, 560f))
            val tt = loop(560f, 5600f)
            val back = 1f - seg(5000f, 5500f, tt) // 一轮末尾回到没配对的样子
            val linked = ease(seg(3100f, 3500f, tt)) * back
            link(tt, linked, intro, back)

            // 手机：蓝牙设置页
            c.save()
            c.translate(-14f * (1f - intro), 0f)
            phoneBody(40f, 56f, 102f, 180f, 10f, intro)
            screenClip.reset()
            screenClip.addRoundRect(44f, 60f, 98f, 176f, 7f, 7f, Path.Direction.CW)
            c.save()
            c.clipPath(screenClip)
            val ink = Ui.alpha(palette.fg, intro)
            val grey = Ui.alpha(palette.sub, intro)
            text("蓝牙", 49f, 74f, 7.5f, 900, ink)
            fill.color = ink
            rr(81f, 67.5f, 93f, 74f, 3.25f, fill)
            fill.color = Ui.alpha(palette.tile, intro)
            c.drawCircle(89.75f, 70.75f, 2.3f, fill)
            text("可被附近设备发现", 49f, 85f, 4.6f, 650, grey)
            text("已配对的设备", 49f, 97f, 4.3f, 750, grey)
            val row = seg(3300f, 3600f, tt) * back
            if (row > 0f) {
                fill.color = Ui.alpha(palette.tile, row * intro)
                rr(47f, 101f, 95f, 114f, 3f, fill)
                miniLaptop(53f, 107.2f, row * intro, 0.8f)
                text("我的电脑", 59f, 109.6f, 5f, 750, Ui.alpha(palette.fg, row * intro))
            }
            text("可用的设备", 49f, 126f, 4.3f, 750, grey)
            fill.color = Ui.alpha(palette.fg, 0.05f * intro)
            rr(47f, 130f, 95f, 141f, 3f, fill)
            rr(47f, 144f, 95f, 155f, 3f, fill)
            val ask = seg(1900f, 2100f, tt) * (1f - seg(2950f, 3150f, tt))
            if (ask > 0f) {
                fill.color = Ui.alpha(0xFF000000.toInt(), 0.22f * ask)
                c.drawRect(44f, 60f, 98f, 176f, fill)
                val s = lerp(0.9f, 1f, pop(seg(1900f, 2200f, tt)))
                c.save()
                c.scale(s, s, 71f, 123f)
                fill.color = Ui.alpha(palette.tile, ask)
                rr(47f, 98f, 95f, 148f, 6f, fill)
                textMid("蓝牙配对请求", 71f, 106.5f, 4.3f, 750, Ui.alpha(palette.sub, ask))
                textMid(CODE, 71f, 119f, 8.2f, 900, Ui.alpha(palette.fg, ask))
                val bs = 1f - 0.08f * hold(tt, 2700f, 2800f)
                c.save()
                c.scale(bs, bs, 71f, 136f)
                fill.color = Ui.alpha(palette.fg, ask)
                rr(54f, 130f, 88f, 142f, 6f, fill)
                textMid("配对", 71f, 136f, 5.2f, 850, Ui.alpha(palette.tile, ask))
                c.restore()
                c.restore()
            }
            c.restore()
            val fa = seg(2350f, 2550f, tt) * (1f - seg(2900f, 3100f, tt))
            if (fa > 0f) {
                val k = ease(seg(2350f, 2650f, tt))
                finger(lerp(96f, 74f, k), lerp(168f, 139f, k), hold(tt, 2700f, 2800f), fa, 0.75f)
            }
            c.restore()

            // 电脑：「添加设备」窗口
            c.save()
            c.translate(14f * (1f - intro), 0f)
            laptop(182f, 70f, 282f, 138f, intro, 5f)
            c.save()
            c.clipRect(187f, 75f, 277f, 133f)
            fill.color = Ui.alpha(palette.tile, intro)
            rr(193f, 80f, 271f, 128f, 3.5f, fill)
            line.color = Ui.alpha(palette.line, intro)
            line.strokeWidth = 0.7f
            rr(193f, 80f, 271f, 128f, 3.5f, line)
            text("添加设备", 198f, 89f, 5.4f, 900, ink)
            text("正在搜索…", 198f, 96.5f, 4f, 650, grey)
            val found = seg(600f, 850f, tt) * back * intro
            if (found > 0f) {
                val chosen = seg(1300f, 1360f, tt)
                val dy = 3f * (1f - found)
                fill.color = Ui.alpha(palette.fg, (0.05f + 0.07f * chosen) * found)
                rr(197f, 100f + dy, 267f, 112f + dy, 2.5f, fill)
                line.color = Ui.alpha(palette.fg, found)
                line.strokeWidth = 0.8f
                rr(201f, 102.5f + dy, 205f, 109.5f + dy, 1f, line)
                val label = shortName ?: fit(phoneName, 4.8f, 750, 54f).also { shortName = it }
                text(label, 208.5f, 108f + dy, 4.8f, 750, Ui.alpha(palette.fg, found))
            }
            if (ask > 0f) {
                val s = lerp(0.9f, 1f, pop(seg(1900f, 2200f, tt)))
                c.save()
                c.scale(s, s, 232f, 105f)
                fill.color = Ui.alpha(palette.tile, ask)
                rr(197f, 84f, 267f, 126f, 4f, fill)
                line.color = Ui.alpha(palette.line, ask)
                line.strokeWidth = 0.8f
                rr(197f, 84f, 267f, 126f, 4f, line)
                textMid(CODE, 232f, 98f, 8f, 900, Ui.alpha(palette.fg, ask))
                fill.color = Ui.alpha(palette.fg, ask)
                rr(216f, 109f, 248f, 120f, 5.5f, fill)
                textMid("连接", 232f, 114.5f, 5f, 850, Ui.alpha(palette.tile, ask))
                c.restore()
            }
            // 鼠标：点列表里的手机 → 点「连接」→ 回去
            val px: Float
            val py: Float
            when {
                tt < 2000f -> {
                    val k = ease(seg(900f, 1250f, tt))
                    px = lerp(262f, 232f, k)
                    py = lerp(126f, 107f, k)
                }
                tt < 3150f -> {
                    val k = ease(seg(2300f, 2650f, tt))
                    px = 232f
                    py = lerp(107f, 115f, k)
                }
                else -> {
                    val k = ease(seg(3200f, 3700f, tt))
                    px = lerp(232f, 262f, k)
                    py = lerp(115f, 126f, k)
                }
            }
            ripple(232f, 107f, seg(1300f, 1550f, tt), 2f, 9f)
            ripple(232f, 115f, seg(2900f, 3150f, tt), 2f, 9f)
            cursor(px, py, intro, 0.55f)
            c.restore()
            c.restore()
        }

        /** 中间：没连上是一串虚点，亮点来回跑、蓝牙标志一圈圈往外扩；连上变实线、标志换成勾 */
        private fun link(tt: Float, linked: Float, a: Float, back: Float) {
            if (linked < 1f) {
                val hx = 106f + 70f * ((tt % 1400f) / 1400f)
                for (i in 0 until 5) {
                    for (x in floatArrayOf(110f + 4f * i, 156f + 4f * i)) {
                        val glow = (1f - abs(x - hx) / 8f).coerceIn(0f, 1f)
                        fill.color = Ui.alpha(palette.fg, (0.22f + 0.5f * glow) * (1f - linked) * a)
                        c.drawCircle(x, 112f, 1.1f, fill)
                    }
                }
            }
            if (linked > 0f) {
                line.color = Ui.alpha(palette.fg, a)
                line.strokeWidth = 1.6f
                c.drawLine(110f, 112f, lerp(110f, 172f, linked), 112f, line)
            }
            if (linked < 1f) ripple(141f, 112f, (tt % 1400f) / 1400f, 15f, 24f, (1f - linked) * a)
            val s = pop(seg(150f, 650f)) * (1f + 0.12f * sin(PI.toFloat() * seg(3100f, 3450f, tt)) * back)
            if (s <= 0f) return
            c.save()
            c.translate(141f, 112f)
            c.scale(s, s)
            fill.color = Ui.alpha(palette.fg, a)
            c.drawCircle(0f, 0f, 15f, fill)
            val done = seg(3100f, 3250f, tt) * back
            if (done < 1f) {
                line.color = Ui.alpha(palette.tile, (1f - done) * a)
                line.strokeWidth = 1.8f
                path.reset()
                path.moveTo(-4.2f, -4.2f)
                path.lineTo(4.2f, 3.8f)
                path.lineTo(0f, 7.6f)
                path.lineTo(0f, -7.6f)
                path.lineTo(4.2f, -3.8f)
                path.lineTo(-4.2f, 4.2f)
                c.drawPath(path, line)
            }
            if (done > 0f) check(0f, 0f, 17f, seg(3200f, 3550f, tt), Ui.alpha(palette.tile, done * a), 2.2f)
            c.restore()
        }
    }

    /** 第 3 步：「选择电脑」列表里点自己的电脑，打勾，下面显示正在连接 → 已连接 */
    @SuppressLint("ViewConstructor")
    private inner class PickScene(ctx: Context) : Scene(ctx, 3760f) {
        var name: String? = null
            set(v) {
                if (field != v) {
                    field = v
                    shortName = null
                }
            }
        private var shortName: String? = null

        override fun render() {
            val intro = ease(seg(0f, 560f))
            val tt = loop(560f, 5400f)
            val back = 1f - seg(4400f, 4800f, tt)
            c.save()
            c.translate(0f, 12f * (1f - intro))
            fill.color = Ui.alpha(palette.tile, intro)
            rr(46f, 10f, 254f, 230f, 18f, fill)
            line.color = Ui.alpha(palette.line, intro)
            line.strokeWidth = 1f
            rr(46f, 10f, 254f, 230f, 18f, line)
            text("选择电脑", 62f, 37f, 12f, 900, Ui.alpha(palette.fg, intro))
            text("列的是和这部手机配对过的电脑", 62f, 53f, 6.4f, 650, Ui.alpha(palette.sub, intro))
            val mine = shortName ?: fit(name ?: "我的笔记本", 8.5f, 750, 130f).also { shortName = it }
            val chosen = seg(1150f, 1250f, tt) * back
            for (i in 0 until 3) {
                val y = 92f + 34f * i
                val a = ease(seg(160f + 90f * i, 460f + 90f * i))
                if (a <= 0f) continue
                c.save()
                c.translate(0f, 6f * (1f - a))
                if (i == 1 && chosen > 0f) {
                    fill.color = Ui.alpha(palette.fg, 0.07f * chosen)
                    rr(54f, y - 15f, 246f, y + 15f, 9f, fill)
                }
                miniLaptop(70f, y, a, 1.4f)
                text(if (i == 1) mine else OTHERS[i / 2], 88f, y + 3f, 8.5f, 750, Ui.alpha(palette.fg, a))
                if (i == 1 && chosen > 0f) {
                    chevron(238f, y, a * (1f - chosen))
                    check(236f, y, 12f, seg(1350f, 1650f, tt), Ui.alpha(palette.fg, a * back), 1.8f)
                } else {
                    chevron(238f, y, a)
                }
                c.restore()
            }
            status(tt, back)
            c.restore()
            val fa = seg(450f, 650f, tt) * (1f - seg(1500f, 1800f, tt))
            if (fa > 0f) {
                val k = ease(seg(520f, 1100f, tt))
                finger(lerp(200f, 150f, k), lerp(250f, 126f, k) + 10f * ease(seg(1400f, 1800f, tt)), hold(tt, 1200f, 1300f), fa)
            }
            ripple(150f, 126f, seg(1200f, 1600f, tt), 10f, 30f)
        }

        /** 底下的小胶囊：「正在连接…」转圈 → 「已连接」打勾 */
        private fun status(tt: Float, back: Float) {
            val a = seg(1700f, 1900f, tt) * back
            if (a <= 0f) return
            val k = ease(seg(2700f, 2900f, tt))
            val w = lerp(textWidth("正在连接…", 7f, 850), textWidth("已连接", 7f, 850), k) + 30f
            fill.color = Ui.blend(Ui.alpha(palette.fg, 0.07f * a), Ui.alpha(palette.fg, a), k)
            rr(150f - w / 2f, 197f, 150f + w / 2f, 215f, 9f, fill)
            val ix = 150f - w / 2f + 12f
            if (k < 1f) {
                line.color = Ui.alpha(palette.sub, a * (1f - k))
                line.strokeWidth = 1.4f
                box.set(ix - 3.5f, 202.5f, ix + 3.5f, 209.5f)
                c.drawArc(box, (tt * 0.5f) % 360f, 260f, false, line)
                textMid("正在连接…", ix + 7f, 206f, 7f, 850, Ui.alpha(palette.sub, a * (1f - k)), Paint.Align.LEFT)
            }
            if (k > 0f) {
                check(ix, 206f, 9f, seg(2750f, 3050f, tt), Ui.alpha(palette.tile, a * k), 1.5f)
                textMid("已连接", ix + 7f, 206f, 7f, 850, Ui.alpha(palette.tile, a * k), Paint.Align.LEFT)
            }
        }
    }

    /** 触控板：屏幕上的鼠标跟着手指走，六个手势轮流演示，下面的格子同步高亮 */
    @SuppressLint("ViewConstructor")
    private inner class PadScene(ctx: Context, private val onGesture: (Int) -> Unit) : Scene(ctx, 4000f) {
        private var lastGesture = -2
        private val fx = FloatArray(3)
        private val fy = FloatArray(3)
        private var fingers = 0

        override fun restart() {
            lastGesture = -2
        }

        private fun put(i: Int, x: Float, y: Float) {
            fx[i] = x
            fy[i] = y
            fingers = max(fingers, i + 1)
        }

        override fun render() {
            val intro = ease(seg(0f, 420f))
            val g = if (t < 420f) -1 else ((t - 420f) / G).toInt() % 6
            val tt = if (t < 420f) 0f else (t - 420f) % G
            if (g != lastGesture) {
                lastGesture = g
                post { onGesture(g) }
            }
            var curX = 150f
            var curY = 40f
            var curA = 1f
            var scroll = 54f
            var fileA = 0f
            var menu = 0f
            var menuA = 0f
            var ring = 0f
            var padRing = 0f
            var tip: String? = null
            var tipA = 0f
            var fingerA = 0f
            var fingerP = 0f
            fingers = 0
            when (g) {
                -1 -> {
                    curX = 104f
                    curY = 66f
                    curA = intro
                    scroll = 0f
                }
                GestureIcon.MOVE -> {
                    val k = smooth(seg(400f, 1700f, tt))
                    val x = lerp(112f, 188f, k)
                    val y = lerp(200f, 158f, k) - 14f * sin(PI.toFloat() * k)
                    put(0, x, y)
                    fingerA = seg(150f, 350f, tt) * (1f - seg(1850f, 2100f, tt))
                    fingerP = 0.5f
                    curX = 104f + (x - 112f) * 0.605f
                    curY = 66f + (y - 200f) * 0.62f
                    scroll = 0f
                    tip = "移动"
                    tipA = seg(500f, 700f, tt) * (1f - seg(1900f, 2100f, tt))
                }
                GestureIcon.TAP -> {
                    put(0, 150f, 182f)
                    fingerA = seg(200f, 400f, tt) * (1f - seg(1100f, 1350f, tt))
                    fingerP = hold(tt, 650f, 760f)
                    padRing = seg(650f, 1000f, tt)
                    ring = seg(650f, 1050f, tt)
                    scroll = 0f
                    tip = "左键"
                    tipA = seg(700f, 850f, tt) * (1f - seg(1900f, 2100f, tt))
                }
                GestureIcon.SCROLL -> {
                    val k = smooth(seg(450f, 1250f, tt))
                    val y = lerp(212f, 152f, k)
                    put(0, 136f, y)
                    put(1, 164f, y)
                    fingerA = seg(200f, 400f, tt) * (1f - seg(1250f, 1500f, tt))
                    fingerP = 0.5f
                    scroll = 34f * k + 20f * ease(seg(1250f, 2000f, tt)) // 松手后还顺着滑一段
                    tip = "滚动"
                    tipA = seg(500f, 700f, tt) * (1f - seg(1900f, 2100f, tt))
                }
                GestureIcon.RIGHT -> {
                    put(0, 138f, 182f)
                    put(1, 162f, 182f)
                    fingerA = seg(200f, 400f, tt) * (1f - seg(1000f, 1250f, tt))
                    fingerP = hold(tt, 600f, 700f)
                    padRing = seg(600f, 950f, tt)
                    menu = pop(seg(700f, 1000f, tt))
                    menuA = seg(700f, 850f, tt) * (1f - seg(2100f, 2350f, tt))
                    tip = "右键"
                    tipA = seg(750f, 900f, tt) * (1f - seg(1900f, 2100f, tt))
                }
                GestureIcon.MIDDLE -> {
                    put(0, 126f, 184f)
                    put(1, 150f, 180f)
                    put(2, 174f, 184f)
                    fingerA = seg(200f, 400f, tt) * (1f - seg(1000f, 1250f, tt))
                    fingerP = hold(tt, 600f, 700f)
                    padRing = seg(600f, 950f, tt)
                    ring = seg(600f, 1000f, tt)
                    tip = "中键"
                    tipA = seg(650f, 800f, tt) * (1f - seg(1900f, 2100f, tt))
                }
                GestureIcon.DRAG -> {
                    val k = smooth(seg(700f, 1700f, tt))
                    val x = lerp(128f, 188f, k)
                    val y = lerp(200f, 176f, k)
                    put(0, x, y)
                    fingerA = seg(100f, 250f, tt) * (1f - seg(1950f, 2200f, tt))
                    // 点一下，再按住拖
                    fingerP = max(hold(tt, 310f, 360f), seg(470f, 520f, tt) * (1f - seg(1800f, 1880f, tt)))
                    padRing = seg(310f, 650f, tt)
                    val reset = seg(2350f, 2550f, tt)
                    if (tt < 2550f) {
                        curX = 150f + (x - 128f) * 0.6f
                        curY = 40f + (y - 200f) * 0.6f
                        curA = 1f - reset
                    } else {
                        curX = 104f
                        curY = 66f
                        curA = seg(2550f, 2700f, tt)
                    }
                    fileA = seg(0f, 250f, tt) * (1f - reset)
                    scroll = 54f * (1f - ease(seg(2350f, 2650f, tt)))
                    tip = "拖动"
                    tipA = seg(750f, 900f, tt) * (1f - seg(1900f, 2100f, tt))
                }
            }

            // 显示器
            c.save()
            c.translate(0f, -8f * (1f - intro))
            fill.color = Ui.alpha(palette.tile, intro)
            rr(74f, 6f, 226f, 94f, 7f, fill)
            line.color = Ui.alpha(palette.fg, intro)
            line.strokeWidth = 2f
            rr(74f, 6f, 226f, 94f, 7f, line)
            fill.color = Ui.alpha(palette.bg, intro)
            rr(79f, 11f, 221f, 89f, 3f, fill)
            fill.color = Ui.alpha(palette.fg, intro)
            rr(144f, 94f, 156f, 102f, 1f, fill)
            rr(126f, 101f, 174f, 105f, 2f, fill)
            c.save()
            c.clipRect(79f, 11f, 221f, 89f)
            fill.color = Ui.alpha(palette.tile, intro)
            rr(96f, 17f - scroll, 204f, 160f - scroll, 2.5f, fill)
            for (i in DOC.indices) {
                val y = 25f + 8f * i - scroll
                if (y < 6f || y > 92f) continue
                fill.color = Ui.alpha(palette.fg, (if (i == 0) 0.32f else 0.13f) * intro)
                rr(104f, y, 104f + DOC[i] * 0.9f, y + if (i == 0) 4.4f else 3.2f, 1.6f, fill)
            }
            if (fileA > 0f) file(curX, curY, fileA)
            if (menuA > 0f) {
                val s = lerp(0.6f, 1f, menu)
                c.save()
                c.scale(s, s, 152f, 42f)
                fill.color = Ui.alpha(palette.tile, menuA)
                rr(152f, 42f, 204f, 82f, 4f, fill)
                line.color = Ui.alpha(palette.line, menuA)
                line.strokeWidth = 0.8f
                rr(152f, 42f, 204f, 82f, 4f, line)
                fill.color = Ui.alpha(palette.fg, 0.24f * menuA)
                for (i in MENU.indices) rr(159f, 49.5f + 8.5f * i, 159f + MENU[i], 52.3f + 8.5f * i, 1.4f, fill)
                c.restore()
            }
            ripple(curX, curY, ring, 2f, 12f)
            if (tip != null) {
                if (g == GestureIcon.MOVE) bubble(tip, curX + 15f, curY + 9f, tipA, Paint.Align.LEFT)
                else bubble(tip, curX - 7f, curY + 8f, tipA, Paint.Align.RIGHT)
            }
            cursor(curX, curY, curA * intro, 0.78f)
            c.restore()
            c.restore()

            // 触控板：手指附近的点亮起来
            c.save()
            c.translate(0f, 10f * (1f - intro))
            fill.color = Ui.alpha(palette.tile, intro)
            rr(58f, 118f, 242f, 234f, 14f, fill)
            line.color = Ui.alpha(palette.line, intro)
            line.strokeWidth = 1.2f
            rr(58f, 118f, 242f, 234f, 14f, line)
            val dim = Ui.alpha(palette.mut, 0.5f * intro)
            val lit = Ui.alpha(palette.fg, 0.8f * intro)
            for (gx in 0 until 17) for (gy in 0 until 9) {
                val x = 70f + 10f * gx
                val y = 128f + 10f * gy
                var glow = 0f
                for (i in 0 until fingers) {
                    val d = hypot(x - fx[i], y - fy[i])
                    if (d < 26f) glow = max(glow, (1f - d / 26f) * (1f - d / 26f))
                }
                glow *= fingerA
                fill.color = Ui.blend(dim, lit, glow)
                c.drawCircle(x, y, 0.9f + 1.1f * glow, fill)
            }
            textMid("触控板", 150f, 223f, 6f, 750, Ui.alpha(palette.mut, intro))
            for (i in 0 until fingers) {
                ripple(fx[i], fy[i], padRing, 8f, 20f)
                finger(fx[i], fy[i], fingerP, fingerA)
            }
            c.restore()
        }

        /** 被拖着走的文件，鼠标尖在它中间偏上 */
        private fun file(x: Float, y: Float, a: Float) {
            val l = x - 7f
            val top = y - 5f
            val r = x + 6f
            val b = y + 11f
            path.reset()
            path.moveTo(l, top)
            path.lineTo(r - 4f, top)
            path.lineTo(r, top + 4f)
            path.lineTo(r, b)
            path.lineTo(l, b)
            path.close()
            fill.color = Ui.alpha(palette.tile, a)
            c.drawPath(path, fill)
            line.color = Ui.alpha(palette.fg, a)
            line.strokeWidth = 1f
            c.drawPath(path, line)
            fill.color = Ui.alpha(palette.fg, 0.35f * a)
            rr(l + 2.5f, top + 7f, r - 2.5f, top + 8.2f, 0.6f, fill)
            rr(l + 2.5f, top + 10f, r - 4f, top + 11.2f, 0.6f, fill)
        }
    }

    /** 按键：点「复制」→ 电脑按 Ctrl+C；按住「删除」一直删；点「撤销」撤回来，长按「撤销」是另一个快捷键 */
    @SuppressLint("ViewConstructor")
    private inner class KeysScene(ctx: Context) : Scene(ctx, 1700f) {
        private val sentence = "明天上午十点开会"
        private val deletes = floatArrayOf(2380f, 2780f, 2930f, 3080f, 3230f, 3380f)
        private val copy = arrayOf("Ctrl", "C")
        private val backspace = arrayOf("Backspace")
        private val undo = arrayOf("Ctrl", "Z")
        private val redo = arrayOf("Ctrl", "Y")

        override fun render() {
            val tt = loop(400f, 8800f)
            // 电脑上的输入框
            val fa = ease(seg(0f, 320f))
            c.save()
            c.translate(0f, -6f * (1f - fa))
            fill.color = Ui.alpha(palette.tile, fa)
            rr(36f, 8f, 264f, 44f, 10f, fill)
            line.color = Ui.alpha(palette.line, fa)
            line.strokeWidth = 1.2f
            rr(36f, 8f, 264f, 44f, 10f, line)
            legend(58f, 8f, fa)
            var n = sentence.length
            if (tt >= 2380f && tt < 4220f) n -= deletes.count { tt >= it }
            if (tt >= 5400f) n = 2
            var ta = fa
            if (tt >= 8300f) {
                if (tt < 8500f) {
                    ta *= 1f - seg(8300f, 8500f, tt)
                } else {
                    n = sentence.length
                    ta *= seg(8500f, 8700f, tt)
                }
            }
            val shown = sentence.substring(0, n)
            textMid(shown, 49f, 26f, 10f, 750, Ui.alpha(palette.fg, ta), Paint.Align.LEFT)
            if (tt in 2310f..3550f || t % 1060f < 600f) {
                val x = 49f + textWidth(shown, 10f, 750) + 1.5f
                fill.color = Ui.alpha(palette.fg, fa)
                c.drawRect(x, 19.5f, x + 1.2f, 32.5f, fill)
            }
            c.restore()

            // 电脑收到的快捷键
            when {
                tt >= 920f && tt < 1900f -> chip(copy, null, seg(920f, 1060f, tt) * (1f - seg(1700f, 1900f, tt)), pop(seg(920f, 1220f, tt)), 0f)
                tt >= 2380f && tt < 3800f -> {
                    var pulse = 0f
                    for (d in deletes) if (tt >= d) pulse = 1f - seg(d, d + 160f, tt)
                    chip(backspace, null, seg(2380f, 2520f, tt) * (1f - seg(3600f, 3800f, tt)), pop(seg(2380f, 2680f, tt)), pulse)
                }
                tt >= 4220f && tt < 5000f -> chip(undo, null, seg(4220f, 4360f, tt) * (1f - seg(4800f, 5000f, tt)), pop(seg(4220f, 4520f, tt)), 0f)
                tt >= 5400f && tt < 6600f -> chip(redo, "长按 ·", seg(5400f, 5540f, tt) * (1f - seg(6400f, 6600f, tt)), pop(seg(5400f, 5700f, tt)), 0f)
            }

            // 手机上的四个键
            key(44f, 86f, 144f, 146f, "复制", "Ctrl+C", null, hold(tt, 900f, 980f), ease(seg(80f, 380f)))
            key(156f, 86f, 256f, 146f, "粘贴", "Ctrl+V", null, 0f, ease(seg(140f, 440f)))
            key(44f, 158f, 144f, 218f, "删除", "Backspace", null, hold(tt, 2350f, 3550f), ease(seg(200f, 500f)))
            key(156f, 158f, 256f, 218f, "撤销", "Ctrl+Z", "长按", max(hold(tt, 4200f, 4300f), hold(tt, 4920f, 5700f)), ease(seg(260f, 560f)))
            bubble("按住不放，一直删", 94f, 230f, seg(2650f, 2850f, tt) * (1f - seg(3600f, 3800f, tt)), size = 7.5f)

            // 手指：复制 → 删除 → 撤销 → 离开
            val px: Float
            val py: Float
            when {
                tt < 1900f -> {
                    val k = ease(seg(300f, 850f, tt))
                    px = lerp(190f, 94f, k)
                    py = lerp(262f, 118f, k)
                }
                tt < 3650f -> {
                    px = 94f
                    py = lerp(118f, 190f, ease(seg(1900f, 2300f, tt)))
                }
                tt < 5800f -> {
                    px = lerp(94f, 206f, ease(seg(3650f, 4150f, tt)))
                    py = 190f
                }
                else -> {
                    val k = ease(seg(5800f, 6300f, tt))
                    px = lerp(206f, 232f, k)
                    py = lerp(190f, 262f, k)
                }
            }
            // 长按：手指外面一圈慢慢走满
            val ringA = seg(4920f, 5000f, tt) * (1f - seg(5400f, 5650f, tt))
            if (ringA > 0f) {
                line.color = Ui.alpha(palette.fg, 0.85f * ringA)
                line.strokeWidth = 2.2f
                box.set(px - 17f, py - 17f, px + 17f, py + 17f)
                c.drawArc(box, -90f, 360f * seg(4920f, 5400f, tt), false, line)
            }
            ripple(206f, 190f, seg(5400f, 5800f, tt), 16f, 30f)
            val fp = maxOf(hold(tt, 900f, 980f), hold(tt, 2350f, 3550f), max(hold(tt, 4200f, 4300f), hold(tt, 4920f, 5700f)))
            finger(px, py, fp, seg(250f, 500f, tt) * (1f - seg(5850f, 6250f, tt)))
        }

        private fun key(l: Float, top: Float, r: Float, b: Float, label: String, sub: String, corner: String?, p: Float, a: Float) {
            if (a <= 0f) return
            val s = (1f - 0.07f * p) * lerp(0.94f, 1f, a)
            val cx = (l + r) / 2f
            val cy = (top + b) / 2f
            c.save()
            c.scale(s, s, cx, cy)
            c.translate(0f, 6f * (1f - a))
            fill.color = Ui.alpha(Ui.blend(palette.tile, palette.fg, p), a)
            rr(l, top, r, b, 14f, fill)
            if (p < 1f) {
                line.color = Ui.alpha(palette.line, a * (1f - p))
                line.strokeWidth = 1f
                rr(l, top, r, b, 14f, line)
            }
            val subInk = Ui.alpha(Ui.blend(palette.sub, palette.tile, p * 0.75f), a)
            textMid(label, cx, cy - 4f, 12f, 850, Ui.alpha(Ui.blend(palette.fg, palette.tile, p), a))
            textMid(sub, cx, cy + 12f, 6f, 750, subInk)
            if (corner != null) {
                val w = textWidth(corner, 5f, 850) + 7f
                fill.color = Ui.alpha(Ui.blend(palette.fg, palette.tile, p), 0.08f * a)
                rr(r - 7f - w, top + 6f, r - 7f, top + 15f, 4.5f, fill)
                textMid(corner, r - 7f - w / 2f, top + 10.5f, 5f, 850, subInk)
            }
            c.restore()
        }

        /** 一排键帽：Ctrl + C */
        private fun chip(keys: Array<String>, prefix: String?, a: Float, k: Float, pulse: Float) {
            if (a <= 0f) return
            val size = 7f
            val gap = 4f
            val plus = textWidth("+", size, 850)
            var w = 0f
            if (prefix != null) w += textWidth(prefix, size, 850) + gap
            keys.forEachIndexed { i, key ->
                if (i > 0) w += plus + 2f * gap
                w += textWidth(key, size, 850) + 10f
            }
            var x = 150f - w / 2f
            val y = 64f
            val s = lerp(0.85f, 1f, k) * (1f + 0.06f * pulse)
            val grey = Ui.alpha(palette.sub, a)
            c.save()
            c.scale(s, s, 150f, y)
            if (prefix != null) {
                textMid(prefix, x, y, size, 850, grey, Paint.Align.LEFT)
                x += textWidth(prefix, size, 850) + gap
            }
            keys.forEachIndexed { i, key ->
                if (i > 0) {
                    textMid("+", x + gap + plus / 2f, y, size, 850, grey)
                    x += plus + 2f * gap
                }
                val kw = textWidth(key, size, 850) + 10f
                fill.color = Ui.alpha(palette.line, a)
                rr(x, y - 5.7f, x + kw, y + 8.3f, 3.5f, fill)
                fill.color = Ui.alpha(palette.tile, a)
                rr(x, y - 7f, x + kw, y + 7f, 3.5f, fill)
                line.color = Ui.alpha(palette.line, a)
                line.strokeWidth = 0.8f
                rr(x, y - 7f, x + kw, y + 7f, 3.5f, line)
                textMid(key, x + kw / 2f, y, size, 850, Ui.alpha(palette.fg, a))
                x += kw
            }
            c.restore()
        }
    }

    /** 语音：按住说话，字打到电脑上；往上滑锁定后松手接着说，点一下结束 */
    @SuppressLint("ViewConstructor")
    private inner class VoiceScene(ctx: Context) : Scene(ctx, 6200f) {
        private val first = "明天上午十点开会"
        private val second = "记得带上电脑"
        private val changes = floatArrayOf(950f, 3100f, 3900f, 4700f, 7500f)

        override fun render() {
            val tt = loop(400f, 10400f)
            val fa = ease(seg(0f, 320f))
            // 电脑上的输入框：说一句打一句
            c.save()
            c.translate(0f, -6f * (1f - fa))
            fill.color = Ui.alpha(palette.tile, fa)
            rr(30f, 6f, 270f, 74f, 10f, fill)
            line.color = Ui.alpha(palette.line, fa)
            line.strokeWidth = 1.2f
            rr(30f, 6f, 270f, 74f, 10f, line)
            legend(52f, 6f, fa)
            var s1 = first.substring(0, (first.length * seg(1300f, 2900f, tt)).toInt())
            if (tt >= 3100f) s1 += "。"
            var s2 = second.substring(0, (second.length * seg(5000f, 6700f, tt)).toInt())
            if (tt >= 7550f) s2 += "。"
            if (tt >= 10000f) {
                s1 = ""
                s2 = ""
            }
            val ink = Ui.alpha(palette.fg, fa * (1f - seg(9700f, 10000f, tt)))
            textMid(s1, 44f, 28f, 10f, 750, ink, Paint.Align.LEFT)
            textMid(s2, 44f, 52f, 10f, 750, ink, Paint.Align.LEFT)
            val rec1 = tt >= 950f && tt < 3100f
            val rec2 = tt >= 3900f && tt < 7500f
            if (rec1 || rec2 || t % 1060f < 600f) {
                val onSecond = s2.isNotEmpty()
                val x = 44f + textWidth(if (onSecond) s2 else s1, 10f, 750) + 1.5f
                val cy = if (onSecond) 52f else 28f
                fill.color = Ui.alpha(palette.fg, fa)
                c.drawRect(x, cy - 6.5f, x + 1.2f, cy + 6.5f, fill)
            }
            c.restore()

            // 手机上的语音键
            val ba = ease(seg(120f, 440f))
            val latched = tt >= 4700f && tt < 7500f
            val lk = seg(4650f, 4800f, tt) * (1f - seg(7500f, 7700f, tt))
            c.save()
            c.translate(0f, 8f * (1f - ba))
            fill.color = Ui.alpha(Ui.blend(palette.tile, palette.red, 0.06f * lk), ba)
            rr(30f, 104f, 270f, 230f, 18f, fill)
            line.color = Ui.alpha(Ui.blend(palette.line, palette.red, lk), ba)
            line.strokeWidth = 1.2f + 0.5f * lk
            rr(30f, 104f, 270f, 230f, 18f, line)
            text("语音输入", 46f, 124f, 8f, 850, Ui.alpha(palette.sub, ba))
            if (rec1 || rec2) {
                val since = tt - if (rec1) 950f else 3900f
                fill.color = Ui.alpha(palette.red, ba * (0.6f + 0.2f * (1f + wave(900f))))
                c.drawCircle(229f, 121.3f, 2.6f, fill)
                text("0:0${(since / 1000f).toInt()}", 235f, 124f, 7f, 850, Ui.alpha(palette.red, ba))
            }
            // 锁：第二次按住时出现，滑上去锁住变红
            val lockA = seg(3900f, 4100f, tt) * (1f - seg(7500f, 7800f, tt))
            if (lockA > 0f) {
                val tone = Ui.blend(palette.fg, palette.red, lk)
                fill.color = Ui.alpha(tone, 0.08f * lockA * ba)
                c.drawCircle(150f, 121f, 10.5f, fill)
                lock(150f, 121f, Ui.alpha(tone, lockA * ba))
                val up = seg(4000f, 4200f, tt) * (1f - seg(4550f, 4700f, tt))
                if (up > 0f) {
                    line.color = Ui.alpha(palette.sub, up * ba)
                    line.strokeWidth = 1.3f
                    val ay = 141f - 4f * ((tt % 700f) / 700f)
                    path.reset()
                    path.moveTo(146f, ay + 3f)
                    path.lineTo(150f, ay - 1f)
                    path.lineTo(154f, ay + 3f)
                    c.drawPath(path, line)
                }
            }
            meter(tt, ba)
            val hint = when {
                latched -> "已锁定 · 说完点一下结束"
                rec1 || rec2 -> "正在听…松手发送"
                else -> "按住说话 · 上滑锁定"
            }
            var near = 1f
            for (x in changes) near = min(near, seg(0f, 140f, abs(tt - x)))
            textMid(hint, 150f, 215f, 7f, 750, Ui.alpha(if (latched) palette.red else palette.sub, ba * near))
            c.restore()

            // 手指：按住说 → 按住往上滑到锁上、松手 → 点一下结束
            val f1 = seg(650f, 900f, tt) * (1f - seg(3200f, 3450f, tt))
            if (f1 > 0f) {
                val k = ease(seg(600f, 920f, tt))
                finger(lerp(232f, 150f, k), lerp(262f, 172f, k), hold(tt, 950f, 3100f), f1)
            }
            val f2 = seg(3600f, 3850f, tt) * (1f - seg(4850f, 5150f, tt))
            if (f2 > 0f) finger(150f, lerp(172f, 121f, ease(seg(4200f, 4700f, tt))), hold(tt, 3900f, 4800f), f2)
            val f3 = seg(7100f, 7300f, tt) * (1f - seg(7800f, 8100f, tt))
            if (f3 > 0f) finger(150f, 172f, hold(tt, 7500f, 7580f), f3)
            ripple(150f, 172f, seg(7500f, 7900f, tt), 10f, 28f)
        }

        /** 音量条：每 70ms 一格，从右往左走；说话时高、没说话时矮、没在录时是一排小点 */
        private fun meter(tt: Float, a: Float) {
            c.save()
            c.clipRect(46f, 140f, 254f, 194f)
            val step = 70f
            val slot = floor(tt / step)
            val shift = (tt - slot * step) / step * 7.6f
            val on = Ui.alpha(palette.fg, a)
            val off = Ui.alpha(palette.mut, 0.55f * a)
            for (i in 0 until 27) {
                val n = slot - 26 + i
                val lvl = level(n * step, n)
                val x = 52f + 7.6f * i - shift
                val h = 2.6f + 30f * lvl
                fill.color = if (lvl > 0f) on else off
                rr(x - 1.6f, 166f - h / 2f, x + 1.6f, 166f + h / 2f, 1.6f, fill)
            }
            c.restore()
        }

        private fun level(at: Float, n: Float): Float {
            val rec = at >= 950f && at < 3100f || at >= 3900f && at < 7500f
            if (!rec) return 0f
            val talk = at >= 1250f && at < 2950f || at >= 4950f && at < 6750f
            val x = sin(n * 12.9898f) * 43758.547f
            val r = x - floor(x)
            return if (talk) 0.3f + 0.7f * r else 0.06f + 0.08f * r
        }

        private fun lock(x: Float, y: Float, color: Int) {
            fill.color = color
            rr(x - 3.8f, y - 1f, x + 3.8f, y + 4.6f, 1.2f, fill)
            line.color = color
            line.strokeWidth = 1.2f
            box.set(x - 2.4f, y - 6f, x + 2.4f, y - 1.2f)
            c.drawArc(box, 180f, 180f, false, line)
            c.drawLine(x - 2.4f, y - 3.6f, x - 2.4f, y - 1f, line)
            c.drawLine(x + 2.4f, y - 3.6f, x + 2.4f, y - 1f, line)
        }
    }

    /** 右上角的「⋯」：点开是一张面板，调布局、暂停按键、换电脑、再看教程都在里面 */
    @SuppressLint("ViewConstructor")
    private inner class MenuScene(ctx: Context) : Scene(ctx, 3100f) {
        private val rows = arrayOf("调整布局", "暂停按键", "选择电脑", "使用教程")
        private val screenClip = Path()

        override fun render() {
            val intro = ease(seg(0f, 500f))
            val tt = loop(500f, 6000f)
            c.save()
            c.translate(0f, 12f * (1f - intro))
            phoneBody(80f, 4f, 220f, 236f, 18f, intro, 5f)
            screenClip.reset()
            screenClip.addRoundRect(85f, 9f, 215f, 231f, 13f, 13f, Path.Direction.CW)
            c.save()
            c.clipPath(screenClip)
            // 主界面：顶栏、触控板、语音键
            text("蒜鸟", 95f, 28f, 9f, 900, Ui.alpha(palette.fg, intro))
            val tap = hold(tt, 1100f, 1270f)
            if (tap > 0f) {
                fill.color = Ui.alpha(palette.fg, 0.08f * tap)
                c.drawCircle(202f, 24.5f, 8f, fill)
            }
            fill.color = Ui.alpha(palette.fg, intro)
            for (i in -1..1) c.drawCircle(202f + 4.5f * i, 24.5f, 1.4f, fill)
            fill.color = Ui.alpha(palette.tile, intro)
            rr(92f, 40f, 208f, 142f, 10f, fill)
            fill.color = Ui.alpha(palette.mut, 0.4f * intro)
            for (gx in 0 until 11) for (gy in 0 until 9) c.drawCircle(100f + 10f * gx, 48f + 11f * gy, 0.8f, fill)
            fill.color = Ui.alpha(palette.tile, intro)
            rr(92f, 150f, 208f, 222f, 10f, fill)
            mic(150f, 180f, Ui.alpha(palette.fg, intro), 1.2f)
            textMid("按住说话", 150f, 206f, 6f, 750, Ui.alpha(palette.sub, intro))
            // 面板从下面滑上来，高亮在几行之间挪
            val up = ease(seg(1250f, 1650f, tt)) * (1f - ease(seg(5000f, 5350f, tt)))
            if (up > 0f) {
                fill.color = Ui.alpha(0xFF000000.toInt(), 0.3f * up)
                c.drawRect(85f, 9f, 215f, 231f, fill)
                val y = lerp(236f, 112f, up)
                fill.color = palette.tile
                rr(85f, y, 215f, y + 140f, 12f, fill)
                fill.color = Ui.alpha(palette.fg, 0.15f)
                rr(140f, y + 6f, 160f, y + 8.5f, 1.25f, fill)
                val ha = seg(1900f, 2050f, tt) * (1f - seg(4800f, 4950f, tt))
                if (ha > 0f) {
                    val hp = (tt - 1900f) / 750f
                    val at = (floor(hp) - 1f + ease(seg(0f, 220f, (hp - floor(hp)) * 750f))).coerceIn(0f, 3f)
                    val hy = y + 26f + 24f * at
                    fill.color = Ui.alpha(palette.fg, 0.07f * ha)
                    rr(89f, hy - 10f, 211f, hy + 10f, 7f, fill)
                }
                rows.forEachIndexed { i, s ->
                    val ry = y + 26f + 24f * i
                    textMid(s, 98f, ry, 8f, 750, palette.fg, Paint.Align.LEFT)
                    chevron(203f, ry, 1f)
                }
            }
            c.restore()
            val fa = seg(350f, 550f, tt) * (1f - seg(1350f, 1650f, tt))
            if (fa > 0f) {
                val k = ease(seg(400f, 1050f, tt))
                finger(lerp(176f, 202f, k), lerp(205f, 26f, k), tap, fa, 0.85f)
            }
            c.restore()
        }
    }

    private companion object {
        const val KEY_SEEN = "tutorial_seen"

        /** 和 MainActivity 里的同名：看过触控板那页就不再弹手势说明 */
        const val KEY_GUIDE_SEEN = "gesture_guide_seen"

        const val PAD = 4
        const val LAST = 8
        const val PAGES = LAST + 1

        /** 触控板每个手势演示多久 */
        const val G = 2700f

        const val CODE = "385 216"

        val GESTURES = listOf(
            Triple(GestureIcon.MOVE, "单指滑动", "移动鼠标"),
            Triple(GestureIcon.TAP, "单指轻点", "左键"),
            Triple(GestureIcon.SCROLL, "双指滑动", "滚动"),
            Triple(GestureIcon.RIGHT, "双指轻点", "右键"),
            Triple(GestureIcon.MIDDLE, "三指轻点", "中键"),
            Triple(GestureIcon.DRAG, "点一下再按住拖", "拖动"),
        )

        /** 选电脑那幅画里另外两台 */
        val OTHERS = arrayOf("客厅的电脑", "办公室电脑")

        /** 触控板那幅画里文档每行的长短、右键菜单每行的长短 */
        val DOC = floatArrayOf(52f, 92f, 84f, 96f, 60f, 88f, 94f, 72f, 90f, 80f, 96f, 66f, 86f, 78f, 92f, 70f)
        val MENU = floatArrayOf(34f, 28f, 38f, 24f)

        val SPARKS = floatArrayOf(96f, 30f, 212f, 212f, 30f, 150f, 270f, 132f)
        val BUBBLES = floatArrayOf(54f, 92f, 246f, 62f, 238f, 170f)

        /** 彩纸用 logo 里的颜色 */
        val CONFETTI = intArrayOf(0xFF9BE03A.toInt(), 0xFFC47BD6.toInt(), 0xFFFF8AA2.toInt(), 0xFFFFB84E.toInt(), 0xFF5DB4F0.toInt())
    }
}
