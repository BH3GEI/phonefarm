//! Python `json.dumps(..., ensure_ascii=False, indent=1)` 的逐字节复刻。
//!
//! loop_v1 的分析链原来是 Python, 下游 (报告、证据归档、其它 agent 的对照脚本) 已经
//! 按那份输出的确切字节在读。搬到 Rust 时口径不能变: 同一份输入必须出同一串字节,
//! 否则 golden 对照就不是对照, 是"看起来差不多"。
//!
//! 三处 Python 特有、serde_json 给不了的行为, 是这个模块存在的全部理由:
//!   1. 缩进 1 个空格 (serde_json 的 PrettyFormatter 固定 2 个);
//!   2. 浮点走 Python `repr` 的科学计数法阈值 —— 指数 < -4 或 >= 16 才用 `e`,
//!      且指数至少补到两位 (`1e-05`, 不是 ryu 的 `0.00001`);
//!   3. int 与 float 是两种东西 —— `3150` 与 `3150.0` 是不同的字节。
//!      Python `statistics.median` 对奇数个 int 返回 int, 偶数个返回 float,
//!      这个区别直接落在 `bw_median` 上, 不能抹平。
//!
//! 还有 `Infinity` / `NaN`: Python 照写, serde_json 写 `null`。变异系数在退化输入上
//! 真会返回 inf, 所以也得照写。
//!
//! 这些加起来就没法复用 serde_json 的 Serializer 了 (它在 f64 非有限时直接走 write_null,
//! 自定义 Formatter 也拦不住), 于是这里自带一个最小 JSON 写出器。

/// Python 侧 JSON 值。`Int` / `Float` 刻意分开 —— 见模块注释第 3 条。
#[derive(Debug, Clone, PartialEq)]
pub enum PyVal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<PyVal>),
    /// 对象按插入顺序输出 (Python 3.7+ dict 保序), 不排序。
    Obj(Vec<(String, PyVal)>),
}

impl From<f64> for PyVal {
    fn from(v: f64) -> Self {
        PyVal::Float(v)
    }
}
impl From<i64> for PyVal {
    fn from(v: i64) -> Self {
        PyVal::Int(v)
    }
}
impl From<usize> for PyVal {
    fn from(v: usize) -> Self {
        PyVal::Int(v as i64)
    }
}
impl From<bool> for PyVal {
    fn from(v: bool) -> Self {
        PyVal::Bool(v)
    }
}
impl From<String> for PyVal {
    fn from(v: String) -> Self {
        PyVal::Str(v)
    }
}
impl From<&str> for PyVal {
    fn from(v: &str) -> Self {
        PyVal::Str(v.to_string())
    }
}
impl<T: Into<PyVal>> From<Option<T>> for PyVal {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(x) => x.into(),
            None => PyVal::Null,
        }
    }
}
impl<T: Into<PyVal>> From<Vec<T>> for PyVal {
    fn from(v: Vec<T>) -> Self {
        PyVal::List(v.into_iter().map(Into::into).collect())
    }
}

/// `serde_json::Value` → `PyVal`, 保留 int/float 之分。
/// 归因那一步要把 summary.json 里的字段原样透传回输出, 数字是 `3150.0` 还是 `3150`
/// 必须跟着输入走 —— 这正是 Python `json.load` 的行为。
impl From<&serde_json::Value> for PyVal {
    fn from(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => PyVal::Null,
            serde_json::Value::Bool(b) => PyVal::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => PyVal::Int(i),
                None => PyVal::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            serde_json::Value::String(s) => PyVal::Str(s.clone()),
            serde_json::Value::Array(a) => PyVal::List(a.iter().map(PyVal::from).collect()),
            serde_json::Value::Object(o) => {
                PyVal::Obj(o.iter().map(|(k, v)| (k.clone(), PyVal::from(v))).collect())
            }
        }
    }
}

/// 按插入顺序建对象。`pyobj!{ "a" => 1i64, "b" => x }`
#[macro_export]
macro_rules! pyobj {
    ($($k:expr => $v:expr),* $(,)?) => {
        $crate::pyjson::PyVal::Obj(vec![ $( ($k.to_string(), $crate::pyjson::PyVal::from($v)) ),* ])
    };
}

/// Python `round(x, ndigits)`: 按**二进制真值**定点舍入, 平局取偶, 再取最近的 double。
///
/// Rust 的 `{:.*}` 走 flt2dec 精确模式, 舍入规则与 CPython 的 `_Py_dg_dtoa` 一致
/// (实测 2.675→2.67、33.53595→33.5359、0.125→0.12 全对得上), 所以"格式化到 n 位再
/// 解析回来"就是 Python `round` 的语义, 不是近似。
pub fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", ndigits, x).parse().unwrap_or(x)
}

/// Python `repr(float)`。
///
/// 与 ryu (serde_json 用的那个) 只在两处不同, 都在这里摆平:
///   · 何时转科学计数法: Python 是指数 < -4 或 >= 16, ryu 是 < -5;
///   · 指数位数: Python 至少两位 (`1e-05` / `1e+16`), ryu 不补。
/// 尾数本身两边都是最短可往返表示, 一致。
pub fn py_repr_f64(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    // `{:e}` 给最短可往返的科学计数法, 从中取十进制指数
    let sci = format!("{:e}", x);
    let (mant, exp) = match sci.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => return sci,
    };
    if (-4..16).contains(&exp) {
        // 定点区间: Display 在这个区间只出定点形式, 整数补 `.0`
        let s = format!("{x}");
        if s.contains('.') {
            s
        } else {
            format!("{s}.0")
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    }
}

