package dev.suanniao

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** 「我的电脑」列表的逻辑：旧版迁移、蓝牙 / 局域网并成一台或拆开、改名、移除（Computers.kt） */
class ComputersTest {
    private lateinit var prefs: MemPrefs
    private lateinit var ctx: Context

    @Before
    fun setUp() {
        prefs = MemPrefs()
        ctx = object : ContextWrapper(null) {
            override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences = prefs
        }
    }

    private fun lan(pc: String, name: String = "局域网$pc", phoneId: String = "p-$pc") =
        LanBinding(pc, name, listOf("10.0.0.1"), 47820, "fp-$pc", phoneId, "sealed-$pc")

    @Test
    fun migratesOldBtAndLanAsOneComputer() {
        prefs.map["pc_address"] = "AA:AA:AA:AA:AA:AA"
        prefs.map["pc_name"] = "书房蓝牙名"
        prefs.map[LanBinding.KEY] = lan("A", "书房台式").toJson().toString()
        val s = Computers.load(ctx)
        assertEquals(1, s.list.size)
        val pc = s.current()!!
        assertEquals("A", pc.id)
        assertEquals("书房台式", pc.name)
        assertEquals("AA:AA:AA:AA:AA:AA", pc.bt)
        assertEquals("p-A", pc.lan!!.phoneId)
        assertTrue("旧的键删掉了", listOf("pc_address", "pc_name", LanBinding.KEY).none { prefs.map.containsKey(it) })
        // 再读一次还是同样的（从新格式读出来）
        assertEquals(s.list, Computers.load(ctx).list)
    }

    @Test
    fun migratesBtOnly() {
        prefs.map["pc_address"] = "aa:bb:cc:dd:ee:ff"
        prefs.map["pc_name"] = "笔记本"
        val pc = Computers.current(ctx)!!
        assertEquals("bt:AA:BB:CC:DD:EE:FF", pc.id)
        assertEquals("笔记本", pc.name)
        assertNull(pc.lan)
    }

    @Test
    fun emptyWhenNothingSaved() {
        val s = Computers.load(ctx)
        assertTrue(s.list.isEmpty())
        assertNull(s.current)
    }

    /** 用户的情况：局域网连本机 A，蓝牙其实连的是另一台 B。蓝牙 hello 说是 B → 拆成两台，正在用的还是 A */
    @Test
    fun splitsWhenBluetoothTurnsOutToBeAnotherPc() {
        prefs.map["pc_address"] = "BB:BB:BB:BB:BB:BB"
        prefs.map[LanBinding.KEY] = lan("A", "本机").toJson().toString()
        Computers.learnBt(ctx, "BB:BB:BB:BB:BB:BB", "B", "客厅笔记本")
        val s = Computers.load(ctx)
        assertEquals(2, s.list.size)
        assertEquals("A", s.current)
        assertNull("A 不再记着 B 的蓝牙", s.byId("A")!!.bt)
        val b = s.byId("B")!!
        assertEquals("BB:BB:BB:BB:BB:BB", b.bt)
        assertEquals("客厅笔记本", b.name)
        assertNull(b.lan)
    }

    /** 同一台：蓝牙 hello 说的编号就是局域网那台 → 不拆 */
    @Test
    fun keepsTogetherWhenSamePc() {
        prefs.map["pc_address"] = "AA:AA:AA:AA:AA:AA"
        prefs.map[LanBinding.KEY] = lan("A", "本机").toJson().toString()
        Computers.learnBt(ctx, "AA:AA:AA:AA:AA:AA", "A", "本机")
        val s = Computers.load(ctx)
        assertEquals(1, s.list.size)
        assertEquals("AA:AA:AA:AA:AA:AA", s.current()!!.bt)
        assertEquals("p-A", s.current()!!.lan!!.phoneId)
    }

