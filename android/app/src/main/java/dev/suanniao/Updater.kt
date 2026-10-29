package dev.suanniao

import android.annotation.SuppressLint
import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.database.Cursor
import android.database.MatrixCursor
import android.graphics.Canvas
import android.graphics.Paint
import android.net.ConnectivityManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.SystemClock
import android.provider.OpenableColumns
import android.provider.Settings
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.LinearLayout
import android.widget.TextView
import dev.suanniao.Overlays.Btn
import dev.suanniao.Overlays.Style
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileNotFoundException
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.MalformedURLException
import java.net.SocketTimeoutException
import java.net.URL
import java.security.MessageDigest
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import kotlin.concurrent.thread
import kotlin.math.roundToInt
import kotlin.math.roundToLong

/**
 * 检查更新：看 GitHub 上最新那一版的 latest.json，有新版本就问要不要更新；要就下载、核对，交给系统安装。
 *
 * - 整个 App 只有这里联网，也只连 GitHub（github.com 和放文件的 *.githubusercontent.com）。语音识别照样在手机上做，不联网
 * - 打开 App 时自己看一眼（离上次超过 6 小时；⋯ 菜单里能关），⋯ 菜单里「检查更新」随时看
 * - 自己查到的新版本：没开着别的面板、教程，没在调整布局、没在说话，手也离开屏幕一会儿了才弹。
 *   「以后再说」一天内不再弹，「跳过这个版本」这一版不再弹（菜单里查照样显示）
 * - 安装包带着语音模型，一百五十多 MB：下到 App 自己的缓存目录，断了接着下（HTTP Range）；
 *   下完核对大小和 SHA-256，再看是不是蒜鸟、是不是更新的版本、签名和装着的对不对得上
 * - 自己编译的调试版和正式版签名不一样，覆盖装不上：调试版不给下载，说清楚情况，给「去网页下载」
 */
class Updater(private val host: Host, private val overlays: Overlays) {

    /** MainActivity 提供的能力 */
    interface Host {
        val context: Context
        /**
         * 现在能弹更新面板吗：在前台，没开着别的面板、教程，没在调整布局、没在说话。
         * manual = false（自己弹的）还要等第一次打开的教程走完、手离开屏幕一会儿
         */
        fun canShowUpdate(manual: Boolean): Boolean
        /** 在 ⋯ 菜单里点了「检查更新」、要弹更新面板了：先把菜单关掉 */
        fun closeMenuForUpdate()
        /** 提示一句（顶栏或卡片） */
        fun updateNotice(text: String)
        /** 状态变了：⋯ 菜单里「检查更新」那行跟着变 */
        fun onUpdateChanged()
    }

    /** latest.json 里手机端用得到的部分 */
    class Info(
        /** 原文：存起来，下次打开还认得 */
        val raw: String,
        val version: String,
        val code: Int,
        val url: String,
        val size: Long,
        val sha256: String,
        /** 交给安装器时显示的文件名 */
        val file: String,
        /** 这一版改了什么，一条一句 */
        val notes: List<String>,
        /** 这一版的发布页（「去网页下载」打开它） */
        val page: String,
    ) {
        /** 比装着的这个新 */
        val newer get() = code > BuildConfig.VERSION_CODE
    }

    /** 正在下的一次（取消了就作废，再下是新的一次） */
    private class Download(val info: Info) {
        @Volatile var cancelled = false
        @Volatile var conn: HttpURLConnection? = null
        /** 已经下了多少字节 */
        @Volatile var done = 0L
        /** 下完了，正在核对 */
        @Volatile var verifying = false
    }

    /** 下好、核对过、等着装的安装包 */
    private class Ready(val info: Info, val file: File)

    /** 说给用户听的失败原因 */
    private class Failure(override val message: String) : Exception(message)
    /** 签名和装着的对不上：覆盖装不上 */
    private class Conflict : Exception()
    private class Cancelled : Exception()

    /** 等着给用户看的是什么 */
    private enum class Why {
        /** 自己查到了新版本：管「跳过」「以后再说」 */
        FOUND,
        /** 收起面板后在后台下完了（或出错了）：告诉一声 */
        DONE,
        /** 用户自己要看（菜单里点的） */
        ASKED,
    }

    private val context get() = host.context
    /** 后台线程只用这个，不拿着界面 */
    private val app: Context = host.context.applicationContext
    private val prefs = app.getSharedPreferences(LinkService.PREFS, Context.MODE_PRIVATE)
    private val main = Handler(Looper.getMainLooper())
    private val palette = overlays.palette
    private val density = app.resources.displayMetrics.density
    private fun dp(v: Float) = (v * density).toInt()
    private fun dpf(v: Float) = v * density

    /** 最近一次查到的版本信息（后来没查成也留着） */
    private var info: Info? = null
    /** 最近一次没查成的原因（接在「检查失败：」后面）；查成了是 null */
    private var checkError: String? = null
    private var checkedAt = 0L
    private var checking = false
    /** 换了测试地址时作废还没回来的那次 */
    private var checkGen = 0
    /** 用户自己点的检查还没回来：回来了要给个说法 */
    private var manualWaiting = false

    /** 等着给用户看的（null = 没有）；现在弹不了就过一会儿再看 */
    private var pending: Why? = null
    private val offerTask = Runnable { tryOffer() }
    private val autoTask = Runnable { if (autoCheck && !detached) check(manual = false) }

    private var sheet: Overlays.Sheet? = null
    /** 面板是自己弹出来的、用户还没选：直接关掉就当「以后再说」 */
    private var undecided = false
    private var bar: Bar? = null
    private var barText: TextView? = null
    /** 菜单那行的下载进度 5% 一档刷新，不用每次都重建菜单 */
    private var menuStep = -1

    private var resumed = false
    private var detached = false
    /** 要先允许「安装未知应用」 */
    private var needPermission = false
    /** 带去设置了：回来时看看允许了没有 */
    private var sentToSettings = false
    private var stillNoPermission = false
    /** 交给系统安装器了：回到蒜鸟时还是这个版本 = 没装上（装上了的话这个进程已经被系统换掉了） */
    private var installerOpened = false
    private var installerReturned = false

