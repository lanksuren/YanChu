//! 润色模型 / API 档案。
//!
//! 控制面板里可以保存多套配置，例如：
//! * 本地 Ollama：`qwen3.5:2b`、`qwen2.5:1.5b`
//! * OpenAI 兼容 API：DeepSeek、OpenAI、自建网关
//!
//! 列表存 `models/polish.json`；当前生效的那一套会同时写进 `models/llm.json`，
//! 这样 `llm.rs` 原有的读取路径不用改，运行时切模型也不需要重启。

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "polish.json";

fn default_timeout() -> u64 {
    900
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishProfile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `ollama` 或 `openai`
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolishSettings {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub profiles: Vec<PolishProfile>,
}

impl PolishProfile {
    pub fn normalized(mut self, index: usize) -> Self {
        let provider = self.provider.trim().to_ascii_lowercase();
        self.provider = if matches!(provider.as_str(), "openai" | "api" | "openai-compatible") {
            "openai".to_string()
        } else if matches!(provider.as_str(), "ollama" | "local") {
            "ollama".to_string()
        } else if self.url.contains("11434") {
            "ollama".to_string()
        } else {
            "openai".to_string()
        };
        self.url = self.url.trim().trim_end_matches('/').to_string();
        self.key = self.key.trim().to_string();
        self.model = self.model.trim().to_string();
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            self.name = if self.model.is_empty() {
                "未命名配置".to_string()
            } else {
                self.model.clone()
            };
        }
        self.id = self.id.trim().to_string();
        if self.id.is_empty() {
            self.id = format!("profile-{}", index + 1);
        }
        self.timeout_ms = self.timeout_ms.clamp(300, 30_000);
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("配置名称不能为空".into());
        }
        if self.url.trim().is_empty() {
            return Err(format!("「{}」没有填接口地址", self.name));
        }
        if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
            return Err(format!(
                "「{}」的地址要以 http:// 或 https:// 开头",
                self.name
            ));
        }
        if self.model.trim().is_empty() {
            return Err(format!("「{}」没有填模型名", self.name));
        }
        if !matches!(self.provider.as_str(), "ollama" | "openai") {
            return Err(format!("「{}」的类型不支持", self.name));
        }
        Ok(())
    }

    pub fn is_local(&self) -> bool {
        self.provider == "ollama"
    }

    pub fn provider_label(&self) -> &'static str {
        if self.is_local() {
            "本地 Ollama"
        } else {
            "OpenAI 兼容 API"
        }
    }

    pub fn to_llm_config(&self, model_root: &Path) -> crate::llm::LlmConfig {
        crate::llm::LlmConfig {
            url: self.url.clone(),
            key: self.key.clone(),
            model: self.model.clone(),
            timeout_ms: self.timeout_ms,
            provider: if self.is_local() {
                crate::llm::Provider::Ollama
            } else {
                crate::llm::Provider::OpenAi
            },
            prompt_dir: model_root.join("prompts"),
        }
    }
}

impl PolishSettings {
    pub fn load(root: &Path) -> Self {
        let path = root.join(FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(settings) = serde_json::from_str::<PolishSettings>(&text) {
                return settings.normalized();
            }
            log::warn!("{} 解析失败，尝试从 llm.json 迁移", path.display());
        }
        Self::migrate_from_llm(root).normalized()
    }

    /// 老版本只有 `llm.json`：把它当成第一个配置，用户打开面板时能直接看到。
    fn migrate_from_llm(root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(root.join("llm.json")) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let url = v["url"].as_str().unwrap_or("").trim().to_string();
        if url.is_empty() {
            return Self::default();
        }
        let model = v["model"]
            .as_str()
            .unwrap_or("qwen3.5:2b")
            .trim()
            .to_string();
        let provider = v["provider"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                if url.contains("11434") {
                    "ollama".to_string()
                } else {
                    "openai".to_string()
                }
            });
        let profile = PolishProfile {
            id: "default".to_string(),
            name: model.clone(),
            provider,
            url,
            key: v["key"].as_str().unwrap_or("").to_string(),
            model,
            timeout_ms: v["timeout_ms"].as_u64().unwrap_or(900),
        }
        .normalized(0);
        Self {
            active: profile.id.clone(),
            profiles: vec![profile],
        }
    }

    pub fn normalized(self) -> Self {
        let mut profiles = Vec::with_capacity(self.profiles.len());
        for (i, profile) in self.profiles.into_iter().enumerate() {
            let mut profile = profile.normalized(i);
            // id 冲突时补后缀，避免界面里两个「使用中」
            let mut suffix = 2;
            while profiles.iter().any(|p: &PolishProfile| p.id == profile.id) {
                profile.id = format!("{}-{suffix}", profile.id);
                suffix += 1;
            }
            profiles.push(profile);
        }
        let active = if profiles.iter().any(|p| p.id == self.active) {
            self.active
        } else {
            profiles.first().map(|p| p.id.clone()).unwrap_or_default()
        };
        Self { active, profiles }
    }

    pub fn active_profile(&self) -> Option<&PolishProfile> {
        self.profiles.iter().find(|p| p.id == self.active)
    }

    pub fn validate(&self) -> Result<(), String> {
        for profile in &self.profiles {
            profile.validate()?;
        }
        if !self.active.is_empty() && self.active_profile().is_none() {
            return Err("当前启用的配置不存在".into());
        }
        Ok(())
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(root)?;
        let body = serde_json::to_string_pretty(&self.clone().normalized())?;
        std::fs::write(root.join(FILE), body)?;
        Ok(())
    }

    /// 把当前启用的配置写进 `llm.json`；没有启用项就删掉 llm.json（关闭大润色）。
    pub fn apply_active(&self, root: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(root)?;
        let path = root.join("llm.json");
        match self.active_profile() {
            Some(profile) => {
                let body = serde_json::json!({
                    "url": profile.url,
                    "key": profile.key,
                    "model": profile.model,
                    "timeout_ms": profile.timeout_ms,
                    "provider": profile.provider,
                });
                std::fs::write(path, serde_json::to_string_pretty(&body)?)?;
                log::info!(
                    "润色配置已生效: {}（{} / {}）",
                    profile.name,
                    profile.provider_label(),
                    profile.model
                );
            }
            None => {
                let _ = std::fs::remove_file(path);
                log::info!("没有启用的润色配置，大润色关闭");
            }
        }
        Ok(())
    }
}