    /** 扫码绑了局域网 C，又从蓝牙列表选了 C 的蓝牙（还不知道是 C）：连上后并成一台，正在用的跟过去 */
    @Test
    fun mergesBluetoothPickIntoKnownPc() {
        Computers.attachLan(ctx, lan("C", "台式"), bt = null)
        Computers.selectBt(ctx, "CC:CC:CC:CC:CC:CC", "DESKTOP-C")
        assertEquals("bt:CC:CC:CC:CC:CC:CC", Computers.load(ctx).current)
        Computers.learnBt(ctx, "CC:CC:CC:CC:CC:CC", "C", "台式")
        val s = Computers.load(ctx)
        assertEquals(1, s.list.size)
        assertEquals("C", s.current)
        assertEquals("CC:CC:CC:CC:CC:CC", s.current()!!.bt)
        assertEquals("p-C", s.current()!!.lan!!.phoneId)
    }

    /** 只从蓝牙列表选过的电脑，第一次连上：换成真编号、用电脑说的名字 */
    @Test
    fun btOnlyPcGetsRealId() {
        Computers.selectBt(ctx, "DD:DD:DD:DD:DD:DD", "蓝牙名")
        Computers.learnBt(ctx, "DD:DD:DD:DD:DD:DD", "D", "主机名")
        val pc = Computers.current(ctx)!!
        assertEquals("D", pc.id)
        assertEquals("主机名", pc.name)
    }

    /** 扫码蓝牙配对：地址原来记在一个「bt:」项上，搬到二维码里的电脑编号下 */
    @Test
    fun attachBtMovesAddress() {
        Computers.selectBt(ctx, "EE:EE:EE:EE:EE:EE", "旧")
        Computers.attachBt(ctx, "E", "扫码的名字", "EE:EE:EE:EE:EE:EE")
        val s = Computers.load(ctx)
        assertEquals(1, s.list.size)
        assertEquals("E", s.current)
        assertEquals("EE:EE:EE:EE:EE:EE", s.current()!!.bt)
    }

    /** 选蓝牙列表里认识的地址 = 切到那台，不多加一台 */
    @Test
    fun selectKnownBtSwitches() {
        Computers.attachBt(ctx, "A", "甲", "AA:AA:AA:AA:AA:AA")
        Computers.attachLan(ctx, lan("B", "乙"), bt = null)
        assertEquals("B", Computers.load(ctx).current)
        Computers.selectBt(ctx, "aa:aa:aa:aa:aa:aa", "甲的蓝牙名")
        val s = Computers.load(ctx)
        assertEquals(2, s.list.size)
        assertEquals("A", s.current)
    }

    @Test
    fun renameSticksAndRemoveCurrentDisconnects() {
        Computers.attachLan(ctx, lan("A", "甲"), bt = null)
        Computers.attachLan(ctx, lan("B", "乙"), bt = null)
        Computers.rename(ctx, "B", "我的乙")
        Computers.learnName(ctx, "B", "主机名")
        Computers.attachLan(ctx, lan("B", "二维码名", phoneId = "p-B2"), bt = null)
        assertEquals("我的乙", Computers.load(ctx).byId("B")!!.name)
        assertEquals("p-B2", Computers.load(ctx).byId("B")!!.lan!!.phoneId)
        Computers.remove(ctx, "B")
        val s = Computers.load(ctx)
        assertEquals(listOf("A"), s.list.map { it.id })
        assertNull("移除正在用的只断开，不自动连别的", s.current)
    }

    /** 连接线程拿着旧绑定改（比如记下连上的地址）：按电脑编号改，不管正在用的是哪台；期间重新扫过码就不改 */
    @Test
    fun updateIfSameTargetsByPc() {
        val a = lan("A")
        Computers.attachLan(ctx, a, bt = null)
        Computers.attachLan(ctx, lan("B"), bt = null)
        LanBinding.updateIfSame(ctx, a) { it.copy(lastAddr = "10.0.0.9") }
        assertEquals("10.0.0.9", Computers.load(ctx).byId("A")!!.lan!!.lastAddr)
        assertEquals("B", Computers.load(ctx).current)
        LanBinding.updateIfSame(ctx, a.copy(phoneId = "old")) { it.copy(revoked = true) }
        assertEquals(false, Computers.load(ctx).byId("A")!!.lan!!.revoked)
    }

