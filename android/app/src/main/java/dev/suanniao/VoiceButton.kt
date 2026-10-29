package dev.suanniao

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Canvas
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.os.Bundle
import android.os.SystemClock
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.text.StaticLayout
import android.text.TextPaint
import android.text.TextUtils
import android.util.Log
import android.view.MotionEvent
import android.view.View
import java.io.File
import java.util.Locale
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sin

/**
 * 语音按钮，不弹输入法：按住说话、松手结束；按住往上滑到锁图标就锁定，可以长时间说，再点一下结束。
 * 轻点一下不会开始录音，只提示怎么用（以前轻点就锁定，误触后会把周围的说话声一直打进电脑）。
 *
 * 不丢话：按下时先看连没连上电脑，没连上就在按钮上写明，照常识别，文字存在手机上（onStash），
 * 连上后由界面问要不要发；说到一半断线也一样，已经打出去的留着，剩下的存起来。连上后绝不自动打进电脑。
 * 没连上时不能锁定。锁定后 20 秒没识别出字就自动结束。
 *
 * 两种识别：
 * - 手机上装了本地模型（见 LocalAsr）就用它：全程不断麦，边说边出字、带标点，默认边说边往电脑上打字
 * - 没装就用系统语音识别（小米自带的「系统语音引擎」）：一句一句出结果、没有标点。
 *   手还按着（或锁定中）时引擎因为停顿结束了一段，就把这一段先发出去，马上接着听；
 *   停顿太久引擎报「没听清 / 超时」也一样接着听，不会自己停
 *
 * 外观：上面一行标题和录音计时，中间是按音量滚动的音量条（快爆音才变红），底下是识别出的字（多行）或提示。
 * 锁定时一圈红边，顶栏常驻「录音中 · 点此结束」（MainActivity 画）。
 */
