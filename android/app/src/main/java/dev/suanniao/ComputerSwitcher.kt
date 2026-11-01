package dev.suanniao

import android.content.Context
import android.graphics.drawable.Drawable
import android.text.InputType
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import dev.suanniao.Overlays.Btn
import dev.suanniao.Overlays.Style

/**
 * 「我的电脑」的界面（打板：xiaomi13\界面打板\多电脑切换，用户选了方案 B）：
 * - PcButton：顶栏「蒜鸟」旁边的电脑按钮（电脑图标 + 名字 + 上下箭头），点了打开「切换电脑」；没有电脑时是「＋ 添加电脑」
 * - ComputerSwitcher：「切换电脑」面板（点一行就切过去，行尾「⋯」或长按 = 改名、移除），改名框、移除确认
 */
class PcButton(context: Context, private val palette: Ui.Palette) : LinearLayout(context) {
    private fun dp(v: Float) = (v * resources.displayMetrics.density).toInt()

    private val icon = ImageView(context)
    private val name = TextView(context).apply {
        setTextColor(palette.fg)
        typeface = Ui.font(context, 800)
        textSize = 13f
        includeFontPadding = false
        maxLines = 1
        maxWidth = dp(118f)
        ellipsize = TextUtils.TruncateAt.END
    }
    private val arrows = ImageView(context).apply { setImageDrawable(tinted(R.drawable.ic_updown, palette.mut)) }

    init {
        orientation = HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(9f), 0, dp(8f), 0)
        background = Ui.round(palette.tile, dp(15f).toFloat(), Ui.hairline(context).toInt(), palette.line)
        addView(icon, LayoutParams(dp(15f), dp(15f)).apply { marginEnd = dp(5f) })
        addView(name, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
        addView(arrows, LayoutParams(dp(12f), dp(12f)).apply { marginStart = dp(4f) })
        Ui.springy(this)
    }

    private fun tinted(res: Int, color: Int): Drawable = context.getDrawable(res)!!.mutate().apply { setTint(color) }

    /** title = null：还没有电脑，显示「＋ 添加电脑」 */
    fun set(title: String?) {
        if (title == null) {
            icon.visibility = View.GONE
            arrows.visibility = View.GONE
            setIfChanged("＋ 添加电脑")
            contentDescription = "添加电脑"
        } else {
            if (icon.drawable == null) icon.setImageDrawable(tinted(R.drawable.ic_laptop, palette.fg))
            icon.visibility = View.VISIBLE
            arrows.visibility = View.VISIBLE
            setIfChanged(title)
            contentDescription = "正在用：$title，点一下切换电脑"
        }
    }

    private fun setIfChanged(s: String) {
        if (name.text.toString() != s) name.text = s
    }
}

