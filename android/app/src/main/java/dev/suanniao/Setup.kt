package dev.suanniao

import android.annotation.SuppressLint
import android.bluetooth.BluetoothClass
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.provider.Settings
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.LinearLayout
import dev.suanniao.Overlays.Btn
import dev.suanniao.Overlays.Style

/**
 * 上手相关的几块界面，都是自绘的：
 * - 使用教程（画面在 Tutorial.kt）：第一次打开时整屏出来，连电脑的三步（在电脑上打开蒜鸟 → 蓝牙配对 → 选电脑）
 *   就在里面做，要权限前先说清楚用途；后面几页讲触控板、按键、语音怎么用。⋯ 菜单里能再看
 * - 选择电脑：只列配对过的电脑（蓝牙设备类型是电脑），可以打开「显示全部设备」，标出当前选的
 * - 触控板手势说明：第一次见到触控板时出来一次（教程里看过触控板那页就不再出），⋯ 菜单里也能再看
 */
class Setup(private val host: Host, private val overlays: Overlays) {

    /** 扫电脑上的二维码要做什么：只蓝牙配对、只局域网绑定、能做的都做 */
    enum class ScanFor { ANY, BT, LAN }

    /** MainActivity 提供的能力 */
    interface Host {
        val context: Context
        fun hasBluetoothPermission(): Boolean
        fun askBluetoothPermission()
        fun openBluetoothSettings()
        fun savedAddress(): String?
        fun savedName(): String?
        fun chooseComputer(address: String, name: String)
        fun finishOnboarding()
        /** 扫电脑上的二维码：蓝牙配对、局域网绑定（forWhat 只做其中一种；ANY 能做的都做） */
        fun scanComputer(forWhat: ScanFor = ScanFor.ANY) {}
        /** 扫码绑定的电脑叫什么；没绑定、电脑上解除了是 null */
        fun lanName(): String? = null
        /** 局域网正连着 */
        fun lanConnected(): Boolean = false
        /** 带去设置里放开后台限制（局域网息屏不断） */
        fun openBatterySettings() {}
        /** 扫码蓝牙配对正在等系统配对的结果（手机上弹着「配对」） */
        fun btPairing(): Boolean = false
        /** 蓝牙正连着 */
        fun btConnected(): Boolean = false
    }

    private val context get() = host.context
    private val palette = overlays.palette
    private val density = context.resources.displayMetrics.density
    private fun dp(v: Float) = (v * density).toInt()

    private val tutorial = Tutorial(host, overlays, this)

    // ───────── 使用教程（Tutorial.kt） ─────────

    /** 第一次打开时的教程开着：这时先不连电脑、不显示顶上的提示条 */
    val onboardingOpen get() = tutorial.isOpen && tutorial.firstRun

    /** 看过（或跳过）教程了，以后打开不再自动出来 */
    val tutorialSeen get() = tutorial.seen

    fun showOnboarding() = tutorial.show(firstRun = true)

    /** ⋯ 菜单里再看一遍，可以直接翻到某一页 */
    fun showTutorial(page: Int = 0) = tutorial.show(firstRun = false, page = page)

    fun closeOnboarding() = tutorial.close()

    /** 权限、配对、选好的电脑变了：教程里连电脑那几页跟着变 */
    fun refreshOnboarding() = tutorial.refresh()

    /** 和这部手机配对过的电脑；没有蓝牙权限、蓝牙没开时是空的 */
    @SuppressLint("MissingPermission")
    fun pairedComputers(): List<BluetoothDevice> {
        if (!host.hasBluetoothPermission()) return emptyList()
        val adapter = context.getSystemService(BluetoothManager::class.java)?.adapter ?: return emptyList()
        return runCatching { adapter.bondedDevices.filter(::isComputer) }.getOrDefault(emptyList())
    }