    /** 调试版测试用的 latest.json 地址（见 debugOverride） */
    private var debugUrl: String? = null
    private var debugInstall = false
    /** 调试版不给下载：签名和正式版不一样，覆盖装不上 */
    private val debugBlocked get() = BuildConfig.DEBUG && !debugInstall

    init {
        attached = this
        load()
        // 上次交给安装器的那一版现在装上了：说一声
        val installing = prefs.getInt(KEY_UPDATE_INSTALLING, 0)
        if (installing in 1..BuildConfig.VERSION_CODE) {
            prefs.edit().remove(KEY_UPDATE_INSTALLING).apply()
            main.postDelayed({ if (!detached) host.updateNotice("已更新到 ${BuildConfig.VERSION_NAME}") }, 1500)
        }
        // 带去「安装未知应用」设置时记下的：有的系统允许以后会把界面重建、甚至把进程关掉重开，回来照样接着装（onResume）
        prefs.getString(KEY_UPDATE_RESUME, null)?.let { saved ->
            prefs.edit().remove(KEY_UPDATE_RESUME).apply()
            val at = saved.substringAfter(' ', "").toLongOrNull() ?: 0L
            val fresh = System.currentTimeMillis() - at in 0..RESUME_MS
            if (fresh && saved.substringBefore(' ') == info?.takeIf { it.newer }?.sha256) sentToSettings = true
        }
        cleanup()
    }

    // ───────── MainActivity 调的 ─────────

    /** ⋯ 菜单里「自动检查更新」（默认开） */
    var autoCheck: Boolean
        get() = prefs.getBoolean(KEY_UPDATE_AUTO, true)
        set(on) {
            prefs.edit().putBoolean(KEY_UPDATE_AUTO, on).apply()
            if (!on) main.removeCallbacks(autoTask)
        }

    /** 回到前台（onStart）：开着自动检查、离上次够久了，过几秒在后台看一眼 */
    fun onForeground() {
        main.removeCallbacks(autoTask)
        if (!autoCheck) return
        val now = System.currentTimeMillis()
        val wait = if (checkError != null) RETRY_FAILED_MS else CHECK_EVERY_MS
        if (now - checkedAt >= wait || now < checkedAt) main.postDelayed(autoTask, AUTO_DELAY_MS)
    }

    /** onResume：从设置回来看看允许安装了没有，从安装器回来说一声没装上；有等着弹的接着等 */
    fun onResume() {
        resumed = true
        if (sentToSettings) {
            sentToSettings = false
            prefs.edit().remove(KEY_UPDATE_RESUME).apply()
            if (app.packageManager.canRequestPackageInstalls()) {
                needPermission = false
                if (ready != null) {
                    install()
                } else {
                    // 进程被关过：下好的安装包还在缓存里，start 先重新核对一遍（不用再下），核对完面板开着就接着装
                    start()
                    if (sheet?.closed != false) offer(Why.ASKED)
                }
            } else {
                stillNoPermission = true
                needPermission = true
                if (sheet?.closed == false) sheet?.refresh() else offer(Why.ASKED)
            }
        }
        if (installerOpened) {
            installerOpened = false
            installerReturned = true
            sheet?.refresh()
        }
        if (pending != null) {
            main.removeCallbacks(offerTask)
            main.postDelayed(offerTask, 800)
        }
    }

    fun onPause() {
        resumed = false
    }

    /** 退到后台（onStop）：先不等着弹、不自动查（下载照样在后台下） */
    fun onBackground() {
        main.removeCallbacks(autoTask)
        main.removeCallbacks(offerTask)
    }

    /** 界面没了（onDestroy）：还在下的交给下一个界面接着显示 */
    fun detach() {
        detached = true
        onBackground()
        if (attached === this) attached = null
    }

    /** ⋯ 菜单里「检查更新」那行的小字 */
    fun menuLine(): String {
        val d = download
        val r = ready?.takeIf { it.info.newer }
        val t = info?.takeIf { it.newer }
        return when {
            checking -> "正在检查…"
            d != null -> if (d.verifying) "正在核对 ${d.info.version}…" else "正在下载 ${d.info.version} · ${percent(d)}%"
            r != null -> "${r.info.version} 已经下好，点这里安装"
            t != null && conflict?.sha256 == t.sha256 -> "${t.version} 没法直接更新，点这里看看"
            t != null && failure != null -> "${t.version} 没下好，点这里重试"
            t != null -> "有新版本 ${t.version}"
            checkError != null -> "检查失败：$checkError"
            checkedAt > 0 -> "已是最新 · ${whenText(checkedAt)} 检查过"
            else -> "看看有没有新版本"
        }
    }

    /** 点了「检查更新」：正在下、下好了、没下好、签名对不上就打开面板，不然马上查一次 */
    fun menuTapped() {
        if (download != null || ready?.info?.newer == true || failure != null || conflict != null) offer(Why.ASKED) else check(manual = true)
    }

    /**
     * 调试版测试用（MainActivity 的 debugUpdateUrl）：latest.json 换成这个地址（null = 换回 GitHub），马上查一次。
     * 换了地址以后查到的结果、「跳过」「以后再说」另存一份（键名前加 debug_），不动真的。
     * auto = 当成打开 App 时的自动检查；install = 调试版也让下载、安装（测整条路，装的得是同一台电脑打的调试版）
     */
    fun debugOverride(url: String?, auto: Boolean, install: Boolean) {
        if (!BuildConfig.DEBUG) return
        debugUrl = url
        debugInstall = install && url != null
        checking = false
        checkGen++
        load()
        refresh()
        check(manual = !auto)
    }

    // ───────── 检查 ─────────

    /** 查一次 latest.json：后台线程去查，结果回到主线程 */
    fun check(manual: Boolean) {
        if (manual) manualWaiting = true
        if (checking) return
        checking = true
        val gen = ++checkGen
        main.removeCallbacks(autoTask)
        host.onUpdateChanged()
        val url = debugUrl ?: MANIFEST_URL
        val test = debugUrl != null
        thread(name = "update-check", isDaemon = true) {
            val result = runCatching { fetchManifest(url, test) }
            val why = result.exceptionOrNull()?.let(::checkReason)
            main.post { if (gen == checkGen) checked(result.getOrNull(), why) }
        }
    }

