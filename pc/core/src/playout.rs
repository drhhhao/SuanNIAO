//! 触控板移动的匀速回放（抖动缓冲）。
//!
//! 蓝牙送消息不匀：常常三四十毫秒一条都不来，接着一毫秒内涌进来好几条。这是电脑这头的蓝牙控制器排的时间，
//! 程序管不了。照收到的节奏走指针，就会一顿一跳。手机在每条移动里带上手指移动的时间（ts），
//! 这边把每段移动摊回它在手机上用的那段时间里，整体晚一点（lag）匀速放：消息只要在轮到它之前到了，
//! 指针就和手指在手机上划的一样匀速。
//!
//! lag 自己调：取最近几百条消息「到达 − 这段移动开始」的高分位数，蓝牙稳就小、抖得厉害就大（有上限）。
//! 来晚了（前面的放完了，下一条还没到）就停在原地等，到了从停下的地方接着放，不跳；
//! 这样多出来的延迟，之后稍微放快一点追回来。
//! 时间都是毫秒。「到达」是电脑上的时间，ts 是手机上的时间：两边的钟差也算在 lag 里，不用对时。

use std::collections::VecDeque;

/// 记最近这么多条消息，按它们算 lag
const HISTORY: usize = 600;
/// 记的还不到这么多条时，摸不清蓝牙抖得多厉害，先多等一点
const WARMUP: usize = 30;
const WARMUP_WAIT: f64 = 30.0;
/// 一段移动最多摊这么长：和上一段隔得更久（刚放上手指、停过一会儿）就只摊 SPAN_NEW
const SPAN_MAX: f64 = 25.0;
const SPAN_NEW: f64 = 8.0;
/// 「到达 − 开始」突然差出这么多：钟跳了（换了手机、手机重启过），以前记的作废
const CLOCK_JUMP: f64 = 1000.0;
/// 旧版手机 App 不带 ts：拿到达时间顶上，一起涌进来的几条至少隔开 NO_TS_GAP
const NO_TS_GAP: f64 = 4.0;
const NO_TS_JOIN: f64 = 100.0;

#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// lag 取「到达 − 开始」的这个分位数：越大停顿越少、延迟越大
    pub quantile: f64,
    /// lag 最多比最快的那条多这么多：蓝牙再抖，延迟也不超过这个
    pub max_lag: f64,
    /// 多等了的延迟按这个时间常数追回来
    pub catch_up: f64,
    /// 追的时候最多快这么多（0.3 = 1.3 倍速）
    pub max_speed_up: f64,
    /// 要多等一点时最多慢这么多
    pub max_slow_down: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { quantile: 0.98, max_lag: 50.0, catch_up: 200.0, max_speed_up: 0.3, max_slow_down: 0.1 }
    }
}

/// 一段移动：手机时间 start..end 之间匀速走完 (dx, dy)，已经走了 done（0～1）
struct Span {
    start: f64,
    end: f64,
    dx: f64,
    dy: f64,
    done: f64,
}

pub struct Playout {
    tuning: Tuning,
    spans: VecDeque<Span>,
    /// 最新一段移动结束的手机时间
    last_ts: Option<f64>,
    /// 上一条到达的时间（不带 ts 时用）
    last_arrival: f64,
    /// 最近的「到达 − 这段开始」，其中最快的一条，和按它们定的 lag 目标
    history: VecDeque<f64>,
    fastest: f64,
    target: f64,
    /// 正在放的手机时间 = 现在 − lag
    lag: f64,
    last_need: f64,
    last_take: f64,
}

impl Playout {
    pub fn new(tuning: Tuning) -> Self {
        Self {
            tuning,
            spans: VecDeque::new(),
            last_ts: None,
            last_arrival: f64::NEG_INFINITY,
            history: VecDeque::new(),
            fastest: 0.0,
            target: 0.0,
            lag: 0.0,
            last_need: 0.0,
            last_take: 0.0,
        }
    }

    /// 收到一段移动。now：到达时间；ts：这段移动最后一个触摸点的手机时间（旧版手机 App 不带）
    pub fn push(&mut self, now: f64, ts: Option<f64>, dx: f64, dy: f64) {
        let ts = ts.unwrap_or(match self.last_ts {
            Some(last) if now - self.last_arrival < NO_TS_JOIN => now.max(last + NO_TS_GAP),
            _ => now,
        });
        self.last_arrival = now;
        let end = self.last_ts.map_or(ts, |last| ts.max(last));
        let start = match self.last_ts {
            Some(last) if end - last <= SPAN_MAX => last,
            _ => end - SPAN_NEW,
        };
        self.last_ts = Some(end);
        let need = now - start;
        self.learn(need);
        if self.spans.is_empty() {
            // 前面的都放完了：这是新的一下，或者这条来晚了。按目标延迟放；已经晚了就从现在开始放，不跳
            self.lag = self.target.max(need);
            self.last_take = now;
        }
        self.spans.push_back(Span { start, end, dx, dy, done: 0.0 });
    }

