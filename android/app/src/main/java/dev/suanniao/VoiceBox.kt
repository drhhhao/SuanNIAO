package dev.suanniao

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PathMeasure
import android.graphics.RectF
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextWatcher
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.animation.LinearInterpolator
import android.view.inputmethod.BaseInputConnection
import android.widget.EditText
import android.widget.LinearLayout

/**
 * 语音输入框：点一下弹出手机输入法，打字或用输入法自带的语音输入。
 * 文字停止变化 delayMs 后（并且输入法没有还没上屏的字）自动发给电脑：发送按钮外圈画一圈倒计时，
 * 倒计时的时候按钮写着「取消」，点它（或点文字）就取消这次自动发送；改了字重新倒计时。
 * 发出去了才清空；没连上电脑时不自动发，点「发送」也只是抖一下、字留着。
 */
@SuppressLint("ViewConstructor")
class VoiceBox(
    context: Context,
    /** 返回 true = 真的发出去了 */
    private val onSend: (String) -> Boolean,
    private val onNotice: (String) -> Unit,
) : LinearLayout(context) {

    var autoSend = true
    var delayMs = 1000L
    /** 没连上电脑：不自动发 */
    var offline = false
        set(value) {
            field = value
            if (value) stopCountdown()
        }

    private val palette = Ui.palette(context)
    private val main = Handler(Looper.getMainLooper())
    private val density = resources.displayMetrics.density
    private val autoSendTask = Runnable { trySend(auto = true) }
    /** 自己清空输入框时不算用户改了字 */
    private var clearing = false
    private val sendButton = SendButton(context)

    val input: EditText = EditText(context).apply {
        hint = "点这里打字，或用输入法的语音输入"
        setTextColor(palette.fg)
        setHintTextColor(palette.sub)
        typeface = Ui.font(context, 600)
        textSize = 15f
        gravity = Gravity.TOP or Gravity.START
        background = null
        addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
            override fun afterTextChanged(s: Editable) {
                if (clearing) return
                if (autoSend && !offline && s.isNotBlank()) startCountdown() else stopCountdown()
            }
        })
        // 倒计时的时候点文字 = 不要自动发了（要改字）
        setOnTouchListener { _, e ->
            if (e.actionMasked == MotionEvent.ACTION_DOWN && counting()) {
                stopCountdown()
                onNotice("这次不自动发了，改好点「发送」")
            }
            false
        }
    }

    init {
        orientation = HORIZONTAL
        setPadding((10 * density).toInt(), (6 * density).toInt(), (6 * density).toInt(), (6 * density).toInt())
        background = GradientDrawable().apply {
            cornerRadius = Ui.RADIUS_DP * density
            setColor(palette.tile)
            setStroke(Ui.hairline(context).toInt(), palette.line)
        }
        addView(input, LayoutParams(0, LayoutParams.MATCH_PARENT, 1f))
        addView(sendButton, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply { gravity = Gravity.BOTTOM })
    }

    private fun counting() = sendButton.progress >= 0f

    private fun startCountdown() {
        main.removeCallbacks(autoSendTask)
        main.postDelayed(autoSendTask, delayMs)
        sendButton.countdown(delayMs)
    }

    private fun stopCountdown() {
        main.removeCallbacks(autoSendTask)
        sendButton.countdown(-1)
    }

    private fun trySend(auto: Boolean) {
        val editable = input.text
        if (auto && BaseInputConnection.getComposingSpanStart(editable) != -1) {
            main.postDelayed(autoSendTask, 300) // 输入法还有没上屏的字，再等等
            return
        }
        stopCountdown()
        val text = editable.toString()
        if (text.isBlank()) return
        if (!onSend(text)) {
            // 没发出去：字留着，抖一下
            Ui.buzz(sendButton, Ui.Buzz.REJECT)
            Ui.shake(sendButton)
            onNotice("未连接电脑 · 没有发送，字还在框里")
            return
        }
        clearing = true
        input.setText("")
        clearing = false
    }

    /** 发送按钮：黑色胶囊；自动发送倒计时时外面一圈描边从顶上顺时针画满，按钮写「取消」 */
    private inner class SendButton(context: Context) : View(context) {
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
        private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            style = Paint.Style.STROKE
            strokeWidth = 2.4f * density
            strokeCap = Paint.Cap.ROUND
        }
        private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            typeface = Ui.font(context, 850)
            textSize = Ui.sp(context, 14f)
            textAlign = Paint.Align.CENTER
        }
        private val path = Path()
        private val piece = Path()
        private val measure = PathMeasure()
        private val box = RectF()
        private var anim: ValueAnimator? = null
        /** 倒计时进度 0～1；-1 = 没在倒计时 */
        var progress = -1f
            private set

        init {
            isClickable = true
            contentDescription = "发送"
            Ui.springy(this)
            setOnClickListener {
                if (counting()) {
                    stopCountdown()
                    onNotice("这次不自动发了")
                } else {
                    trySend(auto = false)
                }
            }
        }

        fun countdown(ms: Long) {
            anim?.cancel()
            if (ms < 0) {
                progress = -1f
                contentDescription = "发送"
                invalidate()
                return
            }
            contentDescription = "取消自动发送"
            anim = ValueAnimator.ofFloat(0f, 1f).apply {
                duration = ms
                interpolator = LinearInterpolator()
                addUpdateListener {
                    progress = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            setMeasuredDimension((78 * density).toInt(), (46 * density).toInt())
        }

        override fun onDraw(canvas: Canvas) {
            val w = width.toFloat()
            val h = height.toFloat()
            val gap = 4.5f * density
            box.set(gap, gap, w - gap, h - gap)
            val r = box.height() / 2
            fill.color = palette.fg
            canvas.drawRoundRect(box, r, r, fill)
            text.color = palette.tile
            val label = if (progress >= 0f) "取消" else "发送"
            canvas.drawText(label, w / 2, h / 2 - (text.ascent() + text.descent()) / 2, text)
            if (progress >= 0f) {
                // 外圈：胶囊外面一圈，从顶边中间开始顺时针
                val o = ring.strokeWidth / 2 + 0.5f
                val l = o
                val t = o
                val rr = w - o
                val b = h - o
                val rad = (b - t) / 2
                path.reset()
                path.moveTo(w / 2, t)
                path.lineTo(rr - rad, t)
                path.arcTo(rr - 2 * rad, t, rr, b, -90f, 180f, false)
                path.lineTo(l + rad, b)
                path.arcTo(l, t, l + 2 * rad, b, 90f, 180f, false)
                path.close()
                measure.setPath(path, false)
                piece.reset()
                measure.getSegment(0f, measure.length * progress.coerceIn(0f, 1f), piece, true)
                ring.color = palette.line
                canvas.drawPath(path, ring)
                ring.color = palette.fg
                canvas.drawPath(piece, ring)
            }
        }
    }
}
