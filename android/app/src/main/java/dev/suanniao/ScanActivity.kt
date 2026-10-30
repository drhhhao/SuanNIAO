package dev.suanniao

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PathMeasure
import android.graphics.RectF
import android.graphics.Shader
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.graphics.PathParser
import com.google.zxing.BarcodeFormat
import com.google.zxing.ResultPoint
import com.journeyapps.barcodescanner.BarcodeCallback
import com.journeyapps.barcodescanner.BarcodeResult
import com.journeyapps.barcodescanner.BarcodeView
import com.journeyapps.barcodescanner.DefaultDecoderFactory
import com.journeyapps.barcodescanner.Size
import kotlin.math.PI
import kotlin.math.min
import kotlin.math.sin

/**
 * 扫电脑上二维码的界面（蒜鸟自己画的，相机和识别用 zxing-android-embedded 的 BarcodeView）。
 * 相机画面上一层深色遮罩挖出圆角取景框，青柠四角 + 一条从上往下扫的光带；上面的小标签说这次扫码做什么
 * （蓝牙配对 / 局域网 / 两种），下面说怎么做，底下「从相册选」，右上角手电筒。
 * - 进场：遮罩变暗、四个角从外面收进来轻轻回弹，上下的字和按钮上浮淡入，然后开始扫
 * - 认出蒜鸟的码：框里闪一下青柠、四个角往里收、中间弹出对勾，提示换成电脑名，停一下再淡出回去（结果给 MainActivity）
 * - 别的码：取景框抖一下、四个角变白、提示「这不是蒜鸟的二维码」，1.5 秒后恢复接着扫
 * - 8 秒没扫到：提示换成办法
 * 画面只动位置、大小、透明度（见 ScanOverlay），120Hz 下也顺
 */
class ScanActivity : Activity() {

    private val main = Handler(Looper.getMainLooper())
    private val density by lazy { resources.displayMetrics.density }
    private fun dp(v: Float) = (v * density).toInt()
    private fun dpf(v: Float) = v * density

    private lateinit var root: Layout
    private lateinit var barcode: BarcodeView
    private lateinit var scanOverlay: ScanOverlay
    private lateinit var topBar: FrameLayout
    private lateinit var torch: FrameLayout
    private lateinit var torchIcon: Icon
    private lateinit var chip: LinearLayout
    private lateinit var hint: LinearLayout
    private lateinit var hintIcon: Icon
    private lateinit var hintTitle: TextView
    private lateinit var hintSub: TextView
    private lateinit var album: LinearLayout

    private var torchOn = false
    private var done = false
    private var wrongText: String? = null
    private var wrongUntil = 0L
    private var insetTop = 0
    private var insetBottom = 0

    private val idleHint get() = Hint("对准电脑上的二维码", "电脑蒜鸟里点「显示二维码」", null, Color.WHITE)
    private val slowTask = Runnable { if (!done) setHint(Hint("离近一点，或把屏幕调亮", "二维码要完整放进框里", null, Color.WHITE)) }
    private val restoreTask = Runnable {
        if (!done) {
            scanOverlay.wrongEnd()
            setHint(idleHint)
        }
    }

    private data class Hint(val title: String, val sub: String, val icon: Array<String>?, val iconColor: Int)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        requestHighestRefreshRate()
        root = Layout(this)
        root.setBackgroundColor(Color.BLACK)

        barcode = BarcodeView(this).apply {
            decoderFactory = DefaultDecoderFactory(listOf(BarcodeFormat.QR_CODE))
            // 相机画面用 SurfaceView：系统单独一层合成。默认的 TextureView 要进蒜鸟自己的渲染线程，
            // 相机每来一帧都跟着重画，小米 13 上扫码界面只有每秒 90 帧（3 帧占 4 个刷新周期）
            isUseTextureView = false
        }
        root.addView(barcode, FrameLayout.LayoutParams(-1, -1))
        scanOverlay = ScanOverlay(this)
        root.addView(scanOverlay, FrameLayout.LayoutParams(-1, -1))

