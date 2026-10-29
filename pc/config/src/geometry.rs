//! 布局的几何计算。模块的位置和大小以「格」为单位，可以是小数（旧配置里的整数照样能读）。
//!
//! 电脑端主界面和手机端（LayoutEditor.kt）用同一套规则：
//! - 拖动时默认对齐到 1/4 格；电脑上按住 Alt、手机上关掉「对齐」就不对齐（精确到 0.01 格）；
//! - 宽、高最小 0.5 格；
//! - 模块之间不能重叠，也不能超出网格。

/// 拖动时对齐到几分之一格
pub const SNAP: f64 = 0.25;
/// 模块最小的宽、高（格）
pub const MIN_SIZE: f64 = 0.5;
/// 比较时的容差：两个模块只是碰到边、或者差一点点，不算重叠
pub const EPS: f64 = 0.004;
/// 移动到放不下的地方时，在这么近（格）的范围里找最近的空位
pub const NUDGE: f64 = 0.6;

/// 拖的是哪条边（两条边组合起来就是角）；0 表示拖中间、整体移动
pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;
pub const TOP: u8 = 4;
pub const BOTTOM: u8 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn area(&self) -> f64 {
        self.w * self.h
    }

    /// 和另一块有没有重叠（只碰到边不算）
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.right() - EPS && o.x < self.right() - EPS && self.y < o.bottom() - EPS && o.y < self.bottom() - EPS
    }

    /// 在 columns × rows 的网格里，而且不小于最小尺寸
    pub fn inside(&self, columns: f64, rows: f64) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite())
            && self.x >= -EPS
            && self.y >= -EPS
            && self.right() <= columns + EPS
            && self.bottom() <= rows + EPS
            && self.w >= MIN_SIZE - EPS
            && self.h >= MIN_SIZE - EPS
    }

    /// 四个数都取到 0.01 格，存进配置时不会出现 0.30000000000000004
    pub fn tidy(self) -> Self {
        Self { x: fine(self.x), y: fine(self.y), w: fine(self.w), h: fine(self.h) }
    }
}

