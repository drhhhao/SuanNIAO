package dev.suanniao

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * 「我的电脑」：这部手机连过的每台电脑一项，同一时间只用其中一台（current）。蓝牙、局域网都只连正在用的那台，
 * 切到另一台就断开旧的、连新的——以前蓝牙选的电脑和扫码绑定的局域网电脑各存各的，可能一条连 A、一条连 B（用户 2026-11-01 反馈）。
 * - id：电脑编号（电脑 hello、二维码里的 pc）。只从蓝牙列表里选过、还没连上过的电脑先记成「bt:蓝牙地址」，连上知道编号后换掉
 * - name：电脑名（电脑 hello 里的 host、二维码里的 n）；renamed = 用户自己改过名，之后不再跟着电脑改
 * - bt：电脑的蓝牙地址；lan：局域网的绑定（扫码得来）
 * 存在偏好 computers（数组）和 current_pc 里；旧版的 pc_address / pc_name / lan_binding 第一次读的时候搬进来
 */
data class Computer(
    val id: String,
    val name: String,
    val bt: String? = null,
    val lan: LanBinding? = null,
    val renamed: Boolean = false,
    /** 上次切到它的时刻：列表按它排 */
    val usedAt: Long = 0,
) {
    /** 这台还能连：有蓝牙地址，或者局域网绑定没被电脑解除 */
    val usable get() = bt != null || (lan != null && !lan.revoked)

    fun toJson(): JSONObject = JSONObject()
        .put("id", id).put("name", name).put("bt", bt).put("lan", lan?.toJson())
        .put("renamed", renamed).put("usedAt", usedAt)

    companion object {
        fun fromJson(o: JSONObject): Computer? = runCatching {
            Computer(
                id = o.getString("id"),
                name = o.optString("name").ifBlank { "电脑" },
                bt = if (o.isNull("bt")) null else o.optString("bt").ifBlank { null },
                lan = o.optJSONObject("lan")?.let { LanBinding.fromJson(it) },
                renamed = o.optBoolean("renamed", false),
                usedAt = o.optLong("usedAt", 0),
            )
        }.getOrNull()
    }
}

object Computers {
    private const val KEY = "computers"
    private const val KEY_CURRENT = "current_pc"
    /** 旧版存的（迁移用） */
    private const val OLD_ADDRESS = "pc_address"
    private const val OLD_NAME = "pc_name"

    class State(val list: List<Computer>, val current: String?) {
        fun current(): Computer? = list.firstOrNull { it.id == current }
        fun byId(id: String) = list.firstOrNull { it.id == id }
        fun byBt(address: String) = list.firstOrNull { it.bt.equals(address, ignoreCase = true) }

        /** 换掉 id 那项（没有就加上）；change 返回 null = 删掉 */
        fun with(id: String, change: (Computer?) -> Computer?): State {
            val old = byId(id)
            val next = change(old)
            val list = when {
                next == null -> list.filter { it.id != id }
                old == null -> list + next
                else -> list.map { if (it.id == id) next else it }
            }
            return State(list, if (list.any { it.id == current }) current else null)
        }

        fun select(id: String?): State =
            State(list.map { if (it.id == id) it.copy(usedAt = System.currentTimeMillis()) else it }, id)
    }

    private val lock = Any()
    private var listeners = listOf<() -> Unit>()

    /** 列表或正在用的那台变了（任何线程）：服务据此换连接 */
    fun listen(l: () -> Unit) = synchronized(lock) { listeners = listeners + l }
    fun unlisten(l: () -> Unit) = synchronized(lock) { listeners = listeners - l }

    private fun prefs(context: Context) = context.getSharedPreferences(LinkService.PREFS, Context.MODE_PRIVATE)

    fun load(context: Context): State = synchronized(lock) { read(context) }

    fun current(context: Context): Computer? = load(context).current()

    /** 按上次用的先后排，正在用的在最前 */
    fun sorted(context: Context): List<Computer> {
        val s = load(context)
        return s.list.sortedWith(compareByDescending<Computer> { it.id == s.current }.thenByDescending { it.usedAt })
    }

    /** 读出现在存着的、改一下、存回去，中间别的线程插不进来；真变了才通知 */
    fun update(context: Context, change: (State) -> State): State {
        val (next, changed, ls) = synchronized(lock) {
            val cur = read(context)
            val next = change(cur)
            val changed = next.current != cur.current || next.list != cur.list
            if (changed) write(context, next)
            Triple(next, changed, listeners)
        }
        if (changed) ls.forEach { it() }
        return next
    }

    private fun read(context: Context): State {
        val p = prefs(context)
        val text = p.getString(KEY, null) ?: return migrate(context)
        val arr = runCatching { JSONArray(text) }.getOrNull() ?: JSONArray()
        val list = (0 until arr.length()).mapNotNull { arr.optJSONObject(it)?.let(Computer::fromJson) }
        val current = p.getString(KEY_CURRENT, null)?.takeIf { id -> list.any { it.id == id } }
        return State(list, current)
    }

    private fun write(context: Context, s: State) {
        val arr = JSONArray()
        s.list.forEach { arr.put(it.toJson()) }
        prefs(context).edit().putString(KEY, arr.toString()).putString(KEY_CURRENT, s.current).commit()
    }