    /** 电脑的蓝牙列表里显示的这部手机的名字 */
    @SuppressLint("MissingPermission")
    fun phoneName(hasPerm: Boolean): String? {
        if (hasPerm) {
            runCatching { context.getSystemService(BluetoothManager::class.java)?.adapter?.name }.getOrNull()
                ?.takeIf { it.isNotBlank() }?.let { return it }
        }
        return runCatching { Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME) }.getOrNull()?.takeIf { it.isNotBlank() }
    }

    // ───────── 选择电脑 ─────────

    private var showAll = false

    /** 只列配对过的电脑；打开「显示全部设备」后耳机、手环也列出来。当前选的排最前、打勾 */
    @SuppressLint("MissingPermission")
    fun showPicker(onPick: (address: String, name: String) -> Unit) {
        lateinit var sheet: Overlays.Sheet
        // 局域网扫码不用蓝牙：没权限、蓝牙没开、手机没蓝牙时也要能点到
        fun Overlays.Sheet.scanRow() {
            row("扫码走局域网", "不用蓝牙：手机和电脑连着同一个网络，扫电脑上的二维码") {
                sheet.dismiss()
                host.scanComputer(ScanFor.LAN)
            }
        }
        // 扫码蓝牙配对：不用去蓝牙设置里搜，扫一下、点「配对」
        fun Overlays.Sheet.btScanRow() {
            row("扫码配对新电脑", "扫电脑蒜鸟上的二维码，手机上点一下「配对」") {
                sheet.dismiss()
                host.scanComputer(ScanFor.BT)
            }
        }
        sheet = overlays.sheet build@{
            header("选择电脑", "列的是和这部手机配对过的电脑")
            if (!host.hasBluetoothPermission()) {
                row("允许「附近的设备」权限", "用蓝牙连电脑要这个权限") {
                    sheet.dismiss()
                    host.askBluetoothPermission()
                }
                btScanRow()
                scanRow()
                return@build
            }
            val adapter = context.getSystemService(BluetoothManager::class.java)?.adapter
            if (adapter == null) {
                row("这部手机没有蓝牙", null, enabled = false, trailing = null, onClick = null)
                scanRow()
                return@build
            }
            if (!adapter.isEnabled) {
                row("手机蓝牙没开", "打开以后回到这里", trailing = overlays.chevron()) { host.openBluetoothSettings() }
                btScanRow()
                scanRow()
                return@build
            }
            val current = host.savedAddress()
            val bonded = runCatching { adapter.bondedDevices.toList() }.getOrDefault(emptyList())
            val list = (if (showAll) bonded else bonded.filter(::isComputer))
                .sortedWith(compareByDescending<BluetoothDevice> { it.address == current }.thenBy { it.name ?: "" })
            if (list.isEmpty()) {
                row(if (showAll) "还没有配对过的设备" else "没找到配对过的电脑", "扫码配对，或者去蓝牙设置里配对", enabled = false, trailing = null, onClick = null)
            }
            list.forEach { d ->
                val name = d.name ?: "没有名字的设备"
                val isCurrent = d.address == current
                val sub = buildString {
                    append(kindOf(d))
                    if (isCurrent) append(" · 当前选的")
                    if (showAll) append(" · ").append(d.address)
                }
                row(name, sub, trailing = if (isCurrent) overlays.check() else null) {
                    sheet.dismiss()
                    onPick(d.address, name)
                }
            }
            divider()
            toggle("显示全部设备", "耳机、手环这些也列出来", showAll) { on ->
                showAll = on
                sheet.body.postDelayed({ sheet.refresh() }, 180)
            }
            btScanRow()
            row("去蓝牙设置配对新电脑", null) { host.openBluetoothSettings() }
            scanRow()
        }
    }

    private fun isComputer(d: BluetoothDevice) =
        runCatching { d.bluetoothClass?.majorDeviceClass == BluetoothClass.Device.Major.COMPUTER }.getOrDefault(false)

    private fun kindOf(d: BluetoothDevice): String = when (runCatching { d.bluetoothClass?.majorDeviceClass }.getOrNull()) {
        BluetoothClass.Device.Major.COMPUTER -> "电脑"
        BluetoothClass.Device.Major.PHONE -> "手机"
        BluetoothClass.Device.Major.AUDIO_VIDEO -> "耳机、音箱"
        BluetoothClass.Device.Major.WEARABLE -> "手表、手环"
        BluetoothClass.Device.Major.PERIPHERAL -> "键盘、鼠标"
        else -> "其他设备"
    }

    // ───────── 触控板手势说明 ─────────

    fun showGestureGuide(onDone: () -> Unit) {
        val list = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
        GESTURES.forEach { (kind, title, body) ->
            list.addView(LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, dp(6f), 0, dp(6f))
                addView(GestureIcon(context, palette, kind), LinearLayout.LayoutParams(dp(46f), dp(46f)))
                addView(LinearLayout(context).apply {
                    orientation = LinearLayout.VERTICAL
                    addView(overlays.text(title, 15f, 850, palette.fg))
                    addView(overlays.text(body, 12.5f, 650, palette.sub).apply { setPadding(0, dp(3f), 0, 0) })
                }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginStart = dp(14f) })
            })
        }
        overlays.dialog("触控板怎么用", null, listOf(Btn("知道了", Style.PRIMARY) { onDone() }), extra = list).apply {
            onDismiss = { onDone() }
        }
    }

    private companion object {
        val GESTURES = listOf(
            Triple(GestureIcon.MOVE, "单指滑动", "移动鼠标"),
            Triple(GestureIcon.TAP, "单指轻点", "左键单击"),
            Triple(GestureIcon.SCROLL, "双指滑动", "滚动；松手还会顺着滑一段"),
            Triple(GestureIcon.RIGHT, "双指轻点", "右键"),
            Triple(GestureIcon.MIDDLE, "三指轻点", "中键"),
            Triple(GestureIcon.DRAG, "轻点一下，马上按住拖", "按住左键拖动（选中文字、拖窗口）"),
        )
    }
}