        // 顶栏：关闭、标题、手电筒
        topBar = FrameLayout(this)
        topBar.addView(round(Icon(this, X)) { leave(RESULT_CANCELED, null) }, FrameLayout.LayoutParams(dp(44f), dp(44f), Gravity.START or Gravity.CENTER_VERTICAL))
        topBar.addView(text("扫码连接电脑", 17.5f, 900, Color.WHITE).apply { gravity = Gravity.CENTER },
            FrameLayout.LayoutParams(-1, -1))
        torchIcon = Icon(this, TORCH)
        torch = round(torchIcon) { setTorch(!torchOn) }
        if (packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_FLASH)) {
            topBar.addView(torch, FrameLayout.LayoutParams(dp(44f), dp(44f), Gravity.END or Gravity.CENTER_VERTICAL))
        }
        root.addView(topBar, FrameLayout.LayoutParams(-1, dp(44f)))

        // 取景框上面：这次扫码做什么
        val mode = intent.getStringExtra(EXTRA_MODE)
        chip = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.round(Ui.alpha(0xFFC3F06A.toInt(), 0.16f), dpf(16f), dp(0.8f), Ui.alpha(0xFFC3F06A.toInt(), 0.38f))
            setPadding(dp(10f), 0, dp(13f), 0)
            addView(Icon(this@ScanActivity, if (mode == "lan") WIFI else BLUETOOTH, CHIP_INK), LinearLayout.LayoutParams(dp(16f), dp(16f)))
            addView(text(intent.getStringExtra(EXTRA_LABEL) ?: "扫码连接", 13.5f, 850, CHIP_INK),
                LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginStart = dp(6f) })
        }
        root.addView(chip, FrameLayout.LayoutParams(WRAP_CONTENT, dp(32f)))

        // 取景框下面：怎么做
        hint = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
        }
        hintIcon = Icon(this, ALERT)
        hintTitle = text("", 18f, 850, Color.WHITE)
        hint.addView(LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(hintIcon, LinearLayout.LayoutParams(dp(19f), dp(19f)).apply { marginEnd = dp(6f) })
            addView(hintTitle)
        }, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
        hintSub = text("", 13.5f, 700, Ui.alpha(Color.WHITE, 0.62f))
        hint.addView(hintSub, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { topMargin = dp(8f) })
        root.addView(hint, FrameLayout.LayoutParams(-1, WRAP_CONTENT))
        showHint(idleHint)

        // 底下：从相册选（不想开相机、截了图的）
        album = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.round(Ui.alpha(Color.WHITE, 0.14f), dpf(24f), dp(0.8f), Ui.alpha(Color.WHITE, 0.16f))
            setPadding(dp(15f), 0, dp(19f), 0)
            addView(Icon(this@ScanActivity, IMAGE), LinearLayout.LayoutParams(dp(20f), dp(20f)))
            addView(text("从相册选", 14.5f, 850, Color.WHITE), LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginStart = dp(8f) })
            Ui.springy(this)
            setOnClickListener { leave(RESULT_OK, Intent().putExtra(EXTRA_ALBUM, true)) }
        }
        root.addView(album, FrameLayout.LayoutParams(WRAP_CONTENT, dp(48f)))

        root.setOnApplyWindowInsetsListener { _, insets ->
            val bars = if (Build.VERSION.SDK_INT >= 30) insets.getInsets(WindowInsets.Type.systemBars()) else null
            @Suppress("DEPRECATION")
            insetTop = bars?.top ?: insets.systemWindowInsetTop
            @Suppress("DEPRECATION")
            insetBottom = bars?.bottom ?: insets.systemWindowInsetBottom
            root.requestLayout()
            insets
        }
        setContentView(root)
        edgeToEdge() // 要在 setContentView 之后：之前 decorView 还没建好，拿不到 insetsController

        barcode.decodeContinuous(object : BarcodeCallback {
            override fun barcodeResult(result: BarcodeResult) = onCode(result.text)
            override fun possibleResultPoints(resultPoints: MutableList<ResultPoint>) {}
        })
        enter()
        if (BuildConfig.DEBUG) intent.getStringExtra(EXTRA_DEBUG_FAKE)?.let { fake -> main.postDelayed({ onCode(fake) }, 1500) }
    }

    override fun onResume() {
        super.onResume()
        barcode.resume()
        if (!done) {
            main.removeCallbacks(slowTask)
            main.postDelayed(slowTask, SLOW_MS)
        }
        scanOverlay.running = true
    }

    override fun onPause() {
        main.removeCallbacks(slowTask)
        scanOverlay.running = false
        if (torchOn) setTorch(false)
        barcode.pause()
        super.onPause()
    }

    @Deprecated("返回键")
    override fun onBackPressed() = leave(RESULT_CANCELED, null)

    // ───────── 扫到了 ─────────

    private fun onCode(text: String?) {
        if (done || text == null) return
        val qr = LanQr.parse(text)
        if (qr != null) {
            done = true
            barcode.stopDecoding()
            main.removeCallbacks(slowTask)
            main.removeCallbacks(restoreTask)
            Ui.buzz(root, Ui.Buzz.CONFIRM)
            scanOverlay.found()
            val doing = when (intent.getStringExtra(EXTRA_MODE)) {
                "bt" -> "正在配对…"
                "lan" -> "正在绑定…"
                else -> "正在连接…"
            }
            setHint(Hint("认出来了", "${qr.name} · $doing", CHECK, LIME))
            // 停一下让人看清，再淡出回去
            val dry = intent.hasExtra(EXTRA_DEBUG_FAKE) // 调试的假扫码：只看动画，不真去连
            main.postDelayed({ leave(if (dry) RESULT_CANCELED else RESULT_OK, Intent().putExtra(EXTRA_TEXT, text)) }, FOUND_HOLD_MS)
            return
        }
        val now = SystemClock.uptimeMillis()
        if (text == wrongText && now < wrongUntil) return // 一直对着同一个别的码：不反复抖
        wrongText = text
        wrongUntil = now + WRONG_MS
        Ui.buzz(root, Ui.Buzz.REJECT)
        scanOverlay.wrong()
        setHint(Hint("这不是蒜鸟的二维码", "电脑蒜鸟里点「显示二维码」", ALERT, Color.WHITE))
        main.removeCallbacks(restoreTask)
        main.postDelayed(restoreTask, WRONG_MS)
    }

    private fun setTorch(on: Boolean) {
        torchOn = on
        runCatching { barcode.setTorch(on) }
        torch.background = if (on) Ui.round(0xFFF3F3F3.toInt(), dpf(22f)) else roundBg()
        torchIcon.color = if (on) 0xFF121212.toInt() else Color.WHITE
        Ui.buzz(torch, Ui.Buzz.TAP)
    }

    /**
     * 退出：结果交给 MainActivity，窗口整个淡出、稍放大（res/anim/scan_exit），直接露出下面的主界面。
     * 不在自己窗口里淡：那样先淡成黑底，再跳回浅色主界面，会闪一下黑
     */
    private fun leave(code: Int, data: Intent?) {
        if (isFinishing) return
        done = true
        main.removeCallbacksAndMessages(null)
        setResult(code, data)
        finish()
        @Suppress("DEPRECATION")
        overridePendingTransition(0, R.anim.scan_exit)
    }

    // ───────── 进场、换提示 ─────────

    private fun enter() {
        scanOverlay.enter()
        listOf(topBar, chip, hint, album).forEachIndexed { i, v ->
            v.alpha = 0f
            v.translationY = dpf(8f)
            v.animate().alpha(1f).translationY(0f).setStartDelay(90L + 30L * i).setDuration(380)
                .setInterpolator(Ui.SOFT_SPRING).start()
        }
    }

    private fun showHint(h: Hint) {
        hintTitle.text = h.title
        hintSub.text = h.sub
        hintIcon.visibility = if (h.icon == null) View.GONE else View.VISIBLE
        if (h.icon != null) {
            hintIcon.setPaths(h.icon)
            hintIcon.color = h.iconColor
        }
    }

    /** 旧的上滑淡出，新的从下面浮上来 */
    private fun setHint(h: Hint) {
        hint.animate().cancel()
        hint.animate().alpha(0f).translationY(-dpf(6f)).setStartDelay(0).setDuration(140).setInterpolator(Ui.EASE).withEndAction {
            showHint(h)
            hint.translationY = dpf(8f)
            hint.animate().alpha(1f).translationY(0f).setDuration(320).setInterpolator(Ui.SOFT_SPRING).start()
        }.start()
    }

    // ───────── 小部件 ─────────

    private fun text(s: String, sp: Float, weight: Int, color: Int) = TextView(this).apply {
        text = s
        setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
        typeface = Ui.font(this@ScanActivity, weight)
        setTextColor(color)
        includeFontPadding = false
        maxLines = 1
    }

    private fun roundBg() = Ui.round(Ui.alpha(Color.WHITE, 0.14f), dpf(22f), dp(0.8f), Ui.alpha(Color.WHITE, 0.14f))

    private fun round(icon: Icon, onClick: () -> Unit) = FrameLayout(this).apply {
        background = roundBg()
        addView(icon, FrameLayout.LayoutParams(dp(22f), dp(22f), Gravity.CENTER))
        Ui.springy(this)
        setOnClickListener { onClick() }
    }

    private fun edgeToEdge() {
        window.statusBarColor = Color.TRANSPARENT
        window.navigationBarColor = Color.TRANSPARENT
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false)
            window.insetsController?.setSystemBarsAppearance(0,
                WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS)
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        }
    }

    /** 和主界面一样要屏幕最高的刷新率（相机预览时系统可能还是会降到 60Hz） */
    private fun requestHighestRefreshRate() {
        @Suppress("DEPRECATION")
        val display = (if (Build.VERSION.SDK_INT >= 30) display else windowManager.defaultDisplay) ?: return
        val current = display.mode
        val best = display.supportedModes
            .filter { it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: return
        window.attributes = window.attributes.apply { preferredDisplayModeId = best.modeId }
    }

    /** 取景框：宽度的 0.672 见方，顶边在屏幕 0.317 高处（和效果图一样） */
    private fun frameRect(w: Int, h: Int, out: RectF) {
        val size = min(w, h) * 0.672f
        val l = (w - size) / 2f
        val t = h * 0.317f
        out.set(l, t, l + size, t + size)
    }

    /** 按取景框摆：顶栏在状态栏下，小标签在框上面，提示在框下面，相册按钮贴底 */
    private inner class Layout(ctx: Context) : FrameLayout(ctx) {
        private val frame = RectF()

        override fun onLayout(changed: Boolean, left: Int, t: Int, right: Int, b: Int) {
            super.onLayout(changed, left, t, right, b)
            val w = right - left
            val h = b - t
            frameRect(w, h, frame)
            scanOverlay.frame.set(frame)
            barcode.setFramingRectSize(Size(frame.width().toInt(), frame.height().toInt()))
            val pad = dp(16f)
            topBar.layout(pad, insetTop + dp(10f), w - pad, insetTop + dp(10f) + dp(44f))
            place(chip, (frame.top - dp(18f) - chip.measuredHeight).toInt(), w)
            place(hint, (frame.bottom + dp(20f)).toInt(), w)
            place(album, h - insetBottom - dp(28f) - album.measuredHeight, w)
        }

        private fun place(v: View, y: Int, w: Int) {
            val x = (w - v.measuredWidth) / 2
            v.layout(x, y, x + v.measuredWidth, y + v.measuredHeight)
        }
    }

    // ───────── 遮罩、取景框、光带、对勾 ─────────

    /**
     * 按时间画：每帧看现在离进场、认出来、扫错各过了多久，算出遮罩深浅、四个角的大小位置、光带位置、对勾大小。
     * running 时一直重画（光带一直在扫）
     */
    @SuppressLint("ViewConstructor")
    private inner class ScanOverlay(ctx: Context) : View(ctx) {
        val frame = RectF()
        var running = false
            set(v) {
                field = v
                if (v) postInvalidateOnAnimation()
            }
        private var enterAt = 0L
        private var foundAt = 0L
        private var wrongAt = 0L
        private var wrongEndAt = 0L

        private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
        private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
        }
        private val scrim = Path().apply { fillType = Path.FillType.EVEN_ODD }
        private val hole = Path()
        private val corner = Path()
        private val tick = Path()
        private val part = Path()
        private val measure = PathMeasure()
        private val box = RectF()
        private var trailShader: LinearGradient? = null
        private var lineShader: LinearGradient? = null
        private var glowShader: LinearGradient? = null
        private var shaderW = 0f

        fun enter() {
            enterAt = SystemClock.uptimeMillis()
            invalidate()
        }

        fun found() {
            foundAt = SystemClock.uptimeMillis()
            invalidate()
        }

        fun wrong() {
            wrongAt = SystemClock.uptimeMillis()
            wrongEndAt = 0L
            invalidate()
        }

        fun wrongEnd() {
            wrongEndAt = SystemClock.uptimeMillis()
            invalidate()
        }

        private fun seg(t: Long, from: Float, len: Float) = ((t - from) / len).coerceIn(0f, 1f)
        private fun ease(k: Float) = Ui.EASE.getInterpolation(k)

        override fun onDraw(canvas: Canvas) {
            val now = SystemClock.uptimeMillis()
            val e = now - enterAt
            val f = if (foundAt > 0) now - foundAt else -1L
            val found = if (f >= 0) ease(seg(f, 0f, 250f)) else 0f

            // 扫错了：左右抖一下，四个角变白（1.5 秒后慢慢变回来）
            var dx = 0f
            var white = 0f
            if (wrongAt > 0) {
                val w = now - wrongAt
                if (w < 360) dx = dpf(6.5f) * sin(w / 360f * 3f * 2f * PI.toFloat()) * (1f - w / 360f)
                white = if (wrongEndAt > 0) 1f - ease(seg(now - wrongEndAt, 0f, 260f)) else ease(seg(w, 0f, 120f))
            }

            val cx = frame.centerX() + dx
            val cy = frame.centerY()
            val radius = dpf(28f)
            box.set(cx - frame.width() / 2f, cy - frame.height() / 2f, cx + frame.width() / 2f, cy + frame.height() / 2f)

            // 遮罩：整屏变暗，挖掉取景框；认出来时再暗一点，把注意力收到框里
            val scrimA = 0.62f * ease(seg(e, 0f, 220f)) + 0.14f * found
            scrim.reset()
            scrim.addRect(0f, 0f, width.toFloat(), height.toFloat(), Path.Direction.CW)
            scrim.addRoundRect(box, radius, radius, Path.Direction.CW)
            fill.shader = null
            fill.color = Ui.alpha(0xFF0A0A0A.toInt(), scrimA)
            canvas.drawPath(scrim, fill)

            // 光带：进场后开始，1.8 秒一趟，两头淡入淡出；认出来就收起
            hole.reset()
            hole.addRoundRect(box, radius, radius, Path.Direction.CW)
            val beamA = seg(e, 380f, 200f) * (1f - (if (f >= 0) seg(f, 0f, 150f) else 0f))
            if (beamA > 0f) drawBeam(canvas, now, beamA)

            // 认出来：框里闪一下青柠
            if (f in 0..300) {
                fill.shader = null
                fill.color = Ui.alpha(LIME, 0.30f * sin(PI.toFloat() * f / 300f))
                canvas.drawPath(hole, fill)
            }

            // 四个角：进场时从 1.16 倍收到 1 倍（轻回弹）；认出来往里收 25dp 贴住二维码
            val k = Ui.SOFT_SPRING.getInterpolation(seg(e, 40f, 380f))
            val scale = 1.16f - 0.16f * k
            val inset = dpf(25f) * found
            val cornerA = seg(e, 20f, 200f)
            val color = blend(LIME, Color.WHITE, white)
            drawCorners(canvas, cx, cy, frame.width() * scale / 2f - inset, radius, color, cornerA)

            // 对勾：青柠圆弹出来，再一笔画出勾
            if (f >= 100) {
                val s = Ui.SPRING.getInterpolation(seg(f, 100f, 380f))
                val r = dpf(48f) * s
                fill.shader = null
                fill.color = Ui.alpha(LIME, 0.18f)
                canvas.drawCircle(cx, cy, r + dpf(9.5f) * s, fill)
                fill.color = LIME
                canvas.drawCircle(cx, cy, r, fill)
                drawTick(canvas, cx, cy, s, seg(f, 260f, 260f))
            }

            if (running) postInvalidateOnAnimation()
        }

        private fun drawBeam(canvas: Canvas, now: Long, a: Float) {
            val phase = ((now - enterAt - 380L) % BEAM_MS) / BEAM_MS.toFloat()
            val pos = Ui.EASE_IN_OUT.getInterpolation(phase)
            val edge = min(1f, min(phase / 0.12f, (1f - phase) / 0.12f)) // 到两头淡掉
            val y = box.top + dpf(15f) + pos * (box.height() - dpf(30f))
            if (shaderW != box.width()) {
                shaderW = box.width()
                val l = box.left
                val r = box.right
                lineShader = LinearGradient(l + dpf(18f), 0f, r - dpf(18f), 0f,
                    intArrayOf(Ui.alpha(LIME, 0f), 0xFFD9FB93.toInt(), 0xFFF2FFD6.toInt(), 0xFFD9FB93.toInt(), Ui.alpha(LIME, 0f)),
                    floatArrayOf(0f, 0.22f, 0.5f, 0.78f, 1f), Shader.TileMode.CLAMP)
                trailShader = LinearGradient(0f, -dpf(84f), 0f, 0f,
                    intArrayOf(Ui.alpha(LIME, 0f), Ui.alpha(LIME, 0.07f), Ui.alpha(LIME, 0.26f)), floatArrayOf(0f, 0.55f, 1f), Shader.TileMode.CLAMP)
                glowShader = LinearGradient(0f, -dpf(5f), 0f, dpf(5f),
                    intArrayOf(Ui.alpha(LIME, 0f), Ui.alpha(LIME, 0.45f), Ui.alpha(LIME, 0f)), null, Shader.TileMode.CLAMP)
            }
            val alpha = a * edge
            if (alpha <= 0f) return
            canvas.save()
            canvas.clipPath(hole)
            canvas.translate(0f, y)
            fill.color = Color.WHITE
            fill.alpha = (alpha * 255).toInt()
            fill.shader = trailShader
            canvas.drawRect(box.left, -dpf(84f), box.right, 0f, fill)
            fill.shader = glowShader
            canvas.drawRect(box.left + dpf(18f), -dpf(5f), box.right - dpf(18f), dpf(5f), fill)
            canvas.restore()
            fill.shader = lineShader
            canvas.drawRoundRect(box.left + dpf(18f), y - dpf(1.1f), box.right - dpf(18f), y + dpf(1.1f), dpf(1.1f), dpf(1.1f), fill)
            fill.shader = null
            fill.alpha = 255
        }

        /** 四个角顺着圆角画：粗、圆头；half 是框的半边长 */
        private fun drawCorners(canvas: Canvas, cx: Float, cy: Float, half: Float, radius: Float, color: Int, a: Float) {
            if (a <= 0f) return
            val r = (radius - (frame.width() / 2f - half) * 0.6f).coerceAtLeast(dpf(11f))
            val len = dpf(33f)
            val l = cx - half
            val t = cy - half
            val rr = cx + half
            val b = cy + half
            corner.reset()
            corner.moveTo(l, t + r + len); corner.lineTo(l, t + r); corner.arcTo(l, t, l + 2 * r, t + 2 * r, 180f, 90f, false); corner.lineTo(l + r + len, t)
            corner.moveTo(rr - r - len, t); corner.lineTo(rr - r, t); corner.arcTo(rr - 2 * r, t, rr, t + 2 * r, 270f, 90f, false); corner.lineTo(rr, t + r + len)
            corner.moveTo(rr, b - r - len); corner.lineTo(rr, b - r); corner.arcTo(rr - 2 * r, b - 2 * r, rr, b, 0f, 90f, false); corner.lineTo(rr - r - len, b)
            corner.moveTo(l + r + len, b); corner.lineTo(l + r, b); corner.arcTo(l, b - 2 * r, l + 2 * r, b, 90f, 90f, false); corner.lineTo(l, b - r - len)
            stroke.shader = null
            stroke.color = Ui.alpha(color, a)
            stroke.strokeWidth = dpf(4.7f)
            canvas.drawPath(corner, stroke)
        }

        private fun drawTick(canvas: Canvas, cx: Float, cy: Float, s: Float, k: Float) {
            if (k <= 0f) return
            val u = dpf(51f) / 24f * s // 勾按 24 格的图标画，51dp 大
            tick.reset()
            tick.moveTo(cx + (20f - 12f) * u, cy + (6f - 12f) * u)
            tick.lineTo(cx + (9f - 12f) * u, cy + (17f - 12f) * u)
            tick.lineTo(cx + (4f - 12f) * u, cy + (12f - 12f) * u)
            measure.setPath(tick, false)
            part.reset()
            measure.getSegment(measure.length * (1f - k), measure.length, part, true) // 从左下那一头画起
            stroke.color = 0xFF121212.toInt()
            stroke.strokeWidth = 2.6f * u
            canvas.drawPath(part, stroke)
        }

        private fun blend(a: Int, b: Int, k: Float): Int {
            fun ch(shift: Int) = (((a shr shift) and 0xFF) * (1f - k) + ((b shr shift) and 0xFF) * k).toInt() shl shift
            return (0xFF shl 24) or ch(16) or ch(8) or ch(0)
        }
    }

    /** 线条图标：照 24×24 的图标路径画，2 粗、圆头，单色 */
    @SuppressLint("ViewConstructor")
    private class Icon(ctx: Context, paths: Array<String>, color: Int = Color.WHITE) : View(ctx) {
        private var path = Path()
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeCap = Paint.Cap.ROUND
            strokeJoin = Paint.Join.ROUND
            strokeWidth = 2f
        }
        var color = color
            set(v) {
                field = v
                invalidate()
            }

        init {
            setPaths(paths)
        }

        fun setPaths(paths: Array<String>) {
            path = Path().apply { paths.forEach { addPath(PathParser.createPathFromPathData(it)) } }
            invalidate()
        }

        override fun onDraw(canvas: Canvas) {
            val s = min(width, height) / 24f
            canvas.save()
            canvas.translate((width - 24f * s) / 2f, (height - 24f * s) / 2f)
            canvas.scale(s, s)
            paint.color = color
            canvas.drawPath(path, paint)
            canvas.restore()
        }
    }

    companion object {
        /** 这次扫码做什么：bt / lan / any（小标签、认出来后的说法跟着变） */
        const val EXTRA_MODE = "mode"
        /** 小标签上的字 */
        const val EXTRA_LABEL = "label"
        /** 结果：扫到的内容 */
        const val EXTRA_TEXT = "text"
        /** 结果：点了「从相册选」 */
        const val EXTRA_ALBUM = "album"
        /** 调试版：打开 1.5 秒后当成扫到了这段内容（MainActivity 的 debugScanFake） */
        const val EXTRA_DEBUG_FAKE = "debugFake"

        private const val LIME = 0xFFBEF264.toInt()
        private const val CHIP_INK = 0xFFD6F79A.toInt()
        private const val BEAM_MS = 1800L
        private const val SLOW_MS = 8000L
        private const val WRONG_MS = 1500L
        private const val FOUND_HOLD_MS = 1100L

        // Lucide 图标（ISC 许可），24×24
        private val X = arrayOf("M18 6 6 18", "m6 6 12 12")
        private val TORCH = arrayOf("M18 6c0 2-2 2-2 4v10a2 2 0 0 1-2 2h-4a2 2 0 0 1-2-2V10c0-2-2-2-2-4V2h12z", "M6 6h12", "M12 12v.01")
        private val IMAGE = arrayOf("M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z", "M11 9a2 2 0 1 1-4 0a2 2 0 1 1 4 0z", "m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21")
        private val BLUETOOTH = arrayOf("m7 7 10 10-5 5V2l5 5L7 17")
        private val WIFI = arrayOf("M12 20h.01", "M2 8.82a15 15 0 0 1 20 0", "M5 12.859a10 10 0 0 1 14 0", "M8.5 16.429a5 5 0 0 1 7 0")
        private val CHECK = arrayOf("M20 6 9 17l-5-5")
        private val ALERT = arrayOf("M22 12a10 10 0 1 1-20 0a10 10 0 1 1 20 0z", "M12 8v4", "M12 16h.01")
    }
}
