package dev.suanniao

import android.app.ActivityManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.OpenableColumns
import android.util.Base64
import android.util.Log
import org.json.JSONObject
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.concurrent.LinkedBlockingQueue
import kotlin.concurrent.thread
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * 把手机上的文件发到电脑（相册、微信里「分享 → 蒜鸟」，或者菜单里「发文件到电脑」）。电脑存进「下载\蒜鸟」，
 * 一张图片放进电脑剪贴板（直接粘进对话框），别的放成文件（见电脑端 filerecv.rs，消息格式也写在那里）。
 * - 每个文件先发 fileStart，电脑回 fileAck（got = 0）才开始发 filePart；发出去的最多比电脑确认写好的（got）领先一个窗口
 *   （局域网 256 KB，每秒约 4 MB，约 60 ms 就发完；蓝牙 12 KB）：连接里不会堆一大截文件数据，触控板、按键的消息随时插得进去。
 *   还会让着触控板：刚动过触控板、按过键就先停一停。
 * - 走局域网发原文件。只连着蓝牙（每秒约 55 KB）时：超过 1 MB 的照片压成长边 2048 的 JPEG；别的文件超过 3 MB 不发，
 *   提示连上局域网再发。
 * - 一个文件从头到尾走开始时那条连接；中途断了这个文件算失败（电脑删掉没收完的），接着发下一个。
 * - 文件在分享界面还开着的时候就打开（拿到读的权限），交给这里慢慢读。
 */
class FileSender(private val service: LinkService) {

    /** 要发的一个文件：已经打开的流（分享来的权限只在分享界面开着时有效，所以先打开） */
    class Item(val name: String, val size: Long, val mime: String?, val input: InputStream)

    /** 打开分享来的、选来的文件；打不开的跳过（返回的列表里没有它） */
    companion object {
        private const val TAG = "SuanNIAO-file"
        private const val LAN_CHUNK = 64 * 1024
        private const val BT_CHUNK = 4 * 1024
        private const val LAN_WINDOW = 4 * LAN_CHUNK
        private const val BT_WINDOW = 3 * BT_CHUNK
        /** 只连着蓝牙时：照片超过这么大就压缩，别的文件超过这么大不发 */
        private const val BT_SHRINK_IMAGE = 1L shl 20
        private const val BT_MAX = 3L shl 20
        private const val SHRINK_SIDE = 2048
        /** 刚有触控板、按键的消息：这么久之内先不发文件 */
        private const val INPUT_YIELD_MS = 400L
        /** 局域网上发了这么多才算速度；每秒不到这么多 KB 算慢 */
        private const val MEASURE_MIN = 1L shl 20
        private const val SLOW_KBPS = 1024

        /** 蒜鸟的界面正开在前台 */
        private fun inForeground(): Boolean = ActivityManager.RunningAppProcessInfo()
            .also { ActivityManager.getMyMemoryState(it) }.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND

        fun open(context: Context, uris: List<Uri>): List<Item> = uris.mapNotNull { uri ->
            runCatching {
                val resolver = context.contentResolver
                var name: String? = null
                var size = -1L
                resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
                    if (c.moveToFirst()) {
                        name = c.getString(0)
                        if (!c.isNull(1)) size = c.getLong(1)
                    }
                }
                val input = resolver.openInputStream(uri) ?: return@runCatching null
                Item(name?.takeIf { it.isNotBlank() } ?: uri.lastPathSegment ?: "文件", size, resolver.getType(uri), input)
            }.onFailure { Log.w(TAG, "open $uri failed: ${it.message}") }.getOrNull()
        }

