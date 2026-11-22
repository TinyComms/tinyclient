//! Microsoft 365 / Teams sovereign cloud endpoints and per-account host allow-lists.
//!
//! Each cloud (Worldwide, US Government GCC High, US Government DoD, and China operated by
//! 21Vianet) has its own login authority, Teams front door, token audiences, trouter host,
//! SharePoint host, and media store hosts. Allow-lists are strictly scoped per cloud: a
//! sovereign session never sends tokens to or accepts service addresses on a worldwide host,
//! and a worldwide session never accepts sovereign hosts.

use protocol::AccountKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Which Microsoft 365 cloud a Teams tenant belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamsCloud {
    /// Commercial / Worldwide (including GCC Moderate).
    #[default]
    Worldwide,
    /// US Government GCC High.
    GccHigh,
    /// US Department of Defense (DoD / IL5).
    Dod,
    /// Microsoft 365 operated by 21Vianet in China.
    China,
}

/// Token audience selector resolved against a cloud's own endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Audience {
    Spaces,
    Aggregator,
    Presence,
    Graph,
    /// Microsoft Search, for server-side message search.
    Substrate,
}

/// Fixed endpoints and host allow-lists for a single Microsoft 365 cloud.
#[derive(Debug, Clone, Copy)]
pub struct CloudEndpoints {
    pub login: &'static str,
    pub teams: &'static str,
    pub spaces: &'static str,
    pub aggregator: &'static str,
    pub presence: &'static str,
    pub graph: &'static str,
    pub trouter: &'static str,
    /// Service host suffixes trusted to receive tokens for this cloud.
    pub ours: &'static [&'static str],
    /// Media-store (AMS / asyncgw) host suffixes taking `skype_token`.
    pub media_hosts: &'static [&'static str],
    /// Open (unauthenticated) media download host suffixes for `blobs/fetch.rs`.
    pub fetch_open: &'static [&'static str],
    /// Session-authenticated media download host suffixes for `blobs/fetch.rs`.
    pub fetch_session: &'static [&'static str],
    /// SharePoint host suffixes mapped to Graph `/shares/u!...` downloads.
    pub sharepoint: &'static [&'static str],
    /// Upload target host suffixes for `upload.rs`.
    pub upload_hosts: &'static [&'static str],
}

impl CloudEndpoints {
    pub(super) fn audience(&self, audience: Audience) -> &'static str {
        match audience {
            Audience::Spaces => self.spaces,
            Audience::Aggregator => self.aggregator,
            Audience::Presence => self.presence,
            Audience::Graph => self.graph,
            // shortcut: the worldwide Substrate host for every cloud, so search only works worldwide; give
            // `CloudEndpoints` a `substrate` field when a sovereign cloud's search host is verified.
            Audience::Substrate => super::SUBSTRATE,
        }
    }
}

pub const WORLDWIDE: CloudEndpoints = CloudEndpoints {
    login: "https://login.microsoftonline.com",
    teams: "https://teams.microsoft.com",
    spaces: "https://api.spaces.skype.com",
    aggregator: "https://chatsvcagg.teams.microsoft.com",
    presence: "https://presence.teams.microsoft.com",
    graph: "https://graph.microsoft.com",
    trouter: "https://go.trouter.teams.microsoft.com",
    ours: &[
        "teams.microsoft.com",
        "skype.com",
        "teams.cloud.microsoft",
        "teams.office.com",
        "graph.microsoft.com",
    ],
    media_hosts: &["asm.skype.com", "asyncgw.teams.microsoft.com"],
    fetch_open: &["giphy.com", "sharepoint.com"],
    fetch_session: &[
        "asm.skype.com",
        "asyncgw.teams.microsoft.com",
        "teams.microsoft.com",
        "graph.microsoft.com",
    ],
    sharepoint: &["sharepoint.com"],
    upload_hosts: &[
        "graph.microsoft.com",
        "sharepoint.com",
        "asm.skype.com",
        "asyncgw.teams.microsoft.com",
        "teams.microsoft.com",
    ],
};