/// 查 Ollama 本地已安装的模型名（给控制面板的下拉提示用）。
pub fn ollama_models(url: &str) -> anyhow::Result<Vec<String>> {
    let base = url.trim().trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    let endpoint = format!("{root}/api/tags");
    let response = ureq::get(&endpoint)
        .timeout(Duration::from_secs(4))
        .call()
        .map_err(|e| anyhow::anyhow!("连不上 Ollama（{endpoint}）：{e}"))?;
    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| anyhow::anyhow!("Ollama 返回的不是 JSON：{e}"))?;
    let mut names: Vec<String> = json["models"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|m| {
                    m["name"]
                        .as_str()
                        .or_else(|| m["model"].as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_provider_and_defaults() {
        let p = PolishProfile {
            id: String::new(),
            name: String::new(),
            provider: "API".into(),
            url: "https://example.com/v1/".into(),
            key: "  sk-1 ".into(),
            model: "chat".into(),
            timeout_ms: 10,
        }
        .normalized(2);
        assert_eq!(p.id, "profile-3");
        assert_eq!(p.name, "chat");
        assert_eq!(p.provider, "openai");
        assert_eq!(p.url, "https://example.com/v1");
        assert_eq!(p.key, "sk-1");
        assert_eq!(p.timeout_ms, 300);
    }

    #[test]
    fn empty_active_falls_back_to_first() {
        let settings = PolishSettings {
            active: "missing".into(),
            profiles: vec![PolishProfile {
                id: "a".into(),
                name: "A".into(),
                provider: "ollama".into(),
                url: "http://127.0.0.1:11434/v1".into(),
                key: String::new(),
                model: "qwen3.5:2b".into(),
                timeout_ms: 900,
            }],
        }
        .normalized();
        assert_eq!(settings.active, "a");
        assert!(settings.validate().is_ok());
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yanchu-model-profiles-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migrates_existing_llm_json() {
        let root = temp_root("migrate");
        std::fs::write(
            root.join("llm.json"),
            r#"{"url":"https://api.example.com/v1","key":"sk-x","model":"chat","timeout_ms":3000}"#,
        )
        .unwrap();
        let settings = PolishSettings::load(&root);
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.active, "default");
        assert_eq!(settings.profiles[0].provider, "openai");
        assert_eq!(settings.profiles[0].model, "chat");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saves_and_applies_active_profile() {
        let root = temp_root("apply");
        let settings = PolishSettings {
            active: "local".into(),
            profiles: vec![
                PolishProfile {
                    id: "local".into(),
                    name: "本地".into(),
                    provider: "ollama".into(),
                    url: "http://127.0.0.1:11434/v1".into(),
                    key: String::new(),
                    model: "qwen2.5:1.5b".into(),
                    timeout_ms: 900,
                },
                PolishProfile {
                    id: "api".into(),
                    name: "API".into(),
                    provider: "openai".into(),
                    url: "https://api.example.com/v1".into(),
                    key: "sk-x".into(),
                    model: "chat".into(),
                    timeout_ms: 4000,
                },
            ],
        }
        .normalized();
        settings.save(&root).unwrap();
        settings.apply_active(&root).unwrap();

        let llm: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("llm.json")).unwrap()).unwrap();
        assert_eq!(llm["model"], "qwen2.5:1.5b");
        assert_eq!(llm["provider"], "ollama");

        let mut api_active = settings.clone();
        api_active.active = "api".into();
        api_active.apply_active(&root).unwrap();
        let llm: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("llm.json")).unwrap()).unwrap();
        assert_eq!(llm["url"], "https://api.example.com/v1");
        assert_eq!(llm["provider"], "openai");
        let _ = std::fs::remove_dir_all(&root);
    }
}
