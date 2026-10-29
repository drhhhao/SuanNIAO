//! 三方合并：主界面里有还没保存的改动时，配置文件又被别处改了（比如在手机上调了布局），两边的改动都留下。

use serde_json::{Map, Value};

/// base 是上次和文件同步时的样子，mine 是主界面里改过的，theirs 是文件里现在的样子。
/// - 只有一边改了的地方，取改了的那边；
/// - 对象逐个键合并；元素都带 "id" 的数组（页面、模块）按 id 合并；别的数组（动作列表）当成一个值；
/// - 两边把同一个值改得不一样时以主界面为准（用户正看着它改）。
///
/// 返回 None 表示这一项被删掉了。
pub fn merge3(base: Option<&Value>, mine: Option<&Value>, theirs: Option<&Value>) -> Option<Value> {
    if mine == base {
        return theirs.cloned();
    }
    if theirs == base || mine == theirs {
        return mine.cloned();
    }
    match (mine, theirs) {
        (Some(Value::Object(m)), Some(Value::Object(t))) => {
            let b = base.and_then(Value::as_object);
            let mut out = Map::new();
            for key in m.keys().chain(t.keys().filter(|k| !m.contains_key(*k))) {
                if let Some(v) = merge3(b.and_then(|b| b.get(key)), m.get(key), t.get(key)) {
                    out.insert(key.clone(), v);
                }
            }
            Some(Value::Object(out))
        }
        (Some(Value::Array(m)), Some(Value::Array(t)))
            if keyed(m) && keyed(t) && base.is_none_or(|b| b.as_array().is_some_and(|a| keyed(a))) =>
        {
            let b = base.and_then(Value::as_array);
            // 顺序以主界面为准，文件里新加的接在后面
            let mut ids: Vec<&str> = m.iter().filter_map(id_of).collect();
            for id in t.iter().filter_map(id_of) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            let out = ids.into_iter().filter_map(|id| merge3(find(b, id), find(Some(m), id), find(Some(t), id))).collect();
            Some(Value::Array(out))
        }
        _ => mine.cloned(),
    }
}

/// 和 merge3 一样，但两边把同一个值改得不一样时算冲突（返回 Err）。
/// 撤销记录要合进别处的改动时用：冲突说明那一步和别处改的是同一个地方，不能撤销到别人的改动上
pub fn merge3_strict(base: Option<&Value>, mine: Option<&Value>, theirs: Option<&Value>) -> Result<Option<Value>, ()> {
    if mine == base {
        return Ok(theirs.cloned());
    }
    if theirs == base || mine == theirs {
        return Ok(mine.cloned());
    }
    match (mine, theirs) {
        (Some(Value::Object(m)), Some(Value::Object(t))) => {
            let b = base.and_then(Value::as_object);
            let mut out = Map::new();
            for key in m.keys().chain(t.keys().filter(|k| !m.contains_key(*k))) {
                if let Some(v) = merge3_strict(b.and_then(|b| b.get(key)), m.get(key), t.get(key))? {
                    out.insert(key.clone(), v);
                }
            }
            Ok(Some(Value::Object(out)))
        }
        (Some(Value::Array(m)), Some(Value::Array(t)))
            if keyed(m) && keyed(t) && base.is_none_or(|b| b.as_array().is_some_and(|a| keyed(a))) =>
        {
            let b = base.and_then(Value::as_array);
            let mut ids: Vec<&str> = m.iter().filter_map(id_of).collect();
            for id in t.iter().filter_map(id_of) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            let mut out = Vec::new();
            for id in ids {
                if let Some(v) = merge3_strict(find(b, id), find(Some(m), id), find(Some(t), id))? {
                    out.push(v);
                }
            }
            Ok(Some(Value::Array(out)))
        }
        _ => Err(()),
    }
}

/// 列表里 id 是这个的那一项
fn find<'a>(list: Option<&'a Vec<Value>>, id: &str) -> Option<&'a Value> {
    list?.iter().find(|v| id_of(v) == Some(id))
}

fn id_of(v: &Value) -> Option<&str> {
    v.get("id")?.as_str()
}

fn keyed(list: &[Value]) -> bool {
    list.iter().all(|v| id_of(v).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_both_sides() {
        let base = json!({"pages": [{"id": "p", "modules": [{"id": "a", "x": 0, "label": "A"}, {"id": "b", "x": 1, "label": "B"}]}], "s": 1});
        // 主界面改了 a 的名字和设置，手机挪了 b
        let mine = json!({"pages": [{"id": "p", "modules": [{"id": "a", "x": 0, "label": "A2"}, {"id": "b", "x": 1, "label": "B"}]}], "s": 2});
        let theirs = json!({"pages": [{"id": "p", "modules": [{"id": "a", "x": 0, "label": "A"}, {"id": "b", "x": 2.5, "label": "B"}]}], "s": 1});
        let merged = merge3(Some(&base), Some(&mine), Some(&theirs)).unwrap();
        assert_eq!(
            merged,
            json!({"pages": [{"id": "p", "modules": [{"id": "a", "x": 0, "label": "A2"}, {"id": "b", "x": 2.5, "label": "B"}]}], "s": 2})
        );
    }

    #[test]
    fn deletions_and_additions() {
        let base = json!([{"id": "a"}, {"id": "b"}]);
        let mine = json!([{"id": "a"}]); // 主界面删了 b
        let theirs = json!([{"id": "a"}, {"id": "b"}, {"id": "c"}]); // 文件里多了 c
        assert_eq!(merge3(Some(&base), Some(&mine), Some(&theirs)).unwrap(), json!([{"id": "a"}, {"id": "c"}]));
        // 动作列表（没有 id）当成一个值：两边都改了以主界面为准
        let base = json!({"click": [{"type": "key", "key": "A"}]});
        let mine = json!({"click": [{"type": "key", "key": "B"}]});
        let theirs = json!({"click": []});
        assert_eq!(merge3(Some(&base), Some(&mine), Some(&theirs)).unwrap(), mine);
    }
}
