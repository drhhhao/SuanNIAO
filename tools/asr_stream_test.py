"""在电脑上模拟手机的流式识别：按 100 ms 一块实时喂音频，看出字节奏、标点、断句，
以及规范化后的文字是不是只往后追加（边说边打字要靠这一点）。

用法：python asr_stream_test.py <模型目录> <wav> [rule2 秒数]
"""
import re
import sys
import time
import wave

import numpy as np
import sherpa_onnx

CJK = "㐀-䶿一-鿿豈-﫿"
CJK_PUNCT = re.escape("，。！？；：、（）《》〈〉【】「」『』“”‘’")
CLOSING_PUNCT = re.escape(",.!?;:%)]}，。！？；：、）》〉】」』”’…")


TAIL_PAD = 1.0


def normalize(text: str) -> str:
    """和手机端 LocalAsr.normalize 一致：去掉汉字/中文标点之间、标点前面的空格。"""
    text = re.sub(rf"(?<=[{CJK}{CJK_PUNCT}])\s+(?=[{CJK}{CJK_PUNCT}])", "", text)
    text = re.sub(rf"\s+(?=[{CLOSING_PUNCT}])", "", text)
    return text.strip()


def load(path):
    with wave.open(path) as w:
        sr, n, ch = w.getframerate(), w.getnframes(), w.getnchannels()
        data = np.frombuffer(w.readframes(n), dtype=np.int16).astype(np.float32) / 32768
    if ch > 1:
        data = data.reshape(-1, ch)[:, 0]
    return data, sr


def main():
    model_dir, wav = sys.argv[1], sys.argv[2]
    rule2 = float(sys.argv[3]) if len(sys.argv) > 3 else 0.8
    global TAIL_PAD
    TAIL_PAD = float(sys.argv[4]) if len(sys.argv) > 4 else 1.0
    t0 = time.perf_counter()
    rec = sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=f"{model_dir}/tokens.txt",
        encoder=f"{model_dir}/encoder.int8.onnx",
        decoder=f"{model_dir}/decoder.onnx",
        joiner=f"{model_dir}/joiner.int8.onnx",
        num_threads=2,
        sample_rate=16000,
        feature_dim=80,
        decoding_method="greedy_search",
        enable_endpoint_detection=rule2 > 0,
        rule1_min_trailing_silence=2.4,
        rule2_min_trailing_silence=rule2,
        rule3_min_utterance_length=30,
    )
    print(f"加载模型 {time.perf_counter() - t0:.2f}s")
    samples, sr = load(wav)
    stream = rec.create_stream()
    step = sr // 10
    shown, raw_last = "", ""
    decode_time, non_monotonic = 0.0, 0
    for i in range(0, len(samples), step):
        stream.accept_waveform(sr, samples[i:i + step])
        t0 = time.perf_counter()
        while rec.is_ready(stream):
            rec.decode_stream(stream)
        decode_time += time.perf_counter() - t0
        raw = rec.get_result(stream)
        text = normalize(raw)
        if text != shown:
            if not text.startswith(shown):
                non_monotonic += 1
                print(f"  !! 不是追加：{shown!r} -> {text!r}")
            print(f"{(i + step) / sr:6.2f}s  +{text[len(shown):]!r}")
            shown, raw_last = text, raw
        if rule2 > 0 and rec.is_endpoint(stream):
            if shown:
                # 句末标点要看到后面的静音才出：补一段静音、告诉它输入结束，把最后的标点挤出来
                t0 = time.perf_counter()
                stream.accept_waveform(sr, np.zeros(int(sr * TAIL_PAD), dtype=np.float32))
                stream.input_finished()
                while rec.is_ready(stream):
                    rec.decode_stream(stream)
                flushed = normalize(rec.get_result(stream))
                decode_time += time.perf_counter() - t0
                if not flushed.startswith(shown):
                    non_monotonic += 1
                print(f"{(i + step) / sr:6.2f}s  [断句] {flushed}   （收尾补出 {flushed[len(shown):]!r}，{(time.perf_counter() - t0) * 1000:.0f} ms）")
            stream = rec.create_stream()
            shown = ""
    stream.accept_waveform(sr, np.zeros(sr, dtype=np.float32))
    stream.input_finished()
    while rec.is_ready(stream):
        rec.decode_stream(stream)
    text = normalize(rec.get_result(stream))
    if text:
        print(f"[结束] {text}")
    print(f"音频 {len(samples) / sr:.1f}s，解码 {decode_time:.2f}s，RTF {decode_time / (len(samples) / sr):.3f}，非追加 {non_monotonic} 次")


if __name__ == "__main__":
    main()