pub const GCC_HIGH: CloudEndpoints = CloudEndpoints {
    login: "https://login.microsoftonline.us",
    teams: "https://gov.teams.microsoft.us",
    // The published GCC High front door is also the token audience. Microsoft does not publish
    // a GCC High Skype spaces host.
    spaces: "https://gov.teams.microsoft.us",
    aggregator: "https://chatsvcagg.gov.teams.microsoft.us",
    presence: "https://presence.gov.teams.microsoft.us",
    graph: "https://graph.microsoft.us",
    trouter: "https://go.trouter.gov.teams.microsoft.us",
    ours: &["gov.teams.microsoft.us", "graph.microsoft.us"],
    media_hosts: &["asyncgw.gov.teams.microsoft.us"],
    fetch_open: &["sharepoint.us"],
    fetch_session: &[
        "asyncgw.gov.teams.microsoft.us",
        "gov.teams.microsoft.us",
        "graph.microsoft.us",
    ],
    sharepoint: &["sharepoint.us"],
    upload_hosts: &[
        "graph.microsoft.us",
        "sharepoint.us",
        "asyncgw.gov.teams.microsoft.us",
        "gov.teams.microsoft.us",
    ],
};

pub const DOD: CloudEndpoints = CloudEndpoints {
    login: "https://login.microsoftonline.us",
    teams: "https://dod.teams.microsoft.us",
    // The published DoD front door is also the token audience. Microsoft does not publish a
    // DoD Skype spaces host.
    spaces: "https://dod.teams.microsoft.us",
    aggregator: "https://chatsvcagg.dod.teams.microsoft.us",
    presence: "https://presence.dod.teams.microsoft.us",
    graph: "https://dod-graph.microsoft.us",
    trouter: "https://go.trouter.dod.teams.microsoft.us",
    ours: &["dod.teams.microsoft.us", "dod-graph.microsoft.us"],
    media_hosts: &["asyncgw.dod.teams.microsoft.us"],
    fetch_open: &["sharepoint-mil.us"],
    fetch_session: &[
        "asyncgw.dod.teams.microsoft.us",
        "dod.teams.microsoft.us",
        "dod-graph.microsoft.us",
    ],
    sharepoint: &["sharepoint-mil.us"],
    upload_hosts: &[
        "dod-graph.microsoft.us",
        "sharepoint-mil.us",
        "asyncgw.dod.teams.microsoft.us",
        "dod.teams.microsoft.us",
    ],
};

pub const CHINA: CloudEndpoints = CloudEndpoints {
    login: "https://login.chinacloudapi.cn",
    teams: "https://teams.microsoftonline.cn",
    // The published 21Vianet front door is also the token audience. Microsoft publishes
    // teams.microsoftonline.cn, not teams.microsoft.cn or a China Skype host.
    spaces: "https://teams.microsoftonline.cn",
    aggregator: "https://chatsvcagg.teams.microsoftonline.cn",
    presence: "https://presence.teams.microsoftonline.cn",
    graph: "https://microsoftgraph.chinacloudapi.cn",
    trouter: "https://go.trouter.teams.microsoftonline.cn",
    ours: &["teams.microsoftonline.cn", "microsoftgraph.chinacloudapi.cn"],
    media_hosts: &["asyncgw.teams.microsoftonline.cn"],
    fetch_open: &["sharepoint.cn"],
    fetch_session: &[
        "asyncgw.teams.microsoftonline.cn",
        "teams.microsoftonline.cn",
        "microsoftgraph.chinacloudapi.cn",
    ],
    sharepoint: &["sharepoint.cn"],
    upload_hosts: &[
        "microsoftgraph.chinacloudapi.cn",
        "sharepoint.cn",
        "asyncgw.teams.microsoftonline.cn",
        "teams.microsoftonline.cn",
    ],
};

