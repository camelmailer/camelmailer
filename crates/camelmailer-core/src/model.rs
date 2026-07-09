//! The core domain model, ported from the ActiveRecord models in
//! `app/models`. These are plain data types; persistence is behind the
//! [`crate::store::Store`] trait so the SMTP server and admin API can be
//! tested without a database.

use serde::Serialize;

pub type Id = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Organization {
    pub id: Id,
    pub uuid: String,
    pub name: String,
    pub permalink: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Server {
    pub id: Id,
    pub uuid: String,
    pub organization_id: Id,
    pub name: String,
    pub permalink: String,
    pub token: String,
    pub mode: ServerMode,
    pub suspended: bool,
    pub suspension_reason: Option<String>,
    pub privacy_mode: bool,
    pub log_smtp_data: bool,
    /// Whether the Sender header may authenticate a message in addition to From
    pub allow_sender: bool,
}

impl Server {
    pub fn full_permalink(&self, organization: &Organization) -> String {
        format!("{}/{}", organization.permalink, self.permalink)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ServerMode {
    Live,
    Development,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Domain {
    pub id: Id,
    pub uuid: String,
    pub owner: DomainOwner,
    pub name: String,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "id")]
pub enum DomainOwner {
    Organization(Id),
    Server(Id),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Route {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub domain_id: Option<Id>,
    pub name: String,
    pub token: String,
    pub mode: RouteMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouteMode {
    Endpoint,
    Accept,
    Hold,
    Bounce,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Credential {
    pub id: Id,
    pub uuid: String,
    pub server_id: Id,
    pub credential_type: CredentialType,
    pub name: String,
    /// For SMTP/API credentials this is the secret key; for SMTP-IP
    /// credentials it is a CIDR (e.g. `1.0.0.0/8`).
    pub key: String,
    pub hold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CredentialType {
    #[serde(rename = "SMTP")]
    Smtp,
    #[serde(rename = "API")]
    Api,
    #[serde(rename = "SMTP-IP")]
    SmtpIp,
}

/// A route joined with its server and domain name — what the SMTP session
/// needs in one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoute {
    pub route: Route,
    pub server: Server,
    /// The name of the route's domain (`route.domain.name` in Ruby)
    pub domain_name: String,
}