class ComputerSwitcher(
    private val context: Context,
    private val overlays: Overlays,
    private val palette: Ui.Palette,
    private val host: Host,
) {
    interface Host {
        /** 正在用的那台现在的连接情况 */
        fun linkStatus(): LinkService.Status
        /** 点了另一台：切过去 */
        fun switchTo(pc: Computer)
        /** 「手机蓝牙选择」：配对过的蓝牙设备里选 */
        fun pickBluetooth()
        /** 「扫码连接电脑」 */
        fun scanComputer()
        fun say(text: String)
    }

    private fun dp(v: Float) = (v * context.resources.displayMetrics.density).toInt()
    private fun dpf(v: Float) = v * context.resources.displayMetrics.density

    var sheet: Overlays.Sheet? = null
        private set

    fun refresh() {
        sheet?.refresh()
    }

    fun show() {
        if (overlays.isOpen) return
        lateinit var s: Overlays.Sheet
        s = overlays.sheet {
            header("切换电脑", "选哪台就连哪台")
            val st = Computers.load(context)
            val list = Computers.sorted(context)
            if (list.isEmpty()) {
                row("还没有电脑", "扫电脑上蒜鸟的二维码", enabled = false, trailing = null, onClick = null)
            }
            list.forEach { pc ->
                view(rowView(pc, pc.id == st.current) {
                    s.dismiss()
                    if (pc.id != st.current) host.switchTo(pc)
                }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
                    setMargins(dp(10f), 0, dp(10f), 0)
                })
            }
            buttons(
                Btn("手机蓝牙选择") { s.dismiss(); host.pickBluetooth() },
                Btn("扫码连接电脑", Style.PRIMARY) { s.dismiss(); host.scanComputer() },
            )
        }
        s.onDismiss = { if (sheet === s) sheet = null }
        sheet = s
    }

    /** 连接方式：「蓝牙 + 局域网」「蓝牙」「局域网」 */
    private fun ways(pc: Computer): String {
        val lan = pc.lan?.takeIf { !it.revoked }
        return listOfNotNull(if (pc.bt != null) "蓝牙" else null, if (lan != null) "局域网" else null).joinToString(" + ")
    }

    /** 每行下面的小字：连接方式 · 状态 */
    private fun subtitle(pc: Computer, current: Boolean): String {
        val ways = ways(pc)
        if (ways.isEmpty()) return if (pc.lan?.revoked == true) "局域网 · 已解绑" else "要重新扫码"
        if (!current) return ago(pc.usedAt)?.let { "$ways · $it" } ?: ways
        val s = host.linkStatus()
        return when {
            s.connected -> "${s.via ?: ways} · 已连接"
            s.phase == LinkService.Phase.CONNECTING -> "$ways · 连接中…"
            else -> "$ways · 没连上"
        }
    }

    private fun ago(at: Long): String? {
        if (at <= 0) return null
        val days = ((System.currentTimeMillis() - at) / 86_400_000L).toInt()
        return when {
            days <= 0 -> "今天用过"
            days == 1 -> "昨天用过"
            days < 30 -> "${days} 天前"
            else -> "很久没用"
        }
    }

    private fun rowView(pc: Computer, current: Boolean, onClick: () -> Unit): View {
        val row = LinearLayout(context).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(64f)
            setPadding(dp(12f), dp(8f), dp(4f), dp(8f))
            background = Ui.round(0, dpf(16f))
        }
        val iconBox = FrameLayout(context).apply {
            background = Ui.round(if (current) palette.fg else Ui.blend(palette.tile, palette.fg, 0.07f), dpf(12f))
            addView(ImageView(context).apply {
                setImageDrawable(context.getDrawable(R.drawable.ic_laptop)!!.mutate().apply { setTint(if (current) palette.tile else palette.fg) })
            }, FrameLayout.LayoutParams(dp(22f), dp(22f), Gravity.CENTER))
        }
        row.addView(iconBox, LinearLayout.LayoutParams(dp(40f), dp(40f)).apply { marginEnd = dp(14f) })
        val sub = subtitle(pc, current)
        row.addView(LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            addView(overlays.text(pc.name, 15.5f, 750, palette.fg).apply {
                maxLines = 1
                ellipsize = TextUtils.TruncateAt.END
            })
            addView(overlays.text(sub, 12.5f, 650, palette.sub).apply {
                setPadding(0, dp(4f), 0, 0)
                maxLines = 1
                ellipsize = TextUtils.TruncateAt.END
            })
        }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
        if (current) {
            row.addView(overlays.text("正在用", 12f, 850, if (palette.dark) 0xFF1C2A06.toInt() else 0xFF3D5C0E.toInt()).apply {
                gravity = Gravity.CENTER
                setPadding(dp(9f), 0, dp(9f), 0)
                background = Ui.round(if (palette.dark) 0xFFC3F06A.toInt() else 0xFFDCF7A1.toInt(), dpf(12f))
            }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(24f)).apply { marginStart = dp(10f) })
        }
        row.addView(ImageView(context).apply {
            setImageDrawable(context.getDrawable(R.drawable.ic_more)!!.mutate().apply { setTint(palette.mut) })
            scaleType = ImageView.ScaleType.CENTER_INSIDE
            setPadding(dp(8f), dp(8f), dp(8f), dp(8f))
            contentDescription = "改名、移除「${pc.name}」"
            Ui.springy(this)
            setOnClickListener { manage(pc) }
        }, LinearLayout.LayoutParams(dp(40f), dp(40f)).apply { marginStart = dp(4f) })
        row.contentDescription = "${pc.name}，$sub" + if (current) "，正在用" else "，点一下切到这台"
        Ui.springy(row, 0.98f)
        row.setOnClickListener {
            Ui.buzz(row, Ui.Buzz.TAP)
            onClick()
        }
        row.setOnLongClickListener {
            Ui.buzz(row, Ui.Buzz.TAP)
            manage(pc)
            true
        }
        return row
    }

    /** 一台电脑的小面板：改名、移除 */
    private fun manage(pc: Computer) {
        sheet?.dismiss()
        lateinit var s: Overlays.Sheet
        s = overlays.sheet {
            header(pc.name, ways(pc).ifEmpty { null })
            row("改名") {
                s.dismiss()
                rename(pc)
            }
            val remove = row("移除", trailing = null) {
                s.dismiss()
                confirmRemove(pc)
            }
            ((remove.tag as Overlays.RowParts).texts.getChildAt(0) as TextView).setTextColor(palette.red)
        }
    }

    private fun rename(pc: Computer) {
        val input = EditText(context).apply {
            setText(pc.name)
            setSelectAllOnFocus(true)
            setSingleLine()
            inputType = InputType.TYPE_CLASS_TEXT
            imeOptions = EditorInfo.IME_ACTION_DONE
            textSize = 15.5f
            typeface = Ui.font(context, 750)
            setTextColor(palette.fg)
            setHintTextColor(palette.mut)
            hint = "电脑名"
            background = Ui.round(palette.bg, dpf(14f))
            setPadding(dp(14f), 0, dp(14f), 0)
        }
        val box = FrameLayout(context).apply {
            addView(input, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, dp(48f)))
        }
        val save = {
            val name = input.text.toString().trim()
            if (name.isNotEmpty() && name != pc.name) Computers.rename(context, pc.id, name)
            hideKeyboard(input)
        }
        // 对话框的按钮点了会自己关掉
        val dialog = overlays.dialog("改名", null, listOf(
            Btn("取消") { hideKeyboard(input) },
            Btn("保存", Style.PRIMARY) { save() },
        ), extra = box)
        input.setOnEditorActionListener { _, action, _ ->
            if (action == EditorInfo.IME_ACTION_DONE) {
                save()
                dialog.dismiss()
            }
            action == EditorInfo.IME_ACTION_DONE
        }
        input.postDelayed({
            input.requestFocus()
            context.getSystemService(InputMethodManager::class.java)?.showSoftInput(input, InputMethodManager.SHOW_IMPLICIT)
        }, 250)
    }

    private fun hideKeyboard(v: View) {
        context.getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(v.windowToken, 0)
    }

    private fun confirmRemove(pc: Computer) {
        val current = Computers.load(context).current == pc.id
        overlays.dialog(
            "移除「${pc.name}」？",
            if (current) "会断开这台，再用要重新扫码" else "再用要重新扫码",
            listOf(Btn("取消"), Btn("移除", Style.DANGER) {
                Computers.remove(context, pc.id)
                host.say("已移除「${pc.name}」")
            }),
        )
    }
}
