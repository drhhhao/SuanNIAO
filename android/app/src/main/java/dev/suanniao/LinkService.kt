package dev.suanniao

import android.Manifest
import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothSocket
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.os.BatteryManager
import android.os.Binder
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import org.json.JSONObject
import java.io.BufferedReader
import java.io.Closeable
import java.io.InputStream
import java.io.InputStreamReader
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.math.roundToInt

/**
 * 保持和电脑的连接：蓝牙串口，和扫码绑定过的电脑的局域网连接（两条可以同时连着，发消息走局域网，断了走蓝牙）。
 * 前台服务：息屏、切到后台都不断开，断了自动重连。
 * - 连上时发 hello（手机的蓝牙名、型号、App 版本）；电量、充电状态变化时发电量
 * - 记着电脑推来的最新配置和电脑的名字、版本：界面重新打开、重新绑定时直接套用，不会露出过时的界面
 * - 发送直接写进 socket：send() 返回 true 才算真的发出去了，调用方据此给反馈（触控板的移动、滚动除外，见 move）
 * - 界面没开着时，配置回执由这里直接回；界面开着时由界面套用完再回
 * - 通知只写中文结论，带「暂停按键 / 恢复按键」「断开」两个按钮
 */
class LinkService : Service() {

    enum class Phase {
        /** 没在连：刚创建还没开始，或者点了「断开」 */
        OFF,
        /** 还没选要连的电脑 */
        NO_DEVICE,
        /** 没有「附近的设备」权限 */
        NO_PERMISSION,
        /** 手机蓝牙关着 */
        BT_OFF,
        CONNECTING,
        /** 没连上，过一会儿自动再试（这期间的自动重试不再来回报「正在连接」，免得界面一直闪） */
        WAITING,
        CONNECTED,
    }

    /** lan、bt：现在哪条连接连着（两条可以同时连着） */
    class Status(val phase: Phase, val pcName: String?, val lan: Boolean = false, val bt: Boolean = false) {
        val connected get() = phase == Phase.CONNECTED
        /** 连着时走的是哪种：「局域网」「蓝牙」「局域网 + 蓝牙」 */
        val via: String? get() = when {
            lan && bt -> "局域网 + 蓝牙"
            lan -> "局域网"
            bt -> "蓝牙"
            else -> null
        }
    }

    /** 局域网那条连接的情况。REVOKED：电脑上解除了这部手机的绑定，等重新扫码 */
    enum class LanPhase { NONE, CONNECTING, WAITING, CONNECTED, REVOKED }

    interface Listener {
        fun onLinkStatus(status: Status)
        fun onLinkMessage(msg: JSONObject)
        /** 在手机上暂停 / 恢复按键的结果：ok = false 表示等了一阵电脑也没改（多半是旧版电脑端） */
        fun onEnableResult(ok: Boolean, enabled: Boolean)
        /** 扫码绑定的结果；也用来提示电脑上解除了绑定 */
        fun onPairResult(ok: Boolean, message: String) {}
    }

    inner class LocalBinder : Binder() {
        val service: LinkService get() = this@LinkService
    }

    var listener: Listener? = null

    @Volatile var connected = false
        private set
    @Volatile var status = Status(Phase.OFF, null)
        private set
    /** 点了「断开」（通知上或菜单里）：界面显示「已断开」，下次打开 App 再连 */
    @Volatile var stoppedByUser = false
        private set
    /** 电脑推来的最新配置（也存在 prefs 里，App 重启后先用它） */
    var lastConfig: JSONObject? = null
        private set
    /** 电脑 hello 里说的电脑名、电脑端版本 */
    var pcHost: String? = null
        private set
    var pcApp: String? = null
        private set
    /** 电脑上是不是暂停了按键（来自最新配置） */
    var paused = false
        private set
    /** 刚请求了暂停（false）/ 恢复（true），还在等电脑推来的配置 */
    var pendingEnabled: Boolean? = null
        private set

    private val binder = LocalBinder()
    private val main = Handler(Looper.getMainLooper())
    private val prefs by lazy { getSharedPreferences(PREFS, MODE_PRIVATE) }
    private val outbox = LinkedBlockingQueue<Any>()
    /** 一条连接（蓝牙或局域网）：读写都用它的流 */
    private class Conn(val lan: Boolean, val socket: Closeable, val input: InputStream, val output: OutputStream) {
        @Volatile var closed = false
        /** 发了心跳还没收到任何回音的起点（0 = 收到过了）：局域网太久没回音就当断了 */
        @Volatile var awaitSince = 0L
        /** 电脑 hello 里的编号（旧版电脑端没有） */
        @Volatile var pcId: String? = null
        /** 电脑说它正连着别的手机，马上会断开 */
        @Volatile var busy = false
        /**
         * 局域网连接的发送队列：Android 不让主线程碰网络（蓝牙不管），而心跳、配置回执这些是在主线程发的，
         * 所以局域网的消息都排进这里，由这条连接自己的写线程按顺序写出去
         */
        val queue: LinkedBlockingQueue<Any>? = if (lan) LinkedBlockingQueue() else null

        fun close() {
            closed = true
            queue?.offer(STOP)
            runCatching { socket.close() }
        }

        companion object {
            val STOP = Any()
        }
    }
    @Volatile private var btConn: Conn? = null
    @Volatile private var lanConn: Conn? = null
    /** 发消息走的连接：局域网优先。蓝牙连的要是另一台电脑（和扫码绑定的不是同一台），只用局域网那台 */
    private fun active(): Conn? = lanConn ?: btConn?.takeUnless { foreign(it) }

