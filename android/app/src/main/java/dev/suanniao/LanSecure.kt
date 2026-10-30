package dev.suanniao

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.JsonReader
import android.util.JsonToken
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.StringReader
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.security.KeyStore
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.Base64
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

/** Successful first pairing. Persist [token] only after [LanSecure.seal]. */
data class Paired(val phoneId: String, val token: String, val pcId: String) {
    override fun toString() = "Paired(phoneId=$phoneId, token=<redacted>, pcId=$pcId)"
}

/** [reason] is badCode, unknown, badToken, timeout or protocol. The socket is closed. */
class LanAuthException(val reason: String, cause: Throwable? = null) :
    IOException("LAN authentication failed: $reason", cause)

/**
 * LAN transport security; all network methods must run off the UI thread.
 *
 * Call connect(host, port, QR fingerprint, timeoutMs), then immediately call pair(socket,
 * QR code, device name, model), or auth(socket, saved phoneId, open(context, saved token)).
 * A null open result requires pairing again. Until pair/auth succeeds, no other reader or
 * writer may use the socket. After success, use its inputStream/outputStream normally;
 * the auth reader consumes exactly through the first LF and leaves following data intact.
 * Compare the returned pcId with the saved/QR identity when the caller has one.
 *
 * connect permits TLS 1.3 and 1.2 only and trusts precisely the SHA-256 fingerprint of the
 * leaf certificate's DER (43 characters, unpadded base64url). It does not use system CAs
 * or hostname verification. The QR is the trust bootstrap; store its fingerprint with
 * the pairing and do not replace it from an unauthenticated discovery result.
 *
 * connect's deadline covers TCP connect and TLS handshake. pair/auth have a fresh 10 s
 * total deadline (the server also enforces its combined handshake/auth deadline).
 * Both set SO_TIMEOUT back to zero on success; any failure closes the socket.
 * DNS resolution uses the platform resolver; QR hosts should be numeric LAN addresses.
 *
 * seal/open use the app's Android Keystore AES-256-GCM key "suanniao-lan". The storage
 * format is v1.<12-byte IV in base64url>.<ciphertext + 16-byte tag in base64url>, without
 * padding. AAD binds this format to this application package. Store the entire string
 * in private SharedPreferences. open never creates a missing key and returns null for
 * corrupt data, an unavailable key, or a reinstall/restore that lost the original key.
 */
object LanSecure {
    private const val AUTH_TIMEOUT_MS = 10_000
    private const val MAX_LINE_BYTES = 64 * 1024
    private const val KEY_ALIAS = "suanniao-lan"
    private val keyLock = Any()
    private val encoder = Base64.getUrlEncoder().withoutPadding()
    private val decoder = Base64.getUrlDecoder()
    private val urlAlphabet = Regex("[A-Za-z0-9_-]+")
    private val failureReasons = setOf("badCode", "unknown", "badToken", "timeout", "protocol")
    private val timeouts = ScheduledThreadPoolExecutor(1) { runnable ->
        Thread(runnable, "suanniao-lan-timeout").apply { isDaemon = true }
    }.apply { removeOnCancelPolicy = true }

