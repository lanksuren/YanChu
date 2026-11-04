//! 润色历史：每次落字的「识别 → 小润色 → 最终」几段，按天落盘。
//!
//! 为什么要落盘：用户要能翻「昨天那条为什么被改成了这样」。
//! 存在 `%APPDATA%\YanChu\history\YYYY-MM-DD.jsonl`（可用 `YANCHU_HISTORY_DIR` 覆盖），
//! 一天一个文件，控制面板按日期翻。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 单次返回上限。一天说几百句也够翻。
const CAPACITY: usize = 500;

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    /// epoch 秒，排序用
    pub at: u64,
    /// 本地日期 YYYY-MM-DD
    pub date: String,
    /// 本地时间 HH:MM:SS
    pub time: String,
    /// 识别出来的原始文本
    pub raw: String,
    /// 只过热词表（拼音/编辑距离）的结果
    pub corrected: String,
    /// 过完小润色（词表 + 规则层）的结果
    pub ruled: String,
    /// 最终落进输入框的文本（可能经过大润色）
    pub final_text: String,
    /// 是否用上了大润色
    pub used_llm: bool,
    /// standard / deep
    pub depth: String,
    /// 从松手到落字的耗时
    pub ms: u64,
}

#[derive(Default)]
pub struct History;

/// 历史目录。`YANCHU_HISTORY_DIR` 优先，其次 `%APPDATA%\YanChu\history`。
fn dir() -> PathBuf {
    if let Ok(p) = std::env::var("YANCHU_HISTORY_DIR") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("YanChu").join("history")
}

impl History {
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        raw: String,
        corrected: String,
        ruled: String,
        final_text: String,
        used_llm: bool,
        depth: String,
        ms: u64,
    ) {
        let now = crate::devlog::local_now();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let time = now
            .iso
            .split('T')
            .nth(1)
            .map(|t| t.split('.').next().unwrap_or(t).to_string())
            .unwrap_or_default();
        let entry = Entry {
            at,
            date: now.date,
            time,
            raw,
            corrected,
            ruled,
            final_text,
            used_llm,
            depth,
            ms,
        };
        if let Err(e) = append_to(&dir(), &entry) {
            log::warn!("写历史失败（不影响落字）: {e}");
        }
    }

    /// 某一天的记录，最新的在前。`date` 为空表示今天。
    pub fn list(&self, date: Option<&str>) -> Vec<Entry> {
        let today = crate::devlog::local_now().date;
        let date = date.filter(|d| !d.trim().is_empty()).unwrap_or(&today);
        let mut entries = load_from(&dir(), date);
        entries.sort_by_key(|e| std::cmp::Reverse(e.at));
        entries.truncate(CAPACITY);
        entries
    }

    /// 有记录的所有日期，最新在前：`[(YYYY-MM-DD, 条数)]`
    pub fn days(&self) -> Vec<(String, usize)> {
        let mut days = days_from(&dir());
        days.sort_by(|a, b| b.0.cmp(&a.0));
        days
    }

    /// 清掉某一天；`date` 为空表示今天。
    pub fn clear(&mut self, date: Option<&str>) {
        let today = crate::devlog::local_now().date;
        let date = date.filter(|d| !d.trim().is_empty()).unwrap_or(&today);
        let path = dir().join(format!("{date}.jsonl"));
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("清空历史失败: {e}");
            }
        }
    }
}

fn append_to(dir: &Path, entry: &Entry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.jsonl", entry.date));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(entry).unwrap_or_default();
    writeln!(file, "{line}")
}

fn load_from(dir: &Path, date: &str) -> Vec<Entry> {
    let path = dir.join(format!("{}.jsonl", date));
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

fn days_from(dir: &Path) -> Vec<(String, usize)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let date = name.strip_suffix(".jsonl")?.to_string();
            let count = std::fs::read_to_string(e.path())
                .ok()
                .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
                .unwrap_or(0);
            Some((date, count))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "yanchu-history-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn sample(date: &str, at: u64, raw: &str) -> Entry {
        Entry {
            at,
            date: date.into(),
            time: "10:00:00".into(),
            raw: raw.into(),
            corrected: raw.into(),
            ruled: format!("{raw}。"),
            final_text: format!("{raw}。"),
            used_llm: false,
            depth: "standard".into(),
            ms: 120,
        }
    }

    #[test]
    fn appends_and_reads_back() {
        let dir = temp_dir("roundtrip");
        append_to(&dir, &sample("2026-11-03", 100, "第一句")).unwrap();
        append_to(&dir, &sample("2026-11-03", 200, "第二句")).unwrap();
        let got = load_from(&dir, "2026-11-03");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].raw, "第一句");
        assert_eq!(got[1].final_text, "第二句。");
    }

    #[test]
    fn lists_days_with_counts() {
        let dir = temp_dir("days");
        append_to(&dir, &sample("2026-11-02", 1, "昨天")).unwrap();
        append_to(&dir, &sample("2026-11-03", 2, "今天")).unwrap();
        append_to(&dir, &sample("2026-11-03", 3, "今天第二句")).unwrap();
        let mut days = days_from(&dir);
        days.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            days,
            vec![("2026-11-02".to_string(), 1), ("2026-11-03".to_string(), 2)]
        );
    }

    #[test]
    fn missing_day_is_empty_not_panic() {
        let dir = temp_dir("missing");
        assert!(load_from(&dir, "2020-01-01").is_empty());
    }
}
