package dev.suanniao

import android.Manifest
import android.animation.ValueAnimator
import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.drawable.LayerDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.speech.RecognitionService
import android.text.TextUtils
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import dev.suanniao.LinkService.Phase
import dev.suanniao.Overlays.Btn
import dev.suanniao.Overlays.Style
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlin.math.sin

/**
 * 手机端界面：按电脑下发的布局（网格 + 模块）显示触控板、按键和语音。
 * 按键映射在电脑端蒜鸟里改；这里负责显示、判定按法，把发生的事发给电脑。
 * 布局也可以在手机上直接调（⋯ →「调整布局」，见 LayoutEditor），点「完成」发给电脑，由电脑校验后保存。
 *
 * 可靠优先：
 * - 没连上电脑时一眼看得出（顶上横幅、按键和触控板变淡），按了不假装成功；说的话存在手机上，连上后问要不要发
 * - 主界面是 singleTask，服务里记着最新配置，重新打开时马上套用，不会叠出旧界面
 * - 触控板、调整布局时排除系统返回手势；返回键只把 App 退到后台，不关掉
 * 菜单、对话框、提示都是自绘的（Overlays），不用系统样式。
 */
class MainActivity : Activity(), LinkService.Listener, TouchpadView.Sink, Setup.Host, Updater.Host {

