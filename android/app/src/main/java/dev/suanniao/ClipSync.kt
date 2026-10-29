package dev.suanniao

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.SharedPreferences
import android.net.Uri
import android.util.Base64
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream

/**
 * 剪贴板同步（电脑端设置里「和手机同步剪贴板」开着时，配置里 clipSync = true；旧版电脑端没有这项，就不同步）：
 * - 电脑 → 手机：电脑上复制了文字，发来 clip，放进手机剪贴板（标签 [LABEL_FROM_PC]）。
 *   复制的是图片时分块发来（clipImg、clipImgPart…、clipImgEnd），存进缓存的 clip 文件夹，收齐了把
 *   content://dev.suanniao.clip/文件名 放进剪贴板（[ClipProvider] 给粘贴的 App 读）。
 * - 手机 → 电脑：安卓只让前台、拿着焦点的 App 读剪贴板，所以蒜鸟拿到焦点（切回蒜鸟、连上电脑）时
 *   看剪贴板变了没有，变了就把文字发给电脑。在手机上复制、切回蒜鸟、按粘贴键，电脑上粘的就是它。
 * - 先只看剪贴板的描述（不读内容，系统不弹「已读取剪贴板」）：电脑发来的、已经发过的（按时间戳）、
 *   标了敏感（密码）的、不是文字的都不读不发。
 */
object ClipSync {
    /** 电脑发来的文字放进剪贴板时用的标签：认出来就不再发回电脑 */
    private const val LABEL_FROM_PC = "蒜鸟·电脑剪贴板"
    /** 上一次发给电脑的剪贴板的时间戳（重开 App 也不重发） */
    private const val KEY_SENT_STAMP = "clip_sent_stamp"
    /** 和电脑端 clipsync.rs 的 MAX_BYTES 一致 */
    private const val MAX_BYTES = 256 * 1024
    /** 安卓 13 的「内容敏感」标记（ClipDescription.EXTRA_IS_SENSITIVE），低版本没有这个常量 */
    private const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"

    fun enabled(config: JSONObject?) = config?.optBoolean("clipSync", false) == true

    // ───────── 电脑发来的图片 ─────────

    /** 正在收的图片 */
    private class Incoming(val id: Long, val mime: String, val size: Long, val file: File, val out: FileOutputStream) {
        var written = 0L
    }

    private var incoming: Incoming? = null

    fun imageDir(context: Context) = File(context.cacheDir, "clip")

    fun mimeOf(name: String) = if (name.endsWith(".png")) "image/png" else "image/jpeg"

    /** clipImg：开始收一张（上一张没收完的丢掉） */
    fun imageStart(context: Context, msg: JSONObject) {
        dropIncoming()
        val mime = if (msg.optString("mime") == "image/png") "image/png" else "image/jpeg"
        val size = msg.optLong("size")
        if (size <= 0 || size > 16L * 1024 * 1024) return
        val dir = imageDir(context).apply { mkdirs() }
        val file = File(dir, ".incoming")
        incoming = runCatching { Incoming(msg.optLong("id"), mime, size, file, FileOutputStream(file)) }
            .onFailure { Log.w(LinkService.TAG, "clip image open failed: ${it.message}") }.getOrNull()
    }

    fun imagePart(msg: JSONObject) {
        val inc = incoming?.takeIf { it.id == msg.optLong("id") } ?: return
        val ok = runCatching {
            val bytes = Base64.decode(msg.optString("d"), Base64.DEFAULT)
            inc.out.write(bytes)
            inc.written += bytes.size
            inc.written <= inc.size
        }.getOrDefault(false)
        if (!ok) dropIncoming()
    }

    fun imageCancel(msg: JSONObject) {
        if (incoming?.id == msg.optLong("id")) dropIncoming()
    }

    /** clipImgEnd：收齐了就放进剪贴板，返回放进去没有；没在收这一张（同步关着、已经丢掉了）返回 null */
    fun imageEnd(context: Context, msg: JSONObject): Boolean? {
        val inc = incoming?.takeIf { it.id == msg.optLong("id") } ?: return null
        incoming = null
        runCatching { inc.out.close() }
        if (inc.written != inc.size) {
            inc.file.delete()
            return false
        }
        val name = "电脑剪贴板-${System.currentTimeMillis()}." + if (inc.mime == "image/png") "png" else "jpg"
        val file = File(inc.file.parentFile, name)
        if (!inc.file.renameTo(file)) return false
        // 留最新的两张（刚粘贴的 App 可能还在读上一张），更早的删掉
        inc.file.parentFile?.listFiles { f -> !f.name.startsWith(".") }
            ?.sortedByDescending { it.lastModified() }?.drop(2)?.forEach { it.delete() }
        val uri = Uri.parse("content://${context.packageName}.clip/" + Uri.encode(name))
        val cm = context.getSystemService(ClipboardManager::class.java) ?: return false
        return runCatching {
            cm.setPrimaryClip(ClipData(ClipDescription(LABEL_FROM_PC, arrayOf(inc.mime)), ClipData.Item(uri)))
        }.onFailure { Log.w(LinkService.TAG, "clip image write failed: ${it.message}") }.isSuccess
    }

    private fun dropIncoming() {
        incoming?.let {
            runCatching { it.out.close() }
            it.file.delete()
        }
        incoming = null
    }

    /** 电脑发来的文字放进手机剪贴板（服务里调，蒜鸟不在前台也能写） */
    fun receive(context: Context, text: String) {
        if (text.isEmpty()) return
        val cm = context.getSystemService(ClipboardManager::class.java) ?: return
        runCatching { cm.setPrimaryClip(ClipData.newPlainText(LABEL_FROM_PC, text)) }
            .onFailure { Log.w(LinkService.TAG, "clip write failed: ${it.message}") }
    }

    /**
     * 蒜鸟拿着焦点时调：剪贴板里有新的文字就发给电脑。返回发出去的字数，没发返回 0。
     * send 没发出去（没连上）就不记时间戳，下次连上再发
     */
    fun offer(context: Context, prefs: SharedPreferences, send: (JSONObject) -> Boolean): Int {
        val cm = context.getSystemService(ClipboardManager::class.java) ?: return 0
        val desc = runCatching { cm.primaryClipDescription }.getOrNull() ?: return 0
        if (desc.label?.toString() == LABEL_FROM_PC) return 0
        val stamp = desc.timestamp
        if (stamp == prefs.getLong(KEY_SENT_STAMP, -1)) return 0
        val sensitive = desc.extras?.getBoolean(EXTRA_IS_SENSITIVE, false) == true
        if (sensitive || !desc.hasMimeType("text/*")) {
            prefs.edit().putLong(KEY_SENT_STAMP, stamp).apply()
            return 0
        }
        val text = runCatching { cm.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.text?.toString() }.getOrNull()
        if (text.isNullOrBlank() || text.toByteArray(Charsets.UTF_8).size > MAX_BYTES) {
            prefs.edit().putLong(KEY_SENT_STAMP, stamp).apply()
            return 0
        }
        if (!send(JSONObject().put("t", "clip").put("s", text))) return 0
        prefs.edit().putLong(KEY_SENT_STAMP, stamp).apply()
        return text.codePointCount(0, text.length)
    }
}
