package dev.phonepad

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import com.k2fsa.sherpa.onnx.FeatureConfig
import com.k2fsa.sherpa.onnx.OnlineModelConfig
import com.k2fsa.sherpa.onnx.OnlineRecognizer
import com.k2fsa.sherpa.onnx.OnlineRecognizerConfig
import com.k2fsa.sherpa.onnx.OnlineStream
import com.k2fsa.sherpa.onnx.OnlineTransducerModelConfig
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlin.math.log10

/**
 * 手机本地的流式语音识别：蒜鸟自己录音，用 sherpa-onnx 跑 X-ASR 中英模型，边说边出字、自带标点，不用联网。
 *
 * 模型放在 /sdcard/Android/data/dev.phonepad/files/models/<名字>/ 下，一个目录一个模型：
 * tokens.txt 加 encoder / decoder / joiner 三个 .onnx。
 *
 * 一次按住（或锁定）从头到尾只用一个识别流、中途不重置：句子之间的「。」要等听到下一句开头才出，
 * 重置会把它丢掉；正好在重置那一刻说的字也会被切坏。实测这样识别出的文字只会往后追加，可以边说边打。
 */
object LocalAsr {
    const val SAMPLE_RATE = 16000
    /** 松手后补的静音：流式模型要看到后面一点才肯出最后几个字 */
    private const val TAIL_PAD_MS = 1000
    /** App 退到后台这么久没回来，就把模型从内存里放掉（约 200 MB） */
    private const val RELEASE_AFTER_MS = 3 * 60_000L
    private const val LOAD_TIMEOUT_MS = 30_000L

    interface Listener {
        /** 模型就绪、开始识别了 */
        fun onReady() {}
        /** 麦克风当前的音量（0～1），每 50 毫秒一次，给语音按钮的音量条用；clipped = 快爆音了（峰值接近满格） */
        fun onLevel(level: Float, clipped: Boolean) {}
        /** 这次按住到现在识别出的全部文字（已整理好空格），有变化就调用 */
        fun onText(text: String)
        /** 收尾完了：最终的全部文字（补好句末标点）；出了错 error 不为空 */
        fun onDone(text: String, error: String?)
    }

    private class Model(val dir: File, val recognizer: OnlineRecognizer) {
        var users = 0
        var retired = false
    }

    private val main = Handler(Looper.getMainLooper())
    private val lock = Any()
    private var current: Model? = null
    private var loading: File? = null
    private var failed: Pair<File, String>? = null
    /** 解码串行：上一次松手后的收尾和下一次按住可能同时在算 */
    private val decodeLock = Any()
    private val releaseIdle = Runnable { synchronized(lock) { current?.let(::retire) } }

    fun modelsRoot(context: Context) = File(context.getExternalFilesDir(null), "models")

    /** 手机上装好的模型（文件齐全的目录），按名字排序 */
    fun models(context: Context): List<File> =
        modelsRoot(context).listFiles()?.filter { it.isDirectory && files(it) != null }?.sortedBy { it.name }.orEmpty()

    fun isLoaded(dir: File) = synchronized(lock) { current?.dir == dir }

    /** 模型每攒多长的声音识别一次（毫秒），也就是出字的间隔：看目录名（x-asr-960ms → 960），看不出来 = 0 */
    fun chunkMs(dir: File): Long = Regex("""(\d+)ms""").find(dir.name)?.groupValues?.get(1)?.toLongOrNull() ?: 0

    /** 后台加载模型，第一次要两三秒；已经加载或正在加载就什么都不做。 */
    fun preload(dir: File) {
        main.removeCallbacks(releaseIdle)
        synchronized(lock) {
            if (current?.dir == dir || loading == dir) return
            loading = dir
            failed = null
        }
        thread(name = "asr-load", isDaemon = true) { load(dir) }
    }

    /** App 退到后台时调用：过一阵还没回来就释放模型，回来时 preload 会取消释放。 */
    fun releaseLater() {
        main.removeCallbacks(releaseIdle)
        main.postDelayed(releaseIdle, RELEASE_AFTER_MS)
    }