    fn learn(&mut self, need: f64) {
        if !self.history.is_empty() && (need - self.fastest).abs() > CLOCK_JUMP {
            self.history.clear();
        }
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(need);
        let mut sorted: Vec<f64> = self.history.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        self.fastest = sorted[0];
        let mut target = sorted[((sorted.len() - 1) as f64 * self.tuning.quantile).round() as usize];
        if sorted.len() < WARMUP {
            target = target.max(self.fastest + WARMUP_WAIT);
        }
        self.target = target.min(self.fastest + self.tuning.max_lag);
        self.last_need = need;
    }

    /// 到 now 为止该走的位移
    pub fn take(&mut self, now: f64) -> (f64, f64) {
        let dt = (now - self.last_take).clamp(0.0, 50.0);
        self.last_take = now;
        if self.spans.is_empty() {
            return (0.0, 0.0);
        }
        // 比目标多等了：稍微放快一点追回来；蓝牙变抖、目标加大了：稍微放慢一点多攒些
        let excess = self.lag - self.target;
        let rate = (excess / self.tuning.catch_up).clamp(-self.tuning.max_slow_down, self.tuning.max_speed_up);
        let step = rate * dt;
        self.lag -= if excess > 0.0 { step.min(excess) } else { step.max(excess) };
        let at = now - self.lag;
        let (mut x, mut y) = (0.0, 0.0);
        while let Some(span) = self.spans.front_mut() {
            let reach = if at >= span.end {
                1.0
            } else if at <= span.start {
                0.0
            } else {
                (at - span.start) / (span.end - span.start)
            };
            if reach > span.done {
                x += span.dx * (reach - span.done);
                y += span.dy * (reach - span.done);
                span.done = reach;
            }
            if reach < 1.0 {
                break;
            }
            self.spans.pop_front();
        }
        (x, y)
    }