/// 取到 0.01 格
pub fn fine(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 对齐到 1/4 格；snap = false 时只取到 0.01 格
pub fn snap_to(v: f64, snap: bool) -> f64 {
    if snap {
        (v / SNAP).round() * SNAP
    } else {
        fine(v)
    }
}

/// 放在这里会不会和 others 里的任何一块重叠
pub fn is_free(rect: &Rect, others: &[Rect]) -> bool {
    !others.iter().any(|o| rect.overlaps(o))
}

/// 拖动中：模块原来在 start，鼠标（手指）移动了 (dx, dy) 格，算出它现在应该在哪。
///
/// - edges：要动的边（LEFT / RIGHT / TOP / BOTTOM 的组合），0 表示整体移动；
/// - snap：对齐到 1/4 格；
/// - others：同一页的其他模块。
///
/// 改大小时，动的那条边对齐后不出网格、不小于最小尺寸，碰到别的模块就停在它旁边，所以总能放下；
/// 移动时对齐后不出网格，放不下就在附近（NUDGE 格以内）找最近的空位，找不到返回 ok = false，
/// 这时的结果是鼠标所在的位置，界面上画成红框，松手不动。
pub fn drag(start: Rect, edges: u8, dx: f64, dy: f64, snap: bool, others: &[Rect], columns: f64, rows: f64) -> (Rect, bool) {
    if edges == 0 {
        let want = Rect {
            x: snap_to(start.x + dx, snap).min(columns - start.w).max(0.0),
            y: snap_to(start.y + dy, snap).min(rows - start.h).max(0.0),
            ..start
        };
        if is_free(&want, others) {
            return (want.tidy(), true);
        }
        // 候选位置：紧贴别的模块的四条边、贴着网格边
        let mut xs = vec![want.x, 0.0, columns - start.w];
        let mut ys = vec![want.y, 0.0, rows - start.h];
        for o in others {
            xs.extend([o.x - start.w, o.right()]);
            ys.extend([o.y - start.h, o.bottom()]);
        }
        let mut best: Option<(f64, Rect)> = None;
        for &x in &xs {
            if x < -EPS || x + start.w > columns + EPS {
                continue;
            }
            for &y in &ys {
                if y < -EPS || y + start.h > rows + EPS {
                    continue;
                }
                let d = (x - want.x).hypot(y - want.y);
                let candidate = Rect { x, y, ..start };
                if d <= NUDGE && best.is_none_or(|(bd, _)| d < bd) && is_free(&candidate, others) {
                    best = Some((d, candidate));
                }
            }
        }
        return match best {
            Some((_, rect)) => (rect.tidy(), true),
            None => (want.tidy(), false),
        };
    }

    // 改大小：先按鼠标算出四条边，再处理碰撞
    let (mut l, mut r, mut t, mut b) = (start.x, start.right(), start.y, start.bottom());
    if edges & LEFT != 0 {
        l = snap_to(start.x + dx, snap).max(0.0).min(r - MIN_SIZE);
    }
    if edges & RIGHT != 0 {
        r = snap_to(start.right() + dx, snap).min(columns).max(l + MIN_SIZE);
    }
    if edges & TOP != 0 {
        t = snap_to(start.y + dy, snap).max(0.0).min(b - MIN_SIZE);
    }
    if edges & BOTTOM != 0 {
        b = snap_to(start.bottom() + dy, snap).min(rows).max(t + MIN_SIZE);
    }
    // 左右两条边：碰到「和 [t0, b0) 竖着有重叠」、原来就在这一侧的模块就停下
    let clamp_h = |mut l: f64, mut r: f64, t0: f64, b0: f64| {
        for o in others.iter().filter(|o| o.y < b0 - EPS && t0 < o.bottom() - EPS) {
            if edges & RIGHT != 0 && o.x >= start.right() - EPS {
                r = r.min(o.x);
            }
            if edges & LEFT != 0 && o.right() <= start.x + EPS {
                l = l.max(o.right());
            }
        }
        (l, r)
    };
    // 上下两条边同理
    let clamp_v = |l0: f64, r0: f64, mut t: f64, mut b: f64| {
        for o in others.iter().filter(|o| o.x < r0 - EPS && l0 < o.right() - EPS) {
            if edges & BOTTOM != 0 && o.y >= start.bottom() - EPS {
                b = b.min(o.y);
            }
            if edges & TOP != 0 && o.bottom() <= start.y + EPS {
                t = t.max(o.bottom());
            }
        }
        (t, b)
    };
    // 拖角时两个方向都要动：先横后竖、先竖后横各算一次，取面积大的（只动一条边时两种一样）
    let (l1, r1) = clamp_h(l, r, start.y, start.bottom());
    let (t1, b1) = clamp_v(l1, r1, t, b);
    let a = Rect::new(l1, t1, r1 - l1, b1 - t1);
    let (t2, b2) = clamp_v(start.x, start.right(), t, b);
    let (l2, r2) = clamp_h(l, r, t2, b2);
    let c = Rect::new(l2, t2, r2 - l2, b2 - t2);
    (if c.area() > a.area() + EPS { c } else { a }.tidy(), true)
}

/// 用加减按钮调一个数：往 dir（+1 / -1）方向走到下一个 1/4 格（本来不在 1/4 格上的先走到最近的那个）
pub fn step(v: f64, dir: i32) -> f64 {
    let k = v / SNAP;
    let next = if dir > 0 { (k + 0.02).floor() + 1.0 } else { (k - 0.02).ceil() - 1.0 };
    next * SNAP
}

/// 界面上显示的数：最多两位小数，去掉末尾的 0（1.5、0.25、2）
pub fn show(v: f64) -> String {
    let text = format!("{:.2}", fine(v));
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".into() } else { text.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<Rect> {
        // 4 × 8：上面一排两个 1×1，右边一个 1×2
        vec![Rect::new(0.0, 0.0, 1.0, 1.0), Rect::new(2.0, 0.0, 1.0, 1.0), Rect::new(3.0, 2.0, 1.0, 2.0)]
    }

    #[test]
    fn resize_right_stops_at_neighbour() {
        let start = Rect::new(0.0, 0.0, 1.0, 1.0);
        let others = &grid()[1..];
        let (r, ok) = drag(start, RIGHT, 2.6, 0.0, true, others, 4.0, 8.0);
        assert!(ok);
        assert_eq!(r, Rect::new(0.0, 0.0, 2.0, 1.0));
    }

    #[test]
    fn resize_snaps_and_keeps_min() {
        let start = Rect::new(0.0, 4.0, 2.0, 2.0);
        let (r, _) = drag(start, RIGHT, -0.62, 0.0, true, &[], 4.0, 8.0);
        assert_eq!(r.w, 1.5);
        let (r, _) = drag(start, RIGHT, -5.0, 0.0, true, &[], 4.0, 8.0);
        assert_eq!(r.w, MIN_SIZE);
        let (r, _) = drag(start, RIGHT, -0.62, 0.0, false, &[], 4.0, 8.0);
        assert_eq!(r.w, 1.38);
        let (r, _) = drag(start, TOP, -10.0, 0.0, true, &[], 4.0, 8.0);
        assert_eq!(r, start); // 竖着没动
        let (r, _) = drag(start, TOP, 0.0, -10.0, true, &[], 4.0, 8.0);
        assert_eq!(r, Rect::new(0.0, 0.0, 2.0, 6.0));
    }

    #[test]
    fn corner_resize_never_overlaps() {
        let start = Rect::new(1.0, 1.0, 1.0, 1.0);
        let others = vec![Rect::new(2.5, 2.5, 1.0, 1.0)];
        let (r, ok) = drag(start, RIGHT | BOTTOM, 3.0, 3.0, true, &others, 4.0, 8.0);
        assert!(ok);
        assert!(is_free(&r, &others), "{r:?}");
        assert!(r.area() > start.area());
    }

    #[test]
    fn move_into_neighbour_nudges_or_fails() {
        let others = vec![Rect::new(1.0, 0.0, 1.0, 1.0)];
        let start = Rect::new(0.0, 2.0, 1.0, 1.0);
        // 落在旁边模块上，差一点点：挤到它旁边
        let (r, ok) = drag(start, 0, 0.6, -1.9, true, &others, 4.0, 8.0);
        assert!(ok);
        assert!(is_free(&r, &others), "{r:?}");
        // 正好压在一个大模块中间：放不下
        let big = vec![Rect::new(0.0, 4.0, 4.0, 4.0)];
        let (_, ok) = drag(start, 0, 1.5, 3.5, true, &big, 4.0, 8.0);
        assert!(!ok);
    }

    #[test]
    fn steps_and_labels() {
        assert_eq!(step(1.0, 1), 1.25);
        assert_eq!(step(1.13, 1), 1.25);
        assert_eq!(step(1.13, -1), 1.0);
        assert_eq!(step(1.25, -1), 1.0);
        assert_eq!(show(1.5), "1.5");
        assert_eq!(show(2.0), "2");
        assert_eq!(show(0.25), "0.25");
        assert_eq!(show(1.1300000001), "1.13");
    }
}