    /** 不用本地识别了（改用系统语音引擎）：马上释放模型，正在加载的加载完也不要。 */
    fun unload() {
        main.removeCallbacks(releaseIdle)
        synchronized(lock) {
            loading = null
            current?.let(::retire)
        }
    }

    private class Files(val tokens: File, val encoder: File, val decoder: File, val joiner: File)

    private fun files(dir: File): Files? {
        val onnx = dir.listFiles { f -> f.name.endsWith(".onnx") }?.toList() ?: return null
        // 有量化版（int8）就用量化版：小一半多、更快，准确率几乎一样
        fun pick(part: String) = onnx.filter { part in it.name }.minByOrNull { if ("int8" in it.name) 0 else 1 }
        val tokens = File(dir, "tokens.txt").takeIf { it.isFile } ?: return null
        return Files(tokens, pick("encoder") ?: return null, pick("decoder") ?: return null, pick("joiner") ?: return null)
    }

    private fun load(dir: File) {
        val t0 = SystemClock.uptimeMillis()
        val result = runCatching {
            val f = files(dir) ?: error("模型文件不全")
            OnlineRecognizer(
                config = OnlineRecognizerConfig(
                    featConfig = FeatureConfig(sampleRate = SAMPLE_RATE, featureDim = 80),
                    modelConfig = OnlineModelConfig(
                        transducer = OnlineTransducerModelConfig(
                            encoder = f.encoder.path,
                            decoder = f.decoder.path,
                            joiner = f.joiner.path,
                        ),
                        tokens = f.tokens.path,
                        numThreads = 2,
                    ),
                    enableEndpoint = false, // 不断句、不重置，见类注释
                    decodingMethod = "greedy_search",
                ),
            )
        }
        synchronized(lock) {
            if (loading != dir) { // 加载期间换了模型，这个不要了
                result.getOrNull()?.release()
                return
            }
            loading = null
            result.onSuccess { recognizer ->
                current?.let(::retire)
                current = Model(dir, recognizer)
            }.onFailure { failed = dir to (it.message ?: it.javaClass.simpleName) }
        }
        result.onSuccess { Log.i(LinkService.TAG, "语音模型 ${dir.name} 加载完成，用时 ${SystemClock.uptimeMillis() - t0} ms") }
            .onFailure { Log.e(LinkService.TAG, "语音模型 ${dir.name} 加载失败", it) }
    }

    private fun acquire(dir: File): Model? = synchronized(lock) { current?.takeIf { it.dir == dir }?.also { it.users++ } }

    private fun release(model: Model) = synchronized(lock) {
        model.users--
        if (model.retired && model.users == 0) model.recognizer.release()
    }

    /** 不再给新的识别用；正在用它的识别结束后才真正释放。调用时要持有 lock。 */
    private fun retire(model: Model) {
        model.retired = true
        if (current === model) current = null
        if (model.users == 0) model.recognizer.release()
    }

    private fun loadState(dir: File): Pair<Boolean, String?> = synchronized(lock) {
        (loading == dir) to failed?.takeIf { it.first == dir }?.second
    }

    // ───────── 文字整理 ─────────

    private const val CJK = "\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff"
    private const val CJK_PUNCT = "，。！？；：、（）《》〈〉【】「」『』“”‘’…—"
    private val SPACE_IN_CJK = Regex("(?<=[$CJK$CJK_PUNCT])\\s+(?=[$CJK$CJK_PUNCT])")
    private val SPACE_BEFORE_PUNCT = Regex("\\s+(?=[,.!?;:%)\\]}，。！？；：、）》〉】」』”’…])")
    private val HAS_CJK = Regex("[$CJK]")
    private const val ENDINGS = "，。！？；：、…,.!?;:"

    /**
     * X-ASR 的分词会在汉字、中文标点之间和标点前面留空格，去掉；中英文之间的空格保留。
     * 末尾的空格也去掉：它该不该留要看下一个字，这样整理后的文字才只往后追加。
     */
    fun normalize(raw: String): String =
        raw.replace(SPACE_IN_CJK, "").replace(SPACE_BEFORE_PUNCT, "").trim().trimStart { it in ENDINGS }

    /** 补句末标点：句子之间的「。」要听到下一句才出，最后一句常常没有 */
    fun withEnding(text: String): String {
        if (text.isEmpty() || text.last() in ENDINGS) return text
        return text + if (HAS_CJK.containsMatchIn(text)) "。" else "."
    }