impl TeamsCloud {
    pub fn parse(raw: &str) -> TeamsCloud {
        match raw.trim().to_ascii_lowercase().as_str() {
            "gcc_high" | "gcchigh" | "gcc-high" | "usgov" | "usgov_high" | "usgovhigh" => TeamsCloud::GccHigh,
            "dod" | "usgov_dod" | "usgovdod" | "usdod" => TeamsCloud::Dod,
            "china" | "21vianet" | "cn" | "gallatin" => TeamsCloud::China,
            _ => TeamsCloud::Worldwide,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TeamsCloud::Worldwide => "worldwide",
            TeamsCloud::GccHigh => "gcc_high",
            TeamsCloud::Dod => "dod",
            TeamsCloud::China => "china",
        }
    }

    pub fn endpoints(self) -> &'static CloudEndpoints {
        match self {
            TeamsCloud::Worldwide => &WORLDWIDE,
            TeamsCloud::GccHigh => &GCC_HIGH,
            TeamsCloud::Dod => &DOD,
            TeamsCloud::China => &CHINA,
        }
    }
}

fn registry() -> &'static Mutex<HashMap<AccountKey, TeamsCloud>> {
    static CLOUDS: OnceLock<Mutex<HashMap<AccountKey, TeamsCloud>>> = OnceLock::new();
    CLOUDS.get_or_init(Mutex::default)
}

/// Records the cloud an active Teams account belongs to so `blobs/fetch.rs` and `upload.rs`
/// scope their host allow-lists per account rather than expanding them globally.
pub(crate) fn register(account: &AccountKey, cloud: TeamsCloud) {
    crate::hub::lock(registry()).insert(account.clone(), cloud);
}

/// Clears a signed-out account's cloud registration.
pub(crate) fn unregister(account: &AccountKey) {
    crate::hub::lock(registry()).remove(account);
}

