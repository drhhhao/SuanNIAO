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
import java.io.InputStreamReader
import java.util.UUID
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.math.roundToInt

/**
 * 保持和电脑的蓝牙串口连接。前台服务：息屏、切到后台都不断开，断了自动重连。
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

    class Status(val phase: Phase, val pcName: String?) {
        val connected get() = phase == Phase.CONNECTED
    }

    interface Listener {
        fun onLinkStatus(status: Status)
        fun onLinkMessage(msg: JSONObject)
        /** 在手机上暂停 / 恢复按键的结果：ok = false 表示等了一阵电脑也没改（多半是旧版电脑端） */
        fun onEnableResult(ok: Boolean, enabled: Boolean)
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
    /** 写 socket 的锁：界面线程的消息和写线程的移动、滚动不会交错 */
    private val writeLock = Any()
    private val outbox = LinkedBlockingQueue<Any>()
    @Volatile private var socket: BluetoothSocket? = null
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
                    send(JSONObject().put("t", "release"))
                    send(JSONObject().put("t", "state").put("screen", "off"))
                }
                Intent.ACTION_SCREEN_ON -> {
                    screenOn = true
                    send(JSONObject().put("t", "state").put("screen", "on"))
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

    /** 亮屏时每秒一次心跳，顺便测往返延迟。息屏时不发，电脑也不会因此断开。 */
    private val heartbeat = object : Runnable {
        override fun run() {
            if (connected && screenOn) sendPing()
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
            socket?.let { runCatching { it.close() } } // 换了电脑：断开旧连接，马上连新的
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
        socket?.let { runCatching { it.close() } }
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
        socket?.let { runCatching { it.close() } }
        super.onDestroy()
    }

    // ───────── 发送 ─────────

    /** 发一条消息：直接写进 socket，写进去了才返回 true。没连上、写失败都返回 false（写失败会断开重连） */
    fun send(msg: JSONObject): Boolean {
        val s = socket
        if (!connected || s == null) return false
        return write(s, msg.toString())
    }

    private fun write(s: BluetoothSocket, line: String): Boolean = synchronized(writeLock) {
        try {
            s.outputStream.write((line + "\n").toByteArray(Charsets.UTF_8))
            s.outputStream.flush()
            true
        } catch (e: Exception) {
            Log.w(TAG, "write failed: ${e.message}")
            runCatching { s.close() } // 读循环会发现断开并重连
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
            outbox.add(msg.toString())
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
                sendPing()
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

    private fun sendPing() {
        send(JSONObject().put("t", "ping").put("id", ++pingId).put("ts", SystemClock.elapsedRealtime()))
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
        backoff = 1000L
        kicks.clear()
        thread(name = "link-connect", isDaemon = true) { connectLoop(gen) }
    }

    /** 等 ms 毫秒，「重试」能提前叫醒 */
    private fun nap(ms: Long) {
        kicks.poll(ms, TimeUnit.MILLISECONDS)
        kicks.clear()
    }

    @SuppressLint("MissingPermission")
    private fun connectLoop(gen: Int) {
        fun alive() = running && gen == loopGen
        while (alive()) {
            val target = address
            val adapter = getSystemService(BluetoothManager::class.java)?.adapter
            if (target == null) {
                setPhase(Phase.NO_DEVICE, null)
                nap(1500)
                continue
            }
            if (!hasBluetoothPermission()) {
                setPhase(Phase.NO_PERMISSION, null)
                nap(2000)
                continue
            }
            if (adapter == null || !adapter.isEnabled) {
                setPhase(Phase.BT_OFF, null)
                nap(3000)
                continue
            }
            var s: BluetoothSocket? = null
            try {
                val device = adapter.getRemoteDevice(target)
                val name = device.name ?: target
                deviceName = name
                if (!quiet) setPhase(Phase.CONNECTING, name)
                s = device.createRfcommSocketToServiceRecord(SERVICE_UUID)
                s.connect()
                if (!alive()) break
                socket = s
                connected = true
                backoff = 1000L
                quiet = false
                send(hello())
                send(JSONObject().put("t", "state").put("screen", if (screenOn) "on" else "off"))
                sendBattery()
                setPhase(Phase.CONNECTED, name)
                readLoop(s)
            } catch (e: Exception) {
                Log.w(TAG, "connect: ${e.message ?: e.javaClass.simpleName}")
            } finally {
                if (socket === s && s != null) {
                    connected = false
                    socket = null
                    main.post { onDisconnected() }
                }
                s?.let { runCatching { it.close() } }
            }
            if (!alive()) break
            quiet = true
            setPhase(Phase.WAITING, deviceName)
            nap(backoff)
            backoff = minOf(backoff * 2, 10_000L)
        }
    }

    private fun onDisconnected() {
        // 暂停 / 恢复的请求随连接一起没了
        if (pendingEnabled != null) {
            pendingEnabled = null
            main.removeCallbacks(enableTimeout)
        }
    }

    /** 手机的名字：蓝牙名（比如「少爷的mi13」），拿不到用设备名，再不行用型号 */
    @SuppressLint("MissingPermission")
    private fun phoneName(): String {
        runCatching { getSystemService(BluetoothManager::class.java)?.adapter?.name }.getOrNull()
            ?.takeIf { it.isNotBlank() }?.let { return it }
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

    private fun hello() = JSONObject()
        .put("t", "hello")
        .put("device", phoneName())
        .put("model", modelName())
        .put("app", BuildConfig.VERSION_NAME)

    private fun readLoop(s: BluetoothSocket) {
        val reader = BufferedReader(InputStreamReader(s.inputStream, Charsets.UTF_8))
        while (true) {
            val line = reader.readLine() ?: return
            val msg = runCatching { JSONObject(line) }.getOrNull() ?: continue
            if (msg.optString("t") == "pong") recordRtt(SystemClock.elapsedRealtime() - msg.optLong("ts"))
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
            "clip" -> if (ClipSync.enabled(lastConfig)) ClipSync.receive(this, msg.optString("s"))
            "clipImg" -> if (ClipSync.enabled(lastConfig)) ClipSync.imageStart(this, msg)
            "clipImgPart" -> ClipSync.imagePart(msg)
            "clipImgCancel" -> ClipSync.imageCancel(msg)
            // 界面看 ok 提示一声
            "clipImgEnd" -> ClipSync.imageEnd(this, msg)?.let { msg.put("ok", it) }
        }
        listener?.onLinkMessage(msg)
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
            val s = socket ?: continue
            if (connected) write(s, line)
        }
    }

    // ───────── 状态和通知 ─────────

    private fun setPhase(phase: Phase, pc: String?) {
        val old = status
        if (old.phase == phase && old.pcName == pc) return
        val s = Status(phase, pc)
        status = s
        updateNotification()
        main.post { listener?.onLinkStatus(s) }
    }

    /** 电脑的名字：电脑 hello 里说的，没有就用蓝牙名 */
    fun pcLabel(): String? = pcHost ?: deviceName

    private fun notificationText(): String = when (status.phase) {
        Phase.CONNECTED -> "已连接 · ${pcLabel() ?: "电脑"}" + if (paused) " · 按键已暂停" else ""
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
        /** 和电脑端 link.rs 里的 SERVICE_UUID 一致。 */
        val SERVICE_UUID: UUID = UUID.fromString("5f6a1c2e-8b3d-4e7f-9a10-2b3c4d5e6f70")

        /** 和桌面图标一样的 Intent：主界面是 singleTask，从通知点回来不会叠出第二个界面 */
        fun launchIntent(context: Context): Intent = Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER)
            .setClass(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
    }
}
