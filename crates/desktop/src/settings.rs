use std::path::Path;

use serde::{Deserialize, Serialize};

/// 桌面壳自身设置（独立于 core 数据）。relay_url 默认空串，
/// 由用户在设置页填写——与 macOS SettingsStore 现状一致。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub relay_url: String,
    #[serde(default)]
    pub autostart: bool,
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

/// 与 crates/core/src/pairing.rs 的 `validate_server`（pub(crate)，不可复用）
/// 规则镜像：wss:// 一律允许；ws:// 仅 debug 构建；拒绝空 authority、
/// userinfo、fragment、空白/控制字符。
pub fn validate_relay_url(url: &str) -> Result<(), &'static str> {
    let rest = if let Some(rest) = url.strip_prefix("wss://") {
        rest
    } else if cfg!(debug_assertions) {
        url.strip_prefix("ws://").ok_or("需要 wss:// 开头的 relay 地址")?
    } else {
        return Err("需要 wss:// 开头的 relay 地址");
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or("地址格式无效")?;
    let valid = !authority.is_empty()
        && !authority.contains('@')
        && !url.contains('#')
        && url
            .bytes()
            .all(|b| !b.is_ascii_whitespace() && !b.is_ascii_control());
    if valid { Ok(()) } else { Err("relay 地址无效") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_wss_url() {
        assert!(validate_relay_url("wss://sync.example.com/ws").is_ok());
    }

    #[test]
    fn rejects_non_wss_in_release_rules() {
        // 校验规则与 core validate_server 对齐：非 wss 一律拒绝
        // （core 中 ws:// 仅 debug 放行；本函数按 release 规则拒绝，
        //  本地联调用 ws:// 时走 cfg!(debug_assertions) 分支）
        assert!(validate_relay_url("https://sync.example.com").is_err());
        assert!(validate_relay_url("sync.example.com").is_err());
        assert!(validate_relay_url("").is_err());
    }

    #[test]
    fn rejects_whitespace_and_userinfo() {
        assert!(validate_relay_url("wss://user@sync.example.com").is_err());
        assert!(validate_relay_url("wss://sync.example.com/w s").is_err());
    }

    #[test]
    fn rejects_fragment() {
        assert!(validate_relay_url("wss://sync.example.com/ws#x").is_err());
    }

    #[test]
    fn ws_allowed_only_in_debug() {
        assert_eq!(
            validate_relay_url("ws://127.0.0.1:8787/ws").is_ok(),
            cfg!(debug_assertions)
        );
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            relay_url: "wss://sync.example.com/ws".to_owned(),
            autostart: true,
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("nope.json")),
            Settings::default()
        );
    }

    #[test]
    fn load_corrupt_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }
}