        fun human(bytes: Long): String = when {
            bytes < 1024 -> "$bytes 字节"
            bytes < 1 shl 20 -> "${(bytes / 1024.0).roundToInt()} KB"
            bytes < 1 shl 30 -> "%.1f MB".format(bytes / 1048576.0)
            else -> "%.2f GB".format(bytes / 1073741824.0)
        }
    }

    /** 进度、结果交给界面和通知（主线程上调） */
    interface Listener {
        /** 正在发第 index 个（从 0 起）、共 count 个；sent / size 字节（size 为 -1 = 不知道） */
        fun onFileProgress(index: Int, count: Int, name: String, sent: Long, size: Long)
        /** 一批发完：发成了几个、失败的原因（没有失败就是空的）；局域网上发得明显比平常慢就带上 slow */
        fun onFilesDone(sent: List<String>, failures: List<String>, slow: Slow?)
    }

    /** 局域网上这一批平均每秒只有 kbps KB；foreground：发的时候大多数时间蒜鸟开在前台 */
    class Slow(val kbps: Int, val foreground: Boolean)

    private val queue = LinkedBlockingQueue<List<Item>>()
    private val lock = Object()
    /** 正在发的文件：电脑确认写好了多少、出错原因、收完的结果 */
    private var currentId = 0L
    private var acked = -1L
    private var ackError: String? = null
    private var done: JSONObject? = null
    @Volatile private var cancelBatch = false
    @Volatile var busy = false
        private set
    /** 这一批在局域网上发的（只算 1 MB 以上的文件，小文件主要是来回等）：字节、用时（不算给触控板让路的时间）、块数、其中蒜鸟在前台的块数 */
    private var lanBytes = 0L
    private var lanMs = 0L
    private var lanParts = 0
    private var lanForegroundParts = 0

    init {
        thread(name = "file-sender", isDaemon = true) { loop() }
    }

    fun enqueue(items: List<Item>) {
        if (items.isNotEmpty()) queue.add(items)
    }

    /** 分享来的是一段文字：放进电脑剪贴板（和剪贴板同步一样的 clip 消息；电脑上关了剪贴板同步就不放） */
    fun sendText(text: String) {
        thread(name = "file-sender-text", isDaemon = true) {
            val link = waitForLink()
            val ok = link != null && link.write(JSONObject().put("t", "clip").put("s", text))
            val synced = ClipSync.enabled(service.lastConfig)
            service.post {
                service.fileListener.onFilesDone(
                    if (ok && synced) listOf("这段文字（已放进电脑剪贴板）") else emptyList(),
                    when {
                        !ok -> listOf("这段文字：没连上电脑")
                        !synced -> listOf("这段文字：电脑上关了「和手机同步剪贴板」")
                        else -> emptyList()
                    },
                    null,
                )
            }
        }
    }

    /** 不发了：正在发的这批停下（电脑删掉没收完的） */
    fun cancel() {
        cancelBatch = true
        synchronized(lock) { lock.notifyAll() }
    }

    /** 读线程收到 fileAck / fileDone */
    fun onReply(msg: JSONObject) = synchronized(lock) {
        if (msg.optLong("id") != currentId) return
        when (msg.optString("t")) {
            "fileAck" -> if (msg.has("error")) ackError = msg.optString("error") else acked = max(acked, msg.optLong("got"))
            "fileDone" -> done = msg
        }
        lock.notifyAll()
    }

    private fun loop() {
        while (true) {
            val batch = queue.take()
            busy = true
            cancelBatch = false
            lanBytes = 0L
            lanMs = 0L
            lanParts = 0
            lanForegroundParts = 0
            val batchId = System.nanoTime() and Long.MAX_VALUE
            val sent = ArrayList<String>()
            val failures = ArrayList<String>()
            batch.forEachIndexed { index, item ->
                if (cancelBatch) {
                    failures += "${item.name}：取消了"
                } else {
                    val error = runCatching { sendOne(item, batchId, index, batch.size) }
                        .getOrElse { e -> Log.w(TAG, "send failed", e); "出错了（${e.message ?: e.javaClass.simpleName}）" }
                    if (error == null) sent += item.name else failures += "${item.name}：$error"
                }
                runCatching { item.input.close() }
            }
            busy = false
            // 局域网平常每秒 3～4 MB；不到 1 MB 算慢（发了 1 MB 以上才算得准）
            val kbps = if (lanBytes >= MEASURE_MIN && lanMs > 0) (lanBytes * 1000 / lanMs / 1024).toInt() else -1
            val slow = if (kbps in 0 until SLOW_KBPS) Slow(kbps, lanForegroundParts * 2 > lanParts) else null
            service.post { service.fileListener.onFilesDone(sent, failures, slow) }
        }
    }

    /** 发一个文件，成功返回 null，失败返回原因 */
    private fun sendOne(original: Item, batchId: Long, index: Int, count: Int): String? {
        val link = waitForLink() ?: return "没连上电脑"
        if (!link.files) return "电脑端版本太旧，先更新电脑端"
        var item = original
        if (!link.lan) {
            val image = item.mime?.startsWith("image/") == true && item.mime != "image/gif"
            if (image && item.size in BT_SHRINK_IMAGE..(64L shl 20)) item = shrink(item)
            if (item.size < 0 || item.size > BT_MAX) {
                val minutes = max(1, ((if (item.size > 0) item.size else BT_MAX) / (55 * 1024) / 60).toInt())
                return if (item.size > 0) "${human(item.size)} 走蓝牙要传约 $minutes 分钟，连上局域网再发" else "不知道多大，蓝牙上不发，连上局域网再发"
            }
        }
        val id = (System.nanoTime() xor (index.toLong() shl 48)) and Long.MAX_VALUE
        synchronized(lock) {
            currentId = id
            acked = -1
            ackError = null
            done = null
        }
        val start = JSONObject().put("t", "fileStart").put("id", id).put("batch", batchId).put("index", index).put("count", count)
            .put("name", item.name).put("size", item.size).put("mime", item.mime ?: "")
        if (!link.write(start)) return "没发出去，连接断了"
        waitFor(8000) { acked >= 0 || ackError != null }?.let { return it }
        val chunk = if (link.lan) LAN_CHUNK else BT_CHUNK
        val window = if (link.lan) LAN_WINDOW else BT_WINDOW
        val buffer = ByteArray(chunk)
        var sent = 0L
        var lastReport = 0L
        val began = System.currentTimeMillis()
        var yielded = 0L
        var parts = 0
        var foregroundParts = 0
        report(index, count, item.name, 0, item.size)
        while (true) {
            val n = readFully(item.input, buffer)
            if (n <= 0) break
            // 电脑确认写好的离发出去的不能差太多：不然连接里堆着文件数据，触控板的消息要排在后面
            waitFor(20_000) { sent - acked + n <= window }?.let { return abort(link, id, it) }
            val y0 = System.currentTimeMillis()
            while (System.currentTimeMillis() - service.lastInputAt < INPUT_YIELD_MS && !cancelBatch) Thread.sleep(50)
            yielded += System.currentTimeMillis() - y0
            if (link.lan) {
                parts++
                if (inForeground()) foregroundParts++
            }
            if (cancelBatch) return abort(link, id, "取消了")
            if (!link.alive) return "连接断了，没发完"
            val data = Base64.encodeToString(buffer, 0, n, Base64.NO_WRAP)
            if (!link.write(JSONObject().put("t", "filePart").put("id", id).put("d", data))) return "连接断了，没发完"
            sent += n
            val now = System.currentTimeMillis()
            if (now - lastReport > 250) {
                lastReport = now
                report(index, count, item.name, sent, item.size)
            }
        }
        if (item.size >= 0 && sent != item.size) return abort(link, id, "文件读到一半读不下去了")
        if (!link.write(JSONObject().put("t", "fileEnd").put("id", id))) return "连接断了，没发完"
        val took = System.currentTimeMillis() - began - yielded
        Log.i(TAG, "sent ${item.name} $sent B via ${if (link.lan) "lan" else "bt"} in $took ms (+$yielded ms yielding), foreground $foregroundParts/$parts")
        if (link.lan && sent >= MEASURE_MIN) {
            lanBytes += sent
            lanMs += took
            lanParts += parts
            lanForegroundParts += foregroundParts
        }
        report(index, count, item.name, sent, item.size)
        waitFor(60_000) { done != null || ackError != null }?.let { return it }
        val result = synchronized(lock) { done }
        return if (result?.optBoolean("ok") == true) null else result?.optString("error")?.ifBlank { null } ?: "电脑没收好"
    }

    /** 等条件成立；出错（电脑回了错误、取消了、连接断了、等太久）返回原因 */
    private fun waitFor(timeoutMs: Long, ready: () -> Boolean): String? {
        val until = System.currentTimeMillis() + timeoutMs
        synchronized(lock) {
            while (true) {
                ackError?.let { return it }
                if (ready()) return null
                if (cancelBatch) return "取消了"
                val left = until - System.currentTimeMillis()
                if (left <= 0) return "电脑一直没回应"
                lock.wait(minOf(left, 500))
            }
        }
    }

    private fun abort(link: LinkService.FileLink, id: Long, why: String): String {
        link.write(JSONObject().put("t", "fileCancel").put("id", id))
        return why
    }

    /** 刚打开 App、刚换网络时可能还没连上：等一会儿 */
    private fun waitForLink(): LinkService.FileLink? {
        repeat(30) {
            service.fileLink()?.let { return it }
            if (cancelBatch) return null
            Thread.sleep(500)
        }
        return null
    }

    private fun report(index: Int, count: Int, name: String, sent: Long, size: Long) =
        service.post { service.fileListener.onFileProgress(index, count, name, sent, size) }

    /** 读满 buffer（流读到头就少读一些）；返回读到的字节数，读到头返回 0 */
    private fun readFully(input: InputStream, buffer: ByteArray): Int {
        var total = 0
        while (total < buffer.size) {
            val n = input.read(buffer, total, buffer.size - total)
            if (n < 0) break
            total += n
        }
        return total
    }

    /** 照片压成长边不超过 2048 的 JPEG（系统解码器会按 EXIF 转正，也认 HEIC）；压不了、压完反而更大就发原图 */
    private fun shrink(item: Item): Item {
        val bytes = item.input.readBytes()
        val whole = Item(item.name, bytes.size.toLong(), item.mime, ByteArrayInputStream(bytes))
        return runCatching { shrunk(item, bytes) }.onFailure { Log.w(TAG, "shrink failed: ${it.message}") }.getOrNull() ?: whole
    }

    private fun shrunk(item: Item, bytes: ByteArray): Item? {
        val source = ImageDecoder.createSource(ByteBuffer.wrap(bytes))
        val bitmap = ImageDecoder.decodeBitmap(source) { decoder, info, _ ->
            val (w, h) = info.size.width to info.size.height
            val scale = minOf(1.0, SHRINK_SIDE.toDouble() / max(w, h))
            decoder.setTargetSize(max(1, (w * scale).roundToInt()), max(1, (h * scale).roundToInt()))
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
        }
        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.JPEG, 82, out)
        bitmap.recycle()
        val jpeg = out.toByteArray()
        if (jpeg.size >= bytes.size) return null
        val stem = item.name.substringBeforeLast('.', item.name)
        return Item("$stem.jpg", jpeg.size.toLong(), "image/jpeg", ByteArrayInputStream(jpeg))
    }
}