/// Python `json.dumps` 的字符串转义 (`ensure_ascii=False`):
/// 只转义 `"`、`\` 与 C0 控制字符; `/` 不转义; 非 ASCII 原样输出。
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_val(out: &mut String, v: &PyVal, depth: usize) {
    match v {
        PyVal::Null => out.push_str("null"),
        PyVal::Bool(true) => out.push_str("true"),
        PyVal::Bool(false) => out.push_str("false"),
        PyVal::Int(i) => out.push_str(&i.to_string()),
        PyVal::Float(f) => out.push_str(&py_repr_f64(*f)),
        PyVal::Str(s) => write_str(out, s),
        PyVal::List(xs) => {
            if xs.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&" ".repeat(depth + 1));
                write_val(out, x, depth + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(depth));
            out.push(']');
        }
        PyVal::Obj(kvs) => {
            if kvs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, x)) in kvs.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&" ".repeat(depth + 1));
                write_str(out, k);
                out.push_str(": ");
                write_val(out, x, depth + 1);
            }
            out.push('\n');
            out.push_str(&" ".repeat(depth));
            out.push('}');
        }
    }
}

/// `json.dumps(v, ensure_ascii=False, indent=1)` —— 不含结尾换行。
pub fn dumps(v: &PyVal) -> String {
    let mut out = String::new();
    write_val(&mut out, v, 0);
    out
}

// ══════════════ Python 语义的统计小件 ══════════════

/// `statistics.median` 的 int 版: 奇数个返回**整数**, 偶数个返回两中位数的**浮点**均值。
/// 这个 int/float 之分会原样落到 JSON 字节上, 所以必须保住。
pub fn median_ints(xs: &[i64]) -> Option<PyVal> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_unstable();
    let n = s.len();
    Some(if n % 2 == 1 {
        PyVal::Int(s[n / 2])
    } else {
        PyVal::Float((s[n / 2 - 1] + s[n / 2]) as f64 / 2.0)
    })
}

/// `statistics.median` 的 float 版。
pub fn median_f64(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    Some(if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对照表全部来自本机 python3 的实际输出 (见 pyjson 模块注释)。
    #[test]
    fn repr_matches_python() {
        let cases: &[(f64, &str)] = &[
            (3150.0, "3150.0"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (1.2e-5, "1.2e-05"),
            (1e-7, "1e-07"),
            (5e-5, "5e-05"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.23e16, "1.23e+16"),
            (1e17, "1e+17"),
            (123456789012345680.0, "1.2345678901234568e+17"),
            (1e22, "1e+22"),
            (33.536, "33.536"),
            (0.1 + 0.2, "0.30000000000000004"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ];
        for (v, want) in cases {
            assert_eq!(&py_repr_f64(*v), want, "repr({v})");
        }
        assert_eq!(py_repr_f64(f64::NAN), "NaN");
    }

    #[test]
    fn round_matches_python() {
        // 都是 Python round() 的实际结果, 包括"看着该进位其实不进"的那几个
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(-2.675, 2), -2.67);
        assert_eq!(py_round(33.53595, 4), 33.5359);
        assert_eq!(py_round(33.53595, 2), 33.54);
        assert_eq!(py_round(1.0000050000001, 4), 1.0);
        assert_eq!(py_round(0.1 + 0.2, 4), 0.3);
        assert_eq!(py_round(1e-5, 4), 0.0);
        assert!(py_round(f64::INFINITY, 4).is_infinite());
    }

    #[test]
    fn dumps_matches_python_indent1() {
        let v = pyobj! {
            "a" => 1i64,
            "b" => pyobj!{ "c" => vec![1i64, 2], "d" => PyVal::Obj(vec![]) },
            "e" => PyVal::List(vec![]),
            "f" => PyVal::Null,
            "g" => "中",
        };
        assert_eq!(
            dumps(&v),
            "{\n \"a\": 1,\n \"b\": {\n  \"c\": [\n   1,\n   2\n  ],\n  \"d\": {}\n },\n \"e\": [],\n \"f\": null,\n \"g\": \"中\"\n}"
        );
    }

    #[test]
    fn str_escape_matches_python() {
        let v = PyVal::Str("a\"b\\c\nd\te/f\u{1}".to_string());
        assert_eq!(dumps(&v), "\"a\\\"b\\\\c\\nd\\te/f\\u0001\"");
    }

    #[test]
    fn median_keeps_int_float_distinction() {
        assert_eq!(median_ints(&[1, 3, 5]), Some(PyVal::Int(3)));
        assert_eq!(median_ints(&[1, 3, 5, 7]), Some(PyVal::Float(4.0)));
        assert_eq!(median_ints(&[]), None);
        assert_eq!(dumps(&median_ints(&[1, 3, 5]).unwrap()), "3");
        assert_eq!(dumps(&median_ints(&[1, 3, 5, 7]).unwrap()), "4.0");
    }

    #[test]
    fn value_passthrough_keeps_number_kind() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a":3150.0,"b":4800}"#).unwrap();
        assert_eq!(dumps(&PyVal::from(&v)), "{\n \"a\": 3150.0,\n \"b\": 4800\n}");
    }
}
