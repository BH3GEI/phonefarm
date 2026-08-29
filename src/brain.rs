//! 唯一模型调用函数。多 provider 按序 failover;HTTP 走 curl 子进程(绕 WAF 指纹问题)。
//! 本层只做 消息→原文回复;JSON 解析由调用方按记录契约执行。
use serde::Deserialize;
use std::process::Command;
use std::time::Instant;

#[derive(Clone, Deserialize)]
pub struct ProviderCfg {
    pub name: String,
    pub url: String,
    pub model: String,
    pub key_env: String,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// 智谱系: 请求体加 thinking:{type:disabled}
    #[serde(default)]
    pub thinking_disable: bool,
    /// 保留字段: v0.2 提示词统一要求 0~999 输出,不再按 provider 换算
    #[serde(default)]
    pub coord_norm: Option<i32>,
    /// 附加请求体字段(如 sglang 的 chat_template_kwargs 关思考)
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
    /// 直连(不走本机http代理)。国内端点走代理反而抖(实测http 000多发自代理隧道)。
    #[serde(default)]
    pub direct: bool,
}

pub struct CallOut {
    pub text: String,
    pub by: String,
    pub ms: u64,
}

pub struct Brain {
    providers: Vec<ProviderCfg>,
    pub calls: u32,
    fails: std::collections::HashMap<String, u32>, // 熔断: 本进程内硬失败计数
    tmp: String,
}

/// 按字符(非字节)截断,避免切在多字节 UTF-8 中间导致 panic
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

impl Brain {
    pub fn new(providers: Vec<ProviderCfg>, tmp: String) -> Self {
        Brain { providers, calls: 0, fails: Default::default(), tmp }
    }

    /// 回复无法按契约解析时,调用方记该 provider 一次硬失败
    pub fn blame(&mut self, name: &str) {
        *self.fails.entry(name.into()).or_insert(0) += 1;
    }

    /// 唯一模型调用函数。imgs 非空时只走视觉 provider。
    /// skip: 跳过链头 N 家可用 provider(上一家回复无法解析时换人重问)。
    pub fn call(&mut self, system: &str, user: &str, imgs: &[&str], max_tokens: u32, skip: usize)
        -> Result<CallOut, String>
    {
        self.calls += 1;
        let need_vision = !imgs.is_empty();
        let user_content = if need_vision {
            let mut parts = vec![serde_json::json!({"type":"text","text":user})];
            for p in imgs {
                let img = b64(&std::fs::read(p).map_err(|e| e.to_string())?);
                parts.push(serde_json::json!({"type":"image_url",
                    "image_url":{"url":format!("data:image/jpeg;base64,{img}")}}));
            }
            serde_json::json!(parts)
        } else {
            serde_json::json!(user)
        };
        let base = serde_json::json!({
            "max_tokens": max_tokens, "temperature": 0,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user_content}
            ]
        });
        for _round in 0..3 {
            if _round > 0 {
                // 冷静期: 全链失败多为限流(429/空响应)。实测窗口时长不定(1分钟~几十分钟),
                // 两级冷静(45s/90s)让中场解封的局能活下来,而不是45秒后就判死。
                let cool = 45 * _round as u64;
                println!("      (全链失败,冷静{cool}秒后再试一轮)");
                std::thread::sleep(std::time::Duration::from_secs(cool));
            }
            let fails = self.fails.clone();
            // 内容过滤(1301)是确定性拒绝:同一画面换 provider/重试都不会过,
            // 不值得冷静等待;全链皆过滤时立即上报,由调用方盲移离开该画面。
            let mut tried = false;
            let mut cf_only = true;
            for p in self.providers.clone().iter()
                .filter(|p| !p.url.is_empty()
                    && (!need_vision || p.vision)
                    && *fails.get(&p.name).unwrap_or(&0) < 2)
                .skip(skip)
            {
                let key = match std::env::var(&p.key_env) {
                    Ok(k) if !k.is_empty() => k,
                    _ => continue,
                };
                let mut b = base.clone();
                b["model"] = serde_json::json!(p.model);
                if p.thinking_disable {
                    b["thinking"] = serde_json::json!({"type": "disabled"});
                }
                if let (Some(serde_json::Value::Object(ex)), Some(bo)) = (p.extra.clone(), b.as_object_mut()) {
                    for (k, v) in ex { bo.insert(k, v); }
                }
                let t = Instant::now();
                match self.post(p, &key, &b) {
                    Ok(text) => {
                        self.fails.insert(p.name.clone(), 0);
                        return Ok(CallOut { text, by: p.name.clone(), ms: t.elapsed().as_millis() as u64 });
                    }
                    Err(e) => {
                        tried = true;
                        if e.starts_with("内容过滤") {
                            // 内容被安全审核拒绝: 不是通道的错,不计熔断(否则下一张正常画面也没人接)
                        } else {
                            cf_only = false;
                            *self.fails.entry(p.name.clone()).or_insert(0) += 1;
                        }
                        println!("      ({} 失败: {})", p.name, clip(&e, 90));
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                    }
                }
            }
            if tried && cf_only {
                return Err("内容过滤".into());
            }
        }
        Err("所有可用 provider 都失败".into())
    }

    fn post(&self, p: &ProviderCfg, key: &str, body: &serde_json::Value) -> Result<String, String> {
        let bp = format!("{}/_req.json", self.tmp);
        std::fs::write(&bp, serde_json::to_string(body).unwrap()).map_err(|e| e.to_string())?;
        let timeout = p.timeout_s.unwrap_or(30).to_string();
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "--max-time", &timeout, "-w", "\n%{http_code}", &p.url,
                  "-H", &format!("Authorization: Bearer {key}"),
                  "-H", "Content-Type: application/json",
                  "-d", &format!("@{bp}")]);
        if p.direct {
            cmd.arg("--noproxy").arg("*");
        }
        let out = cmd.output().map_err(|e| e.to_string())?;
        // 末行是 -w 追加的 HTTP 状态码
        let raw = String::from_utf8_lossy(&out.stdout).to_string();
        let (resp_text, code) = match raw.rfind('\n') {
            Some(i) => (raw[..i].to_string(), raw[i + 1..].trim().to_string()),
            None => (raw.clone(), String::new()),
        };
        if code == "429" {
            return Err("限流429".into());
        }
        if resp_text.trim().is_empty() {
            return Err(format!("空响应(http {code})"));
        }
        let resp: serde_json::Value =
            serde_json::from_str(&resp_text).map_err(|e| format!("非JSON响应: {e}"))?;
        // 内容安全审核(输入侧): 确定性拒绝,换 provider/重试都不过,调用方只能换画面
        if resp["error"]["code"].as_str() == Some("1301") || resp.to_string().contains("contentFilter") {
            return Err("内容过滤1301:输入被安全审核拒绝".into());
        }
        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| format!("无content: {}", clip(&resp.to_string(), 150)))?;
        if content.trim().is_empty() {
            return Err("空content".into());
        }
        Ok(content.to_string())
    }
}
