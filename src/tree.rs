//! 交互网(build_tree.py 离线产出的 tree.json)的运行时侧:
//! 页面识别(核心词身份证) + 熟路导航(goto: BFS 只走"熟且可回放"的边,零模型调用)。
//! 网是只读参考不是权威: 任何一跳落点对不上预期页,立即中断把画面交还模型。
use crate::device::Node;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Page {
    pub id: i64,
    pub name: String,
    pub core: Vec<String>,
    #[serde(default)]
    pub visits: u64,
}

#[derive(Deserialize, Clone)]
pub struct Edge {
    pub from: i64,
    pub label: String,
    pub to: i64,
    #[serde(default)]
    pub n: u64,
    #[serde(default)]
    pub ripe: bool,
}

#[derive(Deserialize)]
pub struct Tree {
    pub pages: Vec<Page>,
    pub edges: Vec<Edge>,
}

/// 与 build_tree.py 的 MATCH 同值: 核心词在场比例达到即认作该页
const MATCH: f64 = 0.8;

impl Tree {
    pub fn load(path: &str) -> Option<Tree> {
        let s = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn page(&self, id: i64) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// 页面身份证: 元素短文字(≤6字)命中某页核心词的比例,最高者 ≥MATCH 即认定。
    /// 信息流碎片只会多出文字、不会挤走骨架词, containment 天然抗噪。
    pub fn page_of(&self, els: &[Node]) -> Option<&Page> {
        let texts: std::collections::HashSet<&str> = els
            .iter()
            .map(|n| n.t.trim())
            .filter(|t| t.chars().count() <= 6)
            .collect();
        let mut best: (f64, usize) = (0.0, 0);
        for (i, p) in self.pages.iter().enumerate() {
            if p.core.is_empty() { continue; }
            let hit = p.core.iter().filter(|c| texts.contains(c.trim())).count();
            let score = hit as f64 / p.core.len() as f64;
            if score > best.0 { best = (score, i); }
        }
        (best.0 >= MATCH).then(|| &self.pages[best.1])
    }

    /// 可回放的边: 文字锚定的tap / 返回 / 滚动 / 回桌面。
    /// 点裸坐标(位置会漂)与"等待"(无导航意义)不参与回放。
    fn replayable(e: &Edge) -> bool {
        e.ripe
            && (e.label.starts_with("点[")
                || matches!(e.label.as_str(), "返回" | "上滑" | "下滑" | "回桌面"))
    }

    /// BFS 找熟路;from==to 返回空路(已在目的地)。
    pub fn route(&self, from: i64, to: i64) -> Option<Vec<Edge>> {
        if from == to { return Some(vec![]); }
        let mut prev: std::collections::HashMap<i64, (i64, usize)> = Default::default();
        let mut seen: std::collections::HashSet<i64> = [from].into_iter().collect();
        let mut queue = std::collections::VecDeque::from([from]);
        while let Some(cur) = queue.pop_front() {
            for (ei, e) in self.edges.iter().enumerate() {
                if e.from != cur || !Self::replayable(e) || seen.contains(&e.to) { continue; }
                prev.insert(e.to, (cur, ei));
                if e.to == to {
                    let mut path = Vec::new();
                    let mut node = to;
                    while node != from {
                        let (p, ei) = prev[&node];
                        path.push(self.edges[ei].clone());
                        node = p;
                    }
                    path.reverse();
                    return Some(path);
                }
                seen.insert(e.to);
                queue.push_back(e.to);
            }
        }
        None
    }

    /// 上下文本地小地图: 当前页 + 邻页熟路(只列可回放的,模型才知道 goto 能到哪)
    pub fn map_line(&self, cur: i64) -> Option<String> {
        let p = self.page(cur)?;
        let cuts: Vec<&Edge> =
            self.edges.iter().filter(|e| e.from == cur && Self::replayable(e)).collect();
        if cuts.is_empty() { return None; }
        let hops: Vec<String> = cuts.iter().take(6).map(|e| {
            let name = self.page(e.to).map(|q| crate::runtime::tcut(&q.name, 10)).unwrap_or_else(|| "?".into());
            format!("{}→P{}[{}]×{}", e.label, e.to, name, e.n)
        }).collect();
        Some(format!(
            "map: 当前P{}[{}](到访{}次) 熟路: {}",
            cur, crate::runtime::tcut(&p.name, 16), p.visits, hops.join(" ; ")
        ))
    }

    /// goto 目标解析: P5 / 5 / 页面名原文
    pub fn resolve(&self, t: &str) -> Option<i64> {
        let t = t.trim();
        if let Ok(id) = t.trim_start_matches('P').parse::<i64>() {
            if self.page(id).is_some() { return Some(id); }
        }
        self.pages.iter().find(|p| p.name == t).map(|p| p.id)
    }

    /// 边标签 → 按钮文字(仅 点[..] 型)
    pub fn tap_text(label: &str) -> Option<&str> {
        label.strip_prefix("点[").and_then(|s| s.strip_suffix(']'))
    }
}