    // ───────── 一次按住（或锁定）的识别 ─────────

    /**
     * 录音和识别各一个线程：识别偶尔慢一点也不会丢音。模型还没加载好时录到的音先攒着，好了再一起识别。
     * 回调都在主线程。wav 不为空时（调试用）不录音，按实际速度把这个文件当麦克风输入，放完自己收尾。
     */
    class Dictation(private val dir: File, private val listener: Listener, private val wav: File? = null) {
        @Volatile private var stopping = false
        @Volatile private var cancelled = false
        @Volatile private var micError: String? = null
        private val queue = LinkedBlockingQueue<FloatArray>()
        private val end = FloatArray(0)

        fun start() {
            preload(dir)
            thread(name = "asr-mic", isDaemon = true) { if (wav != null) replay(wav) else record() }
            thread(name = "asr-decode", isDaemon = true) { decode() }
        }

        /** 说完了：停止录音，把剩下的识别完，结果从 onDone 回来。 */
        fun finish() {
            stopping = true
        }

        /** 不要了：停止录音，之后不再回调。 */
        fun cancel() {
            cancelled = true
            stopping = true
        }

        @SuppressLint("MissingPermission") // 按下时 VoiceButton 已经确认有麦克风权限
        private fun record() {
            var rec: AudioRecord? = null
            try {
                val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
                rec = AudioRecord(
                    MediaRecorder.AudioSource.VOICE_RECOGNITION, SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT,
                    maxOf(minBuffer, SAMPLE_RATE * 2 * 2), // 2 秒的缓冲
                )
                if (rec.state != AudioRecord.STATE_INITIALIZED) {
                    micError = "麦克风打不开"
                    return
                }
                rec.startRecording()
                if (rec.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                    micError = "麦克风被别的应用占用"
                    return
                }
                val buf = ShortArray(SAMPLE_RATE / 20) // 每次读 50 毫秒，音量条更跟手
                while (!stopping) {
                    val n = rec.read(buf, 0, buf.size)
                    if (n < 0) {
                        micError = "录音出错（$n）"
                        break
                    }
                    if (n > 0) {
                        queue.put(FloatArray(n) { buf[it] / 32768f })
                        reportLevel(buf, n)
                    }
                }
                // 松手那一刻录音缓冲里还有一点没读出来，读完再停
                while (!cancelled) {
                    val n = rec.read(buf, 0, buf.size, AudioRecord.READ_NON_BLOCKING)
                    if (n <= 0) break
                    queue.put(FloatArray(n) { buf[it] / 32768f })
                }
            } catch (e: Exception) {
                micError = "麦克风打不开（${e.message ?: e.javaClass.simpleName}）"
            } finally {
                rec?.let {
                    runCatching { it.stop() }
                    it.release()
                }
                queue.put(end)
            }
        }

        /** 算这一小段的音量：-58 dB（安静）～ -12 dB（很响）映射到 0～1；峰值到满格的 97% 算快爆音 */
        private fun reportLevel(buf: ShortArray, n: Int) {
            var sum = 0.0
            var peak = 0
            for (i in 0 until n) {
                val v = buf[i].toInt()
                val s = v / 32768.0
                sum += s * s
                peak = maxOf(peak, if (v < 0) -v else v)
            }
            val db = 10 * log10(sum / n + 1e-12)
            val level = ((db + 58) / 46).toFloat().coerceIn(0f, 1f)
            val clipped = peak >= 31785
            main.post { if (!cancelled) listener.onLevel(level, clipped) }
        }