/**
 * 手势的小图：圆角方块里画几个手指点，加上动作（箭头、一圈波纹）。
 * fill 是方块的底色；active 从 0 到 1 时反色（教程里正在演示的那个手势）
 */
@SuppressLint("ViewConstructor")
class GestureIcon(context: Context, private val palette: Ui.Palette, private val kind: Int, private val fill: Int = palette.bg) : View(context) {
    var active = 0f
        set(v) {
            if (field != v) {
                field = v
                invalidate()
            }
        }
    private val density = context.resources.displayMetrics.density
    private val tile = Paint(Paint.ANTI_ALIAS_FLAG)
    private val dot = Paint(Paint.ANTI_ALIAS_FLAG)
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 1.8f * density
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
    private val path = Path()
    private val box = RectF()

    override fun onDraw(canvas: Canvas) {
        val s = width.toFloat()
        val ink = Ui.blend(palette.fg, palette.tile, active)
        tile.color = Ui.blend(fill, palette.fg, active)
        box.set(0f, 0f, s, s)
        canvas.drawRoundRect(box, s * 0.28f, s * 0.28f, tile)
        dot.color = ink
        val r = 4f * density
        val c = s / 2
        fun fingers(n: Int, cy: Float) {
            val gap = 10f * density
            val start = c - gap * (n - 1) / 2
            for (i in 0 until n) canvas.drawCircle(start + i * gap, cy, r, dot)
        }
        fun ripple(n: Int, cy: Float) {
            stroke.color = Ui.blend(palette.mut, Ui.alpha(palette.tile, 0.6f), active)
            val w = (n - 1) * 10f * density / 2 + 9f * density
            box.set(c - w, cy - 9f * density, c + w, cy + 9f * density)
            canvas.drawRoundRect(box, 9f * density, 9f * density, stroke)
        }
        stroke.color = ink
        when (kind) {
            MOVE -> {
                fingers(1, c)
                arrow(canvas, c + 7 * density, c, c + 17 * density, c)
                arrow(canvas, c - 7 * density, c, c - 17 * density, c)
            }
            TAP -> { fingers(1, c); ripple(1, c) }
            SCROLL -> {
                fingers(2, c)
                arrow(canvas, c, c - 8 * density, c, c - 18 * density)
                arrow(canvas, c, c + 8 * density, c, c + 18 * density)
            }
            RIGHT -> { fingers(2, c); ripple(2, c) }
            MIDDLE -> { fingers(3, c); ripple(3, c) }
            DRAG -> {
                fingers(1, c - 4 * density)
                ripple(1, c - 4 * density)
                stroke.color = ink
                arrow(canvas, c, c + 7 * density, c + 12 * density, c + 17 * density)
            }
        }
    }

    private fun arrow(canvas: Canvas, x0: Float, y0: Float, x1: Float, y1: Float) {
        path.reset()
        path.moveTo(x0, y0)
        path.lineTo(x1, y1)
        val dx = x1 - x0
        val dy = y1 - y0
        val len = kotlin.math.hypot(dx, dy)
        val ux = dx / len
        val uy = dy / len
        val h = 4f * density
        path.moveTo(x1 - ux * h - uy * h, y1 - uy * h + ux * h)
        path.lineTo(x1, y1)
        path.lineTo(x1 - ux * h + uy * h, y1 - uy * h - ux * h)
        canvas.drawPath(path, stroke)
    }

    companion object {
        const val MOVE = 0
        const val TAP = 1
        const val SCROLL = 2
        const val RIGHT = 3
        const val MIDDLE = 4
        const val DRAG = 5
    }
}