    /** 扫码绑局域网时带上已配对的蓝牙地址：地址从原来那项上拿掉，切到别的电脑时蓝牙不会还连着这台 */
    @Test
    fun attachLanTakesBtFromOtherEntry() {
        prefs.map["pc_address"] = "BB:BB:BB:BB:BB:BB" // 旧版：蓝牙其实是 B，局域网是 A，迁移成一台 A
        prefs.map[LanBinding.KEY] = lan("A", "本机").toJson().toString()
        Computers.attachLan(ctx, lan("B", "客厅"), bt = "BB:BB:BB:BB:BB:BB")
        val s = Computers.load(ctx)
        assertEquals("B", s.current)
        assertNull(s.byId("A")!!.bt)
        assertEquals("BB:BB:BB:BB:BB:BB", s.byId("B")!!.bt)
        assertEquals(1, s.list.count { it.bt != null })
    }

    /** 同一台重新扫码、这次蓝牙没配对：原来记着的蓝牙地址留着 */
    @Test
    fun rescanKeepsKnownBt() {
        Computers.attachBt(ctx, "A", "甲", "AA:AA:AA:AA:AA:AA")
        Computers.attachLan(ctx, lan("A", "甲"), bt = null)
        assertEquals("AA:AA:AA:AA:AA:AA", Computers.current(ctx)!!.bt)
    }

    @Test
    fun listenerFiresOnlyOnRealChange() {
        var n = 0
        val l = { n++; Unit }
        Computers.listen(l)
        try {
            Computers.attachLan(ctx, lan("A"), bt = null)
            assertEquals(1, n)
            Computers.select(ctx, "A") // usedAt 变了也算变（列表排序要跟着变）
            val before = n
            Computers.learnName(ctx, "A", "局域网A") // 名字一样：不算
            assertEquals(before, n)
        } finally {
            Computers.unlisten(l)
        }
    }

    @Test
    fun computerJsonRoundTrip() {
        val pc = Computer("A", "甲", bt = "AA", lan = lan("A"), renamed = true, usedAt = 42)
        assertEquals(pc, Computer.fromJson(JSONObject(pc.toJson().toString())))
    }
}

/** 放在内存里的 SharedPreferences（只实现 Computers 用到的） */
class MemPrefs : SharedPreferences {
    val map = HashMap<String, Any?>()

    override fun getAll(): MutableMap<String, *> = map
    override fun getString(key: String?, defValue: String?) = map[key] as? String ?: defValue
    override fun getStringSet(key: String?, defValues: MutableSet<String>?) = defValues
    override fun getInt(key: String?, defValue: Int) = map[key] as? Int ?: defValue
    override fun getLong(key: String?, defValue: Long) = map[key] as? Long ?: defValue
    override fun getFloat(key: String?, defValue: Float) = map[key] as? Float ?: defValue
    override fun getBoolean(key: String?, defValue: Boolean) = map[key] as? Boolean ?: defValue
    override fun contains(key: String?) = map.containsKey(key)
    override fun registerOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) {}
    override fun unregisterOnSharedPreferenceChangeListener(listener: SharedPreferences.OnSharedPreferenceChangeListener?) {}

    override fun edit(): SharedPreferences.Editor = object : SharedPreferences.Editor {
        val puts = HashMap<String, Any?>()
        val removes = HashSet<String>()
        override fun putString(key: String, value: String?) = apply { puts[key] = value }
        override fun putStringSet(key: String, values: MutableSet<String>?) = apply { puts[key] = values }
        override fun putInt(key: String, value: Int) = apply { puts[key] = value }
        override fun putLong(key: String, value: Long) = apply { puts[key] = value }
        override fun putFloat(key: String, value: Float) = apply { puts[key] = value }
        override fun putBoolean(key: String, value: Boolean) = apply { puts[key] = value }
        override fun remove(key: String) = apply { removes += key }
        override fun clear() = apply { removes += map.keys }
        override fun commit(): Boolean {
            removes.forEach { map.remove(it) }
            puts.forEach { (k, v) -> if (v == null) map.remove(k) else map[k] = v }
            return true
        }
        override fun apply() {
            commit()
        }
    }
}