    private fun checked(found: Info?, why: String?) {
        checking = false
        val manual = manualWaiting
        manualWaiting = false
        checkedAt = System.currentTimeMillis()
        checkError = why
        if (found != null) {
            val changed = found.sha256 != info?.sha256
            info = found
            // 换了一版：之前下好的、出过错的都不算了，旧安装包删掉
            if (changed && download == null) {
                ready = null
                failure = null
                conflict = null
                cleanup()
            }
        }
        save()
        if (detached) return
        refresh()
        val t = info?.takeIf { it.newer }
        when {
            t != null && manual -> offer(Why.ASKED)
            t != null -> if (why == null && autoAllowed(t)) offer(Why.FOUND)
            why != null -> if (manual) overlays.notice("检查失败：$why", action = "去网页下载") { openPage() }
            manual -> host.updateNotice("已是最新版本（${BuildConfig.VERSION_NAME}）")
        }
    }

    /** 自己查到的新版本能不能弹：没跳过这一版，也不在「以后再说」的一天里 */
    private fun autoAllowed(t: Info): Boolean {
        if (t.code <= prefs.getInt(k(KEY_UPDATE_SKIPPED), 0)) return false
        val later = prefs.getLong(k(KEY_UPDATE_LATER_AT), 0L)
        val now = System.currentTimeMillis()
        return now < later || now - later >= LATER_MS
    }

    private fun later() {
        undecided = false
        prefs.edit().putLong(k(KEY_UPDATE_LATER_AT), System.currentTimeMillis()).apply()
    }

    private fun skip(t: Info) {
        undecided = false
        prefs.edit().putInt(k(KEY_UPDATE_SKIPPED), t.code).apply()
        host.updateNotice("跳过了 ${t.version}，菜单里还能找到")
    }

    // ───────── 什么时候弹 ─────────

    /** 有东西要给用户看：现在能弹就弹，不能就过一会儿再看（用户自己要看的排最前） */
    private fun offer(why: Why) {
        val now = pending
        if (now == null || why.ordinal > now.ordinal) pending = why
        if (why == Why.ASKED) host.closeMenuForUpdate()
        main.removeCallbacks(offerTask)
        main.postDelayed(offerTask, if (why == Why.ASKED) 250L else 0L)
    }

    private fun tryOffer() {
        val why = pending ?: return
        if (detached) return
        val open = sheet?.takeIf { !it.closed }
        if (open != null) {
            pending = null
            open.refresh()
            return
        }
        val t = info?.takeIf { it.newer }
        val worth = when (why) {
            Why.FOUND -> t != null && download == null && autoAllowed(t)
            Why.DONE -> ready != null || failure != null || conflict != null
            Why.ASKED -> t != null || download != null || ready != null
        }
        if (!worth) {
            pending = null
            return
        }
        // 开着别的面板、在调布局、在说话、手在屏幕上：过一会儿再看（退到后台就停，回来接着等）
        if (!host.canShowUpdate(manual = why == Why.ASKED)) {
            main.postDelayed(offerTask, OFFER_RETRY_MS)
            return
        }
        pending = null
        showSheet(auto = why != Why.ASKED)
    }

    // ───────── 更新面板 ─────────

    /** 打开更新面板（开着就原地刷新）；auto = 自己弹出来的 */
    private fun showSheet(auto: Boolean) {
        sheet?.takeIf { !it.closed }?.let {
            it.refresh()
            return
        }
        installerReturned = false
        stillNoPermission = false
        undecided = auto
        val s = overlays.sheet { buildSheet(this) }
        s.onDismiss = {
            if (sheet === s) {
                sheet = null
                bar = null
                barText = null
            }
            if (undecided) later()
            needPermission = false
            sentToSettings = false
        }
        sheet = s
    }

    private fun refresh() {
        sheet?.refresh()
        host.onUpdateChanged()
    }

    /** 按现在的情况显示：有新版本 / 调试版 / 正在下 / 签名不对 / 出错了 / 要允许安装 / 下好了 */
    private fun buildSheet(s: Overlays.Sheet) {
        bar = null
        barText = null
        with(s) {
            val d = download
            val r = ready?.takeIf { it.info.newer }
            val t = d?.info ?: r?.info ?: info?.takeIf { it.newer }
            if (t == null) {
                header("已是最新版本", "现在是 ${BuildConfig.VERSION_NAME}")
                buttons(Btn("好的", Style.PRIMARY) { s.dismiss() })
                return
            }
            val from = BuildConfig.VERSION_NAME + if (BuildConfig.DEBUG) "（调试版）" else ""
            val line = "$from → ${t.version} · 安装包 ${mb(t.size)}"
            when {
                debugBlocked -> {
                    header("发现新版本 ${t.version}", line)
                    notes(t)
                    paragraph(DEBUG_TEXT)
                    buttons(Btn("去网页下载", Style.PRIMARY) {
                        undecided = false
                        openPage()
                    })
                    moreButtons(Btn("以后再说") { later(); s.dismiss() }, Btn("跳过这个版本") { skip(t); s.dismiss() })
                }
                d != null -> {
                    if (d.verifying) header("正在核对 ${t.version}", "看看安装包完不完整、是不是蒜鸟")
                    else header("正在下载 ${t.version}", "可以先收起，下好了会再告诉你")
                    progress(d)
                    buttons(Btn("取消下载") { cancel() }, Btn("收起") { s.dismiss() })
                    webLink()
                }
                conflict?.sha256 == t.sha256 -> {
                    header("没法直接更新", "$from → ${t.version}")
                    paragraph(CONFLICT_TEXT)
                    buttons(Btn("去网页下载", Style.PRIMARY) { openPage() })
                    moreButtons(Btn("关闭") { s.dismiss() })
                }
                failure != null -> {
                    header("新版本没下好", "$from → ${t.version}")
                    paragraph(failure.orEmpty())
                    buttons(Btn("重试", Style.PRIMARY) { start() })
                    moreButtons(Btn("关闭") { s.dismiss() })
                    webLink()
                }
                needPermission -> {
                    header("先允许蒜鸟安装应用", "${t.version} 已经下好")
                    paragraph("系统要你先同意一次，蒜鸟才能把新版本交给系统安装。点「去允许」，在打开的页面里把蒜鸟的开关打开，再回到这里。")
                    if (stillNoPermission) paragraph("还没打开：在那个页面里把「蒜鸟」的开关打开，再回来。", strong = true)
                    buttons(Btn("去允许", Style.PRIMARY) { openInstallSettings() })
                    moreButtons(Btn("取消") {
                        needPermission = false
                        stillNoPermission = false
                        refresh()
                    })
                    webLink()
                }
                r != null -> {
                    header("${t.version} 已经下好", "按系统的提示装上就行，设置都会留着")
                    if (installerReturned) paragraph("没装上？再点一次「安装」；还不行就去网页下载。", strong = true)
                    buttons(Btn("安装", Style.PRIMARY) {
                        undecided = false
                        install()
                    })
                    moreButtons(Btn("以后再说") { later(); s.dismiss() })
                    webLink()
                }
                else -> {
                    // 下了一半的 .part，或者整个下完了的 .apk（进程被关过、下好的那份还没核对）
                    val have = maxOf(File(dir(app), "${t.sha256}.part").length(), File(dir(app), "${t.sha256}.apk").length())
                    header("发现新版本 ${t.version}", line + if (have > 0) "（已下 ${mb(have)}）" else "")
                    notes(t)
                    if (metered()) paragraph("正在用流量：安装包 ${mb(t.size)}，最好连上 Wi-Fi 再下。", strong = true)
                    buttons(Btn("立即更新", Style.PRIMARY) {
                        undecided = false
                        start()
                    })
                    moreButtons(Btn("以后再说") { later(); s.dismiss() }, Btn("跳过这个版本") { skip(t); s.dismiss() })
                    webLink()
                }
            }
        }
    }