/// Returns the cloud registered for `account`, defaulting to [`TeamsCloud::Worldwide`].
pub(crate) fn cloud_for(account: &AccountKey) -> TeamsCloud {
    crate::hub::lock(registry())
        .get(account)
        .copied()
        .unwrap_or(TeamsCloud::Worldwide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{https_host, within};

    #[test]
    fn cloud_parsing_and_endpoints_are_isolated_per_cloud() {
        assert_eq!(TeamsCloud::parse(""), TeamsCloud::Worldwide);
        assert_eq!(TeamsCloud::parse("worldwide"), TeamsCloud::Worldwide);
        assert_eq!(TeamsCloud::parse("gcc_high"), TeamsCloud::GccHigh);
        assert_eq!(TeamsCloud::parse("GCCHigh"), TeamsCloud::GccHigh);
        assert_eq!(TeamsCloud::parse("dod"), TeamsCloud::Dod);
        assert_eq!(TeamsCloud::parse("21vianet"), TeamsCloud::China);
        assert_eq!(TeamsCloud::parse("china"), TeamsCloud::China);

        let gcc = TeamsCloud::GccHigh.endpoints();
        assert_eq!(gcc.login, "https://login.microsoftonline.us");
        assert_eq!(gcc.teams, "https://gov.teams.microsoft.us");
        assert_eq!(gcc.spaces, "https://gov.teams.microsoft.us");
        assert_eq!(gcc.graph, "https://graph.microsoft.us");
        assert_eq!(gcc.trouter, "https://go.trouter.gov.teams.microsoft.us");

        let is_ours = |ep: &CloudEndpoints, url: &str| {
            https_host(url).is_some_and(|h| ep.ours.iter().any(|d| within(h, d)))
        };
        // GCC High accepts only its own .us endpoints and refuses worldwide, DoD, and China.
        assert!(is_ours(gcc, "https://gov.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(is_ours(gcc, "https://usgov.ng.msg.gov.teams.microsoft.us/v1/users/ME"));
        assert!(is_ours(gcc, "https://graph.microsoft.us/v1.0/me"));
        assert!(!is_ours(gcc, "https://teams.microsoft.com/api/authsvc/v1.0/authz"));
        assert!(!is_ours(gcc, "https://graph.microsoft.com/v1.0/me"));
        assert!(!is_ours(gcc, "https://dod.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(!is_ours(gcc, "https://teams.microsoft.cn/api/authsvc/v1.0/authz"));
        assert!(!is_ours(gcc, "https://teams.microsoftonline.cn/api/authsvc/v1.0/authz"));
        assert!(!is_ours(gcc, "https://api.fl.spaces.skype.us/"));
        assert!(!is_ours(gcc, "https://usgov-api.asm.skype.us/v1/objects/1"));

        // DoD accepts only DoD endpoints and refuses worldwide and GCC High.
        let dod = TeamsCloud::Dod.endpoints();
        assert_eq!(dod.spaces, "https://dod.teams.microsoft.us");
        assert!(is_ours(dod, "https://dod.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(is_ours(dod, "https://dod-graph.microsoft.us/v1.0/me"));
        assert!(!is_ours(dod, "https://gov.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(!is_ours(dod, "https://teams.microsoft.com/api/authsvc/v1.0/authz"));
        assert!(!is_ours(dod, "https://api.dod.spaces.skype.us/"));
        assert!(!is_ours(dod, "https://usgov-api.asm.skype.us/v1/objects/1"));

        // China accepts only published 21Vianet endpoints and refuses worldwide, US Gov, and teams.microsoft.cn.
        let cn = TeamsCloud::China.endpoints();
        assert_eq!(cn.teams, "https://teams.microsoftonline.cn");
        assert_eq!(cn.spaces, "https://teams.microsoftonline.cn");
        assert_eq!(cn.aggregator, "https://chatsvcagg.teams.microsoftonline.cn");
        assert!(is_ours(cn, "https://teams.microsoftonline.cn/api/authsvc/v1.0/authz"));
        assert!(is_ours(cn, "https://chatsvcagg.teams.microsoftonline.cn/v1/users/ME"));
        assert!(is_ours(cn, "https://microsoftgraph.chinacloudapi.cn/v1.0/me"));
        assert!(!is_ours(cn, "https://teams.microsoft.cn/api/authsvc/v1.0/authz"));
        assert!(!is_ours(cn, "https://teams.microsoft.com/api/authsvc/v1.0/authz"));
        assert!(!is_ours(cn, "https://gov.teams.microsoft.us/api/authsvc/v1.0/authz"));

        // Worldwide refuses sovereign clouds.
        let ww = TeamsCloud::Worldwide.endpoints();
        assert!(is_ours(ww, "https://teams.microsoft.com/api/authsvc/v1.0/authz"));
        assert!(!is_ours(ww, "https://gov.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(!is_ours(ww, "https://dod.teams.microsoft.us/api/authsvc/v1.0/authz"));
        assert!(!is_ours(ww, "https://teams.microsoft.cn/api/authsvc/v1.0/authz"));
        assert!(!is_ours(ww, "https://teams.microsoftonline.cn/api/authsvc/v1.0/authz"));
    }

    #[test]
    fn registry_tracks_cloud_per_account_without_global_expansion() {
        let gcc_acct = AccountKey { network: protocol::Network::Teams, account: "gcc-tenant-1".into() };
        let ww_acct = AccountKey { network: protocol::Network::Teams, account: "ww-tenant-1".into() };
        register(&gcc_acct, TeamsCloud::GccHigh);
        assert_eq!(cloud_for(&gcc_acct), TeamsCloud::GccHigh);
        assert_eq!(cloud_for(&ww_acct), TeamsCloud::Worldwide);
        unregister(&gcc_acct);
        assert_eq!(cloud_for(&gcc_acct), TeamsCloud::Worldwide);
    }
}