    /** 蓝牙连着的电脑和局域网连着的不是同一台：这条蓝牙只收心跳，不收配置、不发输入 */
    private fun foreign(c: Conn): Boolean {
        if (c.lan) return false
        val lanPc = lanConn?.pcId ?: return false
        return c.pcId != null && c.pcId != lanPc
    }

    // 还按着的键（直通）、鼠标键：按着期间消息都走按下时那条连接，全松开了才换。
    // 不然按下走蓝牙、松开走刚连上的局域网，松开可能比按下先到，或者到了不认识它的会话，电脑上就一直按着
    private val holdLock = Any()
    private val heldKeys = HashSet<String>()
    private val heldButtons = HashSet<String>()
    private var pinned: Conn? = null

    /** 这条消息走哪条连接（顺便记下按住、松开）；msg 为空 = 移动、滚动 */
    private fun route(msg: JSONObject?): Conn? = synchronized(holdLock) {
        val pin = pinned?.takeIf { !it.closed && !foreign(it) }
        val c = pin ?: active()
        if (msg != null) when (msg.optString("t")) {
            "hold" -> msg.optString("id").let { if (msg.optBoolean("down")) heldKeys += it else heldKeys -= it }
            "mouse" -> when (msg.optString("a")) {
                "down" -> heldButtons += msg.optString("b")
                "up" -> heldButtons -= msg.optString("b")
            }
        }
        pinned = if (heldKeys.isEmpty() && heldButtons.isEmpty()) null else (pin ?: c)
        c
    }

