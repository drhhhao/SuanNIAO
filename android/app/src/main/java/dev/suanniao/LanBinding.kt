package dev.suanniao

import android.content.Context
import android.net.Uri
import org.json.JSONArray
import org.json.JSONObject

/**
 * 扫码绑定的那台电脑（局域网连接用）。存在偏好里，令牌用 Android Keystore 加密后再存（见 LanSecure.seal）。
 * - pc：电脑编号；fp：电脑证书指纹，连的时候核对，不对就不连（防冒充）
 * - addrs、port：电脑的地址，电脑连着时地址变了会发 lan 消息来更新；lastAddr 是上次连上的那个，下次先试它
 * - phoneId、token：电脑发给这部手机的编号和长期令牌
 * - revoked：电脑上解除了绑定（认证时电脑说不认识）：不再自动连，等重新扫码
 */
data class LanBinding(
    val pc: String,
    val name: String,
    val addrs: List<String>,
    val port: Int,
    val fp: String,
    val phoneId: String,
    val sealedToken: String,
    val lastAddr: String? = null,
    val revoked: Boolean = false,
) {
    /** 先试上次连上的地址 */
    fun tryOrder(): List<String> = (listOfNotNull(lastAddr) + addrs).distinct()

    fun toJson(): JSONObject = JSONObject()
        .put("pc", pc).put("name", name).put("addrs", JSONArray(addrs)).put("port", port).put("fp", fp)
        .put("phoneId", phoneId).put("token", sealedToken).put("lastAddr", lastAddr).put("revoked", revoked)

    companion object {
        const val KEY = "lan_binding"

        fun load(context: Context): LanBinding? {
            val text = context.getSharedPreferences(LinkService.PREFS, Context.MODE_PRIVATE).getString(KEY, null) ?: return null
            return runCatching {
                val o = JSONObject(text)
                val list = o.getJSONArray("addrs")
                LanBinding(
                    pc = o.getString("pc"),
                    name = o.optString("name"),
                    addrs = (0 until list.length()).map { list.getString(it) },
                    port = o.getInt("port"),
                    fp = o.getString("fp"),
                    phoneId = o.getString("phoneId"),
                    sealedToken = o.getString("token"),
                    lastAddr = if (o.isNull("lastAddr")) null else o.optString("lastAddr").ifBlank { null },
                    revoked = o.optBoolean("revoked", false),
                )
            }.getOrNull()
        }

        fun save(context: Context, binding: LanBinding?) {
            update(context) { binding }
        }

        private val lock = Any()

        /**
         * 读出现在存着的、改一下、存回去，中间别的线程插不进来。连接线程、扫码配对、解除绑定、电脑发来新地址
         * 都会改它：各自拿着早先读到的旧值去存，会把别人刚存的新绑定冲掉
         */
        fun update(context: Context, change: (LanBinding?) -> LanBinding?) = synchronized(lock) {
            val next = change(load(context))
            val prefs = context.getSharedPreferences(LinkService.PREFS, Context.MODE_PRIVATE).edit()
            if (next == null) prefs.remove(KEY) else prefs.putString(KEY, next.toJson().toString())
            prefs.apply()
        }

        /** 只改同一次绑定（电脑和手机编号都没变）：期间重新扫了码、解除了绑定，就什么都不做 */
        fun updateIfSame(context: Context, binding: LanBinding, change: (LanBinding) -> LanBinding) = update(context) { cur ->
            if (cur != null && cur.pc == binding.pc && cur.phoneId == binding.phoneId) change(cur) else cur
        }
    }
}

/** 电脑上二维码里的内容：suanniao://pair?v=1&pc=…&n=电脑名&a=地址1,地址2&p=端口&fp=指纹&c=一次性配对码 */
data class LanQr(val pc: String, val name: String, val addrs: List<String>, val port: Int, val fp: String, val code: String) {
    companion object {
        fun parse(text: String?): LanQr? {
            val uri = runCatching { Uri.parse(text?.trim() ?: return null) }.getOrNull() ?: return null
            if (uri.scheme != "suanniao" || uri.host != "pair") return null
            if ((uri.getQueryParameter("v") ?: return null) != "1") return null
            val addrs = uri.getQueryParameter("a")?.split(',')?.map { it.trim() }?.filter { it.isNotEmpty() } ?: return null
            val port = uri.getQueryParameter("p")?.toIntOrNull()?.takeIf { it in 1..65535 } ?: return null
            return LanQr(
                pc = uri.getQueryParameter("pc")?.takeIf { it.isNotBlank() } ?: return null,
                name = uri.getQueryParameter("n").orEmpty().ifBlank { "电脑" },
                addrs = addrs.ifEmpty { return null },
                port = port,
                fp = uri.getQueryParameter("fp")?.takeIf { it.isNotBlank() } ?: return null,
                code = uri.getQueryParameter("c")?.takeIf { it.isNotBlank() } ?: return null,
            )
        }
    }
}