    /// 剩下没放的一次给出来（点击、滚轮之前，免得点在半路上）
    pub fn flush(&mut self) -> (f64, f64) {
        let (mut x, mut y) = (0.0, 0.0);
        for span in self.spans.drain(..) {
            x += span.dx * (1.0 - span.done);
            y += span.dy * (1.0 - span.done);
        }
        (x, y)
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// 最近一条比最快的那条晚了多少、lag 的目标和现在的 lag 比最快的那条多等多少（ms）
    pub fn stats(&self) -> [f64; 3] {
        [self.last_need - self.fastest, self.target - self.fastest, self.lag - self.fastest]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手机上匀速往右划（每 ms speed 像素），每 period ms 发一条；arrive(i, ts) 给出第 i 条到电脑的时间。
    /// 每 1 ms 取一次，返回每次走的距离
    fn simulate(p: &mut Playout, count: usize, period: f64, speed: f64, arrive: impl Fn(usize, f64) -> f64) -> Vec<f64> {
        let msgs: Vec<(f64, f64)> = (0..count)
            .map(|i| {
                let ts = 5000.0 + (i + 1) as f64 * period;
                (arrive(i, ts), ts)
            })
            .collect();
        let mut out = Vec::new();
        let mut next = 0;
        let mut t = msgs[0].0.floor();
        while next < msgs.len() || !p.is_empty() {
            while next < msgs.len() && msgs[next].0 <= t {
                p.push(msgs[next].0, Some(msgs[next].1), speed * period, 0.0);
                next += 1;
            }
            out.push(p.take(t).0);
            t += 1.0;
        }
        out
    }

    #[test]
    fn bursty_link_plays_evenly() {
        // 每 8 ms 一条，但每 40 ms 才一起到一次（和这台电脑的蓝牙一样）
        let mut p = Playout::new(Tuning::default());
        let out = simulate(&mut p, 500, 8.0, 1.0, |i, ts| {
            let batch_end = ((i / 5) * 5 + 5) as f64 * 8.0 + 5000.0;
            let _ = ts;
            batch_end + 3.0 + 100_000.0
        });
        let total: f64 = out.iter().sum();
        assert!((total - 4000.0).abs() < 1e-6, "total {total}");
        // 跳过开头 1 秒（还在摸蓝牙的节奏）和结尾，中间每毫秒都该走 1 像素左右
        let middle = &out[1000..3500];
        let (lo, hi) = middle.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(lo > 0.85 && hi < 1.15, "per-ms motion {lo}..{hi}");
    }

    #[test]
    fn late_message_pauses_without_jump() {
        let mut p = Playout::new(Tuning::default());
        // 稳定 5 ms 送到；第 300 条晚到 150 ms（之后的排在它后面一起到）
        let out = simulate(&mut p, 500, 8.0, 1.0, |i, ts| {
            let late = if (300..319).contains(&i) { 150.0 - (i - 300) as f64 * 8.0 } else { 0.0 };
            ts + 5.0 + late.max(0.0) + 100_000.0
        });
        let total: f64 = out.iter().sum();
        assert!((total - 4000.0).abs() < 1e-6, "total {total}");
        let max = out.iter().cloned().fold(0.0, f64::max);
        assert!(max < 1.0 + Tuning::default().max_speed_up + 0.05, "jumped {max} px in 1 ms");
    }

    #[test]
    fn flush_gives_the_rest() {
        let mut p = Playout::new(Tuning::default());
        p.push(100.0, Some(10.0), 5.0, -3.0);
        p.push(101.0, Some(18.0), 5.0, -3.0);
        let first = p.take(101.0);
        let rest = p.flush();
        assert!(p.is_empty());
        assert!((first.0 + rest.0 - 10.0).abs() < 1e-9 && (first.1 + rest.1 + 6.0).abs() < 1e-9);
    }

    #[test]
    fn without_timestamps_still_spreads() {
        let mut p = Playout::new(Tuning::default());
        let mut out = Vec::new();
        for burst in 0..50 {
            let t = burst as f64 * 40.0;
            for _ in 0..5 {
                p.push(t, None, 8.0, 0.0);
            }
            for k in 0..40 {
                out.push(p.take(t + k as f64).0);
            }
        }
        while !p.is_empty() {
            out.push(p.take(2000.0 + out.len() as f64).0);
        }
        let total: f64 = out.iter().sum();
        assert!((total - 2000.0).abs() < 1e-6, "total {total}");
        let max = out.iter().cloned().fold(0.0, f64::max);
        assert!(max < 8.0, "a whole burst came out at once ({max} px)");
    }

    #[test]
    fn clock_jump_relearns() {
        let mut p = Playout::new(Tuning::default());
        simulate(&mut p, 200, 8.0, 1.0, |_, ts| ts + 5.0);
        // 手机重启：手机时间从头开始
        p.push(1_000_000.0, Some(10.0), 8.0, 0.0);
        let [_, target, lag] = p.stats();
        assert!(target <= WARMUP_WAIT + 1e-9 && lag <= WARMUP_WAIT + 1e-9, "target {target} lag {lag}");
    }

    /// 拿 PHONEPAD_TRACE 记下的真实时序离线回放，调参数用。输出和 trace 一样的格式（cursor 换成回放的结果）：
    /// PLAYOUT_REPLAY=trace.txt PLAYOUT_OUT=out.txt cargo test -p phonepad-core replay -- --ignored
    /// 可选 PLAYOUT_Q、PLAYOUT_MAX_LAG、PLAYOUT_CATCH_UP、PLAYOUT_SPEED_UP、PLAYOUT_SLOW_DOWN 覆盖默认参数
    #[test]
    #[ignore]
    fn replay() {
        let var = |name: &str| std::env::var(name).ok();
        let mut tuning = Tuning::default();
        for (name, field) in [
            ("PLAYOUT_Q", &mut tuning.quantile),
            ("PLAYOUT_MAX_LAG", &mut tuning.max_lag),
            ("PLAYOUT_CATCH_UP", &mut tuning.catch_up),
            ("PLAYOUT_SPEED_UP", &mut tuning.max_speed_up),
            ("PLAYOUT_SLOW_DOWN", &mut tuning.max_slow_down),
        ] {
            if let Some(v) = var(name) {
                *field = v.parse().unwrap();
            }
        }
        let text = std::fs::read_to_string(var("PLAYOUT_REPLAY").expect("PLAYOUT_REPLAY")).unwrap();
        let mut p = Playout::new(tuning);
        let mut out = String::new();
        let mut fraction = (0.0, 0.0);
        let mut emit = |out: &mut String, t: f64, (dx, dy): (f64, f64)| {
            fraction.0 += dx;
            fraction.1 += dy;
            let (ix, iy) = (f64::trunc(fraction.0), f64::trunc(fraction.1));
            fraction.0 -= ix;
            fraction.1 -= iy;
            if ix != 0.0 || iy != 0.0 {
                out.push_str(&format!("{:.0} cursor {ix} {iy}\n", t * 1000.0));
            }
        };
        let mut tick: Option<f64> = None;
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 || parts[1] == "cursor" || parts[1] == "play" {
                continue;
            }
            let t = parts[0].parse::<f64>().unwrap() / 1000.0;
            let mut at = tick.unwrap_or(t.floor());
            while at <= t {
                let step = p.take(at);
                emit(&mut out, at, step);
                at += 1.0;
            }
            tick = Some(at);
            out.push_str(line);
            out.push('\n');
            let num = |i: usize| parts.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            match parts[1] {
                "move" => {
                    let ts = num(4);
                    p.push(t, (ts >= 0.0).then_some(ts), num(2), num(3));
                    let [need, target, lag] = p.stats();
                    out.push_str(&format!("{:.0} play {need:.2} {target:.2} {lag:.2}\n", t * 1000.0));
                }
                "mouse" | "scroll" => {
                    let rest = p.flush();
                    emit(&mut out, t, rest);
                }
                _ => {}
            }
        }
        let mut at = tick.unwrap_or(0.0);
        while !p.is_empty() {
            let step = p.take(at);
            emit(&mut out, at, step);
            at += 1.0;
        }
        std::fs::write(var("PLAYOUT_OUT").expect("PLAYOUT_OUT"), out).unwrap();
    }
}