        /** 调试用：16 kHz、16 位、单声道的 wav，每 0.1 秒喂 0.1 秒，和真录音一个节奏 */
        private fun replay(file: File) {
            try {
                val bytes = ByteBuffer.wrap(file.readBytes()).order(ByteOrder.LITTLE_ENDIAN)
                var at = 12 // 跳过 RIFF 头，找 data 块
                while (at + 8 <= bytes.limit() && String(bytes.array(), at, 4, Charsets.US_ASCII) != "data") {
                    at += 8 + bytes.getInt(at + 4)
                }
                bytes.position(at + 8)
                val pcm = bytes.slice().order(ByteOrder.LITTLE_ENDIAN).asShortBuffer()
                val chunk = ShortArray(SAMPLE_RATE / 10)
                while (!stopping && pcm.hasRemaining()) {
                    val n = minOf(chunk.size, pcm.remaining())
                    pcm.get(chunk, 0, n)
                    queue.put(FloatArray(n) { chunk[it] / 32768f })
                    reportLevel(chunk, n)
                    SystemClock.sleep(100)
                }
            } catch (e: Exception) {
                micError = "读不了 ${file.name}：${e.message}"
            } finally {
                queue.put(end)
            }
        }

        private fun decode() {
            var model: Model? = null
            var stream: OnlineStream? = null
            var text = ""
            var audioSamples = 0L
            var decodeMs = 0L
            try {
                // 1. 等模型：刚打开 App 时要加载两三秒，这期间录到的音先攒着
                val early = ArrayList<FloatArray>()
                var ended = false
                val waitFrom = SystemClock.uptimeMillis()
                while (model == null) {
                    if (cancelled) return
                    model = acquire(dir)
                    if (model != null) break
                    val (loading, error) = loadState(dir)
                    val failure = when {
                        error != null -> "语音模型加载失败：$error"
                        ended && early.isEmpty() -> micError ?: "" // 什么都没录到，不用等模型了
                        SystemClock.uptimeMillis() - waitFrom > LOAD_TIMEOUT_MS -> "语音模型加载太久，先停下了"
                        else -> null
                    }
                    if (failure != null) {
                        stopping = true
                        done("", failure.ifEmpty { null })
                        return
                    }
                    if (!loading) preload(dir) // 刚好被释放了
                    val chunk = queue.poll(50, TimeUnit.MILLISECONDS) ?: continue
                    if (chunk === end) ended = true else early += chunk
                }
                val recognizer = (model ?: return).recognizer
                val s = recognizer.createStream().also { stream = it }
                main.post { if (!cancelled) listener.onReady() }

                fun feed(chunk: FloatArray) {
                    s.acceptWaveform(chunk, SAMPLE_RATE)
                    audioSamples += chunk.size
                }

                fun decodeReady(): String {
                    val t = SystemClock.uptimeMillis()
                    val raw = synchronized(decodeLock) {
                        while (recognizer.isReady(s)) recognizer.decode(s)
                        recognizer.getResult(s).text
                    }
                    decodeMs += SystemClock.uptimeMillis() - t
                    return normalize(raw)
                }

                fun report(latest: String) {
                    if (latest == text) return
                    text = latest
                    main.post { if (!cancelled) listener.onText(latest) }
                }

                early.forEach(::feed)
                if (early.isNotEmpty()) report(decodeReady())

                // 2. 边录边识别
                while (!ended) {
                    val chunk = queue.take()
                    if (cancelled) return
                    if (chunk === end) break
                    feed(chunk)
                    report(decodeReady())
                }

                // 3. 收尾：补一段静音、告诉它说完了，把最后几个字（和问句的「？」）挤出来
                val t = SystemClock.uptimeMillis()
                feed(FloatArray(SAMPLE_RATE * TAIL_PAD_MS / 1000))
                s.inputFinished()
                val final = withEnding(decodeReady())
                val seconds = (audioSamples - SAMPLE_RATE * TAIL_PAD_MS / 1000) / SAMPLE_RATE.toFloat()
                Log.i(
                    LinkService.TAG,
                    "本地识别：录音 %.1f 秒，解码共 %d ms（RTF %.3f），收尾 %d ms：「%s」".format(
                        seconds, decodeMs, decodeMs / 1000f / maxOf(seconds, 0.1f), SystemClock.uptimeMillis() - t, final,
                    ),
                )
                done(final, micError)
            } catch (e: Exception) {
                Log.e(LinkService.TAG, "本地识别出错", e)
                stopping = true
                done(withEnding(text), "语音识别出错：${e.message ?: e.javaClass.simpleName}")
            } finally {
                stream?.release()
                model?.let(::release)
            }
        }

        private fun done(text: String, error: String?) {
            main.post { if (!cancelled) listener.onDone(text, error) }
        }
    }
}