    private var link: LinkService? = null
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            val service = (binder as LinkService.LocalBinder).service
            service.listener = this@MainActivity
            link = service
            // 界面可能是刚重新打开的：直接用服务里最新的配置和状态，不显示过时的样子
            service.lastConfig?.let { latest -> if (latest.toString() != configText || editor != null) onConfig(latest) }
            applyStatus(service.status)
            checkPcVersion(service.pcApp)
            service.wake() // 刚回到前台：后台时可能被系统冻结、局域网被掐断了
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            link = null
        }
    }

    override val context: Context get() = this
    private val prefs by lazy { getSharedPreferences(LinkService.PREFS, MODE_PRIVATE) }
    private val main = Handler(Looper.getMainLooper())
    private val palette by lazy { Ui.palette(this) }

    private lateinit var root: FrameLayout
    private lateinit var content: LinearLayout
    private lateinit var overlays: Overlays
    private lateinit var setup: Setup
    /** 检查更新（Updater.kt） */
    private lateinit var updater: Updater

    /** 顶栏右边那行：平时是连接状态，按了键、有提示时临时换上去 */
    private lateinit var readout: Ui.Readout
    /** 锁定录音时顶栏常驻的「录音中 · 点此结束」 */
    private lateinit var recPill: TextView
    private lateinit var brand: ImageView
    /** 方案多于一个时品牌名旁边常驻的方案名 */
    private lateinit var profileTag: TextView
    private lateinit var menuButton: ImageView
    /** 调整布局时顶栏换成：取消、提示、对齐 / 自由、完成 */
    private lateinit var cancelButton: TextView
    private lateinit var snapSwitch: SnapSwitch
    private lateinit var doneButton: TextView
    private lateinit var tabs: LinearLayout
    private lateinit var tabScroller: HorizontalScrollView
    private lateinit var grid: GridLayoutView
    /** 顶栏下面的横幅：没连上电脑、按键已暂停等 */
    private lateinit var bannerBox: FrameLayout
    private lateinit var bannerDot: StatusDot
    private lateinit var bannerTitle: TextView
    private lateinit var bannerSub: TextView
    private lateinit var bannerAction: TextView

    private var status = LinkService.Status(Phase.OFF, null)
    /** 还没从服务拿到过状态（刚打开）：先不把按键变淡，免得每次打开都闪一下 */
    private var statusKnown = false
    private val connected get() = status.connected
    /** 从什么时候开始没连上：超过一会儿才出横幅，刚打开 App 正在连的那一两秒不吓人 */
    private var offlineSince = SystemClock.uptimeMillis()
    private val bannerRecheck = Runnable {
        updateBanner()
        showBase(animate = true)
    }
    private var bannerKey: String? = null
    private var bannerAnim: ValueAnimator? = null
    private var resumed = false

    /** 电脑上暂停了按键映射（按键变淡、按了不发，横幅写着） */
    private var mappingPaused = false
    private var profileId: String? = null
    private var lastSpotName = ""
    private var lastSpotAt = 0L
    /** 这次打开 App 已经显示过的页面：再切回来不重播入场动画 */
    private val seenPages = mutableSetOf<String>()
    private var shownPageId = ""
    private var shownSignature = ""
    private var tabsSignature = ""

    /** 正在调整布局（null = 没在调） */
    private var editor: LayoutEditor? = null
    private var editPageId = ""
    private var editChanged = false
    /** 已经发给电脑、等它回话 */
    private var saving = false
    /** 调整期间电脑推来的配置：先存着，调完再用（那时再回执） */
    private var pendingConfig: JSONObject? = null
    /** 没连上时调好的布局：存在手机上，连上后自动发给电脑；在那之前盖在电脑的配置上显示 */
    private var pendingLayout: JSONObject? = null
    private var layoutInFlight = false
    private val saveTimeout = Runnable {
        if (saving) {
            saving = false
            updateEditBar()
            say("电脑没有回应，布局没保存（电脑端可能是旧版本）")
        }
    }
    private val pendingLayoutTimeout = Runnable {
        if (layoutInFlight) {
            layoutInFlight = false
            say("电脑没有回应，调好的布局还存在手机上，下次连上再发")
        }
    }

    private var config: JSONObject = JSONObject(DEFAULT_CONFIG)
    private var configText = ""
    private var configPendingText = ""
    private var pageIndex = 0
    /** 触控板和语音换布局时尽量复用（换页时也留着），免得正在输入的字、正在说的话丢掉 */
    private val reusable = mutableMapOf<String, View>()
    /** 顶栏右边正在显示临时消息（按了什么键、提示）时，状态变化不要覆盖它 */
    private var detailPinned = false
    private val clearDetail = Runnable { detailPinned = false; showBase(animate = true) }

    private var menuSheet: Overlays.Sheet? = null
    /** 菜单关掉后留着的那些行，下次打开接着用 */
    private var menuBody: LinearLayout? = null
    private var pendingSheet: Overlays.Sheet? = null
    /** 断线期间新存了话：连上、说完后要问一次 */
    private var pendingFresh = false
    private var discardArmed = false
    private var aboutOpen = false
    private var debugOpen = false
    private var latencyText: String? = null
    private var memText: String? = null
    /** 发文件的进度上次在顶上显示的时刻 */
    private var lastFileFlash = 0L
    private var versionWarned = false
    private var lastOfflineTouch = 0L

    /** 闲置一阵把屏幕调暗（只改这个窗口的亮度，不碰系统设置）；一碰就恢复，那一下不算按键 */
    private var dimmed = false
    private var swallowTouch = false
    private var dimAfterMs = DIM_AFTER_MS
    private val dimTask = Runnable { setDimmed(true) }

    /** 最近一次碰屏幕的时间、现在有没有手指按着：自己弹更新面板前看一眼，别挡住正在用的手 */
    private var lastTouchAt = 0L
    private var touching = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        requestHighestRefreshRate()
        setContentView(buildShell())
        applySystemBars()
        prefs.getString(LinkService.KEY_CONFIG, null)?.let { cached -> runCatching { JSONObject(cached) }.getOrNull()?.let { config = it } }
        pendingLayout = prefs.getString(KEY_PENDING_LAYOUT, null)?.let { runCatching { JSONObject(it) }.getOrNull() }
        pageIndex = prefs.getInt(KEY_PAGE, 0)
        applyConfig(force = true)
        if (savedAddress() == null && !setup.tutorialSeen) main.post { setup.showOnboarding() }
        handleDebugIntent(intent)
        handleLinkIntent(intent)
    }

    override fun onStart() {
        super.onStart()
        bindService(Intent(this, LinkService::class.java), connection, BIND_AUTO_CREATE)
        if (!setup.onboardingOpen) startLinkIfReady()
        applyVoiceSettings() // 回到前台：模型如果在后台被释放了，重新加载
        scheduleDim()
        updater.onForeground() // 开着自动检查、离上次够久了：过几秒看一眼有没有新版本
    }

    override fun onResume() {
        super.onResume()
        resumed = true
        if (setup.onboardingOpen) setup.refreshOnboarding() // 可能刚从蓝牙设置、权限设置回来
        updateBanner()
        link?.wake()
        main.postDelayed({ maybeOfferPending(force = false) }, 400)
        main.postDelayed({ offerBattery(force = false) }, 900)
        updater.onResume() // 可能刚从「安装未知应用」设置、系统安装界面回来
        getSystemService(ClipboardManager::class.java)?.addPrimaryClipChangedListener(clipListener) // 在蒜鸟里复制的也同步
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) offerClipboard()
    }

    private val clipListener = ClipboardManager.OnPrimaryClipChangedListener { offerClipboard() }

    /** 剪贴板同步：蒜鸟拿着焦点时才读得到剪贴板，手机上新复制的文字发给电脑（见 ClipSync） */
    private fun offerClipboard() {
        val l = link ?: return
        if (!hasWindowFocus() || !l.connected || !ClipSync.enabled(l.lastConfig)) return
        val n = ClipSync.offer(this, prefs) { l.send(it) }
        if (n > 0) flash("手机剪贴板已同步到电脑（$n 字）", 2000)
    }

    override fun onPause() {
        resumed = false
        getSystemService(ClipboardManager::class.java)?.removePrimaryClipChangedListener(clipListener)
        updater.onPause()
        super.onPause()
    }

    override fun onDestroy() {
        btPair.cancel()
        reusable.values.forEach { (it as? VoiceButton)?.release() }
        updater.detach()
        super.onDestroy()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleDebugIntent(intent)
        handleLinkIntent(intent)
    }

    /**
     * 调试版才有，测试时不用真出声、不用真点：
     * - debugWav：把手机上的一段 wav 当成对着语音按钮说话（加 debugLatch=true 顺便显示成锁定的样子）
     *   adb shell am start -n dev.suanniao/.MainActivity -f 0x20000000 --es debugWav <wav 路径>
     * - debugOnboarding、debugGuide、debugMenu：打开引导、手势说明、⋯ 菜单
     * - debugTutorial：从第 N 页打开使用教程（从 0 数，不算第一次打开）
     * - debugDimMs：闲置多久调暗屏幕（毫秒）
     * - debugConfigFile：临时套用手机上的一份配置（测语音输入框这类真实配置里没有的模块）
     * - debugUpdateUrl：检查更新改用这个地址的 latest.json（空字符串 = 换回 GitHub），马上查一次。
     *   加 debugUpdateAuto=true 当成打开 App 时的自动检查（管「跳过」「以后再说」、等别的面板关掉、手离开屏幕）；
     *   加 debugUpdateInstall=true 让调试版也下载、安装（测整条路，装的得是同一台电脑打的、versionCode 更大的调试版）。
     *   电脑上起个 http 服务再 adb reverse tcp:8765 tcp:8765，调试版放行 127.0.0.1 的 http：
     *   adb shell am start -n dev.suanniao/.MainActivity -f 0x20000000 --es debugUpdateUrl http://127.0.0.1:8765/latest.json
     */
    private fun handleDebugIntent(intent: Intent?) {
        if (!BuildConfig.DEBUG || intent == null) return
        intent.getStringExtra("debugWav")?.let { wav ->
            reusable.values.filterIsInstance<VoiceButton>().firstOrNull()?.debugDictate(File(wav), intent.getBooleanExtra("debugLatch", false))
        }
        if (intent.getBooleanExtra("debugOnboarding", false)) main.post { setup.showOnboarding() }
        intent.getIntExtra("debugTutorial", -1).takeIf { it >= 0 }?.let { page -> main.post { setup.showTutorial(page) } }
        if (intent.getBooleanExtra("debugGuide", false)) main.post { showGestureGuide() }
        if (intent.getBooleanExtra("debugMenu", false)) main.post { showMenu() }
        if (intent.getBooleanExtra("debugPending", false)) main.post { showPendingSheet() }
        intent.getLongExtra("debugDimMs", 0L).takeIf { it > 0 }?.let {
            dimAfterMs = it
            scheduleDim()
        }
        // 临时在手机上套用一份测试配置（不存、不回执，电脑下次推配置就盖掉）；空字符串 = 换回服务里电脑推来的
        intent.getStringExtra("debugConfigFile")?.let { path ->
            val test = if (path.isEmpty()) link?.lastConfig else runCatching { JSONObject(File(path).readText()) }.getOrNull()
            test?.let {
                config = it
                applyConfig(force = true)
            }
        }
        // 当成扫到了这个文件里的二维码内容（测扫码绑定不用对着屏幕扫）；debugPairFor = bt / lan 只做那一种
        intent.getStringExtra("debugPairFile")?.let { path ->
            scanFor = when (intent.getStringExtra("debugPairFor")) {
                "bt" -> Setup.ScanFor.BT
                "lan" -> Setup.ScanFor.LAN
                else -> Setup.ScanFor.ANY
            }
            runCatching { File(path).readText() }.getOrNull()?.let { text -> main.postDelayed({ onScanned(text) }, 800) }
        }
        // 打开扫码界面，1.5 秒后当成扫到了这段内容（看「认出来了」「不是蒜鸟的码」动画，不真去连；见 ScanActivity）
        if (BuildConfig.DEBUG) intent.getStringExtra("debugScanFake")?.let { fake ->
            startActivityForResult(Intent(this, ScanActivity::class.java).putExtra(ScanActivity.EXTRA_MODE, "bt")
                .putExtra(ScanActivity.EXTRA_LABEL, "蓝牙配对").putExtra(ScanActivity.EXTRA_DEBUG_FAKE, fake), REQ_SCAN)
            @Suppress("DEPRECATION")
            overridePendingTransition(R.anim.scan_enter, 0)
        }
        intent.getStringExtra("debugUpdateUrl")?.let { url ->
            val auto = intent.getBooleanExtra("debugUpdateAuto", false)
            val install = intent.getBooleanExtra("debugUpdateInstall", false)
            main.post { updater.debugOverride(url.ifEmpty { null }, auto, install) }
        }
    }

    override fun onStop() {
        // 切到后台：直通按着的键马上松开、连发停下（先发，下面就断开和服务的绑定了）
        releaseKeys()
        // 后台录不了音：正在说的话当作说完了；本地语音模型过一阵还没回来就从内存放掉
        reusable.values.forEach { (it as? VoiceButton)?.stopListening() }
        LocalAsr.releaseLater()
        link?.listener = null
        unbindService(connection)
        // link 留着不清：服务是前台服务还在跑，松手后还在收尾的那一段字照样能发出去（发不出去会存到手机上）
        main.removeCallbacks(dimTask)
        setDimmed(false)
        updater.onBackground()
        super.onStop()
    }

    /** 小米默认可能让普通 App 跑 60Hz，这里请求屏幕支持的最高刷新率，触控板采样和发送都更密。 */
    private fun requestHighestRefreshRate() {
        @Suppress("DEPRECATION")
        val display = (if (Build.VERSION.SDK_INT >= 30) display else windowManager.defaultDisplay) ?: return
        val current = display.mode
        val best = display.supportedModes
            .filter { it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: return
        window.attributes = window.attributes.apply { preferredDisplayModeId = best.modeId }
    }

    // ───────── 权限与连接 ─────────

    override fun hasBluetoothPermission() =
        Build.VERSION.SDK_INT < 31 || checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) == PackageManager.PERMISSION_GRANTED

    /** 申请「附近的设备」权限（用途已经在引导、横幅上写了）；拒绝过、系统不再弹框时带去设置页 */
    override fun askBluetoothPermission() {
        if (hasBluetoothPermission()) return
        val asked = prefs.getBoolean(KEY_BT_ASKED, false)
        if (asked && !shouldShowRequestPermissionRationale(Manifest.permission.BLUETOOTH_CONNECT)) {
            openAppSettings()
            say("在「权限」里把「附近的设备」改成允许")
            return
        }
        prefs.edit().putBoolean(KEY_BT_ASKED, true).apply()
        requestPermissions(arrayOf(Manifest.permission.BLUETOOTH_CONNECT), REQ_BLUETOOTH)
    }

    override fun openBluetoothSettings() {
        runCatching { startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS)) }
            .onFailure { say("打不开蓝牙设置，请到手机的「设置 → 蓝牙」里操作") }
    }

    private fun openAppSettings(pkg: String = packageName) {
        runCatching { startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", pkg, null))) }
    }

    override fun savedAddress(): String? = prefs.getString(LinkService.KEY_ADDRESS, null)

    override fun savedName(): String? = prefs.getString(KEY_PC_NAME, null)

    override fun chooseComputer(address: String, name: String) {
        prefs.edit().putString(LinkService.KEY_ADDRESS, address).putString(KEY_PC_NAME, name).apply()
        if (setup.onboardingOpen) {
            setup.refreshOnboarding()
        } else {
            startLinkIfReady(address)
            flash("正在连接 $name…", 2500)
        }
    }

    override fun finishOnboarding() {
        setup.closeOnboarding()
        // 通知用来显示连接状态（引导第 3 步写了），可以不给
        if (Build.VERSION.SDK_INT >= 33 && savedAddress() != null &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFY)
        }
        startLinkIfReady()
        updateBanner()
        main.postDelayed({ maybeShowGestureGuide() }, 600)
    }

    /** 选好了电脑、有蓝牙权限就让服务开始连（已经在连就什么都不变） */
    private fun startLinkIfReady(address: String? = null) {
        val target = address ?: savedAddress()
        val bluetooth = target != null && hasBluetoothPermission()
        if (!bluetooth && !lanBound()) return
        startForegroundService(Intent(this, LinkService::class.java).apply { if (bluetooth) putExtra(LinkService.EXTRA_ADDRESS, target) })
    }

    /** 扫码绑定过电脑（而且电脑上没解除绑定） */
    private fun lanBound(): Boolean = LanBinding.load(this)?.revoked == false

    override fun lanName(): String? = LanBinding.load(this)?.takeIf { !it.revoked }?.name

    override fun lanConnected(): Boolean = link?.lanPhase == LinkService.LanPhase.CONNECTED

    override fun btPairing(): Boolean = btPair.busy

    override fun btConnected(): Boolean = link?.btConnected == true

    // ───────── 局域网：扫电脑上的二维码绑定 ─────────

    /** 刚从蒜鸟里点了「扫码连接电脑」的时刻：之后几分钟扫回来的链接不用再问一遍 */
    private var scanStartedAt = 0L

    /** 这次扫码要连哪种（教程里选了蓝牙或局域网时只做那种）；扫完换回 ANY */
    private var scanFor = Setup.ScanFor.ANY

    /**
     * 在蒜鸟里直接扫（zxing 的扫码界面）。第一次先说清楚相机拿来干什么再要权限；不给相机的话，可以从相册选二维码截图。
     * 手机自带的相机、「扫一扫」扫到的 suanniao://pair 链接也能用蒜鸟打开（handleLinkIntent）
     */
    override fun scanComputer(forWhat: Setup.ScanFor) {
        scanFor = forWhat
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            val canAsk = !prefs.getBoolean(KEY_CAMERA_ASKED, false) || shouldShowRequestPermissionRationale(Manifest.permission.CAMERA)
            overlays.dialog(
                "扫码要用相机",
                "只在扫电脑上的二维码时打开相机，不拍照、不存图。\n不想给的话，也可以把二维码截图、从相册里选。",
                listOf(
                    Btn("从相册选") { pickQrImage() },
                    if (canAsk) Btn("允许", Style.PRIMARY) {
                        prefs.edit().putBoolean(KEY_CAMERA_ASKED, true).apply()
                        requestPermissions(arrayOf(Manifest.permission.CAMERA), REQ_CAMERA)
                    } else Btn("去设置里打开", Style.PRIMARY) { openAppSettings() },
                ),
            )
            return
        }
        scanStartedAt = SystemClock.elapsedRealtime()
        // 取景框上面的小标签：这次扫码做什么（ANY 时蓝牙开着才顺便配对，见 onScanned）
        val (mode, label) = when (forWhat) {
            Setup.ScanFor.BT -> "bt" to "蓝牙配对"
            Setup.ScanFor.LAN -> "lan" to "局域网"
            Setup.ScanFor.ANY -> if (bluetoothReady()) "any" to "蓝牙 + 局域网" else "lan" to "局域网"
        }
        startActivityForResult(
            Intent(this, ScanActivity::class.java)
                .putExtra(ScanActivity.EXTRA_MODE, mode)
                .putExtra(ScanActivity.EXTRA_LABEL, label),
            REQ_SCAN,
        )
        @Suppress("DEPRECATION")
        overridePendingTransition(R.anim.scan_enter, 0) // 整屏淡入，遮罩、四个角的进场动画扫码界面自己画
    }

    /** 菜单里「发文件到电脑」：系统的选文件界面，能多选 */
    private fun pickFiles() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE)
            .setType("*/*")
            .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        runCatching { startActivityForResult(intent, REQ_PICK_FILES) }.onFailure { say("打不开选文件的界面") }
    }

    /** 选好了：趁着还有读权限先打开，交给连接服务慢慢发 */
    private fun sendPicked(data: Intent) {
        val uris = buildList {
            data.clipData?.let { clip -> for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let { add(it) } }
            if (isEmpty()) data.data?.let { add(it) }
        }
        val l = link ?: return say("没连上电脑")
        val items = FileSender.open(this, uris)
        if (items.isEmpty()) return say("这些文件打不开，没法发")
        l.files.enqueue(items)
        flash(if (items.size == 1) "正在发到电脑：${items[0].name}" else "正在发到电脑：${items.size} 个文件", 3000)
    }

    private fun pickQrImage() {
        scanStartedAt = SystemClock.elapsedRealtime()
        runCatching { startActivityForResult(Intent(Intent.ACTION_GET_CONTENT).setType("image/*"), REQ_PICK_QR) }
            .onFailure { say("打不开相册") }
    }

    @Deprecated("Activity 的老接口：扫码界面、相册、选文件用它回结果")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        when (requestCode) {
            REQ_SCAN -> if (resultCode == RESULT_OK && data != null) {
                if (data.getBooleanExtra(ScanActivity.EXTRA_ALBUM, false)) pickQrImage()
                else data.getStringExtra(ScanActivity.EXTRA_TEXT)?.let { onScanned(it) }
            }
            REQ_PICK_QR -> data?.data?.let { uri -> decodeQrImage(uri) }
            REQ_PICK_FILES -> if (resultCode == RESULT_OK && data != null) sendPicked(data)
            REQ_ENABLE_BT -> pendingBtQr?.let { qr ->
                if (resultCode == RESULT_OK) pairBluetooth(qr)
                else {
                    pendingBtQr = null
                    say("没打开蓝牙，配对不了")
                }
            }
            else -> {
                @Suppress("DEPRECATION")
                super.onActivityResult(requestCode, resultCode, data)
            }
        }
    }

    /** 从图片里认二维码（后台线程：大图解码要几百毫秒） */
    private fun decodeQrImage(uri: android.net.Uri) {
        flash("正在识别二维码…", 2000)
        Thread {
            val text = runCatching {
                val bounds = android.graphics.BitmapFactory.Options().apply { inJustDecodeBounds = true }
                contentResolver.openInputStream(uri)?.use { android.graphics.BitmapFactory.decodeStream(it, null, bounds) }
                var sample = 1
                while (maxOf(bounds.outWidth, bounds.outHeight) / sample > 2000) sample *= 2
                val bitmap = contentResolver.openInputStream(uri)?.use {
                    android.graphics.BitmapFactory.decodeStream(it, null, android.graphics.BitmapFactory.Options().apply { inSampleSize = sample })
                } ?: return@runCatching null
                val pixels = IntArray(bitmap.width * bitmap.height)
                bitmap.getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
                val source = com.google.zxing.RGBLuminanceSource(bitmap.width, bitmap.height, pixels)
                val hints = mapOf(
                    com.google.zxing.DecodeHintType.POSSIBLE_FORMATS to listOf(com.google.zxing.BarcodeFormat.QR_CODE),
                    com.google.zxing.DecodeHintType.TRY_HARDER to true,
                )
                com.google.zxing.MultiFormatReader().decode(com.google.zxing.BinaryBitmap(com.google.zxing.common.HybridBinarizer(source)), hints).text
            }.getOrNull()
            main.post { if (text == null) say("这张图里没认出二维码：拍清楚一点，或者直接截电脑上的二维码") else onScanned(text) }
        }.start()
    }

    /**
     * 别的 App（扫一扫、相机、浏览器）打开了 suanniao://pair 链接。刚从蒜鸟里点了扫码就直接绑定；
     * 不是的话先问一句，免得网页、别的 App 悄悄把手机绑到陌生的电脑上
     */
    private fun handleLinkIntent(intent: Intent?) {
        val data = intent?.data ?: return
        if (intent.action != Intent.ACTION_VIEW || data.scheme != "suanniao") return
        if (BuildConfig.DEBUG) android.util.Log.i("SuanNIAO-scan", "link: $data")
        setIntent(Intent(intent).setData(null)) // 转屏、重建时别再处理一遍
        val text = data.toString()
        val qr = LanQr.parse(text)
        if (qr == null) {
            say(NOT_OUR_QR)
            return
        }
        val fresh = scanStartedAt != 0L && SystemClock.elapsedRealtime() - scanStartedAt < 5 * 60_000L
        scanStartedAt = 0L
        if (fresh) {
            main.post { onScanned(text) }
            return
        }
        scanFor = Setup.ScanFor.ANY
        val where = if (qr.addrs.isEmpty()) "" else "（${qr.addrs.joinToString("、")}）"
        main.post {
            overlays.dialog(
                "连接这台电脑？",
                "「${qr.name}」$where\n连接以后，蒜鸟会自动用蓝牙或局域网连上它。",
                listOf(Btn("取消"), Btn("连接", Style.PRIMARY) { onScanned(text) }),
            )
        }
    }

    /**
     * 扫到的二维码内容：是蒜鸟电脑上的就去连。二维码里局域网地址、蓝牙地址可能都有：
     * 教程里选了哪种就只做哪种；从菜单扫的（ANY）局域网能绑就绑，蓝牙在不用另外要权限、开蓝牙时顺便配对
     * （电脑没连网、只能蓝牙时照样要）
     */
    private fun onScanned(text: String) {
        if (BuildConfig.DEBUG) android.util.Log.i("SuanNIAO-scan", "scanned: $text")
        val forWhat = scanFor.also { scanFor = Setup.ScanFor.ANY }
        val qr = LanQr.parse(text)
        if (qr == null) {
            say(NOT_OUR_QR)
            return
        }
        val lan = forWhat != Setup.ScanFor.BT && qr.addrs.isNotEmpty()
        val bt = qr.bt != null && forWhat != Setup.ScanFor.LAN && (forWhat == Setup.ScanFor.BT || bluetoothReady() || !lan)
        when {
            forWhat == Setup.ScanFor.BT && qr.bt == null ->
                return say("这台电脑扫不了码配对（电脑上的蒜鸟太旧，或者电脑没有蓝牙）：更新电脑上的蒜鸟，或者去蓝牙设置里配对")
            forWhat == Setup.ScanFor.LAN && !lan ->
                return say("这台电脑现在没连网，用不了局域网：可以改用蓝牙")
        }
        if (lan) {
            startForegroundService(Intent(this, LinkService::class.java))
            val l = link
            if (l == null) {
                say("服务还没准备好，稍等一下再扫")
                return
            }
            if (!bt) flash("正在连接「${qr.name}」…", 4000)
            l.pairWith(qr, quietFail = bt)
        }
        if (bt) pairBluetooth(qr)
    }

    // ───────── 扫码蓝牙配对（BtPair.kt） ─────────

    private val btPair by lazy { BtPair(this) }
    /** 等「附近的设备」权限、等打开蓝牙的那次扫码，回来接着配对 */
    private var pendingBtQr: LanQr? = null

    /** 有蓝牙权限、蓝牙开着：扫码时顺便配对不用再弹别的框 */
    private fun bluetoothReady(): Boolean =
        hasBluetoothPermission() && runCatching { getSystemService(android.bluetooth.BluetoothManager::class.java)?.adapter?.isEnabled == true }.getOrDefault(false)

    /** 对二维码里的电脑蓝牙地址发起配对（系统弹「配对」点一下），配上了就选这台电脑、把口令带给电脑核对 */
    private fun pairBluetooth(qr: LanQr) {
        val address = qr.bt ?: return
        pendingBtQr = null
        if (!hasBluetoothPermission()) {
            pendingBtQr = qr
            askBluetoothPermission()
            return
        }
        val adapter = getSystemService(android.bluetooth.BluetoothManager::class.java)?.adapter ?: return say("这部手机没有蓝牙")
        if (!adapter.isEnabled) {
            pendingBtQr = qr
            runCatching { startActivityForResult(Intent(android.bluetooth.BluetoothAdapter.ACTION_REQUEST_ENABLE), REQ_ENABLE_BT) }
                .onFailure {
                    pendingBtQr = null
                    say("先打开手机蓝牙，再扫一次")
                }
            return
        }
        flash("正在用蓝牙配对「${qr.name}」：在弹出的窗口里点「配对」", 8000)
        btPair.start(address) { ok, message ->
            setup.refreshOnboarding() // 教程里扫码配对那页：「正在配对…」换掉
            if (!ok) {
                say(message ?: "没配对上，再扫一次试试")
                return@start
            }
            link?.setBtPairCode(address, qr.code) ?: LinkService.saveBtPairCode(this, address, qr.code)
            Ui.buzz(root, Ui.Buzz.TAP)
            chooseComputer(address, qr.name)
            // 教程开着时 chooseComputer 不连；这里一定要马上连上去交口令，不然电脑 2 分钟后删掉这次配对
            startLinkIfReady(address)
            link?.retryNow() // 等配对时连接线程在歇着，马上叫醒
            say("已用蓝牙配对「${qr.name}」，以后打开蒜鸟就会自动连上")
            updateBanner()
            menuSheet?.refresh()
        }
        setup.refreshOnboarding() // 教程里显示「正在配对…」
    }

    override fun onPairResult(ok: Boolean, message: String) {
        if (ok) Ui.buzz(root, Ui.Buzz.TAP)
        say(message)
        updateBanner()
        menuSheet?.refresh()
        setup.refreshOnboarding() // 教程里扫码那页打勾
        if (ok) main.postDelayed({ offerBattery(force = true) }, 1800)
    }

    // ───────── 局域网在后台别被掐断 ─────────

    /**
     * 扫码绑定过、后台还可能被限制：问一次。各家手机省电时都会在息屏、切到别的 App 一会儿以后冻结后台的 App、
     * 掐断它的网络（澎湃 OS 默认「智能限制后台运行」），只连局域网时就断了（蓝牙不受影响）。改哪一项按牌子说，见 BackgroundGuide。
     * force：刚扫码绑定成功，不管以前问没问过
     */
    private fun offerBattery(force: Boolean) {
        if (!resumed || !lanBound() || !BackgroundGuide.worthOpening(this) || setup.onboardingOpen) return
        if (!force && prefs.getBoolean(KEY_BATTERY_ASKED, false)) return
        prefs.edit().putBoolean(KEY_BATTERY_ASKED, true).apply()
        overlays.dialog(
            "让局域网在后台也连着",
            "手机省电时会在息屏、切到别的 App 一会儿以后冻结蒜鸟，局域网就断了（蓝牙不受影响）。\n\n点「去设置」，${BackgroundGuide.howTo()}。",
            listOf(Btn("以后再说"), Btn("去设置", Style.PRIMARY) { openBatterySettings() }),
        )
    }

    override fun openBatterySettings() {
        if (!BackgroundGuide.open(this)) say("打不开设置：到手机「设置 → 应用 → 蒜鸟」里，${BackgroundGuide.howTo()}")
    }

    /**
     * 局域网发文件明显比平常慢（见 FileSender.Slow）：按当时的情况说怎么快起来，一天最多说一次。
     * 蒜鸟开在前台时慢：有的手机（实测小米澎湃 OS）把前台 App 的上传排进 Wi-Fi 的视频优先级，大块数据又慢又丢包，
     * App 自己改不了（试过 TOS、SO_PRIORITY），从别的 App 分享过来（蒜鸟在后台）就快。
     * 在后台也慢、后台限制没放开：多半是系统在限制蒜鸟，带去设置。别的情况（网络本身慢）不说。
     */
    private fun offerFaster(kbps: Int, foreground: Boolean) {
        val now = System.currentTimeMillis()
        if (now - prefs.getLong(KEY_SLOW_HINT_AT, 0L) < 20 * 3600_000L) return
        val head = "局域网平常每秒能发好几 MB，这次每秒只有 $kbps KB。"
        when {
            foreground -> overlays.dialog(
                "这次发得慢",
                head + "有的手机会在蒜鸟开着的时候换一种方式安排它的网络，发大文件反而慢。\n\n" +
                    "下次在相册、文件管理里选好文件，点「分享 → 蒜鸟」，不用打开蒜鸟，会快很多。",
                listOf(Btn("知道了", Style.PRIMARY)),
            )
            !BackgroundGuide.free(this) -> overlays.dialog(
                "这次发得慢",
                head + "多半是手机在后台限制了蒜鸟。\n\n点「去设置」，${BackgroundGuide.howTo()}。",
                listOf(Btn("以后再说"), Btn("去设置", Style.PRIMARY) { openBatterySettings() }),
            )
            else -> return
        }
        prefs.edit().putLong(KEY_SLOW_HINT_AT, now).apply()
    }

    private fun lanLine(b: LanBinding): String = when {
        b.revoked -> "「${b.name}」上解除了绑定，点这里重新扫码"
        else -> "已绑定「${b.name}」 · " + when (link?.lanPhase) {
            LinkService.LanPhase.CONNECTED -> "正用局域网连着"
            LinkService.LanPhase.CONNECTING -> "正在连接…"
            else -> "在同一个网络里会自动连上"
        }
    }

    private fun forgetLan(b: LanBinding) {
        overlays.dialog(
            "解除局域网绑定？",
            "这部手机不再用局域网连「${b.name}」，蓝牙照常能用。以后想用要重新扫码。\n电脑上的绑定记录在电脑的「设置 → 扫码连接」里删。",
            listOf(Btn("取消"), Btn("解除", Style.DANGER) {
                link?.forgetLan() ?: LanBinding.save(this, null)
                updateBanner()
                menuSheet?.refresh()
                say("已解除局域网绑定")
            }),
        )
    }

    /** 语音按钮用：有麦克风权限返回 true；没有就先说明用途再申请（这次按不了，授权后再按）。 */
    private fun ensureMicPermission(): Boolean {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) return true
        if (overlays.isOpen) return false
        val asked = prefs.getBoolean(KEY_MIC_ASKED, false)
        val canAsk = !asked || shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO)
        val local = chosenModel() != null
        overlays.dialog(
            "语音要用麦克风",
            if (local) "只在按住语音键（或锁定）时录音，识别在手机上完成，录音不会上传。"
            else "只在按住语音键（或锁定）时录音，用的是手机自带的语音识别（要联网）。",
            listOf(
                Btn("不用了"),
                if (canAsk) Btn("允许", Style.PRIMARY) {
                    prefs.edit().putBoolean(KEY_MIC_ASKED, true).apply()
                    requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), REQ_MIC)
                } else Btn("去设置里打开", Style.PRIMARY) { openAppSettings() },
            ),
        )
        return false
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        val granted = grantResults.isNotEmpty() && grantResults.all { it == PackageManager.PERMISSION_GRANTED }
        when (requestCode) {
            REQ_MIC -> say(if (granted) "好了，按住语音键说话" else "没有麦克风权限，语音用不了；想用时再按一下语音键")
            REQ_CAMERA -> if (granted) scanComputer(scanFor) else say("没有相机权限就扫不了码：可以把二维码截图从相册选，或者用蓝牙连电脑")
            REQ_BLUETOOTH -> {
                if (setup.onboardingOpen) setup.refreshOnboarding()
                val qr = pendingBtQr
                pendingBtQr = null
                if (granted && qr != null) pairBluetooth(qr) // 扫码配对时要的权限：接着配对
                else if (granted) startLinkIfReady()
                else say("没有「附近的设备」权限就连不了电脑，随时可以在顶上的提示条里重新允许")
                updateBanner()
            }
        }
    }

    /**
     * 语音按钮用的是手机自带的语音识别服务（小米上是「系统语音引擎」），它录音时自己也要有麦克风权限。
     * 这个权限蒜鸟没法替它申请，只能说明情况、带用户去它的应用设置页开。
     */
    private fun explainEnginePermission() {
        if (overlays.isOpen) return // 一次按压会报两回错，别弹两个框
        val pkg = recognizerPackage()
        val name = runCatching { packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, 0)).toString() }
            .getOrDefault("系统语音引擎")
        overlays.dialog(
            "「$name」没有麦克风权限",
            "语音用的是手机自带的「$name」，它自己也要有麦克风权限才能听。\n\n点「去开启」，在打开的页面里找到「权限」，把「麦克风」改成允许，回来再按住说话。",
            listOf(Btn("取消"), Btn("去开启", Style.PRIMARY) { openAppSettings(pkg) }),
        )
    }

    /** 系统当前用的语音识别服务是哪个 App 提供的。读不到默认设置时，取第一个提供识别服务的 App。 */
    private fun recognizerPackage(): String {
        val configured = runCatching { Settings.Secure.getString(contentResolver, "voice_recognition_service") }.getOrNull()
        ComponentName.unflattenFromString(configured.orEmpty())?.let { return it.packageName }
        return packageManager.queryIntentServices(Intent(RecognitionService.SERVICE_INTERFACE), 0)
            .firstOrNull()?.serviceInfo?.packageName ?: "com.xiaomi.mibrain.speech"
    }

    private fun pickComputer() {
        setup.showPicker { address, name -> chooseComputer(address, name) }
    }

    // ───────── 界面骨架：顶栏 + 横幅 + 页面标签 + 网格，最上面一层是浮层 ─────────

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun buildShell(): View {
        root = FrameLayout(this).apply { setBackgroundColor(palette.bg) }
        content = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val overlayHost = FrameLayout(this)
        root.addView(content, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        root.addView(overlayHost, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        overlays = Overlays(overlayHost)
        setup = Setup(this, overlays)
        updater = Updater(this, overlays)
        // 自己处理系统栏和键盘占的位置：键盘弹出时网格整体变矮，模块都还在键盘上方
        if (Build.VERSION.SDK_INT >= 30) {
            @Suppress("DEPRECATION") // Android 15 起默认就是这样；11～14 还需要显式设置
            window.setDecorFitsSystemWindows(false)
            root.setOnApplyWindowInsetsListener { _, insets ->
                val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime())
                val bars = insets.getInsets(WindowInsets.Type.systemBars())
                content.setPadding(i.left + dp(10), i.top + dp(2), i.right + dp(10), i.bottom + dp(8))
                overlays.insetTop = bars.top
                overlays.insetBottom = bars.bottom
                overlays.noticeTop = bars.top + dp(50)
                WindowInsets.CONSUMED
            }
        } else {
            content.setPadding(dp(10), dp(2), dp(10), dp(8))
            overlays.noticeTop = dp(76)
        }

        // 顶栏：左边名字（和方案），中间靠右一行状态，最右是菜单；调整布局时换成 取消 · 提示 · 对齐/自由 · 完成
        // 品牌字「蒜鸟」三层叠起来：字身跟着深浅色，蒜苗绿色，蛋固定蛋黄色（tools/logo/wordmark.py 生成）
        brand = ImageView(this).apply {
            setImageDrawable(LayerDrawable(arrayOf(
                getDrawable(R.drawable.wordmark)!!.mutate().apply { setTint(palette.fg) },
                getDrawable(R.drawable.wordmark_sprouts)!!.mutate().apply {
                    setTint(if (palette.dark) 0xFFBEF264.toInt() else 0xFF6DB81F.toInt())
                },
                getDrawable(R.drawable.wordmark_egg)!!,
            )))
            contentDescription = "蒜鸟"
        }
        profileTag = TextView(this).apply {
            setTextColor(palette.fg)
            typeface = Ui.font(context, 850)
            textSize = 12f
            includeFontPadding = false
            maxLines = 1
            maxWidth = dp(120)
            ellipsize = TextUtils.TruncateAt.END
            setPadding(dp(9), dp(4), dp(9), dp(4))
            background = Ui.round(palette.tile, dp(11).toFloat(), Ui.hairline(context).toInt(), palette.line)
            visibility = View.GONE
        }
        readout = Ui.Readout(this, palette)
        recPill = TextView(this).apply {
            text = "● 录音中 · 点此结束"
            setTextColor(palette.red)
            typeface = Ui.font(context, 850)
            textSize = 13f
            gravity = Gravity.CENTER
            includeFontPadding = false
            setPadding(dp(12), 0, dp(12), 0)
            background = Ui.round(palette.tile, dp(15).toFloat(), dp(1) + 1, palette.red)
            visibility = View.GONE
            contentDescription = "录音中，点一下结束"
            Ui.springy(this)
            setOnClickListener { stopAllVoice() }
        }
        val statusBox = FrameLayout(this).apply {
            addView(readout, FrameLayout.LayoutParams(MATCH_PARENT, dp(22), Gravity.CENTER_VERTICAL))
            addView(recPill, FrameLayout.LayoutParams(WRAP_CONTENT, dp(30), Gravity.END or Gravity.CENTER_VERTICAL))
        }
        menuButton = ImageView(this).apply {
            setImageResource(R.drawable.ic_more)
            setColorFilter(palette.fg)
            scaleType = ImageView.ScaleType.CENTER
            contentDescription = "菜单"
            Ui.springy(this, 0.9f)
            setOnClickListener { showMenu() }
        }
        cancelButton = chipButton("取消") { cancelEditing() }
        snapSwitch = SnapSwitch(this).apply {
            visibility = View.GONE
            setOnClickListener { toggleSnap() }
        }
        doneButton = chipButton("完成") { saveEditing() }
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(8), 0, 0, 0)
            addView(brand, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
            addView(profileTag, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginStart = dp(8) })
            addView(cancelButton, LinearLayout.LayoutParams(WRAP_CONTENT, dp(30)))
            addView(statusBox, LinearLayout.LayoutParams(0, dp(32), 1f).apply { marginStart = dp(12) })
            addView(snapSwitch, LinearLayout.LayoutParams(WRAP_CONTENT, dp(30)).apply { marginStart = dp(8) })
            addView(doneButton, LinearLayout.LayoutParams(WRAP_CONTENT, dp(30)).apply { marginStart = dp(6); marginEnd = dp(2) })
            addView(menuButton, LinearLayout.LayoutParams(dp(42), dp(44)))
        }

        bannerBox = FrameLayout(this).apply {
            visibility = View.GONE
            addView(buildBanner(), FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        }

        tabs = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(4), dp(2), dp(4), dp(6))
        }
        tabScroller = HorizontalScrollView(this).apply {
            isHorizontalScrollBarEnabled = false
            addView(tabs)
        }
        grid = GridLayoutView(this)
        grid.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> updateGestureGuard() }

        content.addView(header, LinearLayout.LayoutParams(MATCH_PARENT, dp(46)))
        content.addView(bannerBox, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        content.addView(tabScroller, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        content.addView(grid, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        readout.set(baseText(), hot = false, animate = false)
        return root
    }

    /** 横幅：左边一个状态点，中间标题和一行说明，右边一个按钮 */
    private fun buildBanner(): View {
        bannerDot = StatusDot(this)
        bannerTitle = TextView(this).apply {
            setTextColor(palette.fg)
            typeface = Ui.font(context, 850)
            textSize = 14.5f
            includeFontPadding = false
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.END
        }
        bannerSub = TextView(this).apply {
            setTextColor(palette.sub)
            typeface = Ui.font(context, 650)
            textSize = 12.5f
            includeFontPadding = false
            maxLines = 2
            ellipsize = TextUtils.TruncateAt.END
            setPadding(0, dp(4), 0, 0)
        }
        bannerAction = TextView(this).apply {
            setTextColor(palette.tile)
            typeface = Ui.font(context, 850)
            textSize = 13.5f
            gravity = Gravity.CENTER
            includeFontPadding = false
            setPadding(dp(15), 0, dp(15), 0)
            background = Ui.round(palette.fg, dp(16).toFloat())
            Ui.springy(this)
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = Ui.round(palette.tile, dp(18).toFloat(), Ui.hairline(context).toInt(), palette.line)
            setPadding(dp(14), dp(11), dp(10), dp(11))
            addView(bannerDot, LinearLayout.LayoutParams(dp(12), dp(12)))
            addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                addView(bannerTitle)
                addView(bannerSub)
            }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f).apply { marginStart = dp(12); marginEnd = dp(10) })
            addView(bannerAction, LinearLayout.LayoutParams(WRAP_CONTENT, dp(34)))
        }.let { card ->
            FrameLayout(this).apply {
                setPadding(dp(4), dp(2), dp(4), dp(8))
                addView(card, FrameLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
            }
        }
    }

    /** 状态栏、导航栏的图标跟着深浅色走（界面本身铺到系统栏下面，底色就是界面的底色） */
    private fun applySystemBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            val mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS or WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS
            window.insetsController?.setSystemBarsAppearance(if (palette.dark) 0 else mask, mask)
        }
        if (Build.VERSION.SDK_INT >= 29) window.isNavigationBarContrastEnforced = false
    }

    /** 顶栏的小胶囊按钮（调整布局时用） */
    private fun chipButton(label: String, onClick: () -> Unit) = TextView(this).apply {
        text = label
        typeface = Ui.font(context, 850)
        textSize = 13f
        gravity = Gravity.CENTER
        includeFontPadding = false
        setPadding(dp(14), 0, dp(14), 0)
        visibility = View.GONE
        Ui.springy(this)
        setOnClickListener { onClick() }
    }

    // ───────── 按配置搭界面 ─────────

    /**
     * 用当前 config 重建页面标签和网格。force=false 时配置没变就什么都不做（重连时不打断正在输入的字）。
     * animate=false：布局变了也不播入场动画（刚在手机上调完布局，模块已经在新位置上了）。
     */
    private fun applyConfig(force: Boolean = false, animate: Boolean = true) {
        if (editor != null) return // 调整布局时不重建，调完再用
        val serialized = config.toString()
        val pendingText = pendingLayout?.toString().orEmpty()
        if (!force && serialized == configText && pendingText == configPendingText) return
        configText = serialized
        configPendingText = pendingText

        val layout = withPendingLayout(config.optJSONObject("layout") ?: JSONObject(DEFAULT_CONFIG).getJSONObject("layout"))
        val pages = layout.optJSONArray("pages") ?: return
        if (pages.length() == 0) return
        pageIndex = pageIndex.coerceIn(0, pages.length() - 1)
        buildTabs(pages)

        val touchpad = config.optJSONObject("touchpad") ?: JSONObject()
        val voice = config.optJSONObject("voice") ?: JSONObject()
        val triggers = config.optJSONObject("triggers") ?: JSONObject()
        val doubleMs = triggers.optLong("doubleClickMs", 300)
        val longMs = triggers.optLong("longPressMs", 500)
        val repeatDelayMs = triggers.optLong("repeatDelayMs", 400)
        val repeatIntervalMs = triggers.optLong("repeatIntervalMs", 50)
        val longRepeatDelayMs = triggers.optLong("longPressRepeatDelayMs", 350)
        val longRepeatIntervalMs = triggers.optLong("longPressRepeatIntervalMs", 100)
        val paused = config.optBoolean("paused", false)
        if (paused != mappingPaused) {
            mappingPaused = paused
            updateBanner()
            menuSheet?.refresh()
        }
        updateProfile(config.optJSONObject("profile"))

        releaseKeys()
        grid.removeAllViews() // 触控板、语音在 reusable 里留着，下面会重新加回来
        grid.columns = layout.optInt("columns", 4)
        grid.rows = layout.optInt("rows", 8)

        val page = pages.getJSONObject(pageIndex)
        val modules = page.optJSONArray("modules") ?: JSONArray()
        val onPage = mutableSetOf<String>()
        for (i in 0 until modules.length()) {
            val m = modules.getJSONObject(i)
            val id = m.optString("id")
            val label = m.optString("label")
            val cell = GridLayoutView.Cell(m.optDouble("x", 0.0), m.optDouble("y", 0.0), m.optDouble("w", 1.0), m.optDouble("h", 1.0))
            val view: View = when (m.optString("kind")) {
                "touchpad" -> (reusable[id] as? TouchpadView ?: TouchpadView(this)).also { pad ->
                    pad.sink = this
                    pad.label = label.ifBlank { "触控板" }
                    pad.sensitivity = touchpad.optDouble("sensitivity", 2.5)
                    pad.acceleration = touchpad.optDouble("acceleration", 1.2)
                    pad.scrollSpeed = touchpad.optDouble("scrollSpeed", 1.0)
                    pad.naturalScroll = touchpad.optBoolean("naturalScroll", true)
                }
                "voice" -> (reusable[id] as? VoiceBox ?: VoiceBox(this, onSend = { text -> sendText(text) }, onNotice = { say(it) })).also { box ->
                    box.autoSend = voice.optBoolean("autoSend", true)
                    box.delayMs = voice.optLong("delayMs", 1000)
                }
                "voiceButton" -> (reusable[id] as? VoiceButton ?: newVoiceButton(label)).also { it.label = label.ifBlank { "语音按钮" } }
                else -> newKey(m, id, label.ifBlank { "?" }, doubleMs, longMs, repeatDelayMs, repeatIntervalMs, longRepeatDelayMs, longRepeatIntervalMs, paused)
            }
            (view.parent as? ViewGroup)?.removeView(view)
            if (view is TouchpadView || view is VoiceBox || view is VoiceButton) {
                reusable[id] = view
                onPage += id
            }
            grid.add(view, id, cell)
        }
        // 留着整个布局里还有的（别的页上的也留着，换回来时正在输入的字还在）；别的页上正在说的话当作说完了
        val everywhere = mutableSetOf<String>()
        for (p in 0 until pages.length()) {
            val list = pages.getJSONObject(p).optJSONArray("modules") ?: continue
            for (j in 0 until list.length()) everywhere += list.getJSONObject(j).optString("id")
        }
        reusable.entries.filter { it.key !in everywhere }.forEach { (it.value as? VoiceButton)?.release() }
        reusable.keys.retainAll(everywhere)
        reusable.filterKeys { it !in onPage }.values.forEach { (it as? VoiceButton)?.stopListening() }
        grid.requestLayout()

        // 入场动画：每页第一次显示时播；同一页布局变了（电脑上改了）也播；切回看过的页只淡入一下
        val pageId = page.optString("id", pageIndex.toString())
        val signature = layoutSignature(layout)
        val firstTime = seenPages.add(pageId)
        val samePage = pageId == shownPageId
        if (animate && (firstTime || (samePage && signature != shownSignature))) {
            grid.animateIn()
        } else if (!samePage) {
            grid.alpha = 0.4f
            grid.animate().alpha(1f).setDuration(160).setInterpolator(Ui.EASE).start()
        }
        shownPageId = pageId
        shownSignature = signature
        applyVoiceSettings()
        updateOfflineLook()
        maybeShowGestureGuide()
    }

    private fun buildTabs(pages: JSONArray) {
        val sig = StringBuilder().append(pageIndex)
        for (i in 0 until pages.length()) sig.append('|').append(pages.getJSONObject(i).optString("name"))
        if (sig.toString() == tabsSignature && tabs.childCount == pages.length()) return
        tabsSignature = sig.toString()
        tabs.removeAllViews()
        tabScroller.visibility = if (pages.length() > 1) View.VISIBLE else View.GONE
        for (i in 0 until pages.length()) {
            val page = pages.getJSONObject(i)
            val selected = i == pageIndex
            tabs.addView(TextView(this).apply {
                text = page.optString("name", "第 ${i + 1} 页")
                setTextColor(if (selected) palette.tile else palette.fg)
                typeface = Ui.font(context, 800)
                textSize = 13f
                gravity = Gravity.CENTER
                setPadding(dp(14), 0, dp(14), 0)
                background = Ui.round(if (selected) palette.fg else palette.tile, dp(15).toFloat(),
                    if (selected) 0 else Ui.hairline(context).toInt(), palette.line)
                Ui.springy(this, 0.92f)
                setOnClickListener {
                    if (editor != null || i == pageIndex) return@setOnClickListener // 调整布局时不换页
                    pageIndex = i
                    prefs.edit().putInt(KEY_PAGE, i).apply()
                    applyConfig(force = true)
                }
            }, LinearLayout.LayoutParams(WRAP_CONTENT, dp(30)).apply { marginEnd = dp(6) })
        }
    }

    private fun newKey(
        m: JSONObject, id: String, label: String, doubleMs: Long, longMs: Long,
        repeatDelayMs: Long, repeatIntervalMs: Long, longRepeatDelayMs: Long, longRepeatIntervalMs: Long, paused: Boolean,
    ): KeyButton {
        val t = m.optJSONObject("triggers") ?: JSONObject()
        // 按住时怎么办：电脑 0.4.0 起发 hold；更早的只有 repeat（连发）
        val hold = m.optString("hold").ifEmpty { if (m.optBoolean("repeat")) KeyButton.HOLD_REPEAT_CLICK else KeyButton.HOLD_LONG }
        return KeyButton(
            this, id, label,
            hasDoubleClick = t.optBoolean("doubleClick"),
            hasLongPress = t.optBoolean("longPress"),
            doubleClickMs = doubleMs,
            longPressMs = longMs,
            hold = hold,
            repeatDelayMs = repeatDelayMs,
            repeatIntervalMs = repeatIntervalMs,
            longRepeatDelayMs = longRepeatDelayMs,
            longRepeatIntervalMs = longRepeatIntervalMs,
            paused = paused,
            onTrigger = { buttonId, trigger, seq, heldMs ->
                val msg = JSONObject().put("t", "button").put("id", buttonId).put("trigger", trigger)
                // 连发、重复长按：第几下、手机上的时刻（电脑跟不上时据此跳过过时的几下）、按住多久了（光标加速用）
                if (seq >= 0) msg.put("seq", seq).put("ts", SystemClock.uptimeMillis())
                if (heldMs >= 0) msg.put("held", heldMs)
                val sent = link?.send(msg) == true
                if (sent) echo(label, trigger, seq)
                sent
            },
            onHold = { buttonId, down ->
                val sent = link?.send(JSONObject().put("t", "hold").put("id", buttonId).put("down", down)) == true
                if (sent && down) flash("按住 $label", 1400)
                sent
            },
            onRefused = { why ->
                when (why) {
                    KeyButton.Refusal.OFFLINE -> flash("未连接电脑 · 没有发送", 2200)
                    KeyButton.Refusal.PAUSED -> say("按键已暂停 · 触控板和语音照常")
                    KeyButton.Refusal.SLID_OFF -> flash("移出按键 · 松手不发送", 1600)
                }
            },
        ).also { it.offline = statusKnown && !connected }
    }

    private fun newVoiceButton(label: String) = VoiceButton(
        this, label.ifBlank { "语音按钮" },
        hasMicPermission = { ensureMicPermission() },
        isConnected = { connected },
        onSend = { text -> sendText(text) },
        onTyping = { delete, insert, last -> typeLive(delete, insert, last) },
        onStash = { text -> stash(text) },
        onNotice = { msg -> say(msg) },
        onLatch = { on -> showRecPill(on) },
        onIdle = {
            scheduleDim()
            main.postDelayed({ maybeOfferPending(force = false) }, 300)
        },
        onEngineDenied = { explainEnginePermission() },
    )

    /** 只看这一页模块的位置、大小、名字：换方案（只改了按法、按住时）不重播入场动画 */
    private fun layoutSignature(layout: JSONObject): String {
        val sb = StringBuilder().append(pageIndex).append(':').append(layout.optInt("columns")).append('x').append(layout.optInt("rows"))
        val modules = layout.optJSONArray("pages")?.optJSONObject(pageIndex)?.optJSONArray("modules") ?: return sb.toString()
        for (i in 0 until modules.length()) {
            val m = modules.getJSONObject(i)
            sb.append('|').append(m.optString("id")).append(',').append(m.optString("kind")).append(',').append(m.optString("label"))
            for (k in arrayOf("x", "y", "w", "h")) sb.append(',').append(m.optDouble(k))
        }
        return sb.toString()
    }

    /** 离线时调好、还没存到电脑上的布局：先盖在电脑的配置上显示 */
    private fun withPendingLayout(layout: JSONObject): JSONObject {
        val pending = pendingLayout ?: return layout
        val cells = pending.optJSONArray("modules") ?: return layout
        val copy = JSONObject(layout.toString())
        val pages = copy.optJSONArray("pages") ?: return layout
        for (i in 0 until pages.length()) {
            val page = pages.getJSONObject(i)
            if (page.optString("id") != pending.optString("page")) continue
            val modules = page.optJSONArray("modules") ?: continue
            for (j in 0 until modules.length()) {
                val m = modules.getJSONObject(j)
                for (k in 0 until cells.length()) {
                    val c = cells.getJSONObject(k)
                    if (c.optString("id") != m.optString("id")) continue
                    for (key in arrayOf("x", "y", "w", "h")) m.put(key, c.optDouble(key))
                }
            }
        }
        return copy
    }

    /** 方案：多于一个时品牌名旁常驻方案名；换了方案在屏幕中间浮出大号方案名，明显震一下 */
    private fun updateProfile(p: JSONObject?) {
        val id = p?.optString("id").orEmpty()
        val name = profileTitle(p?.optString("name").orEmpty())
        val count = p?.optInt("count", 1) ?: 1
        val switched = profileId != null && id.isNotEmpty() && id != profileId
        if (id.isNotEmpty()) profileId = id
        val show = count > 1 && name.isNotEmpty() && editor == null
        profileTag.text = name
        profileTag.contentDescription = "当前方案：$name"
        profileTag.visibility = if (show) View.VISIBLE else View.GONE
        if (switched) spotlightProfile(name)
    }

    /** 旧版电脑端的「默认配置」就是「默认方案」 */
    private fun profileTitle(name: String) = if (name == "默认配置") "默认方案" else name

    private fun spotlightProfile(name: String) {
        if (name.isEmpty()) return
        val now = SystemClock.uptimeMillis()
        if (name == lastSpotName && now - lastSpotAt < 2500) return // 电脑先发 profile 再推配置，别闪两次
        lastSpotName = name
        lastSpotAt = now
        overlays.spotlight("切换到方案", name)
        Ui.buzz(root, Ui.Buzz.STRONG)
    }

    // ───────── 连接状态：顶栏、横幅、按键变淡 ─────────

    private fun applyStatus(s: LinkService.Status) {
        val was = status
        val wasKnown = statusKnown
        status = s
        statusKnown = true
        // 记下电脑的名字（旧版没存过，菜单里不然只能显示蓝牙地址）
        if (s.connected && !s.lan && !s.pcName.isNullOrBlank() && savedName() != s.pcName) prefs.edit().putString(KEY_PC_NAME, s.pcName).apply()
        if (s.connected && !was.connected) {
            if (wasKnown) {
                // 重新连上：闪一下连的是哪台电脑，轻震一下
                flash("已连接 · ${pcName() ?: "电脑"}", 2200)
                Ui.buzz(root, Ui.Buzz.TAP)
            }
            sendPendingLayout()
            main.postDelayed({ maybeOfferPending(force = true) }, 1700) // 等「已连接」「切换方案」这些先播完
        }
        if (!s.connected && (was.connected || !wasKnown)) offlineSince = SystemClock.uptimeMillis()
        updateOfflineLook()
        updateBanner()
        showBase(animate = true)
        menuSheet?.refresh()
        pendingSheet?.refresh()
        setup.refreshOnboarding() // 教程开着时，扫码那页跟着局域网连没连上变
    }

    override fun onLinkStatus(status: LinkService.Status) = applyStatus(status)

    /** 电脑的名字：电脑 hello 里说的，没有就用蓝牙名、选电脑时记下的名字 */
    private fun pcName(): String? = link?.pcLabel() ?: status.pcName ?: savedName()

    private fun updateOfflineLook() {
        val off = statusKnown && !connected
        for (i in 0 until grid.childCount) {
            when (val v = grid.getChildAt(i)) {
                is KeyButton -> v.offline = off
                is TouchpadView -> v.offline = off
                is VoiceButton -> v.offline = off
                is VoiceBox -> v.offline = off
            }
        }
    }

    /** 平时顶栏右边显示的：连上了是电脑名，没连上是为什么；调整布局时是操作提示 */
    private fun baseText(): String = when {
        editor != null -> editHint()
        connected -> "已连接 · ${pcName() ?: "电脑"}"
        status.phase == Phase.OFF && link?.stoppedByUser == true -> "已断开"
        !hasBluetoothPermission() && !lanBound() -> "需要蓝牙权限"
        (savedAddress() == null && !lanBound()) || status.phase == Phase.NO_DEVICE -> "还没选电脑"
        status.phase == Phase.BT_OFF -> "手机蓝牙没开"
        status.phase == Phase.WAITING || offlineLong() -> "未连接电脑"
        else -> "正在连接…"
    }

    /** 没连上已经超过一会儿（刚打开 App 正在连的那一两秒不算） */
    private fun offlineLong() = statusKnown && !connected && SystemClock.uptimeMillis() - offlineSince >= OFFLINE_GRACE_MS

    private fun showBase(animate: Boolean) {
        if (detailPinned) return
        val hot = editor == null && !connected && statusKnown && (offlineLong() || status.phase != Phase.CONNECTING)
        readout.set(baseText(), hot = hot, animate = animate)
    }

    private class BannerSpec(
        val key: String,
        val title: String,
        val sub: String,
        val action: String,
        val busy: Boolean = false,
        val paused: Boolean = false,
        val onBody: (() -> Unit)? = null,
        val onAction: () -> Unit,
    )

    private fun bannerSpec(): BannerSpec? {
        if (setup.onboardingOpen) return null
        val pending = pendingText()
        val pendingLine = if (pending.isNotEmpty()) "有 ${count(pending)} 字没发出去 · 点这里看" else null
        val openPending: (() -> Unit)? = if (pending.isNotEmpty()) ({ showPendingSheet() }) else null
        return when {
            !connected && savedAddress() == null && LanBinding.load(this)?.revoked == true ->
                BannerSpec("lanoff", "电脑上解除了绑定", "重新扫电脑上的二维码就能再用局域网连", "扫码") { scanComputer() }
            !hasBluetoothPermission() && !lanBound() -> BannerSpec("perm", "需要「附近的设备」权限", "用蓝牙连电脑要用到它，只连你选的那台", "允许") { askBluetoothPermission() }
            savedAddress() == null && !lanBound() -> BannerSpec("nodev", "还没选电脑", "蓝牙选电脑，或者扫电脑上的二维码", "选择电脑") { pickComputer() }
            connected && mappingPaused -> BannerSpec("paused", "按键已暂停", "触控板和语音照常", "恢复", paused = true) { requestEnabled(true) }
            connected -> null
            status.phase == Phase.OFF && link?.stoppedByUser == true ->
                BannerSpec("off", "已断开", pendingLine ?: "下次打开 App 时会再连", "连接", onBody = openPending) { reconnect() }
            status.phase == Phase.BT_OFF -> BannerSpec("bt", "手机蓝牙没开", pendingLine ?: "打开以后会自动连接", "去打开", onBody = openPending) { openBluetoothSettings() }
            !offlineLong() -> {
                main.removeCallbacks(bannerRecheck)
                main.postDelayed(bannerRecheck, OFFLINE_GRACE_MS - (SystemClock.uptimeMillis() - offlineSince) + 50)
                null
            }
            else -> BannerSpec(
                "offline", "未连接电脑",
                pendingLine ?: if (status.phase == Phase.CONNECTING) "正在连接…" else "在电脑上打开蒜鸟，会自动连上",
                "重试", busy = status.phase == Phase.CONNECTING, onBody = openPending,
            ) { retryLink() }
        }
    }

    /** 横幅出现、消失时高度平滑地变（网格跟着变矮、变高）；调整布局时不动，免得网格跳 */
    private fun updateBanner() {
        if (editor != null) return
        val spec = bannerSpec()
        if (spec != null) {
            bannerTitle.text = spec.title
            bannerSub.text = spec.sub
            bannerAction.text = spec.action
            bannerAction.setOnClickListener { spec.onAction() }
            bannerDot.set(if (spec.paused) DOT_PAUSED else DOT_OFFLINE, spec.busy)
            val card = (bannerBox.getChildAt(0) as ViewGroup).getChildAt(0)
            card.setOnClickListener(spec.onBody?.let { body -> View.OnClickListener { body() } })
            card.isClickable = spec.onBody != null
        }
        val key = spec?.key
        if (key == bannerKey) return
        val wasShown = bannerKey != null
        bannerKey = key
        if (spec != null && !wasShown) expandBanner(true) else if (spec == null && wasShown) expandBanner(false)
    }

    /**
     * 横幅展开、收起。网格的格子高度跟着可用高度走，横幅一出来所有按键都要变矮；
     * 以前每帧改横幅高度，整张网格每帧重新排版、重画几十个键上的字，一帧要 20~100 毫秒，动画一卡一卡。
     * 现在只排一次版：展开时先直接排成最终的样子，再用位移 + 纵向缩放把网格「放回」原来的位置和大小，
     * 然后把这两个量动画回 0、1；收起时反过来，先用变换动到收起后的样子，最后一帧才真的改布局（改完画面不变）。
     * 位移、缩放只改图层属性，每帧不排版也不重画。
     */
    private fun expandBanner(show: Boolean) {
        bannerAnim?.end() // 上一个还没走完：先直接走到终点，从稳定的状态开始
        bannerAnim = null
        if (content.width == 0 || grid.height == 0) { // 还没排版（刚打开）：直接显示，不做动画
            bannerBox.visibility = if (show) View.VISIBLE else View.GONE
            bannerBox.alpha = 1f
            return
        }
        val movers = listOf<View>(tabScroller, grid)
        val gridH = grid.height.toFloat()
        if (show) {
            val child = bannerBox.getChildAt(0)
            child.measure(View.MeasureSpec.makeMeasureSpec(content.width - content.paddingLeft - content.paddingRight, View.MeasureSpec.EXACTLY), View.MeasureSpec.UNSPECIFIED)
            val full = child.measuredHeight.toFloat()
            if (full <= 0f || full >= gridH) {
                bannerBox.visibility = View.VISIBLE
                return
            }
            val scale0 = gridH / (gridH - full)
            // 这一帧：横幅占好位置，网格排成变矮后的样子，但看起来和原来一模一样
            bannerBox.visibility = View.VISIBLE
            bannerBox.alpha = 0f
            grid.pivotY = 0f
            grid.scaleY = scale0
            movers.forEach { it.translationY = -full }
            bannerAnim = bannerAnimator(240) { f ->
                bannerBox.alpha = f
                grid.scaleY = scale0 + (1f - scale0) * f
                movers.forEach { it.translationY = -full * (1f - f) }
            }.also { startAfterLayout(it) }
        } else {
            val h = bannerBox.height.toFloat()
            if (h <= 0f) {
                bannerBox.visibility = View.GONE
                return
            }
            val scale1 = (gridH + h) / gridH
            grid.pivotY = 0f
            bannerAnim = bannerAnimator(220) { f ->
                bannerBox.alpha = 1f - f
                grid.scaleY = 1f + (scale1 - 1f) * f
                movers.forEach { it.translationY = -h * f }
            }.apply {
                addListener(object : android.animation.AnimatorListenerAdapter() {
                    override fun onAnimationEnd(animation: android.animation.Animator) {
                        // 和下一次排版同一帧生效：排完的网格正好在动画终点的位置、大小
                        bannerBox.visibility = View.GONE
                        bannerBox.alpha = 1f
                        grid.scaleY = 1f
                        movers.forEach { it.translationY = 0f }
                    }
                })
                start()
            }
        }
    }

    private fun bannerAnimator(ms: Long, step: (Float) -> Unit) = ValueAnimator.ofFloat(0f, 1f).apply {
        duration = ms
        interpolator = Ui.EASE
        addUpdateListener { step(it.animatedValue as Float) }
        addListener(object : android.animation.AnimatorListenerAdapter() {
            override fun onAnimationEnd(animation: android.animation.Animator) = step(1f)
        })
    }

    /** 排版、重画这些重活在这一帧做完，下一帧再开始计时，动画不会一出来就跳掉一截 */
    private fun startAfterLayout(anim: ValueAnimator) {
        content.viewTreeObserver.addOnPreDrawListener(object : android.view.ViewTreeObserver.OnPreDrawListener {
            override fun onPreDraw(): Boolean {
                content.viewTreeObserver.removeOnPreDrawListener(this)
                android.view.Choreographer.getInstance().postFrameCallback { if (bannerAnim === anim) anim.start() }
                return true
            }
        })
    }

    private fun retryLink() {
        if (link == null || status.phase == Phase.OFF) startLinkIfReady()
        link?.retryNow()
        flash("正在连接…", 2000)
    }

    private fun reconnect() {
        startLinkIfReady()
        flash("正在连接…", 2000)
    }

    private fun disconnect() {
        link?.stopLink()
        releaseKeys()
    }

    // ───────── 没发出去的话：存在手机上，连上后问发不发 ─────────

    private fun pendingText(): String = prefs.getString(KEY_PENDING_TEXT, "").orEmpty()

    private fun count(text: String) = text.codePointCount(0, text.length)

    /** 语音没发出去的字，接在之前没发的后面存起来 */
    private fun stash(text: String) {
        val old = pendingText()
        val joined = when {
            old.isEmpty() -> text
            old.last().code < 128 && old.last().isLetterOrDigit() && text.first().code < 128 && text.first().isLetterOrDigit() -> "$old $text"
            else -> old + text
        }
        prefs.edit().putString(KEY_PENDING_TEXT, joined).apply()
        pendingFresh = true
        updateBanner()
        menuSheet?.refresh()
        pendingSheet?.refresh()
        if (!connected) say("未连接电脑 · 说的 ${count(joined)} 字存在手机上了，连上后可以发")
    }

    private fun clearPending() {
        prefs.edit().remove(KEY_PENDING_TEXT).apply()
        pendingFresh = false
        updateBanner()
        menuSheet?.refresh()
    }

    /** 连上了、说完了：有没发出去的话就弹面板问（force = 刚连上，不管之前问没问过） */
    private fun maybeOfferPending(force: Boolean) {
        if (!connected || pendingText().isEmpty()) return
        if (!force && !pendingFresh) return
        if (!resumed || editor != null || overlays.isOpen || anyVoiceBusy()) return
        showPendingSheet()
    }

    private fun showPendingSheet() {
        pendingSheet?.takeIf { !it.closed }?.let { it.refresh(); return }
        pendingFresh = false
        discardArmed = false
        lateinit var sheet: Overlays.Sheet
        sheet = overlays.sheet build@{
            val text = pendingText()
            if (text.isEmpty()) {
                header("没有没发出去的话")
                return@build
            }
            header(
                "刚才说的 ${count(text)} 字没发出去",
                if (connected) "发送后会打在电脑当前的窗口里" else "连上电脑后才能发送；也可以先复制",
            )
            view(overlays.text(text, 15f, 650, palette.fg).apply {
                setLineSpacing(dp(3).toFloat(), 1f)
                maxLines = 10
                ellipsize = TextUtils.TruncateAt.END
                background = Ui.round(palette.bg, dp(16).toFloat())
                setPadding(dp(16), dp(14), dp(16), dp(14))
                setTextIsSelectable(true)
            }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { setMargins(dp(20), dp(4), dp(20), dp(4)) })
            buttons(
                Btn(if (discardArmed) "确定丢弃？" else "丢弃", Style.DANGER) {
                    if (discardArmed) {
                        clearPending()
                        sheet.dismiss()
                        flash("已丢弃", 1600)
                    } else {
                        discardArmed = true
                        sheet.refresh()
                        main.postDelayed({
                            if (discardArmed && !sheet.closed) {
                                discardArmed = false
                                sheet.refresh()
                            }
                        }, 3000)
                    }
                },
                Btn("复制") {
                    getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("蒜鸟", text))
                    flash("已复制 ${count(text)} 字", 1600)
                },
                Btn(if (connected) "发送" else "未连接", Style.PRIMARY, enabled = connected) { sendPending(sheet) },
            )
        }
        sheet.onDismiss = { if (pendingSheet === sheet) pendingSheet = null }
        pendingSheet = sheet
    }

    private fun sendPending(sheet: Overlays.Sheet) {
        val text = pendingText()
        if (text.isEmpty()) return sheet.dismiss()
        if (link?.send(JSONObject().put("t", "text").put("s", text)) == true) {
            clearPending()
            sheet.dismiss()
            Ui.buzz(root, Ui.Buzz.CONFIRM)
            flash("已发送 ${count(text)} 字", 1800)
        } else {
            Ui.buzz(root, Ui.Buzz.REJECT)
            say("没发出去：未连接电脑")
            sheet.refresh()
        }
    }

    // ───────── ⋯ 菜单（自绘的底部面板） ─────────

    private fun showMenu() {
        if (overlays.isOpen) return
        lateinit var sheet: Overlays.Sheet
        sheet = overlays.sheet(menuBody) { buildMenu(this) }
        menuBody = sheet.body
        sheet.onDismiss = { if (menuSheet === sheet) menuSheet = null }
        menuSheet = sheet
    }

    private fun buildMenu(s: Overlays.Sheet) = with(s) {
        header("蒜鸟", if (connected) "已连接 · ${pcName() ?: "电脑"}" + (status.via?.let { " · $it" } ?: "") else baseText())
        val pending = pendingText()
        if (pending.isNotEmpty()) {
            row("刚才没发出去的 ${count(pending)} 字", if (connected) "发送、复制或丢弃" else "连上电脑后可以发送，也可以先复制") {
                s.dismiss()
                showPendingSheet()
            }
        }
        row("调整布局", "拖动模块、改大小") {
            s.dismiss()
            startEditing()
        }
        val pend = link?.pendingEnabled
        toggle(
            "暂停按键",
            when {
                pend != null -> if (pend) "正在恢复…" else "正在暂停…"
                !connected -> "连上电脑后才能用"
                mappingPaused -> "已暂停 · 触控板和语音照常"
                else -> "触控板和语音照常"
            },
            checked = if (pend != null) !pend else mappingPaused,
            enabled = connected && pend == null,
        ) { on -> requestEnabled(!on) }
        row("发文件到电脑", if (connected) "照片、文档发过去；相册里分享给蒜鸟也行" else "连上电脑后才能用", enabled = connected) {
            s.dismiss()
            pickFiles()
        }

        section("语音识别")
        val models = LocalAsr.models(this@MainActivity)
        val model = chosenModel()
        models.forEach { m ->
            radio(modelTitle(m), modelSubtitle(m), m == model) {
                chooseAsr(m.name)
                s.refresh()
            }
        }
        if (models.isEmpty()) row("本地识别", "手机上还没装语音模型", enabled = false, trailing = null, onClick = null)
        radio("系统引擎（联网，无标点）", "小米自带的语音识别；一句一句出字", model == null) {
            chooseAsr(ASR_SYSTEM)
            s.refresh()
        }
        toggle("边说边打字", "说一段打一段；关掉后停顿一下再整句发", prefs.getBoolean(KEY_LIVE_TYPING, true)) { on ->
            prefs.edit().putBoolean(KEY_LIVE_TYPING, on).apply()
            applyVoiceSettings()
        }

        divider()
        row("选择电脑", "蓝牙 · " + (savedName() ?: if (savedAddress() != null) "已选一台电脑" else "还没选")) {
            s.dismiss()
            pickComputer()
        }
        val lan = LanBinding.load(this@MainActivity)
        row("扫码连接电脑", if (lan == null) "蓝牙配对；同一个网络里还能走局域网" else lanLine(lan)) {
            s.dismiss()
            scanComputer()
        }
        if (lan != null && !lan.revoked && BackgroundGuide.worthOpening(this@MainActivity)) {
            row("后台保持局域网", "别让手机在后台限制蒜鸟，息屏也不断") {
                s.dismiss()
                openBatterySettings()
            }
        }
        if (lan != null) {
            row("解除局域网绑定", "只在这部手机上忘掉「${lan.name}」", trailing = null) {
                s.dismiss()
                forgetLan(lan)
            }
        }
        row("使用教程", "一步步看怎么连电脑、怎么用") {
            s.dismiss()
            setup.showTutorial()
        }
        row("手势说明", "触控板怎么点、怎么滚、怎么拖") {
            s.dismiss()
            showGestureGuide()
        }
        // 点了马上查一次：有新版本就关掉菜单弹更新面板，没有就只改这行的小字
        row("检查更新", updater.menuLine(), trailing = null) { updater.menuTapped() }
        toggle("自动检查更新", "打开 App 时看看有没有新版本，只连 GitHub", updater.autoCheck) { on -> updater.autoCheck = on }
        row("关于", aboutLine(), trailing = null) {
            aboutOpen = !aboutOpen
            s.refresh()
        }
        if (aboutOpen) {
            aboutDetails().forEach { line ->
                view(overlays.text(line, 13f, 650, palette.sub).apply {
                    setLineSpacing(dp(2).toFloat(), 1f)
                    setPadding(dp(24), dp(2), dp(24), dp(6))
                })
            }
            row("开源许可", "蒜鸟用到的开源软件、语音模型和字体") {
                s.dismiss()
                showLicenses()
            }
        }
        row("调试", if (debugOpen) "给开发看的" else "测延迟、电脑后台内存", trailing = null) {
            debugOpen = !debugOpen
            s.refresh()
        }
        if (debugOpen) {
            row("测延迟", latencyText ?: "连发 100 次，看消息来回一趟要多久", trailing = null) { latencyTest() }
            row("电脑后台内存", memText ?: "看看电脑上的后台占多少内存", trailing = null) { askMemory() }
        }
        if (status.phase != Phase.OFF) {
            divider()
            row("断开连接", "不再自动重连，下次打开 App 时再连", trailing = null) {
                s.dismiss()
                disconnect()
            }
        }
    }

    private fun modelTitle(m: LocalAsr.Model) = when {
        "480" in m.name -> "本地识别 · 快（推荐）"
        "960" in m.name -> "本地识别 · 更准"
        else -> "本地识别 · ${m.name}"
    }

    private fun modelSubtitle(m: LocalAsr.Model) = when {
        "480" in m.name -> "在手机上识别，不用联网，带标点，出字快"
        "960" in m.name -> "出字慢一点，错字少一点"
        else -> "在手机上识别，不用联网"
    }

    private fun aboutLine(): String {
        val pc = link?.pcApp
        return "App ${BuildConfig.VERSION_NAME}" + if (pc != null) " · 电脑端 $pc" else ""
    }

    private fun aboutDetails(): List<String> {
        val lines = mutableListOf("手机 App：${BuildConfig.VERSION_NAME}")
        val host = link?.pcHost
        val app = link?.pcApp
        lines += when {
            host != null || app != null -> "电脑：${host ?: pcName() ?: "未知"} · 蒜鸟 ${app ?: "未知版本"}"
            else -> "电脑：${pcName() ?: "还没连过"}"
        }
        if (app != null && older(app, MIN_PC_VERSION)) lines += "电脑端版本较旧，更新到 $MIN_PC_VERSION 以后才能在手机上暂停按键、确认同步"
        return lines
    }

    private fun latencyTest() {
        val l = link
        if (l == null || !connected) {
            latencyText = "未连接电脑，测不了"
            menuSheet?.refresh()
            return
        }
        latencyText = "正在测…（大约 3 秒）"
        menuSheet?.refresh()
        l.latencyBurst(100)
        main.postDelayed({
            latencyText = l.rttSummary(100) ?: "没测到，可能断开了"
            menuSheet?.refresh()
        }, 100 * 20 + 1500L)
    }

    private fun askMemory() {
        memText = if (link?.send(JSONObject().put("t", "mem")) == true) "正在问…" else "未连接电脑"
        menuSheet?.refresh()
    }

    private fun showGestureGuide() {
        setup.showGestureGuide { prefs.edit().putBoolean(KEY_GUIDE_SEEN, true).apply() }
    }

    /** 开源软件、语音模型和字体的许可（licenses.txt 最后接 Nunito 字体自带的 OFL 全文） */
    private fun showLicenses() {
        val text = listOf("licenses.txt", "fonts/Nunito-OFL.txt").joinToString("\n\n") { name ->
            runCatching { assets.open(name).bufferedReader().use { it.readText().trim() } }.getOrDefault("")
        }
        overlays.sheet {
            header("开源许可")
            view(overlays.text(text, 12f, 600, palette.sub).apply {
                setLineSpacing(dp(2).toFloat(), 1f)
                setPadding(dp(24), dp(4), dp(24), dp(16))
            })
        }
    }

    /** 第一次见到触控板时说明一次手势 */
    private fun maybeShowGestureGuide() {
        if (prefs.getBoolean(KEY_GUIDE_SEEN, false)) return
        if ((0 until grid.childCount).none { grid.getChildAt(it) is TouchpadView }) return
        main.postDelayed({
            if (!prefs.getBoolean(KEY_GUIDE_SEEN, false) && resumed && !overlays.isOpen && editor == null &&
                (0 until grid.childCount).any { grid.getChildAt(it) is TouchpadView }
            ) showGestureGuide()
        }, 900)
    }

    // ───────── 检查更新（Updater.kt 的 Host） ─────────

    override fun canShowUpdate(manual: Boolean): Boolean {
        if (!resumed || editor != null || overlays.isOpen || setup.onboardingOpen || anyVoiceBusy()) return false
        if (manual) return true
        // 第一次打开、引导教程还没走：先不弹
        if (savedAddress() == null && !setup.tutorialSeen) return false
        // 手在屏幕上、刚碰过：等手离开一会儿，别挡住正在用的触控板、按键
        return !touching && SystemClock.uptimeMillis() - lastTouchAt >= UPDATE_QUIET_MS
    }

    override fun closeMenuForUpdate() {
        menuSheet?.dismiss()
    }

    override fun updateNotice(text: String) = say(text)

    override fun onUpdateChanged() {
        menuSheet?.refresh()
    }

    // ───────── 在手机上暂停 / 恢复按键 ─────────

    private fun requestEnabled(enabled: Boolean) {
        val l = link
        val what = if (enabled) "恢复" else "暂停"
        if (l == null || !connected) {
            say("未连接电脑，没法${what}按键")
            return
        }
        if (l.pendingEnabled != null) return
        if (!l.requestEnabled(enabled)) {
            say("没发出去：未连接电脑")
            return
        }
        flash("正在${what}按键…", 4500)
        menuSheet?.refresh()
    }

    override fun onEnableResult(ok: Boolean, enabled: Boolean) {
        if (ok) {
            flash(if (enabled) "按键已恢复" else "按键已暂停", 1800)
            Ui.buzz(root, Ui.Buzz.CONFIRM)
        } else {
            detailPinned = false
            showBase(animate = true)
            say(if (enabled) "电脑端版本较旧，不支持在手机上恢复按键" else "电脑端版本较旧，不支持在手机上暂停")
        }
        menuSheet?.refresh()
    }

    // ───────── 在手机上调整布局 ─────────

    private val editListener = object : LayoutEditor.Listener {
        override fun onEditChanged(changed: Boolean) {
            editChanged = changed
            showBase(animate = true)
            updateGestureGuard()
        }

        override fun onEditNotice(text: String) = flash(text, 2200)

        override fun onEditSelect(id: String?) {
            showBase(animate = true)
            updateGestureGuard()
        }
    }

    /**
     * 系统返回手势的排除区。设在根布局上（模块离屏幕边还有十几 dp，设在模块自己身上盖不到屏幕边），
     * 靠边的一直延伸到屏幕边。系统每条边最多让出 200dp：
     * - 平时：每个触控板护 200dp 高的一段，跟着最近一次按下的位置走（手常放在哪就护哪；还没按过时护最下面）
     * - 调整布局：选中的模块中间那一段（LayoutEditor.guardRect），没选中时整个网格
     */
    private fun updateGestureGuard() {
        if (Build.VERSION.SDK_INT < 29 || !::grid.isInitialized || root.width == 0) return
        val gridAt = IntArray(2)
        val rootAt = IntArray(2)
        grid.getLocationInWindow(gridAt)
        root.getLocationInWindow(rootAt)
        val ox = gridAt[0] - rootAt[0]
        val oy = gridAt[1] - rootAt[1]
        val rects = mutableListOf<Rect>()
        val ed = editor
        if (ed != null) {
            rects += (ed.guardRect() ?: Rect(0, 0, grid.width, grid.height)).apply { offset(ox, oy) }
        } else {
            for (i in 0 until grid.childCount) {
                val pad = grid.getChildAt(i) as? TouchpadView ?: continue
                val band = minOf(pad.height, dp(200))
                val center = if (pad.lastDownY >= 0) pad.lastDownY.toInt() else pad.height - band / 2
                val top = (center - band / 2).coerceIn(0, maxOf(0, pad.height - band))
                rects += Rect(pad.left + ox, pad.top + top + oy, pad.right + ox, pad.top + top + band + oy)
            }
        }
        val edge = dp(48)
        rects.forEach { r ->
            if (r.left < edge) r.left = 0
            if (r.right > root.width - edge) r.right = root.width
        }
        if (rects == guardRects) return // 没变就不打扰系统（横幅动画、拖动时每帧都会排版）
        guardRects = rects
        root.systemGestureExclusionRects = rects
    }

    private var guardRects: List<Rect> = emptyList()

    private fun editHint() = when {
        saving -> "正在保存…"
        editChanged -> "调好了点「完成」"
        editor?.selectedId() != null -> "拖边角改大小"
        else -> "点一下选中模块"
    }

    /** 进入调整：当前页的每个模块显示虚线边框，按键、触控板、语音都暂停（触摸被网格拦下，不会发给电脑） */
    private fun startEditing() {
        if (editor != null) return
        val layout = config.optJSONObject("layout") ?: return
        editPageId = layout.optJSONArray("pages")?.optJSONObject(pageIndex)?.optString("id") ?: return
        // 正在说的话先收尾，按着的键松开，输入框收起键盘
        stopAllVoice()
        releaseKeys()
        currentFocus?.let { focused ->
            getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(focused.windowToken, 0)
            focused.clearFocus()
        }
        val ed = LayoutEditor(grid, layout.optDouble("snap", 0.25), layout.optDouble("minSize", 0.5), editListener)
        editor = ed
        editChanged = false
        saving = false
        grid.editor = ed
        ed.start()
        showEditBar(true)
        updateGestureGuard()
        Ui.buzz(root, Ui.Buzz.TAP)
    }

    /** 完成：没改就直接退出；改了就发给电脑，等它说存好了再退出；没连上就先存在手机上 */
    private fun saveEditing() {
        val ed = editor ?: return
        if (saving) return
        if (!ed.changed()) {
            endEditing()
            return
        }
        val modules = JSONArray()
        ed.result().forEach { (id, c) ->
            modules.put(JSONObject().put("id", id).put("x", c.x).put("y", c.y).put("w", c.w).put("h", c.h))
        }
        val msg = JSONObject().put("t", "layout").put("page", editPageId).put("modules", modules)
        if (!connected || link?.send(msg) != true) {
            pendingLayout = msg
            prefs.edit().putString(KEY_PENDING_LAYOUT, msg.toString()).apply()
            endEditing()
            say("先存在手机上了，连上电脑后自动保存")
            Ui.buzz(root, Ui.Buzz.CONFIRM)
            return
        }
        saving = true
        updateEditBar()
        showBase(animate = true)
        main.postDelayed(saveTimeout, 5000)
    }

    /** 连上了：把离线时调好的布局发给电脑 */
    private fun sendPendingLayout() {
        val msg = pendingLayout ?: return
        if (layoutInFlight) return
        if (link?.send(msg) == true) {
            layoutInFlight = true
            main.removeCallbacks(pendingLayoutTimeout)
            main.postDelayed(pendingLayoutTimeout, 6000)
        }
    }

    /** 电脑回话：存好了就退出调整（新配置已经先推过来了）；没存上就留在调整里，说明原因 */
    private fun onLayoutSaved(msg: JSONObject) {
        val ok = msg.optBoolean("ok")
        val error = msg.optString("error").ifEmpty { "电脑没有收下" }
        if (layoutInFlight && !saving) {
            layoutInFlight = false
            main.removeCallbacks(pendingLayoutTimeout)
            pendingLayout = null
            prefs.edit().remove(KEY_PENDING_LAYOUT).apply()
            if (ok) {
                flash("手机上调的布局已保存到电脑", 2200)
            } else {
                applyConfig(force = true)
                say("电脑没收下刚才在手机上调的布局：$error，已换回电脑上的布局")
            }
            return
        }
        if (!saving) return
        saving = false
        main.removeCallbacks(saveTimeout)
        if (ok) {
            endEditing()
            flash("布局已保存到电脑", 2000)
            Ui.buzz(root, Ui.Buzz.CONFIRM)
        } else {
            updateEditBar()
            showBase(animate = true)
            say("没保存：$error")
        }
    }

    private fun cancelEditing() {
        val ed = editor ?: return
        ed.revert()
        endEditing()
    }

    private fun confirmDiscardEditing() {
        overlays.dialog(
            "放弃这次调整？",
            "刚才的移动和改大小都会还原。",
            listOf(Btn("继续调整"), Btn("放弃", Style.DANGER) { cancelEditing() }),
        )
    }

    private fun endEditing() {
        val ed = editor ?: return
        main.removeCallbacks(saveTimeout)
        ed.stop()
        editor = null
        saving = false
        showEditBar(false)
        // 调整期间电脑推来的新配置（包括刚存好的这份）现在用上并回执；模块已经在位置上了，不播入场动画
        pendingConfig?.let {
            config = it
            link?.ackConfig(it)
        }
        pendingConfig = null
        applyConfig(force = true, animate = false)
        updateBanner()
        updateGestureGuard()
    }

    private fun toggleSnap() {
        val ed = editor ?: return
        ed.snap = !ed.snap
        updateEditBar()
        Ui.buzz(snapSwitch, Ui.Buzz.TAP)
        flash(if (ed.snap) "对齐到 1/4 格" else "自由：随意调大小和位置", 2000)
    }

    /** 顶栏在「平时」和「调整布局」之间切换：新出现的按钮从下面淡入 */
    private fun showEditBar(on: Boolean) {
        val appear = if (on) listOf(cancelButton, snapSwitch, doneButton) else listOf(brand, menuButton)
        val vanish = if (on) listOf(brand, menuButton, profileTag) else listOf(cancelButton, snapSwitch, doneButton)
        vanish.forEach { it.visibility = View.GONE }
        appear.forEach { v ->
            v.visibility = View.VISIBLE
            v.alpha = 0f
            v.translationY = dp(6).toFloat()
            v.animate().alpha(1f).translationY(0f).setStartDelay(0).setDuration(320).setInterpolator(Ui.SOFT_SPRING).start()
        }
        if (!on) updateProfile(config.optJSONObject("profile"))
        tabs.animate().alpha(if (on) 0.35f else 1f).setStartDelay(0).setDuration(200).start()
        updateEditBar()
        detailPinned = false
        main.removeCallbacks(clearDetail)
        showBase(animate = true)
    }

    private fun updateEditBar() {
        val snapOn = editor?.snap != false
        snapSwitch.set(snapOn)
        cancelButton.setTextColor(palette.fg)
        cancelButton.background = Ui.round(palette.tile, dp(15).toFloat(), Ui.hairline(this).toInt() + 1, palette.line)
        doneButton.setTextColor(palette.tile)
        doneButton.background = Ui.round(palette.fg, dp(15).toFloat())
        doneButton.text = if (saving) "保存中…" else "完成"
        doneButton.alpha = if (saving) 0.55f else 1f
    }

    @Deprecated("返回：先关浮层；调整布局时问要不要放弃；平时只退到后台，不关掉")
    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        when {
            overlays.back() -> {}
            editor != null -> if (editor?.changed() == true) confirmDiscardEditing() else cancelEditing()
            else -> moveTaskToBack(true)
        }
    }

    // ───────── 语音识别设置 ─────────

    /** 语音按钮用哪个识别：选过的本地模型（没选过就用装好的，480ms 的优先）；null = 系统语音引擎 */
    private fun chosenModel(): LocalAsr.Model? {
        val chosen = prefs.getString(KEY_ASR, null)
        if (chosen == ASR_SYSTEM) return null
        val models = LocalAsr.models(this)
        return models.firstOrNull { it.name == chosen } ?: models.firstOrNull { "480" in it.name } ?: models.firstOrNull()
    }

    private fun chooseAsr(name: String) {
        prefs.edit().putString(KEY_ASR, name).apply()
        applyVoiceSettings()
    }

    /** 把识别设置交给语音按钮；用本地模型就提前在后台加载好 */
    private fun applyVoiceSettings() {
        val buttons = reusable.values.filterIsInstance<VoiceButton>()
        if (buttons.isEmpty()) return
        val model = chosenModel()
        val live = prefs.getBoolean(KEY_LIVE_TYPING, true)
        buttons.forEach {
            it.localModel = model
            it.liveTyping = live
        }
        if (model != null) LocalAsr.preload(model) else LocalAsr.unload()
    }

    private fun anyVoiceBusy() = reusable.values.any { (it as? VoiceButton)?.isBusy() == true }

    private fun stopAllVoice() {
        reusable.values.forEach { (it as? VoiceButton)?.stopListening() }
    }

    /** 锁定录音时顶栏常驻「录音中 · 点此结束」 */
    private fun showRecPill(on: Boolean) {
        if (on) {
            recPill.visibility = View.VISIBLE
            recPill.alpha = 0f
            recPill.scaleX = 0.9f
            recPill.scaleY = 0.9f
            recPill.animate().alpha(1f).scaleX(1f).scaleY(1f).setDuration(220).setInterpolator(Ui.SOFT_SPRING).start()
            readout.animate().alpha(0f).setDuration(120).start()
            main.removeCallbacks(dimTask)
        } else {
            recPill.animate().alpha(0f).setDuration(160).withEndAction { recPill.visibility = View.GONE }.start()
            readout.animate().alpha(1f).setDuration(200).start()
            scheduleDim()
        }
    }

    /** 发一段文字（语音输入框、说完一句再发）：真的发出去了才返回 true */
    private fun sendText(text: String): Boolean {
        val sent = link?.send(JSONObject().put("t", "text").put("s", text)) == true
        if (sent) {
            Ui.buzz(root, Ui.Buzz.CONFIRM)
            flash("已发送 ${count(text)} 字", 1400)
        }
        return sent
    }

    /**
     * 边说边打：先退格 delete 个字，再逐字打 insert（不走剪贴板，一小段一小段地打）。
     * 中间的几段不回车；最后一段（last）回不回车按电脑端「发送后回车」的设置。返回 false = 没发出去（没连上、刚断）
     */
    private fun typeLive(delete: Int, insert: String, last: Boolean): Boolean {
        val l = link ?: return false
        if (!l.connected) return false
        repeat(delete) { if (!l.send(JSONObject().put("t", "key").put("k", "backspace"))) return false }
        if (insert.isEmpty() && !last) return true
        val msg = JSONObject().put("t", "text").put("s", insert).put("mode", "type")
        if (!last) msg.put("enter", false)
        if (!l.send(msg)) return false
        if (last) Ui.buzz(root, Ui.Buzz.CONFIRM)
        return true
    }

    // ───────── 提示 ─────────

    /** 提示：顶栏放得下就在顶栏闪 4 秒；放不下就换成顶栏下面的卡片（不截断，也不两处同时说） */
    private fun say(text: String) {
        if (editor == null && readout.fits(text)) flash(text, 4000) else overlays.notice(text)
    }

    /** 顶栏右边临时换成 text，ms 毫秒后回到连接状态；太长就改用卡片 */
    private fun flash(text: String, ms: Long) {
        if (!readout.fits(text)) {
            overlays.notice(text, ms = maxOf(ms, 2600))
            return
        }
        detailPinned = true
        readout.set(text, hot = true, animate = true)
        main.removeCallbacks(clearDetail)
        main.postDelayed(clearDetail, ms)
    }

    /** 按了键：顶栏右边闪一下按的是哪个键、怎么按的；连发、重复长按时原地数下数（删除 ×12） */
    private fun echo(label: String, trigger: String, seq: Int) {
        if (seq > 0) {
            detailPinned = true
            readout.set(if (trigger == "longPress") "长按 $label ×${seq + 1}" else "$label ×${seq + 1}", hot = true, animate = false)
            main.removeCallbacks(clearDetail)
            main.postDelayed(clearDetail, 1400)
            return
        }
        flash(
            when (trigger) {
                "doubleClick" -> "双击 $label"
                "longPress" -> "长按 $label"
                else -> label
            },
            1400,
        )
    }

    /** 当前页上的按键：停掉连发、松开直通按着的键 */
    private fun releaseKeys() {
        for (i in 0 until grid.childCount) (grid.getChildAt(i) as? KeyButton)?.cancelPress()
    }

    // ───────── 屏幕调暗 ─────────

    override fun dispatchTouchEvent(ev: MotionEvent): Boolean {
        lastTouchAt = SystemClock.uptimeMillis()
        touching = ev.actionMasked != MotionEvent.ACTION_UP && ev.actionMasked != MotionEvent.ACTION_CANCEL
        if (ev.actionMasked == MotionEvent.ACTION_DOWN) {
            if (dimmed) {
                // 调暗时的第一下只用来恢复亮度，不算按键
                setDimmed(false)
                swallowTouch = true
            }
            scheduleDim()
        }
        if (swallowTouch) {
            if (ev.actionMasked == MotionEvent.ACTION_UP || ev.actionMasked == MotionEvent.ACTION_CANCEL) swallowTouch = false
            return true
        }
        if (ev.actionMasked == MotionEvent.ACTION_DOWN) edgePad = padBeside(ev.x, ev.y)
        edgePad?.let { pad ->
            // 从屏幕边（网格外那条边距）起手、旁边就是触控板：这一下交给触控板，从边上划进来也能动鼠标
            val at = IntArray(2)
            pad.getLocationInWindow(at)
            val copy = MotionEvent.obtain(ev)
            copy.offsetLocation(-at[0].toFloat(), -at[1].toFloat())
            pad.dispatchTouchEvent(copy)
            copy.recycle()
            if (ev.actionMasked == MotionEvent.ACTION_UP || ev.actionMasked == MotionEvent.ACTION_CANCEL) edgePad = null
            return true
        }
        return super.dispatchTouchEvent(ev)
    }

    private var edgePad: TouchpadView? = null

    /** 手指按在屏幕边和触控板之间的那条边距上（窗口坐标），返回旁边的触控板 */
    private fun padBeside(x: Float, y: Float): TouchpadView? {
        if (editor != null || overlays.isOpen) return null
        val at = IntArray(2)
        for (i in 0 until grid.childCount) {
            val pad = grid.getChildAt(i) as? TouchpadView ?: continue
            pad.getLocationInWindow(at)
            if (y < at[1] || y > at[1] + pad.height) continue
            val left = at[0].toFloat()
            val right = left + pad.width
            // 触控板贴着屏幕左边（或右边），手指按在它和屏幕边之间
            val besideLeft = left <= dp(40) && x < left
            val besideRight = right >= root.width - dp(40) && x > right
            if (besideLeft || besideRight) return pad
        }
        return null
    }

    private fun scheduleDim() {
        main.removeCallbacks(dimTask)
        main.postDelayed(dimTask, dimAfterMs)
    }

    private fun setDimmed(on: Boolean) {
        if (on && (anyVoiceBusy() || editor != null || !resumed)) {
            scheduleDim()
            return
        }
        if (dimmed == on) return
        dimmed = on
        window.attributes = window.attributes.apply {
            screenBrightness = if (on) DIM_BRIGHTNESS else WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
        }
    }

    // ───────── 连接回调 ─────────

    override fun onLinkMessage(msg: JSONObject) {
        when (msg.optString("t")) {
            "hello" -> {
                checkPcVersion(msg.optString("app"))
                showBase(animate = false)
                menuSheet?.refresh()
            }
            "config" -> {
                onConfig(msg)
                offerClipboard() // 刚连上（配置里才知道开没开同步）：在前台的话把手机上新复制的发过去
            }
            "layoutSaved" -> onLayoutSaved(msg)
            // 按键里的「切换方案」切了方案：屏幕中间浮出新方案名
            "profile" -> spotlightProfile(profileTitle(msg.optString("to")))
            // 电脑上复制了图片：蓝牙上要传一会儿，传完才能粘贴
            "clipImg" -> if (ClipSync.enabled(link?.lastConfig)) {
                flash("正在接收电脑上复制的图片（${(msg.optLong("size") + 1023) / 1024} KB）…", 15_000)
            }
            "clipImgCancel" -> if (ClipSync.enabled(link?.lastConfig)) flash("电脑上的剪贴板又变了，图片不传了", 2000)
            "clipImgEnd" -> if (msg.has("ok")) {
                flash(if (msg.optBoolean("ok")) "电脑上复制的图片已放进手机剪贴板" else "图片没收全，没放进剪贴板", 2500)
            }
            // 正在发文件到电脑（见 FileSender）：顶上显示进度，一秒最多换一次
            "fileProgress" -> {
                val now = SystemClock.uptimeMillis()
                if (now - lastFileFlash >= 1000 || msg.optLong("sent") == msg.optLong("size")) {
                    lastFileFlash = now
                    val size = msg.optLong("size")
                    val count = msg.optInt("count")
                    val pct = if (size > 0) "${msg.optLong("sent") * 100 / size}%" else FileSender.human(msg.optLong("sent"))
                    flash("正在发到电脑" + (if (count > 1) " ${msg.optInt("index") + 1}/$count" else "") + " · $pct", 3000)
                }
            }
            "filesDone" -> {
                lastFileFlash = 0L
                say(msg.optString("text"))
                if (msg.has("slowKBps")) offerFaster(msg.optInt("slowKBps"), msg.optBoolean("slowFg"))
            }
            "mem" -> {
                memText = "工作集 %.1f MB · 私有 %.1f MB".format(msg.optDouble("ws") / 1_048_576, msg.optDouble("private") / 1_048_576)
                menuSheet?.refresh()
            }
        }
    }

    /** 电脑推来配置：套用完回执；调整布局时先存着，调完再套用、回执 */
    private fun onConfig(msg: JSONObject) {
        if (editor != null) {
            pendingConfig = msg
            return
        }
        config = msg
        applyConfig()
        link?.ackConfig(msg)
    }

    /** 电脑端太旧（不认识手机上暂停、同步回执）：每个旧版本只提示一次（之后在 ⋯ →「关于」里能看到） */
    private fun checkPcVersion(version: String?) {
        if (version.isNullOrBlank() || versionWarned) return
        if (!older(version, MIN_PC_VERSION)) return
        versionWarned = true
        if (prefs.getString(KEY_WARNED_PC_VERSION, null) == version) return
        prefs.edit().putString(KEY_WARNED_PC_VERSION, version).apply()
        main.postDelayed({ say("电脑端版本较旧（$version），更新到 $MIN_PC_VERSION 以后才能在手机上暂停按键") }, 2500)
    }

    private fun older(version: String, than: String): Boolean {
        val a = version.split('.').map { it.takeWhile(Char::isDigit).toIntOrNull() ?: 0 }
        val b = than.split('.').map { it.takeWhile(Char::isDigit).toIntOrNull() ?: 0 }
        for (i in 0 until maxOf(a.size, b.size)) {
            val x = a.getOrElse(i) { 0 }
            val y = b.getOrElse(i) { 0 }
            if (x != y) return x < y
        }
        return false
    }

    // ───────── 触控板回调 ─────────

    override fun move(dx: Double, dy: Double, t: Double) {
        link?.move(dx, dy, t)
    }

    override fun scroll(dx: Int, dy: Int) {
        link?.scroll(dx, dy)
    }

    override fun click(button: String) {
        link?.sendInOrder(JSONObject().put("t", "mouse").put("b", button).put("a", "click"))
    }

    override fun press(button: String, down: Boolean) {
        link?.sendInOrder(JSONObject().put("t", "mouse").put("b", button).put("a", if (down) "down" else "up"))
    }

    override fun touchEnded() {
        main.post { updateGestureGuard() }
    }

    override fun offlineTouch() {
        val now = SystemClock.uptimeMillis()
        if (now - lastOfflineTouch < 2500) return
        lastOfflineTouch = now
        flash("未连接电脑 · 触控板没有发送", 2000)
    }

    // ───────── 小部件 ─────────

    /** 横幅左边的状态点：没连上是空心圈（正在连时一闪一闪），暂停是两道竖线 */
    private inner class StatusDot(context: Context) : View(context) {
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        private var kind = DOT_OFFLINE
        private var busy = false

        fun set(kind: Int, busy: Boolean) {
            this.kind = kind
            this.busy = busy
            invalidate()
        }

        override fun onDraw(canvas: Canvas) {
            val s = width.toFloat()
            val c = s / 2
            val pulse = if (busy) 0.45f + 0.55f * (0.5f + 0.5f * sin(SystemClock.uptimeMillis() / 160f)) else 1f
            if (kind == DOT_PAUSED) {
                paint.style = Paint.Style.FILL
                paint.color = palette.fg
                val bw = s * 0.22f
                canvas.drawRoundRect(c - bw * 1.6f, s * 0.12f, c - bw * 0.6f, s * 0.88f, bw / 2, bw / 2, paint)
                canvas.drawRoundRect(c + bw * 0.6f, s * 0.12f, c + bw * 1.6f, s * 0.88f, bw / 2, bw / 2, paint)
            } else {
                paint.style = Paint.Style.STROKE
                paint.strokeWidth = s * 0.17f
                paint.color = palette.fg
                paint.alpha = (pulse * 255).toInt()
                canvas.drawCircle(c, c, c - paint.strokeWidth, paint)
            }
            if (busy) postInvalidateOnAnimation()
        }
    }

    /** 调整布局时的「对齐 | 自由」：两段式开关，当前的那段是黑底白字，一眼看得出 */
    private inner class SnapSwitch(context: Context) : View(context) {
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            typeface = Ui.font(context, 850)
            textSize = Ui.sp(context, 13f)
            textAlign = Paint.Align.CENTER
        }
        private var t = 0f
        private var anim: ValueAnimator? = null
        private var on = true

        init {
            isClickable = true
            contentDescription = "对齐开关"
        }

        fun set(snap: Boolean) {
            if (snap == on && t == (if (snap) 0f else 1f)) return
            on = snap
            contentDescription = if (snap) "现在对齐到格子，点一下改成自由" else "现在自由摆放，点一下改成对齐"
            anim?.cancel()
            anim = ValueAnimator.ofFloat(t, if (snap) 0f else 1f).apply {
                duration = 220
                interpolator = Ui.EASE
                addUpdateListener {
                    t = it.animatedValue as Float
                    invalidate()
                }
                start()
            }
        }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            setMeasuredDimension(dp(104), dp(30))
        }

        override fun onDraw(canvas: Canvas) {
            val w = width.toFloat()
            val h = height.toFloat()
            paint.color = palette.tile
            canvas.drawRoundRect(0f, 0f, w, h, h / 2, h / 2, paint)
            paint.style = Paint.Style.STROKE
            paint.strokeWidth = Ui.hairline(context) + 1
            paint.color = palette.line
            canvas.drawRoundRect(paint.strokeWidth / 2, paint.strokeWidth / 2, w - paint.strokeWidth / 2, h - paint.strokeWidth / 2, h / 2, h / 2, paint)
            paint.style = Paint.Style.FILL
            val half = w / 2
            val inset = dp(3).toFloat()
            val left = inset + (half - inset) * t
            paint.color = palette.fg
            canvas.drawRoundRect(left, inset, left + half - inset, h - inset, (h - 2 * inset) / 2, (h - 2 * inset) / 2, paint)
            val base = h / 2 - (text.ascent() + text.descent()) / 2
            text.color = Ui.blend(palette.tile, palette.fg, t)
            canvas.drawText("对齐", half / 2 + inset / 2, base, text)
            text.color = Ui.blend(palette.fg, palette.tile, t)
            canvas.drawText("自由", half + half / 2 - inset / 2, base, text)
        }
    }

    companion object {
        private const val KEY_PAGE = "page_index"
        /** 语音识别用哪个：本地模型的目录名，或 ASR_SYSTEM */
        private const val KEY_ASR = "asr_engine"
        private const val ASR_SYSTEM = "system"
        private const val KEY_LIVE_TYPING = "live_typing"
        /** 没发出去的语音文字 */
        private const val KEY_PENDING_TEXT = "pending_text"
        /** 没连上时调好、还没存到电脑上的布局 */
        private const val KEY_PENDING_LAYOUT = "pending_layout"
        /** 选电脑时记下的电脑名 */
        private const val KEY_PC_NAME = "pc_name"
        private const val KEY_GUIDE_SEEN = "gesture_guide_seen"
        private const val KEY_BT_ASKED = "bt_permission_asked"
        private const val KEY_MIC_ASKED = "mic_permission_asked"
        /** 问过要不要把省电策略改成「无限制」（只主动问一次，菜单里一直能改） */
        private const val KEY_BATTERY_ASKED = "battery_asked"
        /** 上次提示「发文件慢，怎么快起来」的时间 */
        private const val KEY_SLOW_HINT_AT = "slow_hint_at"
        /** 已经提示过「电脑端版本较旧」的那个版本 */
        private const val KEY_WARNED_PC_VERSION = "warned_pc_version"
        private const val DOT_OFFLINE = 0
        private const val DOT_PAUSED = 1
        private const val REQ_BLUETOOTH = 1
        private const val REQ_MIC = 2
        private const val REQ_NOTIFY = 3
        private const val REQ_CAMERA = 4
        private const val KEY_CAMERA_ASKED = "camera_asked"
        private const val REQ_PICK_QR = 5
        private const val REQ_PICK_FILES = 6
        private const val REQ_ENABLE_BT = 7
        private const val REQ_SCAN = 8
        private const val NOT_OUR_QR = "这不是蒜鸟的二维码：在电脑蒜鸟「设置 → 扫码连接」里点「显示二维码」"
        /** 没连上超过这么久才出横幅、顶栏才说「未连接电脑」 */
        private const val OFFLINE_GRACE_MS = 1500L
        /** 闲置这么久把屏幕调暗 */
        private const val DIM_AFTER_MS = 2 * 60_000L
        private const val DIM_BRIGHTNESS = 0.02f
        /** 手离开屏幕这么久以后，才自己弹更新面板 */
        private const val UPDATE_QUIET_MS = 2500L
        /** 这个版本起电脑端认识手机上暂停（setEnabled）和配置回执（configAck） */
        private const val MIN_PC_VERSION = "0.6.0"

        /** 还没连过电脑时先用这个：上面语音框，下面触控板。连上后以电脑下发的为准。 */
        private const val DEFAULT_CONFIG = """{"layout":{"columns":4,"rows":8,"pages":[{"id":"main","name":"主页","modules":[
            {"id":"voice","kind":"voice","label":"语音输入框","x":0,"y":0,"w":4,"h":2},
            {"id":"touchpad","kind":"touchpad","label":"触控板","x":0,"y":2,"w":4,"h":6}]}]}}"""
    }
}
