package dev.suanniao

import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.content.ServiceConnection
import android.net.Uri
import android.os.Bundle
import android.os.IBinder
import android.widget.Toast

/**
 * 系统「分享」菜单里的「蒜鸟」：在相册、微信、文件管理里选好文件分享过来，就发到电脑（见 FileSender）。
 * 只分享了一段文字（没有文件）就放进电脑剪贴板。
 * 看不见的界面：趁着还开着把文件打开（分享来的读权限只在它开着时有效），交给连接服务发，然后马上关掉。
 */
class ShareActivity : Activity() {

    private var items: List<FileSender.Item> = emptyList()
    private var text: String? = null
    private var bound = false

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            val service = (binder as? LinkService.LocalBinder)?.service
            if (service != null) {
                val t = text
                if (t != null) service.files.sendText(t) else service.files.enqueue(items)
                items = emptyList()
            }
            done()
        }

        override fun onServiceDisconnected(name: ComponentName?) {}
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val uris = sharedUris(intent)
        if (uris.isEmpty()) {
            text = intent.getStringExtra(Intent.EXTRA_TEXT)?.takeIf { it.isNotBlank() }
            if (text == null) return say("没有要发的文件").also { finish() }
            say("正在把这段文字发到电脑剪贴板…")
        } else {
            items = FileSender.open(this, uris)
            if (items.isEmpty()) return say("这些文件打不开，没法发").also { finish() }
            val skipped = uris.size - items.size
            say(
                (if (items.size == 1) "正在发到电脑：${items[0].name}" else "正在发到电脑：${items.size} 个文件") +
                    (if (skipped > 0) "（$skipped 个打不开，跳过了）" else "")
            )
        }
        // 连接服务没在运行（刚开机、点过「断开」）也拉起来：要发文件就得连上
        runCatching { startForegroundService(Intent(this, LinkService::class.java)) }
        bound = bindService(Intent(this, LinkService::class.java), connection, BIND_AUTO_CREATE)
        if (!bound) {
            say("蒜鸟的连接服务开不起来，没发出去")
            done()
        }
    }

    private fun done() {
        if (bound) {
            runCatching { unbindService(connection) }
            bound = false
        }
        items.forEach { runCatching { it.input.close() } }
        items = emptyList()
        finish()
    }

    private fun say(message: String) {
        Toast.makeText(applicationContext, message, Toast.LENGTH_SHORT).show()
    }

    companion object {
        /** 分享来的文件：一个（EXTRA_STREAM）、好几个（EXTRA_STREAM 列表），有的 App 只放在 ClipData 里 */
        @Suppress("DEPRECATION") // 带类型的新接口要安卓 13
        fun sharedUris(intent: Intent): List<Uri> {
            val list = LinkedHashSet<Uri>()
            when (intent.action) {
                Intent.ACTION_SEND -> (intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))?.let { list += it }
                Intent.ACTION_SEND_MULTIPLE -> intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.let { list += it }
            }
            intent.clipData?.let { clip -> for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let { list += it } }
            return list.toList()
        }
    }
}