    /** 全松开了（息屏）：下一条消息可以换连接 */
    private fun clearHolds() = synchronized(holdLock) {
        heldKeys.clear()
        heldButtons.clear()
        pinned = null
    }
    @Volatile private var btPhase = Phase.OFF
    @Volatile var lanPhase = LanPhase.NONE
        private set
    /** 局域网绑定的电脑名 */
    @Volatile private var lanPcName: String? = null
    private val lanKicks = LinkedBlockingQueue<Unit>()
    @Volatile private var lanBackoff = 1000L
    @Volatile private var lanQuiet = false
    private var wifiLock: WifiManager.WifiLock? = null
    /** 连上 Wi-Fi 时马上试局域网，不用等重试 */
    private val wifiCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            lanBackoff = 1000L
            lanQuiet = false
            lanKicks.offer(Unit)
        }
    }
    @Volatile private var running = false
    /** 每次开始连接循环加一：旧的循环看到不一样就自己退出 */
    @Volatile private var loopGen = 0
    @Volatile private var backoff = 1000L
    /** 「重试」：叫醒正在等的连接循环 */
    private val kicks = LinkedBlockingQueue<Unit>()
    /** 上一次自动重连失败了：之后的自动重试不再报「正在连接」 */
    @Volatile private var quiet = false
    @Volatile private var screenOn = true
    @Volatile private var address: String? = null
    /** 正在连的电脑的蓝牙名 */
    @Volatile private var deviceName: String? = null
    private var foreground = false
    /** 通知上临时显示的一句话（界面没开着时的提示） */
    private var noteOverride: String? = null

    // 移动和滚动先累加：写线程忙不过来时自动合并，不会越积越多造成延迟
    private val accLock = Any()
    /** 排进了发送队列、还能接着累加的那批移动 */
    private var openMove: MoveBatch? = null
    private var scrollX = 0
    private var scrollY = 0
    private var scrollQueued = false
    /** 一批移动：位移累加起来，t 是最后一个触摸点的时间 */
    private class MoveBatch {
        var x = 0.0
        var y = 0.0
        var t = 0.0
    }
    private object ScrollToken

    /** 上次发给电脑的电量（-1 = 还不知道），只有变了才再发 */
    @Volatile private var batteryLevel = -1
    @Volatile private var batteryCharging = false
    private val batteryReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) = onBattery(intent)
    }

    private val rtts = ArrayDeque<Long>()
    private var pingId = 0

    private val screenReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                Intent.ACTION_SCREEN_OFF -> {
                    screenOn = false
                    sendAll(JSONObject().put("t", "release"))
                    clearHolds()
                    sendAll(JSONObject().put("t", "state").put("screen", "off"))
                }
                Intent.ACTION_SCREEN_ON -> {
                    screenOn = true
                    sendAll(JSONObject().put("t", "state").put("screen", "on"))
                }
            }
        }
    }

    /** 蓝牙刚打开：马上去连，不用等重试 */
    private val bluetoothReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1) == BluetoothAdapter.STATE_ON) retryNow()
        }
    }

    /**
     * 亮屏时每秒一次心跳，顺便测往返延迟（两条连接都发：电脑那边每条连接各有看门狗）。息屏时不发，电脑也不会因此断开。
     * 局域网发了心跳 4.5 秒都没收到任何回音就当它断了、关掉，消息改走蓝牙：电脑换了网络、拔了网线时，
     * 手机这边的 socket 不会报错，要等系统重传十几分钟才放弃
     */
    private val heartbeat = object : Runnable {
        override fun run() {
            if (screenOn) {
                lanConn?.let { c ->
                    val since = c.awaitSince
                    if (since != 0L && SystemClock.elapsedRealtime() - since > LAN_DEAD_MS) {
                        Log.w(TAG, "lan: ${LAN_DEAD_MS / 1000.0} 秒没回音，断开")
                        c.close()
                    } else {
                        sendPing(c)
                    }
                }
                btConn?.let { sendPing(it) }
            }
            main.postDelayed(this, 1000)
        }
    }

    /** 请求暂停 / 恢复后电脑一直没改：多半是旧版电脑端 */
    private val enableTimeout = Runnable {
        val want = pendingEnabled ?: return@Runnable
        pendingEnabled = null
        val l = listener
        if (l != null) {
            l.onEnableResult(false, want)
        } else {
            showNote(if (want) "电脑端版本较旧，不支持在手机上恢复按键" else "电脑端版本较旧，不支持在手机上暂停")
        }
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onCreate() {
        super.onCreate()
        getSystemService(NotificationManager::class.java)
            .createNotificationChannel(NotificationChannel(CHANNEL, "连接状态", NotificationManager.IMPORTANCE_LOW))
        registerReceiver(screenReceiver, IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_OFF)
            addAction(Intent.ACTION_SCREEN_ON)
        })
        registerReceiver(bluetoothReceiver, IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED))
        // 电量变化是粘性广播：注册时就拿到现在的电量
        registerReceiver(batteryReceiver, IntentFilter(Intent.ACTION_BATTERY_CHANGED))?.let { onBattery(it) }
        prefs.getString(KEY_CONFIG, null)?.let { cached ->
            runCatching { JSONObject(cached) }.getOrNull()?.let {
                lastConfig = it
                paused = it.optBoolean("paused", false)
            }
        }
        runCatching {
            getSystemService(ConnectivityManager::class.java)?.registerNetworkCallback(
                NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).build(), wifiCallback,
            )
        }
        thread(name = "link-writer", isDaemon = true) { writeLoop() }
        main.post(heartbeat)
    }

    /** 电量广播很频繁（电压、温度变了也发）：只在百分比或充电状态变了时发给电脑 */
    private fun onBattery(intent: Intent) {
        val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
        val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
        if (level < 0 || scale <= 0) return
        val percent = (level * 100f / scale).roundToInt()
        val state = intent.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
        val charging = state == BatteryManager.BATTERY_STATUS_CHARGING || state == BatteryManager.BATTERY_STATUS_FULL
        if (percent == batteryLevel && charging == batteryCharging) return
        batteryLevel = percent
        batteryCharging = charging
        sendBattery()
    }

    private fun sendBattery() {
        if (batteryLevel >= 0) send(JSONObject().put("t", "battery").put("level", batteryLevel).put("charging", batteryCharging))
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        goForeground()
        if (intent?.action == ACTION_DISCONNECT) {
            stopLink()
            return START_NOT_STICKY
        }
        stoppedByUser = false
        val requested = intent?.getStringExtra(EXTRA_ADDRESS) ?: prefs.getString(KEY_ADDRESS, null)
        val changed = requested != address
        address = requested
        if (!running) {
            startLoop()
        } else if (changed) {
            btConn?.close() // 换了电脑：断开旧的蓝牙连接，马上连新的
            retryNow()
        }
        when (intent?.action) {
            ACTION_PAUSE -> if (!requestEnabled(false)) showNote("未连接电脑，没法暂停按键")
            ACTION_RESUME -> if (!requestEnabled(true)) showNote("未连接电脑，没法恢复按键")
        }
        return START_STICKY
    }

    private fun goForeground() {
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFY_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIFY_ID, notification)
        }
        foreground = true
    }

    /** 「断开」：断掉连接、不再重连、去掉通知。界面开着的话服务还绑着，显示「已断开」 */
    fun stopLink() {
        stoppedByUser = true
        running = false
        loopGen++
        kicks.offer(Unit)
        lanKicks.offer(Unit)
        btConn?.close()
        lanConn?.close()
        holdWifi(false)
        setPhase(Phase.OFF, null)
        foreground = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** 马上再连一次（「重试」按钮、蓝牙刚打开） */
    fun retryNow() {
        backoff = 1000L
        quiet = false
        kicks.offer(Unit)
        lanBackoff = 1000L
        lanQuiet = false
        lanKicks.offer(Unit)
    }

    override fun onDestroy() {
        running = false
        loopGen++
        kicks.offer(Unit)
        main.removeCallbacks(heartbeat)
        main.removeCallbacks(enableTimeout)
        runCatching { unregisterReceiver(screenReceiver) }
        runCatching { unregisterReceiver(batteryReceiver) }
        runCatching { unregisterReceiver(bluetoothReceiver) }
        runCatching { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(wifiCallback) }
        lanKicks.offer(Unit)
        btConn?.close()
        lanConn?.close()
        holdWifi(false)
        super.onDestroy()
    }

    // ───────── 发送 ─────────

    /** 发一条消息：直接写进 socket，写进去了才返回 true。没连上、写失败都返回 false（写失败会断开重连） */
    fun send(msg: JSONObject): Boolean {
        val c = route(msg) ?: return false
        return write(c, msg.toString())
    }

    /** 两条连接都发（亮屏、息屏这种每个会话都要知道的） */
    private fun sendAll(msg: JSONObject) {
        val line = msg.toString()
        lanConn?.let { write(it, line) }
        btConn?.let { write(it, line) }
    }

    /** 每条连接一把写锁：界面线程的消息和写线程的移动、滚动不会交错；蓝牙写得慢时不耽误局域网 */
    private fun write(c: Conn, line: String): Boolean {
        val queue = c.queue ?: return writeNow(c, line)
        if (c.closed) return false
        queue.offer(line)
        return true
    }

    /** 局域网连接的写线程：一条一条写出去，写不出去就关掉连接（读循环会发现并重连） */
    private fun lanWriter(c: Conn) {
        val queue = c.queue ?: return
        while (!c.closed) {
            val item = queue.take()
            if (item === Conn.STOP || c.closed) return
            if (!writeNow(c, item as String)) return
        }
    }

    private fun writeNow(c: Conn, line: String): Boolean = synchronized(c) {
        try {
            c.output.write((line + "\n").toByteArray(Charsets.UTF_8))
            c.output.flush()
            true
        } catch (e: Exception) {
            Log.w(TAG, "write failed: ${e.message}")
            c.close() // 读循环会发现断开并重连
            false
        }
    }

    /**
     * 触控板移动：先累加，写线程空了再合成一条发，发不出去就丢（没连上时触控板本来就不动）。
     * t 是最后一个触摸点的时间，跟着发过去，电脑按它匀速回放
     */
    fun move(dx: Double, dy: Double, t: Double) {
        if (!connected) return
        synchronized(accLock) {
            val batch = openMove ?: MoveBatch().also {
                openMove = it
                outbox.add(it)
            }
            batch.x += dx
            batch.y += dy
            batch.t = t
        }
    }

    /** 鼠标按键：和移动走同一个发送队列，排在之前的移动后面、之后的移动前面（拖拽按下、松开的位置才对） */
    fun sendInOrder(msg: JSONObject) {
        if (!connected) return
        synchronized(accLock) {
            openMove = null // 之后的移动另起一批，排在这条后面
            outbox.add(msg)
        }
    }

    fun scroll(dx: Int, dy: Int) {
        if (!connected) return
        synchronized(accLock) {
            scrollX += dx
            scrollY += dy
            if (!scrollQueued) {
                scrollQueued = true
                outbox.add(ScrollToken)
            }
        }
    }

    /** 套用完电脑推来的配置后回执（配置里有 rev 才回；旧版电脑端没有） */
    fun ackConfig(msg: JSONObject) {
        if (!msg.has("rev")) return
        send(JSONObject().put("t", "configAck").put("rev", msg.opt("rev")))
    }

    /**
     * 在手机上暂停（false）/ 恢复（true）按键：发给电脑，等它推来的配置里 paused 变了才算成，
     * 一直没变就通过 onEnableResult(false) 告诉界面。返回 false = 没连上，没发出去。
     */
    fun requestEnabled(enabled: Boolean): Boolean {
        if (!send(JSONObject().put("t", "setEnabled").put("enabled", enabled))) return false
        pendingEnabled = enabled
        main.removeCallbacks(enableTimeout)
        main.postDelayed(enableTimeout, ENABLE_TIMEOUT_MS)
        updateNotification()
        return true
    }

    /** 连发 count 次心跳测延迟，结果用 rttSummary(count) 看。 */
    fun latencyBurst(count: Int) {
        thread(isDaemon = true) {
            repeat(count) {
                active()?.let { sendPing(it) }
                SystemClock.sleep(20)
            }
        }
    }

    @Synchronized
    fun rttSummary(last: Int): String? {
        if (rtts.isEmpty()) return null
        val values = rtts.toList().takeLast(last).sorted()
        fun at(q: Double) = values[((values.size - 1) * q).roundToInt()]
        return "一般 ${at(0.5)} ms，慢的时候 ${at(0.95)} ms，最慢 ${values.last()} ms（测了 ${values.size} 次）"
    }

    /** 最近 10 次心跳往返延迟的中位数（毫秒）；还没有数据是 null */
    @Synchronized
    fun latestRtt(): Long? {
        if (rtts.isEmpty()) return null
        val values = rtts.toList().takeLast(10).sorted()
        return values[values.size / 2]
    }

    private fun sendPing(c: Conn) {
        if (c.awaitSince == 0L) c.awaitSince = SystemClock.elapsedRealtime()
        write(c, JSONObject().put("t", "ping").put("id", ++pingId).put("ts", SystemClock.elapsedRealtime()).toString())
    }

    @Synchronized
    private fun recordRtt(ms: Long) {
        rtts.addLast(ms)
        while (rtts.size > 300) rtts.removeFirst()
    }

    // ───────── 连接 ─────────

    private fun hasBluetoothPermission() =
        Build.VERSION.SDK_INT < 31 || checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED

    private fun startLoop() {
        val gen = ++loopGen
        running = true
        quiet = false
        lanQuiet = false
        backoff = 1000L
        lanBackoff = 1000L
        kicks.clear()
        lanKicks.clear()
        thread(name = "link-connect", isDaemon = true) { connectLoop(gen) }
        thread(name = "lan-connect", isDaemon = true) { lanLoop(gen) }
    }

    /** 等 ms 毫秒，「重试」能提前叫醒 */
    private fun nap(ms: Long, queue: LinkedBlockingQueue<Unit> = kicks) {
        queue.poll(ms, TimeUnit.MILLISECONDS)
        queue.clear()
    }

    /** 新连上一条：先报 hello、亮屏状态、电量（每条连接都报，电脑那边每条连接是一个会话） */
    private fun greet(c: Conn) {
        write(c, hello().toString())
        write(c, JSONObject().put("t", "state").put("screen", if (screenOn) "on" else "off").toString())
        if (batteryLevel >= 0) write(c, JSONObject().put("t", "battery").put("level", batteryLevel).put("charging", batteryCharging).toString())
    }

    @SuppressLint("MissingPermission")
    private fun connectLoop(gen: Int) {
        fun alive() = running && gen == loopGen
        while (alive()) {
            val target = address
            val adapter = getSystemService(BluetoothManager::class.java)?.adapter
            if (target == null) {
                setBt(Phase.NO_DEVICE)
                nap(1500)
                continue
            }
            if (!hasBluetoothPermission()) {
                setBt(Phase.NO_PERMISSION)
                nap(2000)
                continue
            }
            if (adapter == null || !adapter.isEnabled) {
                setBt(Phase.BT_OFF)
                nap(3000)
                continue
            }
            var s: BluetoothSocket? = null
            var c: Conn? = null
            try {
                val device = adapter.getRemoteDevice(target)
                val name = device.name ?: target
                deviceName = name
                if (!quiet) setBt(Phase.CONNECTING)
                s = device.createRfcommSocketToServiceRecord(SERVICE_UUID)
                s.connect()
                if (!alive()) break
                c = Conn(false, s, s.inputStream, s.outputStream)
                btConn = c
                backoff = 1000L
                quiet = false
                greet(c)
                setBt(Phase.CONNECTED)
                readLoop(c)
                if (c.busy) backoff = BUSY_BACKOFF_MS
            } catch (e: Exception) {
                Log.w(TAG, "connect: ${e.message ?: e.javaClass.simpleName}")
            } finally {
                if (c != null && btConn === c) {
                    btConn = null
                    linkLost()
                }
                s?.let { runCatching { it.close() } }
            }
            if (!alive()) break
            quiet = true
            setBt(Phase.WAITING)
            nap(backoff)
            backoff = minOf(backoff * 2, 10_000L)
        }
    }

    /**
     * 局域网：扫码绑定过电脑就一直试着连（只在连着 Wi-Fi / 有线网时试），先试上次连上的地址。
     * 电脑说不认识这部手机了（电脑上解除了绑定）就停下，等重新扫码。
     */
    private fun lanLoop(gen: Int) {
        fun alive() = running && gen == loopGen
        while (alive()) {
            val binding = LanBinding.load(this)
            if (binding == null || binding.revoked) {
                setLan(if (binding?.revoked == true) LanPhase.REVOKED else LanPhase.NONE, binding?.name)
                nap(5000, lanKicks)
                continue
            }
            if (!onLocalNetwork()) {
                setLan(LanPhase.WAITING, binding.name)
                nap(5000, lanKicks)
                continue
            }
            val token = LanSecure.open(this, binding.sealedToken)
            if (token == null) {
                // 令牌解不开（比如系统清掉了密钥）：只能重新扫码
                LanBinding.updateIfSame(this, binding) { it.copy(revoked = true) }
                continue
            }
            if (!lanQuiet) setLan(LanPhase.CONNECTING, binding.name)
            var c: Conn? = null
            var socket: javax.net.ssl.SSLSocket? = null
            try {
                var lastError: Exception? = null
                for (addr in binding.tryOrder()) {
                    if (!alive()) return
                    try {
                        val s = LanSecure.connect(addr, binding.port, binding.fp, LAN_CONNECT_TIMEOUT_MS, localNetwork())
                        try {
                            LanSecure.auth(s, binding.phoneId, token)
                        } catch (e: Exception) {
                            runCatching { s.close() }
                            throw e
                        }
                        socket = s
                        if (addr != binding.lastAddr) LanBinding.updateIfSame(this, binding) { it.copy(lastAddr = addr) }
                        break
                    } catch (e: LanAuthException) {
                        if (e.reason == "unknown" || e.reason == "badToken") {
                            Log.w(TAG, "lan: 电脑不认这部手机了（${e.reason}）")
                            LanBinding.updateIfSame(this, binding) { it.copy(revoked = true) }
                            main.post { listener?.onPairResult(false, "「${binding.name}」上解除了这部手机的绑定，要用局域网的话重新扫码") }
                        }
                        lastError = e
                        break
                    } catch (e: Exception) {
                        lastError = e
                    }
                }
                val s = socket ?: throw lastError ?: java.io.IOException("no address")
                if (!alive()) return
                // 连的时候重新扫了别的电脑、或者解除了绑定：这条不要了
                val now = LanBinding.load(this)
                if (now == null || now.pc != binding.pc || now.phoneId != binding.phoneId) throw java.io.IOException("binding changed")
                c = Conn(true, s, s.inputStream, s.outputStream)
                val conn = c
                thread(name = "lan-writer", isDaemon = true) { lanWriter(conn) }
                lanConn = c
                lanPcName = binding.name
                lanBackoff = 1000L
                lanQuiet = false
                greet(c)
                setLan(LanPhase.CONNECTED, binding.name)
                holdWifi(true)
                readLoop(c)
                if (c.busy) {
                    Log.w(TAG, "lan: 电脑正连着另一部手机")
                    lanBackoff = BUSY_BACKOFF_MS
                }
            } catch (e: Exception) {
                Log.w(TAG, "lan: ${e.message ?: e.javaClass.simpleName}")
            } finally {
                if (c != null && lanConn === c) {
                    lanConn = null
                    holdWifi(false)
                    linkLost()
                }
                socket?.let { runCatching { it.close() } }
            }
            if (!alive()) break
            lanQuiet = true
            setLan(LanPhase.WAITING, binding.name)
            nap(lanBackoff, lanKicks)
            lanBackoff = minOf(lanBackoff * 2, maxOf(lanBackoff, 10_000L))
        }
    }

    /** 连着 Wi-Fi 或有线网（只用流量时连不到局域网里的电脑，不白试） */
    private fun onLocalNetwork(): Boolean {
        if (getSystemService(ConnectivityManager::class.java) == null) return true
        return localNetwork() != null
    }

    /** Wi-Fi 或有线网（不是 VPN 那层）：局域网的 socket 绑在它上面 */
    private fun localNetwork(): Network? {
        val cm = getSystemService(ConnectivityManager::class.java) ?: return null
        return cm.allNetworks.firstOrNull { n ->
            val caps = cm.getNetworkCapabilities(n) ?: return@firstOrNull false
            !caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN) &&
                (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) || caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET))
        }
    }

    /** 局域网连着时让 Wi-Fi 别省电（省电时延迟会忽然跳到上百毫秒）；断了就放开 */
    private fun holdWifi(on: Boolean) {
        if (Build.VERSION.SDK_INT < 29) return
        synchronized(this) {
            if (on) {
                val lock = wifiLock ?: runCatching {
                    getSystemService(WifiManager::class.java)?.createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "SuanNIAO:lan")
                        ?.apply { setReferenceCounted(false) }
                }.getOrNull()
                wifiLock = lock
                runCatching { lock?.acquire() }
            } else {
                runCatching { wifiLock?.takeIf { it.isHeld }?.release() }
            }
        }
    }

    /** 扫到了电脑上的二维码：去配对，成了就记下这台电脑，局域网马上用它连。结果通过 onPairResult 告诉界面 */
    fun pairWith(qr: LanQr) {
        if (!running) startLoop()
        thread(name = "lan-pair", isDaemon = true) {
            var lastError: Exception? = null
            // 同一台电脑重新扫码：带上原来的编号和令牌，电脑把旧记录换掉，不在手机列表里多出一条
            val prev = LanBinding.load(this)?.takeIf { it.pc == qr.pc }?.let { old ->
                LanSecure.open(this, old.sealedToken)?.let { old.phoneId to it }
            }
            for (addr in qr.addrs) {
                try {
                    val s = LanSecure.connect(addr, qr.port, qr.fp, PAIR_CONNECT_TIMEOUT_MS, localNetwork())
                    val paired = try {
                        LanSecure.pair(s, qr.code, phoneName(), modelName(), prev)
                    } finally {
                        runCatching { s.close() }
                    }
                    val sealed = LanSecure.seal(this, paired.token)
                    LanBinding.save(this, LanBinding(qr.pc, qr.name, qr.addrs, qr.port, qr.fp, paired.phoneId, sealed, lastAddr = addr))
                    lanConn?.close() // 原来连着另一台电脑：换成这台
                    retryNow()
                    main.post { listener?.onPairResult(true, "已绑定「${qr.name}」，以后在同一个网络里会自动连上") }
                    return@thread
                } catch (e: LanAuthException) {
                    lastError = e
                    if (e.reason == "badCode") break
                } catch (e: Exception) {
                    Log.w(TAG, "pair $addr: ${e.message ?: e.javaClass.simpleName}")
                    lastError = e
                }
            }
            val message = when {
                (lastError as? LanAuthException)?.reason == "badCode" -> "二维码过期了或者已经用过：在电脑上重新点「显示二维码」再扫"
                !onLocalNetwork() -> "手机没连 Wi-Fi：连上和电脑同一个网络再扫"
                else -> "连不上这台电脑：手机和电脑要连着同一个网络；也看看电脑的防火墙有没有拦住蒜鸟"
            }
            main.post { listener?.onPairResult(false, message) }
        }
    }

    /** 解除局域网绑定（只在手机上忘掉这台电脑；电脑上的记录在电脑的设置里删） */
    fun forgetLan() {
        LanBinding.save(this, null)
        lanConn?.close()
        lanKicks.offer(Unit)
    }

    private fun linkLost() {
        main.post { onDisconnected() }
    }

    private fun onDisconnected() {
        // 暂停 / 恢复的请求随连接一起没了（另一条还连着就不算）
        if (pendingEnabled != null && !connected) {
            pendingEnabled = null
            main.removeCallbacks(enableTimeout)
        }
    }

    /** 手机的名字：蓝牙名（比如「少爷的mi13」），拿不到用设备名，再不行用型号 */
    @SuppressLint("MissingPermission")
    private fun phoneName(): String {
        if (hasBluetoothPermission()) {
            runCatching { getSystemService(BluetoothManager::class.java)?.adapter?.name }.getOrNull()
                ?.takeIf { it.isNotBlank() }?.let { return it }
        }
        runCatching { Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME) }.getOrNull()
            ?.takeIf { it.isNotBlank() }?.let { return it }
        return Build.MODEL
    }

    /** 机型名：小米的 Build.MODEL 是「2211133C」这种代号，系统属性里有「Xiaomi 13」这样的正式名字 */
    private fun modelName(): String =
        runCatching {
            Class.forName("android.os.SystemProperties").getMethod("get", String::class.java)
                .invoke(null, "ro.product.marketname") as String
        }.getOrNull()?.takeIf { it.isNotBlank() } ?: Build.MODEL

    /** lan：扫码绑定时电脑给的手机编号，电脑靠它认出蓝牙、局域网连上来的是不是同一部手机 */
    private fun hello() = JSONObject()
        .put("t", "hello")
        .put("device", phoneName())
        .put("model", modelName())
        .put("app", BuildConfig.VERSION_NAME)
        .put("lan", LanBinding.load(this)?.takeUnless { it.revoked }?.phoneId)

    private fun readLoop(c: Conn) {
        val reader = BufferedReader(InputStreamReader(c.input, Charsets.UTF_8))
        while (true) {
            val line = reader.readLine() ?: return
            c.awaitSince = 0L
            val msg = runCatching { JSONObject(line) }.getOrNull() ?: continue
            when (msg.optString("t")) {
                "pong" -> {
                    // 两条连接都发心跳（电脑从收到的那条回）；延迟只记现在发消息走的那条
                    if (c === active()) recordRtt(SystemClock.elapsedRealtime() - msg.optLong("ts"))
                    continue
                }
                "busy" -> {
                    c.busy = true
                    continue
                }
                "hello" -> c.pcId = msg.optString("pc").ifBlank { null }
            }
            // 蓝牙连的是另一台电脑：它的配置、提示不要（布局会来回跳）
            if (foreign(c)) continue
            main.post { onMessage(msg) }
        }
    }

    /** 主线程：先记下服务自己要的（配置、电脑名），再交给界面；界面没开着时配置回执由这里回 */
    private fun onMessage(msg: JSONObject) {
        when (msg.optString("t")) {
            "hello" -> {
                pcHost = msg.optString("host").ifBlank { null }
                pcApp = msg.optString("app").ifBlank { null }
                updateNotification()
            }
            "config" -> {
                lastConfig = msg
                prefs.edit().putString(KEY_CONFIG, msg.toString()).apply()
                val nowPaused = msg.optBoolean("paused", false)
                val changed = nowPaused != paused
                paused = nowPaused
                pendingEnabled?.let { want ->
                    if (nowPaused == !want) {
                        pendingEnabled = null
                        main.removeCallbacks(enableTimeout)
                        listener?.onEnableResult(true, want)
                    }
                }
                if (changed) updateNotification()
                if (listener == null) ackConfig(msg)
            }
            "lan" -> updateLanAddress(msg)
            "clip" -> if (ClipSync.enabled(lastConfig)) ClipSync.receive(this, msg.optString("s"))
            "clipImg" -> if (ClipSync.enabled(lastConfig)) ClipSync.imageStart(this, msg)
            "clipImgPart" -> ClipSync.imagePart(msg)
            "clipImgCancel" -> ClipSync.imageCancel(msg)
            // 界面看 ok 提示一声
            "clipImgEnd" -> ClipSync.imageEnd(this, msg)?.let { msg.put("ok", it) }
        }
        listener?.onLinkMessage(msg)
    }

    /** 电脑说它现在的局域网地址（地址变了、刚连上时发）：是绑定的那台、证书也一样，就记下新地址 */
    private fun updateLanAddress(msg: JSONObject) {
        val list = msg.optJSONArray("addrs") ?: return
        val addrs = (0 until list.length()).map { list.optString(it) }.filter { it.isNotBlank() }
        val port = msg.optInt("port", 0)
        if (addrs.isEmpty() || port !in 1..65535) return
        var changed = false
        LanBinding.update(this) { cur ->
            if (cur == null || msg.optString("pc") != cur.pc || msg.optString("fp") != cur.fp) return@update cur
            if (addrs == cur.addrs && port == cur.port) return@update cur
            changed = true
            cur.copy(addrs = addrs, port = port)
        }
        if (changed && lanConn == null) lanKicks.offer(Unit)
    }

    private fun writeLoop() {
        while (true) {
            val item = outbox.take()
            val line = when (item) {
                is MoveBatch -> synchronized(accLock) {
                    if (openMove === item) openMove = null
                    JSONObject().put("t", "move").put("x", round2(item.x)).put("y", round2(item.y))
                        .put("ts", Math.round(item.t * 100) / 100.0).toString()
                }
                ScrollToken -> synchronized(accLock) {
                    scrollQueued = false
                    val msg = JSONObject().put("t", "scroll").put("x", scrollX).put("y", scrollY)
                    scrollX = 0
                    scrollY = 0
                    msg.toString()
                }
                else -> item.toString()
            }
            val c = route(item as? JSONObject) ?: continue
            write(c, line)
        }
    }

    // ───────── 状态和通知 ─────────

    private fun setBt(phase: Phase) {
        btPhase = phase
        recompute()
    }

    private fun setLan(phase: LanPhase, name: String?) {
        lanPhase = phase
        if (name != null) lanPcName = name
        recompute()
    }

    /**
     * 两条连接合成一个状态给界面和通知：有一条连着就是已连接（名字用局域网那台）；
     * 都没连着时，蓝牙用不了（没选电脑、没权限、蓝牙关着）而局域网绑定过，就报局域网的情况
     */
    @Synchronized
    private fun recompute() {
        val lanOn = lanConn != null
        val btOn = btConn != null
        connected = lanOn || btOn
        if (!running) return
        val lanUsable = lanPhase == LanPhase.CONNECTING || lanPhase == LanPhase.WAITING || lanPhase == LanPhase.CONNECTED
        val btUnusable = btPhase == Phase.NO_DEVICE || btPhase == Phase.NO_PERMISSION || btPhase == Phase.BT_OFF || btPhase == Phase.OFF
        val (phase, pc) = when {
            lanOn -> Phase.CONNECTED to lanPcName
            btOn -> Phase.CONNECTED to deviceName
            !lanUsable -> btPhase to deviceName
            btUnusable -> (if (lanPhase == LanPhase.CONNECTING) Phase.CONNECTING else Phase.WAITING) to lanPcName
            else -> (if (btPhase == Phase.CONNECTING || lanPhase == LanPhase.CONNECTING) Phase.CONNECTING else Phase.WAITING) to (deviceName ?: lanPcName)
        }
        setPhase(phase, pc, lanOn, btOn)
    }

    private fun setPhase(phase: Phase, pc: String?, lan: Boolean = false, bt: Boolean = false) {
        val old = status
        if (old.phase == phase && old.pcName == pc && old.lan == lan && old.bt == bt) return
        val s = Status(phase, pc, lan, bt)
        status = s
        updateNotification()
        main.post { listener?.onLinkStatus(s) }
    }

    /** 电脑的名字：电脑 hello 里说的，没有就用局域网绑定时的名字、蓝牙名 */
    fun pcLabel(): String? = pcHost ?: (if (lanConn != null) lanPcName else null) ?: deviceName ?: lanPcName

    private fun notificationText(): String = when (status.phase) {
        Phase.CONNECTED -> "已连接 · ${pcLabel() ?: "电脑"}" + (status.via?.let { " · $it" } ?: "") + if (paused) " · 按键已暂停" else ""
        Phase.CONNECTING -> "正在连接…"
        Phase.WAITING -> "未连接电脑 · 正在重试"
        Phase.NO_DEVICE -> "还没选电脑"
        Phase.NO_PERMISSION -> "需要「附近的设备」权限"
        Phase.BT_OFF -> "手机蓝牙没开"
        Phase.OFF -> "已断开"
    }

    private fun updateNotification() {
        if (!foreground) return
        getSystemService(NotificationManager::class.java).notify(NOTIFY_ID, buildNotification())
    }

    /** 通知上临时显示一句话（界面没开着时用），4 秒后恢复 */
    private fun showNote(text: String) {
        noteOverride = text
        updateNotification()
        main.postDelayed({
            if (noteOverride == text) {
                noteOverride = null
                updateNotification()
            }
        }, 4000)
    }

    private fun actionIntent(action: String): PendingIntent {
        val intent = Intent(this, LinkService::class.java).setAction(action)
        return PendingIntent.getForegroundService(this, action.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun buildNotification(): Notification {
        val open = PendingIntent.getActivity(this, 0, launchIntent(this), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val b = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_pad)
            .setContentTitle("蒜鸟")
            .setContentText(noteOverride ?: notificationText())
            .setContentIntent(open)
            .setOngoing(true)
            .setShowWhen(false)
            .setOnlyAlertOnce(true)
        if (status.connected) {
            val resume = paused
            b.addAction(Notification.Action.Builder(null as Icon?, if (resume) "恢复按键" else "暂停按键", actionIntent(if (resume) ACTION_RESUME else ACTION_PAUSE)).build())
        }
        b.addAction(Notification.Action.Builder(null as Icon?, "断开", actionIntent(ACTION_DISCONNECT)).build())
        return b.build()
    }

    private fun round2(v: Double) = (v * 100).roundToInt() / 100.0

    companion object {
        // 日志标签、偏好文件名是改名前起的，不改（改了偏好文件名，选过的电脑、菜单里的设置都会丢）
        const val TAG = "SuanNIAO"
        const val PREFS = "suanniao"
        const val KEY_ADDRESS = "pc_address"
        /** 电脑推来的最新配置（服务和界面共用） */
        const val KEY_CONFIG = "layout_config"
        const val EXTRA_ADDRESS = "address"
        const val ACTION_PAUSE = "dev.suanniao.PAUSE"
        const val ACTION_RESUME = "dev.suanniao.RESUME"
        const val ACTION_DISCONNECT = "dev.suanniao.DISCONNECT"
        private const val CHANNEL = "link"
        private const val NOTIFY_ID = 1
        /** 请求暂停 / 恢复后最多等这么久电脑推来新配置 */
        private const val ENABLE_TIMEOUT_MS = 4000L
        /** 局域网每个地址连接 + 握手最多等多久；扫码配对时多给一点 */
        private const val LAN_CONNECT_TIMEOUT_MS = 2500
        private const val PAIR_CONNECT_TIMEOUT_MS = 4000
        /** 局域网发了心跳这么久没收到任何回音就当断了（亮屏时每秒一次心跳，正常几毫秒就回） */
        private const val LAN_DEAD_MS = 4500L
        /** 电脑说正连着另一部手机：隔久一点再试，别和那部来回抢 */
        private const val BUSY_BACKOFF_MS = 30_000L
        /** 和电脑端 link.rs 里的 SERVICE_UUID 一致。 */
        val SERVICE_UUID: UUID = UUID.fromString("5f6a1c2e-8b3d-4e7f-9a10-2b3c4d5e6f70")

        /** 和桌面图标一样的 Intent：主界面是 singleTask，从通知点回来不会叠出第二个界面 */
        fun launchIntent(context: Context): Intent = Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER)
            .setClass(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
    }
}
