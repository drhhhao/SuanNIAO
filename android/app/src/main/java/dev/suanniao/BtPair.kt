package dev.suanniao

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.Looper

/**
 * 扫码蓝牙配对：电脑上的二维码里有电脑的蓝牙地址（bt）。直接对这个地址发起配对，不用去系统蓝牙里搜；
 * 手机上系统弹「配对」点一下就行（电脑那边二维码开着时蒜鸟自动同意，不弹框）。
 * 配上了就选这台电脑；之后蓝牙连上时把二维码里的配对码（c）当口令发给电脑（见 LinkService.btPairCode），
 * 电脑核对过才算数——对不上电脑会删掉这次配对，防止旁边的人趁二维码开着配进去。
 * 已经配对过就不再弹框，直接选上。
 */
class BtPair(private val context: Context) {

    private val main = Handler(Looper.getMainLooper())
    private var receiver: BroadcastReceiver? = null
    private var timeout: Runnable? = null

    /** 正在等系统配对的结果 */
    val busy get() = receiver != null

    /**
     * 对 address 发起配对；结果在主线程回 onDone(ok, 说明)。调之前要有蓝牙权限、蓝牙开着。
     * ok 时说明是 null（已经配对过也算 ok）
     */
    @SuppressLint("MissingPermission")
    fun start(address: String, onDone: (ok: Boolean, message: String?) -> Unit) {
        cancel()
        val adapter = context.getSystemService(BluetoothManager::class.java)?.adapter
            ?: return onDone(false, "这部手机没有蓝牙")
        val device = runCatching { adapter.getRemoteDevice(address.uppercase()) }.getOrNull()
            ?: return onDone(false, "二维码里的蓝牙地址不对：在电脑上重新显示二维码再扫")
        if (device.bondState == BluetoothDevice.BOND_BONDED) return onDone(true, null)

        var sawBonding = false
        val r = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
                @Suppress("DEPRECATION")
                val d = intent.getParcelableExtra<BluetoothDevice>(BluetoothDevice.EXTRA_DEVICE) ?: return
                if (!d.address.equals(device.address, ignoreCase = true)) return
                when (intent.getIntExtra(BluetoothDevice.EXTRA_BOND_STATE, BluetoothDevice.ERROR)) {
                    BluetoothDevice.BOND_BONDING -> sawBonding = true
                    BluetoothDevice.BOND_BONDED -> finish(onDone, true, null)
                    BluetoothDevice.BOND_NONE -> if (sawBonding) {
                        finish(onDone, false, "没配对上：在弹出的窗口里点「配对」；电脑上的二维码要开着、电脑蓝牙要开着")
                    }
                }
            }
        }
        receiver = r
        context.registerReceiver(r, IntentFilter(BluetoothDevice.ACTION_BOND_STATE_CHANGED))
        // 正在搜设备会让配对很慢（有的手机直接失败）
        runCatching { if (adapter.isDiscovering) adapter.cancelDiscovery() }
        if (!runCatching { device.createBond() }.getOrDefault(false)) {
            // 已经在配对（比如上一次的系统弹框还开着）也会返回 false：等结果；真发不起来就靠超时
            if (device.bondState != BluetoothDevice.BOND_BONDING) {
                finish(onDone, false, "发起不了蓝牙配对：把手机蓝牙关了再开，然后重新扫")
                return
            }
            sawBonding = true
        }
        timeout = Runnable {
            finish(onDone, false, if (sawBonding) "配对超时了：在弹出的窗口里点「配对」，再扫一次" else "找不到这台电脑的蓝牙：电脑蓝牙要开着、离手机近一点，再扫一次")
        }.also { main.postDelayed(it, TIMEOUT_MS) }
    }

    fun cancel() {
        receiver?.let { runCatching { context.unregisterReceiver(it) } }
        receiver = null
        timeout?.let { main.removeCallbacks(it) }
        timeout = null
    }

    private fun finish(onDone: (Boolean, String?) -> Unit, ok: Boolean, message: String?) {
        if (receiver == null) return // 已经回过了
        cancel()
        onDone(ok, message)
    }

    companion object {
        /** 系统配对弹框自己大约 30 秒超时；电脑这边发现不了也在这个范围 */
        private const val TIMEOUT_MS = 60_000L

        /** 二维码里的蓝牙地址：AA:BB:CC:DD:EE:FF */
        fun validAddress(text: String?): String? = text?.trim()?.uppercase()?.takeIf { BluetoothAdapter.checkBluetoothAddress(it) }
    }
}
