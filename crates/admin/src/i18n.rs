//! Multi-language compile-time translation catalogs and locale resolution.
//!
//! Spec: ./docs/srdd/6. SRDD - Functional Requirements.md#611-web-ui-and-audit-logging-ui--aud (UI-5, UI-6)
//! Spec: ./docs/srdd/14.y. SRDD - Accessibility (WCAG 2.1 AA), Keyboard Navigation, Internationalization, and Responsive Breakpoints.md#14y3

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I18nCatalog {
    pub personal_workspace: &'static str,
    pub admin_console: &'static str,
    pub ready: &'static str,
    pub sign_out: &'static str,
    pub overview: &'static str,
    pub client_keys: &'static str,
    pub providers: &'static str,
    pub sharing: &'static str,
    pub routing_matrix: &'static str,
    pub active_sessions: &'static str,
    pub logs: &'static str,
    pub usage_ledger: &'static str,
}

pub fn get_catalog(locale: &str) -> I18nCatalog {
    match locale {
        "id" => I18nCatalog {
            personal_workspace: "Ruang Kerja Pribadi",
            admin_console: "Konsol Admin",
            ready: "Siap",
            sign_out: "Keluar",
            overview: "Ringkasan",
            client_keys: "Kunci Klien",
            providers: "Penyedia & Kunci",
            sharing: "Berbagi",
            routing_matrix: "Matriks Perutean",
            active_sessions: "Sesi Aktif",
            logs: "Log",
            usage_ledger: "Buku Besar Penggunaan",
        },
        "zh-CN" | "zh" => I18nCatalog {
            personal_workspace: "个人工作区",
            admin_console: "管理控制台",
            ready: "就绪",
            sign_out: "退出登录",
            overview: "概览",
            client_keys: "客户端密钥",
            providers: "提供商与密钥",
            sharing: "共享",
            routing_matrix: "路由矩阵",
            active_sessions: "活跃会话",
            logs: "日志",
            usage_ledger: "使用账本",
        },
        _ => I18nCatalog {
            personal_workspace: "Personal Workspace",
            admin_console: "Admin Console",
            ready: "Ready",
            sign_out: "Sign Out",
            overview: "Overview",
            client_keys: "Client Keys",
            providers: "Providers & Keys",
            sharing: "Sharing",
            routing_matrix: "Routing Matrix",
            active_sessions: "Active Sessions",
            logs: "Logs",
            usage_ledger: "Usage Ledger",
        },
    }
}

/// Resolve effective locale from explicit user preference with Accept-Language fallback.
pub fn resolve_locale(accept_language: Option<&str>, user_pref: Option<&str>) -> &'static str {
    if let Some(pref) = user_pref {
        match pref {
            "id" => return "id",
            "zh-CN" | "zh" => return "zh-CN",
            _ => return "en",
        }
    }

    if let Some(accept) = accept_language {
        let lower = accept.to_ascii_lowercase();
        if lower.starts_with("id") || lower.contains(",id") {
            return "id";
        }
        if lower.starts_with("zh") || lower.contains(",zh") {
            return "zh-CN";
        }
    }

    "en"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i18n_catalogs_and_locale_resolution() {
        let en = get_catalog("en");
        assert_eq!(en.personal_workspace, "Personal Workspace");
        assert_eq!(en.sign_out, "Sign Out");

        let id = get_catalog("id");
        assert_eq!(id.personal_workspace, "Ruang Kerja Pribadi");
        assert_eq!(id.sign_out, "Keluar");

        let zh = get_catalog("zh-CN");
        assert_eq!(zh.personal_workspace, "个人工作区");
        assert_eq!(zh.sign_out, "退出登录");

        assert_eq!(resolve_locale(None, Some("id")), "id");
        assert_eq!(resolve_locale(None, Some("zh-CN")), "zh-CN");
        assert_eq!(resolve_locale(Some("id-ID,id;q=0.9"), None), "id");
        assert_eq!(resolve_locale(Some("zh-CN,zh;q=0.9"), None), "zh-CN");
        assert_eq!(resolve_locale(Some("en-US,en;q=0.9"), None), "en");
    }
}