    @JvmStatic
    fun connect(host: String, port: Int, fingerprint: String, timeoutMs: Int, network: android.net.Network? = null): SSLSocket {
        require(timeoutMs > 0) { "timeoutMs must be positive" }
        require(port in 1..65535) { "port must be in 1..65535" }
        val expected = try {
            decodeCanonical(fingerprint).also { require(it.size == 32) }
        } catch (e: IllegalArgumentException) {
            throw CertificateException("Invalid LAN certificate fingerprint", e)
        }
        val trust = object : X509TrustManager {
            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()

            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                throw CertificateException("Client certificates are not supported")
            }

            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                val leaf = chain?.firstOrNull() ?: throw CertificateException("Missing certificate")
                val actual = MessageDigest.getInstance("SHA-256").digest(leaf.encoded)
                if (!MessageDigest.isEqual(expected, actual)) {
                    throw CertificateException("LAN certificate fingerprint mismatch")
                }
            }
        }
        val context = SSLContext.getInstance("TLS").apply { init(null, arrayOf(trust), null) }
        val tcp = Socket()
        val deadline = Deadline(tcp, timeoutMs)
        var tls: SSLSocket? = null
        try {
            tcp.tcpNoDelay = true
            tcp.keepAlive = true
            // 走 Wi-Fi / 有线：开着全局 VPN 时，默认网络会把局域网地址也送进隧道，连不上电脑
            network?.bindSocket(tcp)
            tcp.connect(InetSocketAddress(host, port), deadline.remainingMillis())
            tls = context.socketFactory.createSocket(tcp, host, port, true) as SSLSocket
            tls.useClientMode = true
            tls.enabledProtocols = arrayOf("TLSv1.3", "TLSv1.2")
                .filter { it in tls.supportedProtocols }.toTypedArray()
            if (tls.enabledProtocols.isEmpty()) throw IOException("TLS 1.2 or newer is required")
            tls.sslParameters = tls.sslParameters.apply { endpointIdentificationAlgorithm = null }
            tls.soTimeout = deadline.remainingMillis()
            tls.startHandshake()
            tls.soTimeout = 0
            deadline.finish()
            return tls
        } catch (e: Exception) {
            val timedOut = deadline.expired()
            closeQuietly(tcp)
            tls?.let(::closeQuietly)
            if (timedOut && e !is SocketTimeoutException) {
                throw SocketTimeoutException("LAN TLS connection timed out").apply { initCause(e) }
            }
            throw e
        } finally {
            deadline.cancel()
        }
    }

    /** [prev] is the prior phone ID and plaintext token for this same PC, if available. */
    @JvmStatic
    @JvmOverloads
    fun pair(socket: SSLSocket, code: String, device: String, model: String,
             prev: Pair<String, String>? = null): Paired =
        exchange(socket, JSONObject().put("t", "pair").put("code", code)
            .put("device", device).put("model", model).apply {
                prev?.let { put("prev", JSONObject().put("id", it.first).put("token", it.second)) }
            }) { response ->
            requireFields(response, setOf("t", "id", "token", "pc"), "paired")
            Paired(identifier(response, "id", 16), identifier(response, "token", 32),
                identifier(response, "pc", 16))
        }

    @JvmStatic
    fun auth(socket: SSLSocket, phoneId: String, token: String): String =
        exchange(socket, JSONObject().put("t", "auth").put("id", phoneId).put("token", token)) { response ->
            requireFields(response, setOf("t", "pc"), "authOk")
            identifier(response, "pc", 16)
        }

    private fun <T> exchange(socket: SSLSocket, request: JSONObject, result: (Map<String, String>) -> T): T {
        val deadline = Deadline(socket, AUTH_TIMEOUT_MS)
        try {
            val bytes = (request.toString() + "\n").toByteArray(Charsets.UTF_8)
            if (bytes.size > MAX_LINE_BYTES) throw LanAuthException("protocol")
            socket.soTimeout = deadline.remainingMillis()
            socket.outputStream.write(bytes)
            socket.outputStream.flush()
            val response = readResponse(socket, deadline)
            if (response["t"] == "authFail") {
                requireFields(response, setOf("t", "reason"), "authFail")
                throw LanAuthException(response["reason"]?.takeIf { it in failureReasons } ?: "protocol")
            }
            val value = result(response)
            socket.soTimeout = 0
            deadline.finish()
            return value
        } catch (e: Exception) {
            val reason = when {
                deadline.expired() || e is SocketTimeoutException -> "timeout"
                e is LanAuthException -> e.reason
                else -> "protocol"
            }
            closeQuietly(socket)
            if (e is LanAuthException && e.reason == reason) throw e
            throw LanAuthException(reason, e)
        } finally {
            deadline.cancel()
        }
    }

    private fun readResponse(socket: SSLSocket, deadline: Deadline): Map<String, String> {
        val line = ByteArrayOutputStream()
        val input = socket.inputStream
        while (line.size() < MAX_LINE_BYTES) {
            socket.soTimeout = deadline.remainingMillis()
            val value = input.read() // Deliberately no BufferedReader: the next message belongs to the caller.
            if (value == -1) throw LanAuthException("protocol")
            if (value == '\n'.code) {
                val text = decodeUtf8(line.toByteArray())
                return JsonReader(StringReader(text)).use { json ->
                    json.isLenient = false
                    val fields = LinkedHashMap<String, String>()
                    json.beginObject()
                    while (json.hasNext()) {
                        val key = json.nextName()
                        if (fields.containsKey(key) || json.peek() != JsonToken.STRING) {
                            throw LanAuthException("protocol")
                        }
                        fields[key] = json.nextString()
                    }
                    json.endObject()
                    if (json.peek() != JsonToken.END_DOCUMENT) throw LanAuthException("protocol")
                    fields
                }
            }
            line.write(value)
        }
        throw LanAuthException("protocol")
    }

    private fun requireFields(response: Map<String, String>, fields: Set<String>, type: String) {
        if (response.keys != fields || response["t"] != type) throw LanAuthException("protocol")
    }

    private fun identifier(response: Map<String, String>, field: String, size: Int): String {
        val value = response[field] ?: throw LanAuthException("protocol")
        if (decodeCanonical(value).size != size) throw LanAuthException("protocol")
        return value
    }

    @JvmStatic
    fun seal(context: Context, secret: String): String {
        val key = synchronized(keyLock) { getKey(create = true)!! }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key) // Android Keystore chooses a fresh cryptographic random IV.
        cipher.updateAAD(aad(context))
        val encrypted = cipher.doFinal(secret.toByteArray(Charsets.UTF_8))
        check(cipher.iv.size == 12) { "Unexpected GCM IV length" }
        return "v1.${encoder.encodeToString(cipher.iv)}.${encoder.encodeToString(encrypted)}"
    }

    @JvmStatic
    fun open(context: Context, sealed: String): String? = try {
        val parts = sealed.split('.', limit = 4)
        require(parts.size == 3 && parts[0] == "v1")
        val iv = decodeCanonical(parts[1])
        val encrypted = decodeCanonical(parts[2])
        require(iv.size == 12 && encrypted.size >= 16)
        val key = synchronized(keyLock) { getKey(create = false) }
        if (key == null) null else {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, iv))
            cipher.updateAAD(aad(context))
            decodeUtf8(cipher.doFinal(encrypted))
        }
    } catch (_: Exception) {
        null
    }

    private fun getKey(create: Boolean): SecretKey? {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        if (store.containsAlias(KEY_ALIAS)) {
            return store.getKey(KEY_ALIAS, null) as? SecretKey
                ?: throw IOException("LAN Keystore entry is not an AES key")
        }
        if (!create) return null
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(KeyGenParameterSpec.Builder(KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .build())
            generateKey()
        }
    }

    private fun aad(context: Context) =
        "suanniao-lan:v1:${context.applicationContext.packageName}".toByteArray(Charsets.UTF_8)

    private fun decodeCanonical(value: String): ByteArray {
        require(urlAlphabet.matches(value)) { "Invalid base64url" }
        return decoder.decode(value).also { require(encoder.encodeToString(it) == value) }
    }

    private fun decodeUtf8(bytes: ByteArray): String = Charsets.UTF_8.newDecoder()
        .onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)
        .decode(ByteBuffer.wrap(bytes)).toString()

    private fun closeQuietly(socket: Socket) {
        try { socket.close() } catch (_: Exception) { }
    }

    /** Also bounds writes and the whole handshake; SO_TIMEOUT alone only bounds one read. */
    private class Deadline(private val socket: Socket, timeoutMs: Int) {
        private val end = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs.toLong())
        private val active = AtomicBoolean(true)
        private val timedOut = AtomicBoolean(false)
        private val task = timeouts.schedule({
            if (active.compareAndSet(true, false)) {
                timedOut.set(true)
                closeQuietly(socket)
            }
        }, timeoutMs.toLong(), TimeUnit.MILLISECONDS)

        fun expired() = timedOut.get() || end - System.nanoTime() <= 0L

        fun remainingMillis(): Int {
            val remaining = end - System.nanoTime()
            if (timedOut.get() || remaining <= 0L) throw SocketTimeoutException("LAN deadline expired")
            return ((remaining + 999_999L) / 1_000_000L).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
        }

        fun finish() {
            if (!active.compareAndSet(true, false) || expired()) {
                closeQuietly(socket)
                throw SocketTimeoutException("LAN deadline expired")
            }
            task.cancel(false)
        }

        fun cancel() {
            active.set(false)
            task.cancel(false)
        }
    }
}