@SuppressLint("ViewConstructor")
class VoiceButton(
    context: Context,
    label: String,
    private val hasMicPermission: () -> Boolean,
    private val isConnected: () -> Boolean,
    /** 发一段文字；返回 true = 真的发出去了 */
    private val onSend: (String) -> Boolean,
    /** 边说边打：先在电脑上退格 delete 个字，再打 insert；last = 这次按住的最后一段。返回 true = 发出去了 */
    private val onTyping: (delete: Int, insert: String, last: Boolean) -> Boolean,
    /** 没发出去的话：存在手机上，连上后让用户决定发不发 */
    private val onStash: (String) -> Unit,
    private val onNotice: (String) -> Unit,
    /** 锁定 / 解除锁定 */
    private val onLatch: (Boolean) -> Unit,
    /** 说完了、识别也收完尾了（界面这时才弹「没发出去的话」） */
    private val onIdle: () -> Unit,
    /** 识别引擎（小米是「系统语音引擎」）自己没有麦克风权限，得带用户去它的设置页开 */
    private val onEngineDenied: () -> Unit,
) : View(context), RecognitionListener {

    var label: String = label
        set(value) {
            field = value
            contentDescription = "$value，按住说话"
            refresh()
        }

    /** 本地识别用的模型；null = 用系统语音识别 */
    var localModel: LocalAsr.Model? = null
    /** 本地识别时边说边往电脑上打字；false = 停顿一下（一句说完）再整句发 */
    var liveTyping = true
    /** 没连上电脑（待机时提示说的话会先存在手机上） */
    var offline = false
        set(value) {
            if (field == value) return
            field = value
            refresh()
        }

    /** 本地识别：这次按住（或锁定）；松手后收尾完才清掉 */
    private var press: LocalPress? = null

    private var recognizer: SpeechRecognizer? = null
    private var holding = false
    private var latched = false
        set(value) {
            if (field == value) return
            field = value
            onLatch(value)
        }
    private var listening = false
    /** 已经松手（或点了结束），正在等这一句的最终结果 */
    private var finishing = false
    private var downAt = 0L
    private var downX = 0f
    private var downY = 0f
    /** 这次按住往上滑了多少（0～1，到 1 就锁定） */
    private var lockProgress = 0f
    /** 没连上时滑到了锁上：已经提示过不能锁 */
    private var lockRefused = false
    /** 系统语音识别这次按住没连上电脑（或中途断了）：识别出的字都存到手机上 */
    private var pressOffline = false
    private var startedAt = 0L
    /** 这一段是什么时候松手（或点了结束）的 */
    private var stoppedAt = 0L
    /** 锁定后最近一次识别出字的时间；太久没说话就自动结束，免得麦克风一直开着 */
    private var lastHeardAt = 0L
    /** 连着「一开始就出错」的次数：真出了问题（比如断网）时别没完没了地重试 */
    private var quickFailures = 0
    /** 这次按住（或锁定）以来已经发出去过字：之后的「没听清」就不用提示了 */
    private var sentThisPress = false
    /** 这次按住（或锁定）遇到了重试也没用的错，已经停下 */
    private var gaveUp = false
    private var lastSentText = ""
    private var lastSentAt = 0L
    /** 上一次识别出字的时间：小米引擎这之后还要收尾一阵，见 start() */
    private var lastResultAt = 0L
    private var partial = ""
    /** 调试：这次放 wav 时假装锁定了（只为看锁定的样子） */
    private var debugLatched = false
    /** 这次是锁定后太久没说话自动结束的：收尾时不再补「没听到说话」，免得把自动结束的提示盖掉 */
    private var idleStopped = false

    private val palette = Ui.palette(context)
    private val density = resources.displayMetrics.density
    private val radius = Ui.dp(context, Ui.RADIUS_DP)
    private val lockDistance = 56 * density
    private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = Ui.hairline(context)
    }
    private val redLine = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 2.4f * resources.displayMetrics.density
    }
    private val headPaint = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 13f)
    }
    private val timePaint = Paint(headPaint).apply {
        fontFeatureSettings = "'tnum'"
        textAlign = Paint.Align.RIGHT
    }
    private val textPaint = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 650)
        textSize = Ui.sp(context, 13.5f)
    }
    private val smallPaint = TextPaint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Ui.font(context, 800)
        textSize = Ui.sp(context, 11f)
    }
    private val dotPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val barPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val guidePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 1.6f * resources.displayMetrics.density
        strokeCap = Paint.Cap.ROUND
        pathEffect = DashPathEffect(floatArrayOf(2f * resources.displayMetrics.density, 4f * resources.displayMetrics.density), 0f)
    }
    private val arcPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 2.4f * resources.displayMetrics.density
        strokeCap = Paint.Cap.ROUND
    }
    private val lockPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 1.8f * resources.displayMetrics.density
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
    private val lockPath = Path()
    private val arcBox = RectF()
    /** 底下那段的提示；识别出字时显示字 */
    private var hint = ""
    private var active = false
    private var activeSince = 0L
    /** 底下多行文字的排版（文字、宽度没变就不重排） */
    private var textLayout: StaticLayout? = null
    private var layoutText = ""
    private var layoutWidth = 0
    /** 音量条：每 BAR_MS 记一格当前音量，最新的在最右边，整条平滑往左滚；快爆音的那几格画红色 */
    private val bars = FloatArray(BAR_COUNT)
    private val clips = BooleanArray(BAR_COUNT)
    private var level = 0f
    private var levelTarget = 0f
    private var levelAt = 0L
    private var clipAt = 0L
    private var lastBarAt = 0L
    private var lastFrameAt = 0L

    /** 上一句迟迟不出结果，就不等了，直接开始新的一段 */
    private val forceStart = Runnable {
        if (finishing && keepListening()) {
            finishing = false
            start()
        }
    }

    /** 等小米引擎收完上一句的尾再开始，见 start() */
    private val deferredStart = Runnable { if (keepListening() && !listening) start() }

    /** 本地识别锁定中：太久没识别出字就自动结束（系统引擎在 continueListening 里检查） */
    private val idleCheck = object : Runnable {
        override fun run() {
            if (!latched || holding || press == null) return
            if (SystemClock.uptimeMillis() - lastHeardAt > IDLE_LIMIT_MS) {
                latched = false
                idleStopped = true
                finish()
                onNotice("锁定后 20 秒没听到说话，已自动结束")
            } else {
                postDelayed(this, 2000)
            }
        }
    }

    init {
        contentDescription = "$label，按住说话"
        refresh()
    }

    /** 正在录音或者还在收尾（这时别弹面板、别调暗屏幕） */
    fun isBusy() = active

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (latched) { // 锁定中再点一下 = 说完了
                    latched = false
                    finish()
                    return true
                }
                if (!hasMicPermission()) return true
                if (localModel == null && !SpeechRecognizer.isRecognitionAvailable(context)) {
                    onNotice("手机上没有可用的语音识别服务")
                    return true
                }
                holding = true
                downAt = e.eventTime
                downX = e.x
                downY = e.y
                lockProgress = 0f
                lockRefused = false
                pressOffline = !isConnected()
                quickFailures = 0
                sentThisPress = false
                gaveUp = false
                idleStopped = false
                Ui.buzz(this, Ui.Buzz.START)
                start()
            }
            MotionEvent.ACTION_MOVE -> {
                if (!holding || latched) return true
                val rise = downY - e.y
                lockProgress = (rise / lockDistance).coerceIn(0f, 1f)
                if (rise >= lockDistance) {
                    if (pressOffline || press?.isOffline == true || !isConnected()) {
                        if (!lockRefused) {
                            lockRefused = true
                            Ui.buzz(this, Ui.Buzz.REJECT)
                            onNotice("未连接电脑，不能锁定 · 松手就结束")
                        }
                    } else {
                        latch()
                    }
                }
                postInvalidateOnAnimation()
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (!holding) return true
                holding = false
                lockProgress = 0f
                when {
                    gaveUp -> refresh()
                    latched -> refresh() // 锁定了：松手接着听
                    e.actionMasked == MotionEvent.ACTION_UP && e.eventTime - downAt < TAP_MS -> {
                        // 轻点：刚开始的录音里不会有话，丢掉，只提示怎么用
                        cancelTap()
                        onNotice("按住说话 · 上滑锁定")
                    }
                    else -> finish()
                }
            }
        }
        return true
    }

    private fun latch() {
        latched = true
        lastHeardAt = SystemClock.uptimeMillis()
        Ui.buzz(this, Ui.Buzz.CONFIRM)
        if (press != null) {
            removeCallbacks(idleCheck)
            postDelayed(idleCheck, 2000)
        }
        refresh()
    }

    /** 这次按下没说话就松手了：录音直接丢掉 */
    private fun cancelTap() {
        val local = press
        if (local != null) {
            local.cancel()
            press = null
        } else if (listening) {
            recognizer?.cancel()
            endSegment()
        }
        removeCallbacks(deferredStart)
        removeCallbacks(forceStart)
        refresh()
    }

    private fun start() {
        localModel?.let { model ->
            // 上一次松手后可能还在收尾，新的一次照样马上开始录音，字等它发完再发
            press = LocalPress(model, liveTyping, previous = press, offline = pressOffline).also { it.start() }
            refresh()
            return
        }
        val r = recognizer ?: SpeechRecognizer.createSpeechRecognizer(context).also {
            it.setRecognitionListener(this)
            recognizer = it
        }
        if (listening) {
            // 上一段说了一阵才松手，可能还在出结果，取消它这句话就丢了：等结果回来（最多 0.8 秒），
            // onResults / onError 看手还按着会接着听。刚开始就松手的那段里不会有话，直接丢掉
            if (finishing && stoppedAt - startedAt >= SHORT_SEGMENT_MS) {
                removeCallbacks(forceStart)
                postDelayed(forceStart, 800)
                refresh()
                return
            }
            r.cancel()
        } else {
            // 小米引擎出结果后约 0.26 秒才收尾上一句，收尾时会把麦克风关掉（0.3～0.45 秒关完）。
            // 这之前开始的新一段，录音会被一起关掉，之后说什么都听不到，所以出结果后先等一会儿
            val wait = lastResultAt + SETTLE_AFTER_RESULT_MS - SystemClock.uptimeMillis()
            if (wait > 0) {
                removeCallbacks(deferredStart)
                postDelayed(deferredStart, wait)
                refresh()
                return
            }
        }
        removeCallbacks(forceStart)
        removeCallbacks(deferredStart)
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, "zh-CN")
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            putExtra(RecognizerIntent.EXTRA_CALLING_PACKAGE, context.packageName)
            // 尽量别因为说话中间的停顿就结束
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, 3000L)
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, 3000L)
        }
        finishing = false
        partial = ""
        listening = true
        startedAt = SystemClock.uptimeMillis()
        refresh()
        r.startListening(intent)
    }

    /** 用户说完了：让识别收尾，最终结果会在 onResults（本地识别是 LocalPress.onDone）里到。 */
    private fun finish() {
        val local = press
        if (local != null) {
            if (!local.finishing) local.finish()
        } else if (listening && !finishing) {
            finishing = true
            stoppedAt = SystemClock.uptimeMillis()
            recognizer?.stopListening()
        }
        removeCallbacks(idleCheck)
        Ui.buzz(this, Ui.Buzz.TAP)
        refresh()
    }

    /** 一段结束了（出结果或报错）。小米引擎偶尔会把上一段的结果、报错迟到报过来，这里不管它属于哪一段，一律当作当前这段结束。 */
    private fun endSegment() {
        listening = false
        finishing = false
        partial = ""
        removeCallbacks(forceStart)
    }

    /** 系统语音识别出了一段：发给电脑；没连上（或刚断）就存到手机上。小米引擎偶尔把同一句报两次，2 秒内重复的同一句不再发。 */
    private fun send(text: String) {
        val now = SystemClock.uptimeMillis()
        if (text == lastSentText && now - lastSentAt < 2000) return
        lastSentText = text
        lastSentAt = now
        lastHeardAt = now
        sentThisPress = true
        if (pressOffline || !onSend(text)) {
            pressOffline = true
            onStash(text)
        }
    }

    /** 一段识别结束了（出了结果或停顿超时），手还按着或锁定中就马上接着听。 */
    private fun continueListening(delayMs: Long) {
        if (latched && !holding && SystemClock.uptimeMillis() - lastHeardAt > IDLE_LIMIT_MS) {
            latched = false
            idleStopped = true
            refresh()
            onNotice("锁定后 20 秒没听到说话，已自动结束")
            return
        }
        postDelayed({ if (keepListening() && !listening) start() }, delayMs)
    }

    /** 调试用：把一段 wav 当成麦克风，按实际速度走一遍本地识别和往电脑上打字，放完自己结束。latch = 顺便显示成锁定的样子 */
    fun debugDictate(wav: File, latch: Boolean) {
        val model = localModel ?: return onNotice("没有本地语音模型")
        pressOffline = !isConnected()
        idleStopped = false
        press = LocalPress(model, liveTyping, previous = press, wav = wav, offline = pressOffline).also { it.start() }
        if (latch) {
            debugLatched = true
            latched = true
            // 和真锁定一样：太久没识别出字就自动结束（放一段静音就能测）
            lastHeardAt = SystemClock.uptimeMillis()
            removeCallbacks(idleCheck)
            postDelayed(idleCheck, 2000)
        }
        refresh()
    }

    /** App 退到后台（后台录不了音）、换页、点了顶栏的「点此结束」：正在听就当作说完了。 */
    fun stopListening() {
        if (!holding && !latched) return
        holding = false
        latched = false
        finish()
    }

    fun release() {
        holding = false
        latched = false
        removeCallbacks(forceStart)
        removeCallbacks(deferredStart)
        removeCallbacks(idleCheck)
        recognizer?.destroy()
        recognizer = null
        listening = false
        finishing = false
        press?.cancel()
        press = null
    }

    private fun refresh() {
        val local = press
        val nowActive = listening || latched || holding || local != null
        if (nowActive && !active) activeSince = SystemClock.uptimeMillis()
        val wasActive = active
        active = nowActive
        val savingLocally = pressOffline || local?.isOffline == true
        hint = when {
            local != null && !local.ready && (holding || latched) -> "正在加载语音模型…可以先说"
            latched -> "已锁定 · 说完点一下结束"
            holding && !gaveUp -> if (savingLocally) "说的话先存在手机上" else "正在听…松手发送"
            listening || local != null -> "正在识别…"
            offline -> "未连接电脑 · 说的话会先存在手机上"
            else -> "按住说话 · 上滑锁定"
        }
        postInvalidateOnAnimation()
        if (wasActive && !nowActive) onIdle()
    }

    /** 当前音量（0～1）：本地识别每 50 毫秒报一次，系统语音引擎在 onRmsChanged 里报（有的引擎不报） */
    private fun setLevel(value: Float, clipped: Boolean) {
        levelTarget = value.coerceIn(0f, 1f)
        levelAt = SystemClock.uptimeMillis()
        if (clipped) clipAt = levelAt
        postInvalidateOnAnimation()
    }

    private fun title() = if (label.isBlank() || label == "语音按钮") "语音" else label

    private fun layoutFor(text: String, width: Int): StaticLayout {
        val cached = textLayout
        if (cached != null && text == layoutText && width == layoutWidth) return cached
        return StaticLayout.Builder.obtain(text, 0, text.length, textPaint, width.coerceAtLeast(1))
            .setIncludePad(false)
            .setLineSpacing(2 * density, 1f)
            .build()
            .also {
                textLayout = it
                layoutText = text
                layoutWidth = width
            }
    }

    override fun onDraw(canvas: Canvas) {
        val p = palette
        val now = SystemClock.uptimeMillis()
        val frames = ((now - lastFrameAt).coerceIn(1, 64)) / 16.7f
        lastFrameAt = now
        // 一阵没收到音量（系统引擎可能不报）就让条轻轻起伏，看得出正在听
        val target = when {
            !active -> 0f
            now - levelAt > 500 -> 0.1f + 0.06f * sin(now / 180f)
            else -> levelTarget
        }
        val rate = if (target > level) 0.5f else 0.18f // 声音来了马上涨，走了慢慢落
        level += (target - level) * (1f - (1f - rate).pow(frames))
        if (now - lastBarAt >= BAR_MS) {
            System.arraycopy(bars, 1, bars, 0, BAR_COUNT - 1)
            System.arraycopy(clips, 1, clips, 0, BAR_COUNT - 1)
            bars[BAR_COUNT - 1] = level
            clips[BAR_COUNT - 1] = active && now - clipAt < BAR_MS * 2
            lastBarAt = now
        }

        val w = width.toFloat()
        val h = height.toFloat()
        val padX = 14 * density
        Ui.drawTile(canvas, w, h, radius, p.tile, p.line, 1f, fillPaint, linePaint)
        if (latched) {
            // 锁定：一圈红边，底色带一点点红
            fillPaint.color = Ui.alpha(p.red, if (p.dark) 0.1f else 0.05f)
            canvas.drawRoundRect(0f, 0f, w, h, radius, radius, fillPaint)
            redLine.color = p.red
            val i = redLine.strokeWidth / 2
            canvas.drawRoundRect(i, i, w - i, h - i, radius - i, radius - i, redLine)
        }

        // 标题行：左边名字（没连上时写明），右边（录音时）锁、录音点和计时；待机时右边什么都不画
        val headBase = 12 * density - headPaint.ascent()
        val savingLocally = active && (pressOffline || press?.isOffline == true)
        var headRight = w - padX
        // 上滑锁定的目标圈压到标题行上时，标题行先让开（淡出）
        val lockAppear = if (holding && !latched) ((now - downAt - 180) / 160f).coerceIn(0f, 1f) else 0f
        val lockTop = max(downY - lockDistance, 24 * density) - 16 * density
        val headAlpha = if (lockAppear > 0f && lockTop < headBase + headPaint.descent() + 4 * density) 1f - lockAppear else 1f
        if (active) {
            val seconds = ((now - activeSince) / 1000).toInt()
            val time = String.format(Locale.ROOT, "%d:%02d", seconds / 60, seconds % 60)
            timePaint.color = Ui.alpha(p.fg, headAlpha)
            canvas.drawText(time, headRight, headBase, timePaint)
            headRight -= timePaint.measureText(time) + 7 * density
            val dotR = 4.5f * density
            val dotX = headRight - dotR
            val dotY = headBase + (headPaint.ascent() + headPaint.descent()) / 2
            val blink = 0.35f + 0.65f * (0.5f + 0.5f * cos(now * 2 * PI / 1100).toFloat())
            dotPaint.color = p.red
            dotPaint.alpha = (blink * 0.3f * 255).toInt()
            canvas.drawCircle(dotX, dotY, dotR * 2.2f, dotPaint)
            dotPaint.alpha = (blink * 255).toInt()
            canvas.drawCircle(dotX, dotY, dotR, dotPaint)
            headRight = dotX - dotR * 2.2f - 4 * density
            if (latched) {
                lockPaint.color = p.red
                drawLock(canvas, headRight - 6 * density, dotY, 0.85f)
                headRight -= 16 * density
            }
        }
        headPaint.color = Ui.alpha(if (active) p.fg else p.sub, headAlpha)
        val head = if (savingLocally) "未连接电脑 · 先存在手机上" else title()
        val shownHead = TextUtils.ellipsize(head, headPaint, (headRight - padX - 6 * density).coerceAtLeast(0f), TextUtils.TruncateAt.END)
        canvas.drawText(shownHead, 0, shownHead.length, padX, headBase, headPaint)
        val headBottom = headBase + headPaint.descent()

        // 底下：识别出的字（多行，太多只留最后几行），没有字就显示提示
        val message = partial.ifEmpty { hint }
        textPaint.color = if (partial.isNotEmpty() || active) p.fg else p.sub
        val layout = layoutFor(message, (w - 2 * padX).toInt())
        val lineH = textPaint.fontSpacing + 2 * density
        val maxLines = ((h - headBottom - 11 * density - 30 * density) / lineH).toInt().coerceIn(1, 4)
        val shown = min(layout.lineCount, maxLines)
        val first = layout.lineCount - shown
        val textBottom = h - 11 * density
        val textHeight = (layout.getLineBottom(layout.lineCount - 1) - layout.getLineTop(first)).toFloat()
        val textTop = textBottom - textHeight
        canvas.save()
        canvas.clipRect(padX - density, textTop - density, w - padX + density, textBottom + density)
        canvas.translate(padX, textTop - layout.getLineTop(first))
        layout.draw(canvas)
        canvas.restore()

        // 中间的音量条：圆头细条，越响越高，快爆音的变红；左边渐隐
        val top = headBottom + 6 * density
        val bottom = textTop - 6 * density
        val meterH = bottom - top
        if (meterH > 4 * density) {
            val bw = 3 * density
            val pitch = 6 * density
            val phase = ((now - lastBarAt) / BAR_MS.toFloat()).coerceIn(0f, 1f)
            val right = w - padX
            val mid = (top + bottom) / 2
            for (i in BAR_COUNT - 1 downTo 0) {
                val x = right - bw - (BAR_COUNT - 1 - i + phase) * pitch
                if (x < padX) break
                val v = bars[i]
                val bh = maxOf(bw, v * (meterH - 2 * density))
                barPaint.color = if (clips[i]) p.red else p.fg
                barPaint.alpha = (((x - padX) / ((right - padX) * 0.45f)).coerceIn(0.12f, 1f) * 255).toInt()
                canvas.drawRoundRect(x, mid - bh / 2, x + bw, mid + bh / 2, bw / 2, bw / 2, barPaint)
            }
        }

        // 按住、还没锁定：手指上方画个锁，往上滑到它就锁定
        if (holding && !latched) drawLockTarget(canvas, now, w, padX)

        var settled = !active && level < 0.005f
        if (settled) for (v in bars) if (v > 0.005f) { settled = false; break }
        if (!settled) postInvalidateOnAnimation()
    }

    /** 上滑锁定的目标：按住 0.18 秒后淡入；一圈进度跟着手指往上走；没连上时是灰的 */
    private fun drawLockTarget(canvas: Canvas, now: Long, w: Float, padX: Float) {
        val appear = ((now - downAt - 180) / 160f).coerceIn(0f, 1f)
        if (appear <= 0f) return
        val p = palette
        val canLock = !(pressOffline || press?.isOffline == true)
        val r = 16 * density
        val tx = downX.coerceIn(padX + r + 2 * density, w - padX - r - 2 * density)
        val ty = max(downY - lockDistance, r + 8 * density)
        val a = (appear * 255).toInt()
        // 手指到锁之间的虚线
        val from = downY - 30 * density
        if (from > ty + r + 6 * density) {
            guidePaint.color = p.mut
            guidePaint.alpha = a
            canvas.drawLine(tx, from, tx, ty + r + 6 * density, guidePaint)
        }
        fillPaint.color = p.tile
        fillPaint.alpha = a
        canvas.drawCircle(tx, ty, r, fillPaint)
        linePaint.color = p.line
        linePaint.alpha = a
        canvas.drawCircle(tx, ty, r, linePaint)
        if (canLock && lockProgress > 0f) {
            arcPaint.color = p.fg
            arcPaint.alpha = a
            arcBox.set(tx - r, ty - r, tx + r, ty + r)
            canvas.drawArc(arcBox, -90f, 360f * lockProgress, false, arcPaint)
        }
        lockPaint.color = if (canLock) p.fg else p.mut
        lockPaint.alpha = a
        drawLock(canvas, tx, ty, 1f)
        // 旁边一行小字
        val caption = if (canLock) "上滑锁定" else "没连上，不能锁定"
        smallPaint.color = p.sub
        smallPaint.alpha = a
        val cw = smallPaint.measureText(caption)
        val cy = ty - (smallPaint.ascent() + smallPaint.descent()) / 2
        val rightRoom = w - padX - (tx + r + 8 * density)
        if (rightRoom >= cw) {
            canvas.drawText(caption, tx + r + 8 * density, cy, smallPaint)
        } else {
            canvas.drawText(caption, tx - r - 8 * density - cw, cy, smallPaint)
        }
    }

    /** 小锁：锁身圆角方块 + 上面的锁梁 */
    private fun drawLock(canvas: Canvas, cx: Float, cy: Float, scale: Float) {
        val s = density * scale
        val bodyW = 11 * s
        val bodyH = 8.5f * s
        val top = cy - 0.5f * s
        arcBox.set(cx - bodyW / 2, top, cx + bodyW / 2, top + bodyH)
        canvas.drawRoundRect(arcBox, 2.2f * s, 2.2f * s, lockPaint)
        lockPath.reset()
        lockPath.moveTo(cx - 3.4f * s, top)
        lockPath.lineTo(cx - 3.4f * s, top - 2.6f * s)
        lockPath.cubicTo(cx - 3.4f * s, top - 7.2f * s, cx + 3.4f * s, top - 7.2f * s, cx + 3.4f * s, top - 2.6f * s)
        lockPath.lineTo(cx + 3.4f * s, top)
        canvas.drawPath(lockPath, lockPaint)
    }

    private fun keepListening() = (holding || latched) && !gaveUp

    /** 出了重试也没用的错：这次按住（或锁定）就到此为止，下次按下再说 */
    private fun giveUp() {
        gaveUp = true
        latched = false
        removeCallbacks(deferredStart)
        refresh()
    }

    // ───────── 本地识别 ─────────

    /**
     * 本地识别的一次按住（或锁定）。松手后还要收尾零点几秒，这期间可能已经开始了下一次，
     * 所以每次各记各的；下一次的字等上一次全部发完再发，免得在电脑上交错。
     * offline：这次没连上电脑（或说到一半断了）：不再往电脑上发，剩下的字收尾后存到手机上。
     */
    private inner class LocalPress(
        model: LocalAsr.Model,
        private val live: Boolean,
        private var previous: LocalPress?,
        wav: File? = null,
        private var offline: Boolean,
    ) : LocalAsr.Listener {
        private val dictation = LocalAsr.Dictation(model, this, wav)
        private val fromWav = wav != null
        private var next: LocalPress? = null
        /** 识别出的全部文字 / 其中已经发到电脑上的部分 */
        private var text = ""
        private var sent = ""
        var ready = false
            private set
        var finishing = false
            private set
        var done = false
            private set
        private var cancelled = false
        /** 「说完一句再发」：停顿 pauseMs 没出新字，就把这一句发出去 */
        private val commit = Runnable { flush(last = false) }
        /** 模型隔一阵才出一次字（960ms 的模型约 1 秒一次），停顿要比这个间隔长出一截，不然话没说完就被一段段发出去 */
        private val pauseMs = maxOf(SENTENCE_PAUSE_MS, LocalAsr.chunkMs(model) * 3 / 2)

        val isOffline get() = offline

        init {
            previous?.next = this
        }

        fun start() = dictation.start()

        fun finish() {
            finishing = true
            dictation.finish()
        }

        fun cancel() {
            removeCallbacks(commit)
            dictation.cancel()
            cancelled = true
            done = true
            next?.resume()
        }

        override fun onReady() {
            ready = true
            if (press === this) refresh()
        }

        override fun onLevel(level: Float, clipped: Boolean) {
            if (press === this) setLevel(level, clipped)
        }

        override fun onText(text: String) {
            this.text = text
            lastHeardAt = SystemClock.uptimeMillis()
            if (live) {
                flush(last = false)
            } else {
                removeCallbacks(commit)
                postDelayed(commit, pauseMs)
            }
            if (press === this) {
                partial = text
                refresh()
            }
        }

        override fun onDone(text: String, error: String?) {
            removeCallbacks(commit)
            this.text = text
            done = true
            flush(last = true)
            if (press === this) {
                press = null
                partial = ""
                if (fromWav && debugLatched) {
                    debugLatched = false
                    latched = false
                }
                if (error != null) giveUp() // 手可能还按着，别让它一直显示「正在听」
                refresh()
            }
            when {
                error != null -> onNotice(error)
                text.isEmpty() -> if (!idleStopped) onNotice("没听到说话")
            }
        }

        /** 把还没发到电脑上的字发出去。上一次按住的字还没发完就先攒着，它发完会叫 resume()。 */
        private fun flush(last: Boolean) {
            if (cancelled || previous?.done == false) return
            previous = null
            if (!offline) {
                val common = sent.commonPrefixWith(text).length
                val delete = sent.codePointCount(common, sent.length)
                val insert = text.substring(common)
                val ok = if (live) {
                    // 一个字都没识别出来就不发最后那段：电脑端设了「发送后回车」时会白按一下回车
                    if (delete > 0 || insert.isNotEmpty() || (last && text.isNotEmpty())) onTyping(delete, insert, last) else true
                } else {
                    (delete == 0 || onTyping(delete, "", false)) && (insert.isBlank() || onSend(insert))
                }
                if (ok) {
                    sent = text
                } else {
                    // 说到一半断了：已经打出去的留着，剩下的收尾后存到手机上，连上后也不自动补发
                    offline = true
                    sent = text.substring(0, common)
                    if (press === this) refresh()
                }
            }
            if (offline && last) {
                val rest = text.substring(sent.commonPrefixWith(text).length).trim()
                if (rest.isNotEmpty()) onStash(rest)
                sent = text
            }
            if (last) next?.resume()
        }

        /** 上一次按住的字发完了，轮到这次 */
        fun resume() {
            flush(last = done)
        }
    }

    // ───────── 系统语音识别的回调 ─────────

    override fun onPartialResults(results: Bundle?) {
        results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.let {
            if (it.isNotBlank()) partial = it
        }
        refresh()
    }

    override fun onResults(results: Bundle?) {
        val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.trim().orEmpty()
            .ifEmpty { partial.trim() }
        Log.i(LinkService.TAG, "识别结果「$text」（这一段开始后 ${SystemClock.uptimeMillis() - startedAt} ms）")
        endSegment()
        if (text.isNotEmpty()) {
            lastResultAt = SystemClock.uptimeMillis()
            quickFailures = 0
            send(text)
        }
        if (keepListening()) continueListening(20) else refresh()
    }

    override fun onError(error: Int) {
        val elapsed = SystemClock.uptimeMillis() - startedAt
        Log.w(LinkService.TAG, "语音识别出错：$error（这一段开始后 $elapsed ms）")
        // 已经识别出一部分但最终结果没来，就把这部分发出去，别让话白说
        val pending = partial.trim()
        endSegment()
        if (pending.isNotEmpty() && error != SpeechRecognizer.ERROR_CLIENT) send(pending)
        // 按下时已经确认过蒜鸟自己有麦克风权限，这时还报缺权限，说明缺的是识别引擎那边，重试也没用
        if (error == SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS &&
            context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED
        ) {
            giveUp()
            onEngineDenied()
            return
        }
        if (keepListening()) {
            // 按住或锁定中，报什么错都接着听：小米引擎常把上一段的「没听清」「5 秒超时」迟到报过来，
            // 分不清是哪一段的，不能因此停下。只有一开始就出错、还连着好几次（比如真断网）才停
            quickFailures = if (elapsed < 1000) quickFailures + 1 else 0
            if (quickFailures < MAX_QUICK_FAILURES) {
                continueListening(
                    when {
                        error == SpeechRecognizer.ERROR_RECOGNIZER_BUSY -> 300
                        quickFailures >= 2 -> 600
                        else -> 20
                    }
                )
                return
            }
            quickFailures = 0
            giveUp()
            onNotice(if (error == SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS) "没有麦克风权限" else "语音识别一直出错，先停下了")
            return
        }
        refresh()
        // 已经松手：这次按住期间发出去过字，之后的「没听清 / 没听到」多半是最后那一小段，不用提示
        val pause = error == SpeechRecognizer.ERROR_NO_MATCH || error == SpeechRecognizer.ERROR_SPEECH_TIMEOUT ||
            (error == SpeechRecognizer.ERROR_NETWORK_TIMEOUT && elapsed >= PAUSE_TIMEOUT_MS)
        val message = when {
            error == SpeechRecognizer.ERROR_CLIENT -> null // 我们自己取消的
            pause -> if (sentThisPress || idleStopped) null else "没听清，再说一次"
            error == SpeechRecognizer.ERROR_NETWORK_TIMEOUT -> "网络超时，再试一次"
            error == SpeechRecognizer.ERROR_NETWORK || error == SpeechRecognizer.ERROR_SERVER ||
                error == SpeechRecognizer.ERROR_SERVER_DISCONNECTED -> "语音识别连不上网络"
            error == SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> "没有麦克风权限"
            error == SpeechRecognizer.ERROR_AUDIO -> "麦克风被别的应用占用"
            error == SpeechRecognizer.ERROR_RECOGNIZER_BUSY -> "识别服务忙，稍后再试"
            else -> "语音识别出错了，再试一次"
        }
        message?.let(onNotice)
    }

    // 下面这几个只记日志，用来摸清小米引擎什么时候判断开始说话、说完了
    override fun onReadyForSpeech(params: Bundle?) {
        Log.d(LinkService.TAG, "语音：准备好了")
    }

    override fun onBeginningOfSpeech() {
        Log.d(LinkService.TAG, "语音：开始说话")
    }

    override fun onEndOfSpeech() {
        Log.d(LinkService.TAG, "语音：说完了")
    }

    /** 系统语音引擎报的音量，大致在 -2～10 dB 之间 */
    override fun onRmsChanged(rmsdB: Float) {
        setLevel((rmsdB + 2f) / 12f, rmsdB >= 9.8f)
    }
    override fun onBufferReceived(buffer: ByteArray?) {}
    override fun onEvent(eventType: Int, params: Bundle?) {}

    private companion object {
        /** 小米引擎开始后约 5 秒没听到说话会报「网络超时」；超过这个时长的「网络超时」按停顿处理 */
        const val PAUSE_TIMEOUT_MS = 4000L
        /** 锁定后这么久一句话都没识别出来，就自动结束 */
        const val IDLE_LIMIT_MS = 20_000L
        /** 按下到松手不到这么久算轻点：不录音，只提示 */
        const val TAP_MS = 300L
        /** 开始后这么短就松手的一段，里面不会有完整的话，重按时直接丢掉 */
        const val SHORT_SEGMENT_MS = 600L
        /** 按住或锁定中，连着这么多次一开始就出错才停下 */
        const val MAX_QUICK_FAILURES = 5
        /** 出结果后至少等这么久再开始下一段（小米引擎收尾最晚约 0.45 秒关完麦克风） */
        const val SETTLE_AFTER_RESULT_MS = 550L
        /** 本地识别「说完一句再发」：停顿这么久没出新字就把这句发出去（出字间隔长的模型再加长，见 LocalPress.pauseMs） */
        const val SENTENCE_PAUSE_MS = 800L
        /** 音量条每格的时长和最多记多少格（够铺满最宽的语音按钮） */
        const val BAR_MS = 70L
        const val BAR_COUNT = 96
    }
}