    /** 这一版改了什么：最多列 5 条，多的去网页看 */
    private fun Overlays.Sheet.notes(t: Info) {
        if (t.notes.isEmpty()) return
        val box = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            background = Ui.round(palette.bg, dpf(16f))
            setPadding(dp(16f), dp(12f), dp(16f), dp(12f))
        }
        t.notes.take(MAX_NOTES).forEachIndexed { i, note ->
            box.addView(LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                if (i > 0) setPadding(0, dp(6f), 0, 0)
                addView(overlays.text("·", 13.5f, 900, palette.fg), LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply { marginEnd = dp(8f) })
                addView(overlays.text(note, 13.5f, 650, palette.fg).apply {
                    maxLines = 3
                    ellipsize = TextUtils.TruncateAt.END
                    setLineSpacing(dpf(2f), 1f)
                }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            })
        }
        val more = t.notes.size - MAX_NOTES
        if (more > 0) box.addView(overlays.text("还有 $more 条，去网页看全部", 12.5f, 650, palette.sub).apply { setPadding(0, dp(8f), 0, 0) })
        view(box, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { setMargins(dp(20f), dp(2f), dp(20f), dp(6f)) })
    }

    /** 一段说明；strong = 要用户注意的（黑字） */
    private fun Overlays.Sheet.paragraph(text: String, strong: Boolean = false) {
        view(overlays.text(text, 14f, if (strong) 750 else 600, if (strong) palette.fg else palette.sub).apply {
            setLineSpacing(dpf(3f), 1f)
            setPadding(dp(24f), dp(4f), dp(24f), dp(6f))
        })
    }

    /** 下载进度：一根进度条，下面是百分比和 MB */
    private fun Overlays.Sheet.progress(d: Download) {
        val b = Bar(context, palette).apply { fraction = d.done.toFloat() / d.info.size }
        view(b, LinearLayout.LayoutParams(MATCH_PARENT, dp(8f)).apply { setMargins(dp(24f), dp(6f), dp(24f), 0) })
        val label = overlays.text(progressText(d), 13f, 750, palette.sub).apply { setPadding(dp(24f), dp(8f), dp(24f), dp(2f)) }
        view(label)
        bar = b
        barText = label
    }

    /** 第二排按钮（比第一排靠得近一点） */
    private fun Overlays.Sheet.moreButtons(vararg list: Btn) {
        view(overlays.buttonRow(list.toList()), LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { setMargins(dp(20f), dp(10f), dp(20f), 0) })
    }

    /** 「去网页下载」：哪一步不行，都能去 GitHub 的发布页自己下 */
    private fun Overlays.Sheet.webLink() {
        view(overlays.text("去网页下载", 13.5f, 800, palette.sub).apply {
            gravity = Gravity.CENTER
            setPadding(dp(18f), dp(12f), dp(18f), dp(6f))
            Ui.springy(this)
            setOnClickListener {
                undecided = false
                openPage()
            }
        }, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply {
            gravity = Gravity.CENTER_HORIZONTAL
            topMargin = dp(4f)
        })
    }

    private fun progressText(d: Download) =
        if (d.verifying) "下好了，正在核对…" else "${percent(d)}% · ${mb1(d.done)} / ${mb1(d.info.size)} MB"

    /** 下载线程报的进度：面板上的进度条直接改，菜单那行 5% 一档 */
    private fun progressed(d: Download) {
        if (detached || download !== d) return
        bar?.fraction = d.done.toFloat() / d.info.size
        barText?.text = progressText(d)
        val step = percent(d) / 5
        if (step != menuStep) {
            menuStep = step
            host.onUpdateChanged()
        }
    }

    private fun openPage() {
        val page = (download?.info ?: ready?.info ?: info)?.page ?: RELEASES_PAGE
        runCatching { context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(page)).addCategory(Intent.CATEGORY_BROWSABLE)) }
            .onFailure { host.updateNotice("没找到浏览器。网址：$page") }
    }

    // ───────── 下载 ─────────

    /** 「立即更新」「重试」：下好了就直接装，不然开始下载（下完核对好了、面板还开着，就接着装） */
    private fun start() {
        if (debugBlocked) return
        val t = info?.takeIf { it.newer } ?: return
        failure = null
        conflict = null
        if (download != null) return refresh()
        val r = ready
        if (r != null && r.info.sha256 == t.sha256 && r.file.isFile) return install()
        ready = null
        val d = Download(t)
        download = d
        menuStep = -1
        val dir = dir(app)
        val test = debugUrl != null
        thread(name = "update-download", isDaemon = true) {
            val result = runCatching { fetchApk(d, dir, test) }
            val e = result.exceptionOrNull()
            val why = if (e == null || e is Cancelled || e is Conflict || d.cancelled) null else downloadReason(e)
            main.post { finished(d, result.getOrNull(), e is Conflict, why) }
        }
        refresh()
    }

    /** 取消下载：下了的部分留着，下次接着下 */
    private fun cancel() {
        val d = download ?: return
        d.cancelled = true
        download = null
        // 断开连接要碰网络，不能在主线程做；断开后下载线程马上读不到数据、退出
        thread(name = "update-cancel", isDaemon = true) { runCatching { d.conn?.disconnect() } }
        refresh()
        host.updateNotice("已取消下载")
    }

    /** 下载线程结束（finished 转过来的）：面板开着、人在看就接着装，不然找机会告诉一声 */
    private fun downloadDone(d: Download, ok: Boolean) {
        if (detached) return
        refresh()
        if (d.cancelled) return
        if (ok && resumed && sheet?.closed == false) install() else offer(Why.DONE)
    }

    /**
     * 下载线程：下到 dir/<sha256>.part（断了下次接着下），下完改名成 .apk 再核对；返回核对过的安装包。
     * 文件名带 SHA-256，所以只会接着下同一个安装包；别的版本留下的一律删掉。
     */
    private fun fetchApk(d: Download, dir: File, test: Boolean): File = synchronized(fileLock) {
        if (d.cancelled) throw Cancelled()
        val t = d.info
        dir.mkdirs()
        val apk = File(dir, "${t.sha256}.apk")
        val part = File(dir, "${t.sha256}.part")
        dir.listFiles()?.forEach { if (it != apk && it != part) it.delete() }
        if (!apk.isFile || apk.length() != t.size) {
            apk.delete()
            if (part.length() > t.size) part.delete()
            val need = t.size - part.length() + SPARE_BYTES
            if (dir.usableSpace < need) throw Failure("手机存储空间不够：还要大约 ${mb(need)}，清理一下再试。")
            if (part.length() < t.size) pull(d, part, test)
            if (!part.renameTo(apk)) throw IOException("改名失败")
        }
        d.verifying = true
        main.post { attached?.refresh() }
        verify(d, apk)
        apk
    }

    /** 从 part 现有的长度接着下；服务器不认续传（回 200）就从头下 */
    private fun pull(d: Download, part: File, test: Boolean) {
        val t = d.info
        var have = part.length()
        d.done = have
        var c = open(t.url, test, from = have, gzip = false)
        try {
            if (c.responseCode == 416 && have > 0) {
                // 服务器说从这里接不上：从头下
                c.disconnect()
                part.delete()
                have = 0
                c = open(t.url, test, from = 0, gzip = false)
            }
            d.conn = c
            val append = when (val code = c.responseCode) {
                HttpURLConnection.HTTP_PARTIAL -> {
                    if (have == 0L || rangeStart(c) != have) {
                        part.delete()
                        throw IOException("续传的位置对不上")
                    }
                    true
                }
                HttpURLConnection.HTTP_OK -> false
                HttpURLConnection.HTTP_NOT_FOUND -> throw Failure("GitHub 上找不到这个安装包（404），去网页看看。")
                else -> throw Failure("下载失败：GitHub 返回 $code，过一会儿再试。")
            }
            if (!append) have = 0
            d.done = have
            val length = c.contentLengthLong
            if (length >= 0 && have + length != t.size) throw Failure("安装包和更新信息里的大小对不上，过一会儿再试。")
            if (d.cancelled) throw Cancelled()
            FileOutputStream(part, append).use { out ->
                c.inputStream.use { input ->
                    val buf = ByteArray(64 * 1024)
                    var shownAt = 0L
                    while (true) {
                        val n = input.read(buf)
                        if (d.cancelled) throw Cancelled()
                        if (n < 0) break
                        if (have + n > t.size) throw Failure("安装包比更新信息里说的大，没装。")
                        out.write(buf, 0, n)
                        have += n
                        d.done = have
                        val now = SystemClock.uptimeMillis()
                        if (now - shownAt >= 150) {
                            shownAt = now
                            main.post { attached?.progressed(d) }
                        }
                    }
                }
            }
            main.post { attached?.progressed(d) }
            if (have != t.size) throw IOException("没下完")
        } finally {
            d.conn = null
            c.disconnect()
        }
    }

    /** 核对：大小、SHA-256，再看是不是蒜鸟、是不是更新的版本、签名和装着的对不对得上（对不上系统会拒绝覆盖安装） */
    private fun verify(d: Download, apk: File) {
        val t = d.info
        if (apk.length() != t.size) bad(apk, "下载的安装包大小不对，已删掉，点「重试」重新下。")
        val digest = MessageDigest.getInstance("SHA-256")
        apk.inputStream().use { input ->
            val buf = ByteArray(256 * 1024)
            while (true) {
                if (d.cancelled) throw Cancelled()
                val n = input.read(buf)
                if (n < 0) break
                digest.update(buf, 0, n)
            }
        }
        if (hex(digest.digest()) != t.sha256) bad(apk, "安装包校验没通过（多半是下载时出了错），已删掉，点「重试」重新下。")
        val pm = app.packageManager
        val theirs = archiveInfo(pm, apk.path) ?: bad(apk, "安装包打不开，已删掉，点「重试」重新下。")
        if (theirs.packageName != app.packageName) bad(apk, "下载的不是蒜鸟的安装包，没装。")
        if (theirs.longVersionCode <= BuildConfig.VERSION_CODE) bad(apk, "下载的安装包不比现在的新，没装。")
        if (!sameSigner(installedInfo(pm), theirs)) {
            apk.delete()
            throw Conflict()
        }
    }

    private fun bad(apk: File, why: String): Nothing {
        apk.delete()
        throw Failure(why)
    }

    @Suppress("DEPRECATION")
    private fun archiveInfo(pm: PackageManager, path: String): PackageInfo? = runCatching {
        if (Build.VERSION.SDK_INT >= 33) {
            pm.getPackageArchiveInfo(path, PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong()))
        } else {
            pm.getPackageArchiveInfo(path, PackageManager.GET_SIGNING_CERTIFICATES)
        }
    }.getOrNull()

    @Suppress("DEPRECATION")
    private fun installedInfo(pm: PackageManager): PackageInfo? = runCatching {
        if (Build.VERSION.SDK_INT >= 33) {
            pm.getPackageInfo(app.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong()))
        } else {
            pm.getPackageInfo(app.packageName, PackageManager.GET_SIGNING_CERTIFICATES)
        }
    }.getOrNull()

    /**
     * 签名对得上：装着的这个现在用的签名，新安装包里都有（新包换过密钥的话，旧的在它的签名历史里）。
     * 哪边读不到签名就不拦，交给系统安装器判断
     */
    private fun sameSigner(mine: PackageInfo?, theirs: PackageInfo): Boolean {
        val current = mine?.signingInfo?.apkContentsSigners ?: return true
        val signing = theirs.signingInfo ?: return true
        val known = signing.apkContentsSigners.orEmpty().toSet() + signing.signingCertificateHistory.orEmpty()
        return current.all { it in known }
    }

    // ───────── 安装 ─────────

    /**
     * 交给系统安装：ACTION_VIEW + ApkProvider 给的 content:// 地址，不用 PackageInstaller 会话。
     * 小米（MIUI / HyperOS）改过系统的安装流程，第三方 App 用 PackageInstaller 会话安装有已知的毛病
     * （常要关掉开发者选项里的「MIUI 优化」才正常，有的应用商店因此在小米手机上专门改回这种方式）；
     * ACTION_VIEW 走的是小米自己的安装界面，和在浏览器、文件管理里点安装包一样，最稳。
     * 代价是拿不到安装结果：签名对不上这种一定装不上的，下完在 verify 里先查掉、说清楚；
     * 别的失败（取消了、空间不够）安装界面自己会说，回到蒜鸟时还是这个版本，面板上提示「没装上？」。
     */
    private fun install() {
        val r = ready ?: return
        if (!r.file.isFile) {
            ready = null
            failure = "安装包不见了（可能清理过缓存），点「重试」重新下。"
            return refresh()
        }
        if (!app.packageManager.canRequestPackageInstalls()) {
            needPermission = true
            return refresh()
        }
        needPermission = false
        stillNoPermission = false
        installerReturned = false
        val uri = Uri.Builder().scheme("content").authority(app.packageName + AUTHORITY_SUFFIX)
            .appendPath(r.info.sha256).appendPath(r.info.file).build()
        val intent = Intent(Intent.ACTION_VIEW).setDataAndType(uri, APK_MIME).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        runCatching { context.startActivity(intent) }
            .onSuccess {
                installerOpened = true
                prefs.edit().putInt(KEY_UPDATE_INSTALLING, r.info.code).apply()
            }
            .onFailure { failure = "打不开系统的安装界面。" }
        refresh()
    }

    /** 带去「安装未知应用」里蒜鸟那一项；回来时（onResume）允许了就接着装 */
    private fun openInstallSettings() {
        val opened = runCatching {
            context.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${app.packageName}")))
        }.isSuccess || runCatching { context.startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)) }.isSuccess
        if (!opened) return host.updateNotice("打不开设置。请到手机「设置」里找到蒜鸟，允许它安装应用")
        sentToSettings = true
        // 也记到存储里：回来时界面重建了、进程重开了，新的 Updater 也知道要接着装（init 里读）
        (ready?.info ?: info)?.let { prefs.edit().putString(KEY_UPDATE_RESUME, "${it.sha256} ${System.currentTimeMillis()}").apply() }
    }

    /** 删掉用不上的安装包：装好了的、换了版本的。正在下的、下好了等着装的不动 */
    private fun cleanup() {
        if (download != null) return
        val keep = (ready?.info ?: info)?.takeIf { it.newer }?.sha256
        val dir = dir(app)
        thread(name = "update-cleanup", isDaemon = true) {
            synchronized(fileLock) { dir.listFiles()?.forEach { if (keep == null || !it.name.startsWith(keep)) it.delete() } }
        }
    }

    // ───────── 联网 ─────────

    /** 读 latest.json（最多 256 KB） */
    private fun fetchManifest(url: String, test: Boolean): Info {
        val c = open(url, test, from = 0, gzip = true)
        try {
            when (val code = c.responseCode) {
                HttpURLConnection.HTTP_OK -> {}
                HttpURLConnection.HTTP_NOT_FOUND -> throw Failure("GitHub 上还没有更新信息")
                else -> throw Failure("GitHub 返回 $code")
            }
            val bytes = c.inputStream.use { readAtMost(it, MAX_MANIFEST) } ?: throw Failure("更新信息太大，读不懂")
            return parse(String(bytes, Charsets.UTF_8))
        } finally {
            c.disconnect()
        }
    }

    /**
     * 发一个 GET。跳转自己跟（最多几次），每一跳都核对：只走 https、只去 GitHub
     * （github.com 会先跳到那一版的发布，再跳到 release-assets.githubusercontent.com 这类放文件的地址）。
     * 调试版的测试地址不限（调试版的网络设置另外只放行本机 127.0.0.1 的 http）
     */
    private fun open(url: String, test: Boolean, from: Long, gzip: Boolean): HttpURLConnection {
        var u = URL(url)
        repeat(MAX_HOPS) {
            if (!allowed(u, test)) throw Failure("地址不在 GitHub 上（${u.host}），没连")
            val c = u.openConnection() as HttpURLConnection
            c.instanceFollowRedirects = false
            c.connectTimeout = 15_000
            c.readTimeout = 30_000
            c.useCaches = false
            c.setRequestProperty("User-Agent", "SuanNIAO-Android/${BuildConfig.VERSION_NAME}")
            // 安装包不让压缩：要靠 Content-Length 核对大小、用 Range 接着下
            if (!gzip) c.setRequestProperty("Accept-Encoding", "identity")
            if (from > 0) c.setRequestProperty("Range", "bytes=$from-")
            val code = c.responseCode
            if (code !in REDIRECTS) return c
            val next = c.getHeaderField("Location")
            c.disconnect()
            if (next.isNullOrBlank()) throw Failure("GitHub 返回 $code")
            u = URL(u, next)
        }
        throw Failure("跳转太多次")
    }

    private fun allowed(u: URL, test: Boolean): Boolean {
        if (test) return u.protocol == "https" || u.protocol == "http"
        val host = u.host.lowercase()
        return u.protocol == "https" && (host == "github.com" || host.endsWith(".githubusercontent.com"))
    }

    private fun readAtMost(input: InputStream, limit: Int): ByteArray? {
        val out = ByteArrayOutputStream()
        val buf = ByteArray(8192)
        while (true) {
            val n = input.read(buf)
            if (n < 0) return out.toByteArray()
            if (out.size() + n > limit) return null
            out.write(buf, 0, n)
        }
    }

    /** 续传时服务器回的 Content-Range「bytes 起点-终点/总长」里的起点 */
    private fun rangeStart(c: HttpURLConnection): Long =
        c.getHeaderField("Content-Range")?.let { RANGE.find(it)?.groupValues?.get(1)?.toLongOrNull() } ?: -1L

    /** 检查失败的原因（接在「检查失败：」后面） */
    private fun checkReason(e: Throwable): String = when {
        e is Failure -> e.message
        e is MalformedURLException -> "更新信息里的地址不对"
        offline() -> "没联网"
        e is SocketTimeoutException -> "连 GitHub 超时"
        else -> "连不上 GitHub"
    }

    private fun downloadReason(e: Throwable): String = when {
        e is Failure -> e.message
        e is MalformedURLException -> "更新信息里的下载地址不对，去网页下载吧。"
        offline() -> "没联网，没下完。已下的部分留着，联网后点「重试」接着下。"
        else -> "连不上 GitHub，没下完。已下的部分留着，点「重试」接着下。"
    }

    private fun offline() = runCatching { app.getSystemService(ConnectivityManager::class.java)?.activeNetwork == null }.getOrDefault(false)

    /** 正在用按流量计费的网络 */
    private fun metered() = runCatching { app.getSystemService(ConnectivityManager::class.java)?.isActiveNetworkMetered == true }.getOrDefault(false)

    // ───────── 存取 ─────────

    /** 调试版换了测试地址期间，查到的结果、「跳过」「以后再说」另存一份（键名前加 debug_），不动真的 */
    private fun k(key: String) = if (debugUrl != null) "debug_$key" else key

    private fun load() {
        val saved = prefs.getString(k(KEY_UPDATE_RESULT), null)?.let { runCatching { JSONObject(it) }.getOrNull() }
        info = (saved?.opt("manifest") as? String)?.let { runCatching { parse(it) }.getOrNull() }
        checkError = (saved?.opt("error") as? String)?.takeIf { it.isNotEmpty() }
        checkedAt = prefs.getLong(k(KEY_UPDATE_CHECKED_AT), 0L)
    }

    private fun save() {
        val o = JSONObject()
        info?.let { o.put("manifest", it.raw) }
        checkError?.let { o.put("error", it) }
        prefs.edit().putLong(k(KEY_UPDATE_CHECKED_AT), checkedAt).putString(k(KEY_UPDATE_RESULT), o.toString()).apply()
    }

    // ───────── 小零件 ─────────

    private fun percent(d: Download) = (d.done * 100 / d.info.size).toInt().coerceIn(0, 100)

    private fun mb(bytes: Long) = "${maxOf(1, (bytes / MB).roundToInt())} MB"

    /** 保留一位小数的 MB（不带单位） */
    private fun mb1(bytes: Long): String {
        val tenths = (bytes * 10 / MB).roundToLong()
        return "${tenths / 10}.${tenths % 10}"
    }

    /** 「今天 14:03」「昨天 09:12」「10月28日 20:40」 */
    private fun whenText(at: Long): String {
        val zone = ZoneId.systemDefault()
        val t = Instant.ofEpochMilli(at).atZone(zone)
        val hm = t.hour.toString().padStart(2, '0') + ":" + t.minute.toString().padStart(2, '0')
        val today = LocalDate.now(zone)
        return when (t.toLocalDate()) {
            today -> "今天 $hm"
            today.minusDays(1) -> "昨天 $hm"
            else -> "${t.monthValue}月${t.dayOfMonth}日 $hm"
        }
    }

    private fun hex(bytes: ByteArray) = bytes.joinToString("") { (it.toInt() and 0xFF).toString(16).padStart(2, '0') }

    /** 下载进度条：黑白圆角，和开关一个样子 */
    @SuppressLint("ViewConstructor")
    private class Bar(context: Context, private val palette: Ui.Palette) : View(context) {
        private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        var fraction = 0f
            set(v) {
                field = v.coerceIn(0f, 1f)
                invalidate()
            }

        override fun onDraw(canvas: Canvas) {
            val w = width.toFloat()
            val h = height.toFloat()
            paint.color = palette.line
            canvas.drawRoundRect(0f, 0f, w, h, h / 2, h / 2, paint)
            if (fraction > 0f) {
                paint.color = palette.fg
                canvas.drawRoundRect(0f, 0f, maxOf(h, w * fraction), h, h / 2, h / 2, paint)
            }
        }
    }

    companion object {
        /** 打开 App 时自动检查更新（默认开） */
        private const val KEY_UPDATE_AUTO = "update_auto_check"
        /** 上次检查更新的时间（毫秒；查成、没查成都记） */
        private const val KEY_UPDATE_CHECKED_AT = "update_checked_at"
        /** 上次检查的结果：{"manifest": 最近查到的 latest.json 原文（后来没查成也留着）, "error": 没查成的原因} */
        private const val KEY_UPDATE_RESULT = "update_result"
        /** 「跳过这个版本」跳过的 versionCode：这一版（和更旧的）不再自己弹 */
        private const val KEY_UPDATE_SKIPPED = "update_skipped_code"
        /** 点「以后再说」的时间：之后一天内不再自己弹 */
        private const val KEY_UPDATE_LATER_AT = "update_later_at"
        /** 交给系统安装器的那一版的 versionCode：新版第一次打开时说一声「已更新」 */
        private const val KEY_UPDATE_INSTALLING = "update_installing_code"
        /** 带去「安装未知应用」设置时记下「<sha256> <毫秒>」：回来时界面、进程重开过也接着装（半小时内有效） */
        private const val KEY_UPDATE_RESUME = "update_resume_install"
        private const val RESUME_MS = 30 * 60_000L

        /** 最新一版的更新信息：GitHub 先跳到那一版的发布，再跳到放文件的地址 */
        private const val MANIFEST_URL = "https://github.com/drhhhao/SuanNIAO/releases/latest/download/latest.json"
        /** 更新信息里没写发布页（或写的不是 GitHub）时，「去网页下载」打开这里 */
        private const val RELEASES_PAGE = "https://github.com/drhhhao/SuanNIAO/releases/latest"
        /** 认得的 latest.json 格式，比它新的读不懂 */
        private const val SCHEMA = 1
        private const val CHECK_EVERY_MS = 6 * 3600_000L
        /** 上次没查成：一小时后再试 */
        private const val RETRY_FAILED_MS = 3600_000L
        /** 「以后再说」：一天内不再自己弹 */
        private const val LATER_MS = 24 * 3600_000L
        /** 打开 App 几秒后再查，不和连电脑、加载语音模型抢 */
        private const val AUTO_DELAY_MS = 3000L
        /** 等着弹的时候隔多久再看一眼 */
        private const val OFFER_RETRY_MS = 2500L
        private const val MAX_HOPS = 6
        private val REDIRECTS = setOf(301, 302, 303, 307, 308)
        private const val MAX_MANIFEST = 256 * 1024
        /** 下载前除了安装包本身，还要留出这么多空间 */
        private const val SPARE_BYTES = 64L * 1024 * 1024
        private const val MB = 1024.0 * 1024.0
        private const val MAX_NOTES = 5
        private const val AUTHORITY_SUFFIX = ".update"
        const val APK_MIME = "application/vnd.android.package-archive"
        val HEX64 = Regex("[0-9a-f]{64}")
        private val FILE_NAME = Regex("[A-Za-z0-9._-]{1,80}\\.apk")
        private val RANGE = Regex("""bytes\s+(\d+)-""")

        private const val DEBUG_TEXT = "这是自己编译的调试版，签名和正式版不一样，不能直接更新。要换成正式版得先卸载这个，手机上的设置会丢。"
        private const val CONFLICT_TEXT =
            "手机上这个蒜鸟和新版本的签名不一样（多半是自己编译的调试版），不能直接更新。要换成正式版得先卸载这个，手机上的设置会丢。"

        // 下载、核对、交给安装器这一路的状态，整个进程只有一份：界面重建了（比如切了深色模式）也接得上
        private var download: Download? = null
        private var ready: Ready? = null
        /** 下载、核对没成的原因 */
        private var failure: String? = null
        /** 签名对不上的那一版 */
        private var conflict: Info? = null
        /** 现在的界面（最新建的那个 Updater） */
        private var attached: Updater? = null
        /** 同一时间只有一个线程动安装包文件（取消后马上重下时，等上一个线程退出） */
        private val fileLock = Any()

        /** 安装包放在这里（App 自己的缓存目录） */
        fun dir(context: Context) = File(context.cacheDir, "update")

        /** 读 latest.json：schema 不认识、缺必需的字段，都当没有可用的更新信息（抛 Failure，不崩） */
        private fun parse(text: String): Info {
            val j = runCatching { JSONObject(text) }.getOrNull() ?: throw Failure("更新信息读不懂")
            val schema = j.optInt("schema", 0)
            if (schema > SCHEMA) throw Failure("更新信息是新格式，去网页看看")
            if (schema < 1) throw Failure("更新信息读不懂")
            val a = j.optJSONObject("android") ?: throw Failure("更新信息里没有手机端")
            val version = a.str("version")
            val code = a.optLong("code", 0L)
            val url = a.str("url")
            val size = a.optLong("size", 0L)
            val sha = a.str("sha256").lowercase()
            if (version.isEmpty() || code < 1 || code > Int.MAX_VALUE || url.isEmpty() || size <= 0 || !HEX64.matches(sha)) {
                throw Failure("更新信息不完整")
            }
            // 更新说明：CHANGELOG.md 里分开写了的只要手机端那几条（android.notes），没分开写的是整段（notes）
            val notes = (a.optJSONArray("notes")?.takeIf { it.length() > 0 } ?: j.optJSONArray("notes"))?.let { arr ->
                (0 until arr.length()).mapNotNull { i -> (arr.opt(i) as? String)?.trim()?.takeIf { it.isNotEmpty() } }
            }.orEmpty()
            val page = j.str("page").takeIf { it.startsWith("https://github.com/") } ?: RELEASES_PAGE
            val file = a.str("file").takeIf { FILE_NAME.matches(it) } ?: "SuanNIAO-Android-$version.apk"
            return Info(text, version, code.toInt(), url, size, sha, file, notes, page)
        }

        /** 字符串字段（没有、是 null、不是字符串都算空） */
        private fun JSONObject.str(key: String) = (opt(key) as? String)?.trim().orEmpty()

        /** 下载线程的结果回到主线程：记下来，告诉现在的界面 */
        private fun finished(d: Download, file: File?, isConflict: Boolean, why: String?) {
            if (download === d) download = null
            when {
                file != null -> ready = Ready(d.info, file)
                isConflict -> conflict = d.info
                why != null -> failure = why
            }
            attached?.downloadDone(d, file != null)
        }
    }
}