    /**
     * 旧版：一个蓝牙地址 + 一个局域网绑定，可能是同一台也可能不是，现在分不出来。先当成同一台（和旧版行为一样），
     * 蓝牙连上、电脑 hello 说编号和局域网那台不一样时再拆开（见 learnBt）
     */
    private fun migrate(context: Context): State {
        val p = prefs(context)
        val address = p.getString(OLD_ADDRESS, null)
        val name = p.getString(OLD_NAME, null)
        val lan = p.getString(LanBinding.KEY, null)?.let { runCatching { LanBinding.fromJson(JSONObject(it)) }.getOrNull() }
        val now = System.currentTimeMillis()
        val pc = when {
            lan != null -> Computer(lan.pc, lan.name.ifBlank { name ?: "电脑" }, bt = address, lan = lan, usedAt = now)
            address != null -> Computer(btId(address), name ?: address, bt = address, usedAt = now)
            else -> null
        }
        val s = State(listOfNotNull(pc), pc?.id)
        write(context, s)
        p.edit().remove(OLD_ADDRESS).remove(OLD_NAME).remove(LanBinding.KEY).apply()
        return s
    }

    fun btId(address: String) = "bt:" + address.uppercase()

    // ───────── 常用的改法 ─────────

    fun select(context: Context, id: String) = update(context) { it.select(id) }

    /** 从蓝牙列表里选了一台：认识这个地址就切过去，不认识就加一台 */
    fun selectBt(context: Context, address: String, name: String): Computer = update(context) { s ->
        val known = s.byBt(address)
        val next = known ?: Computer(btId(address), name, bt = address)
        s.with(next.id) { it ?: next }.select(next.id)
    }.current()!!

    /**
     * 这个蓝牙地址以前记在别的项上（旧版只从蓝牙列表选过、迁移时和局域网那台并在了一起）：从那项上拿掉，
     * 一个地址只属于一台电脑。那项只剩一个「bt:」编号、没有局域网就整项删掉
     */
    private fun State.releaseBt(address: String, keep: String): State {
        val other = byBt(address)?.takeIf { it.id != keep } ?: return this
        return with(other.id) { o -> if (o!!.lan == null && o.id.startsWith("bt:")) null else o.copy(bt = null) }
    }

    /** 扫码蓝牙配对成功：二维码里的电脑编号认这个蓝牙地址，切过去 */
    fun attachBt(context: Context, pc: String, name: String, address: String) = update(context) { s ->
        s.releaseBt(address, pc).with(pc) { it?.copy(bt = address) ?: Computer(pc, name, bt = address) }.select(pc)
    }

    /** 扫码绑定了局域网：记在这台电脑上，切过去。bt：二维码里的蓝牙地址（手机已经和它配对过才有） */
    fun attachLan(context: Context, binding: LanBinding, bt: String?) = update(context) { s ->
        val st = if (bt != null) s.releaseBt(bt, binding.pc) else s
        st.with(binding.pc) {
            it?.copy(lan = binding, bt = bt ?: it.bt, name = if (it.renamed) it.name else binding.name)
                ?: Computer(binding.pc, binding.name, bt = bt, lan = binding)
        }.select(binding.pc)
    }

    /** 改局域网绑定（按电脑编号找，不一定是正在用的那台） */
    fun updateLan(context: Context, pc: String, change: (LanBinding?) -> LanBinding?) = update(context) { s ->
        if (s.byId(pc) == null) s else s.with(pc) { it!!.copy(lan = change(it.lan)) }
    }

    /**
     * 蓝牙连上 address，电脑 hello 说自己的编号是 pcId、名字是 host：
     * - 这项还是「bt:地址」：换成真编号；列表里已经有这个编号（扫码绑定过局域网）就并进去
     * - 这项的编号和电脑说的不一样（旧版迁移时把两台当成一台了）：把蓝牙拆出去，单独一项
     * 返回蓝牙这台现在的编号
     */
    fun learnBt(context: Context, address: String, pcId: String, host: String?): String {
        update(context) { s ->
            val e = s.byBt(address) ?: return@update s
            var st = s
            if (e.id != pcId) {
                val wasCurrent = s.current == e.id
                val merged = e.id.startsWith("bt:")
                st = if (merged) st.with(e.id) { null } else st.with(e.id) { it!!.copy(bt = null) }
                st = st.with(pcId) { it?.copy(bt = address) ?: Computer(pcId, host ?: e.name, bt = address, usedAt = e.usedAt) }
                // 合并的：还是用户选的那台，跟过去；拆开的：正在用的仍是局域网那台
                if (wasCurrent && merged) st = State(st.list, pcId)
            }
            if (host != null) st = st.with(pcId) { if (it!!.renamed || it.name == host) it else it.copy(name = host) }
            st
        }
        return pcId
    }

    /** 电脑 hello 里的名字（局域网连上时）：没改过名就跟着电脑 */
    fun learnName(context: Context, pcId: String, host: String) = update(context) { s ->
        val e = s.byId(pcId) ?: return@update s
        if (e.renamed || e.name == host) s else s.with(pcId) { it!!.copy(name = host) }
    }

    fun rename(context: Context, id: String, name: String) = update(context) { s ->
        s.with(id) { it?.copy(name = name.trim().ifBlank { it.name }, renamed = name.isNotBlank()) }
    }

    /** 从列表里移除；移除的是正在用的就断开，不自动连别的（打板时定的：免得连上一台意想不到的电脑） */
    fun remove(context: Context, id: String) = update(context) { s -> s.with(id) { null } }
}