/**
 * 把下好的新版本安装包交给系统安装器读：只读，只认 App 缓存里 update/ 下核对过的 <sha256>.apk。
 * 不对外开放（exported=false），安装器靠蒜鸟给的临时读权限（FLAG_GRANT_READ_URI_PERMISSION）来读。
 * 这个工程没用 AndroidX，没有 FileProvider，所以自己写这个最小的。
 */
class ApkProvider : ContentProvider() {
    override fun onCreate() = true

    /** content://dev.suanniao.update/<sha256>/<文件名> → 缓存里的 update/<sha256>.apk */
    private fun fileOf(uri: Uri): File? {
        val ctx = context ?: return null
        val seg = uri.pathSegments
        if (seg.size != 2 || !Updater.HEX64.matches(seg[0])) return null
        return File(Updater.dir(ctx), "${seg[0]}.apk").takeIf { it.isFile }
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode.any { it in "wat+" }) throw FileNotFoundException("只能读")
        val file = fileOf(uri) ?: throw FileNotFoundException(uri.toString())
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun getType(uri: Uri) = Updater.APK_MIME

    /** 安装器可能会问文件名和大小 */
    override fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
        val file = fileOf(uri) ?: return null
        val cols = projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
        val row = cols.map { col ->
            when (col) {
                OpenableColumns.DISPLAY_NAME -> uri.lastPathSegment
                OpenableColumns.SIZE -> file.length()
                else -> null
            }
        }
        return MatrixCursor(cols, 1).apply { addRow(row) }
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?) = 0
}
